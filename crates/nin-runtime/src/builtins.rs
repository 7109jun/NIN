//! Builtin globals, builtin methods on core values, reflection, JSON conversion.

use crate::interp::*;
use crate::value::*;
use indexmap::IndexMap;
use nin_ast::*;
use nin_diag::suggest;
use std::cmp::Ordering;
use std::sync::Arc;

// ───────────────────────── argument helpers ─────────────────────────

pub fn a_str(ex: &mut Exec, args: &[Value], i: usize, f: &str) -> R<String> {
    match args.get(i) {
        Some(Value::Str(s)) => Ok(s.to_string()),
        Some(o) => ex.err("TypeError", format!("{}: argument {} must be string, got {}", f, i + 1, o.type_name())),
        None => ex.err("TypeError", format!("{}: missing argument {}", f, i + 1)),
    }
}

pub fn a_int(ex: &mut Exec, args: &[Value], i: usize, f: &str) -> R<i64> {
    match args.get(i) {
        Some(Value::Int(n)) => Ok(*n),
        Some(Value::Float(x)) if x.fract() == 0.0 => Ok(*x as i64),
        Some(o) => ex.err("TypeError", format!("{}: argument {} must be int, got {}", f, i + 1, o.type_name())),
        None => ex.err("TypeError", format!("{}: missing argument {}", f, i + 1)),
    }
}

pub fn a_list(ex: &mut Exec, args: &[Value], i: usize, f: &str) -> R<Vec<Value>> {
    match args.get(i) {
        Some(Value::List(l)) => Ok(lk(l).clone()),
        Some(o) => ex.err("TypeError", format!("{}: argument {} must be list, got {}", f, i + 1, o.type_name())),
        None => ex.err("TypeError", format!("{}: missing argument {}", f, i + 1)),
    }
}

pub fn a_fn(ex: &mut Exec, args: &[Value], i: usize, f: &str) -> R<Value> {
    match args.get(i) {
        Some(v) if v.is_callable() => Ok(v.clone()),
        Some(o) => ex.err("TypeError", format!("{}: argument {} must be a function, got {}", f, i + 1, o.type_name())),
        None => ex.err("TypeError", format!("{}: missing function argument", f)),
    }
}

pub fn cmp_values(ex: &mut Exec, a: &Value, b: &Value) -> R<Ordering> {
    let o = match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        _ => None,
    };
    match o {
        Some(o) => Ok(o),
        None => ex.err("TypeError", format!("cannot order {} and {}", a.type_name(), b.type_name())),
    }
}

fn truthy_loose(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Int(i) => *i != 0,
        Value::Float(f) => *f != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::List(l) => !lk(l).is_empty(),
        Value::Map(m) => !lk(m).is_empty(),
        _ => true,
    }
}

fn cond(ex: &mut Exec, v: Value) -> R<bool> {
    let sp = ex.cur_span;
    ex.truthy(v, sp)
}

// ───────────────────────── JSON conversion ─────────────────────────

pub fn value_to_json(v: &Value) -> Result<serde_json::Value, String> {
    value_to_json_d(v, 0)
}

fn value_to_json_d(v: &Value, d: usize) -> Result<serde_json::Value, String> {
    use serde_json::Value as J;
    if d > 64 {
        return Err("value nested too deeply (cycle?)".into());
    }
    Ok(match v {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Int(i) => J::from(*i),
        Value::Float(f) => match serde_json::Number::from_f64(*f) {
            Some(n) => J::Number(n),
            None => return Err("NaN/Infinity cannot be represented in JSON".into()),
        },
        Value::Str(s) => J::String(s.to_string()),
        Value::List(l) => {
            let items = lk(l).clone();
            let mut out = Vec::with_capacity(items.len());
            for i in &items {
                out.push(value_to_json_d(i, d + 1)?);
            }
            J::Array(out)
        }
        Value::Map(m) => {
            let items = lk(m).clone();
            let mut out = serde_json::Map::new();
            for (k, i) in &items {
                out.insert(k.clone(), value_to_json_d(i, d + 1)?);
            }
            J::Object(out)
        }
        Value::Object(o) => {
            if o.class.kind == ClassKind::Enum {
                return Ok(J::String(display_simple(lk(&o.fields).get("name").unwrap_or(&Value::Null))));
            }
            let items = lk(&o.fields).clone();
            let mut out = serde_json::Map::new();
            for (k, i) in &items {
                out.insert(k.clone(), value_to_json_d(i, d + 1)?);
            }
            J::Object(out)
        }
        other => return Err(format!("{} cannot be converted to JSON", other.type_name())),
    })
}

pub fn json_to_value(j: &serde_json::Value) -> Value {
    use serde_json::Value as J;
    match j {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => match n.as_i64() {
            Some(i) => Value::Int(i),
            None => Value::Float(n.as_f64().unwrap_or(f64::NAN)),
        },
        J::String(s) => Value::str(s),
        J::Array(a) => Value::list(a.iter().map(json_to_value).collect()),
        J::Object(o) => Value::map(o.iter().map(|(k, v)| (k.clone(), json_to_value(v))).collect()),
    }
}

pub fn deep_clone(v: &Value) -> Value {
    match v {
        Value::List(l) => Value::list(lk(l).iter().map(deep_clone).collect()),
        Value::Map(m) => Value::map(lk(m).iter().map(|(k, x)| (k.clone(), deep_clone(x))).collect()),
        other => other.clone(),
    }
}

// ───────────────────────── globals ─────────────────────────

fn def(interp: &Interp, name: &str, min: usize, max: Option<usize>, f: impl Fn(&mut Exec, &[Value]) -> R<Value> + Send + Sync + 'static) {
    interp.root.define(name, Native::value(name, min, max, f), true);
}

pub fn install_globals(interp: &Arc<Interp>) {
    def(interp, "len", 1, Some(1), |ex, a| match &a[0] {
        Value::Str(s) => Ok(Value::Int(s.chars().count() as i64)),
        Value::List(l) => Ok(Value::Int(lk(l).len() as i64)),
        Value::Map(m) => Ok(Value::Int(lk(m).len() as i64)),
        o => ex.err("TypeError", format!("len() not supported for {}", o.type_name())),
    });
    def(interp, "str", 1, Some(1), |ex, a| Ok(Value::str(ex.to_display(&a[0])?)));
    def(interp, "int", 1, Some(1), |ex, a| match &a[0] {
        Value::Int(i) => Ok(Value::Int(*i)),
        Value::Float(f) if f.is_finite() => Ok(Value::Int(f.trunc() as i64)),
        Value::Bool(b) => Ok(Value::Int(*b as i64)),
        Value::Str(s) => match s.trim().parse::<i64>() {
            Ok(i) => Ok(Value::Int(i)),
            Err(_) => match s.trim().parse::<f64>() {
                Ok(f) if f.is_finite() => Ok(Value::Int(f.trunc() as i64)),
                _ => ex.err("ValueError", format!("cannot convert {} to int", quote_str(s))),
            },
        },
        o => ex.err("TypeError", format!("cannot convert {} to int", o.type_name())),
    });
    def(interp, "float", 1, Some(1), |ex, a| match &a[0] {
        Value::Int(i) => Ok(Value::Float(*i as f64)),
        Value::Float(f) => Ok(Value::Float(*f)),
        Value::Str(s) => match s.trim().parse::<f64>() {
            Ok(f) => Ok(Value::Float(f)),
            Err(_) => ex.err("ValueError", format!("cannot convert {} to float", quote_str(s))),
        },
        o => ex.err("TypeError", format!("cannot convert {} to float", o.type_name())),
    });
    def(interp, "bool", 1, Some(1), |_, a| Ok(Value::Bool(truthy_loose(&a[0]))));
    def(interp, "typeof", 1, Some(1), |_, a| Ok(Value::str(a[0].type_name())));
    def(interp, "range", 1, Some(3), |ex, a| {
        let (start, end, step) = match a.len() {
            1 => (0, a_int(ex, a, 0, "range")?, 1),
            2 => (a_int(ex, a, 0, "range")?, a_int(ex, a, 1, "range")?, 1),
            _ => (a_int(ex, a, 0, "range")?, a_int(ex, a, 1, "range")?, a_int(ex, a, 2, "range")?),
        };
        if step == 0 {
            return ex.err("ValueError", "range() step must not be zero");
        }
        let mut out = vec![];
        let mut i = start;
        while (step > 0 && i < end) || (step < 0 && i > end) {
            out.push(Value::Int(i));
            if out.len() > 50_000_000 {
                return ex.err("ValueError", "range() too large");
            }
            i += step;
        }
        Ok(Value::list(out))
    });
    def(interp, "keys", 1, Some(1), |ex, a| match &a[0] {
        Value::Map(m) => Ok(Value::list(lk(m).keys().map(Value::str).collect())),
        o => ex.err("TypeError", format!("keys() needs a map, got {}", o.type_name())),
    });
    def(interp, "values", 1, Some(1), |ex, a| match &a[0] {
        Value::Map(m) => Ok(Value::list(lk(m).values().cloned().collect())),
        o => ex.err("TypeError", format!("values() needs a map, got {}", o.type_name())),
    });
    def(interp, "has", 2, Some(2), |ex, a| match (&a[0], &a[1]) {
        (Value::Map(m), Value::Str(k)) => Ok(Value::Bool(lk(m).contains_key(k.as_ref()))),
        (Value::List(l), x) => Ok(Value::Bool(lk(l).iter().any(|y| values_equal(x, y)))),
        (Value::Str(s), Value::Str(k)) => Ok(Value::Bool(s.contains(k.as_ref()))),
        (Value::Object(o), Value::Str(k)) => Ok(Value::Bool(lk(&o.fields).contains_key(k.as_ref()))),
        (o, _) => ex.err("TypeError", format!("has() not supported for {}", o.type_name())),
    });
    def(interp, "push", 2, Some(2), |ex, a| match &a[0] {
        Value::List(l) => {
            lk(l).push(a[1].clone());
            Ok(a[0].clone())
        }
        o => ex.err("TypeError", format!("push() needs a list, got {}", o.type_name())),
    });
    def(interp, "pop", 1, Some(1), |ex, a| match &a[0] {
        Value::List(l) => {
            let v = lk(l).pop();
            match v {
                Some(v) => Ok(v),
                None => ex.err("IndexError", "pop from empty list"),
            }
        }
        o => ex.err("TypeError", format!("pop() needs a list, got {}", o.type_name())),
    });
    def(interp, "error", 1, Some(2), |ex, a| {
        let msg = ex.to_display(&a[0])?;
        let e = ex.make_error("Error", msg);
        if let (Value::Object(o), Some(c)) = (&e, a.get(1)) {
            lk(&o.fields).insert("cause".into(), c.clone());
        }
        Ok(e)
    });
    def(interp, "panic", 1, Some(1), |ex, a| {
        let msg = ex.to_display(&a[0])?;
        ex.err("Error", msg)
    });
    def(interp, "assert_eq", 2, Some(3), |ex, a| {
        if ex.equals(&a[0], &a[1])? {
            Ok(Value::Null)
        } else {
            let extra = match a.get(2) {
                Some(m) => format!("{}: ", ex.to_display(m)?),
                None => String::new(),
            };
            ex.err("AssertionError", format!("{}assert_eq failed: {} != {}", extra, repr(&a[0]), repr(&a[1])))
        }
    });
    def(interp, "clone", 1, Some(1), |_, a| {
        Ok(match &a[0] {
            Value::List(l) => Value::list(lk(l).clone()),
            Value::Map(m) => Value::map(lk(m).clone()),
            o => o.clone(),
        })
    });
    def(interp, "deep_clone", 1, Some(1), |_, a| Ok(deep_clone(&a[0])));
    def(interp, "env", 1, Some(2), |ex, a| {
        let n = a_str(ex, a, 0, "env")?;
        Ok(match std::env::var(&n) {
            Ok(v) => Value::str(v),
            Err(_) => a.get(1).cloned().unwrap_or(Value::Null),
        })
    });
    let argv = interp.args.clone();
    def(interp, "args", 0, Some(0), move |_, _| Ok(Value::list(argv.iter().map(Value::str).collect())));
    interp.root.define("reflect", Value::Module(Arc::new(Module { name: "reflect".into(), kind: ModuleKind::Native(reflect_module()), path: None })), true);
}

// ───────────────────────── reflection ─────────────────────────

fn class_arg(ex: &mut Exec, v: &Value, f: &str) -> R<Arc<Class>> {
    match v {
        Value::Class(c) => Ok(c.clone()),
        Value::Object(o) => Ok(o.class.clone()),
        Value::Str(s) => {
            let found = lk(&ex.interp.classes).get(s.as_ref()).cloned();
            match found {
                Some(c) => Ok(c),
                None => ex.err("NameError", format!("{}: unknown class `{}`", f, s)),
            }
        }
        o => ex.err("TypeError", format!("{}: expected class, object or class name, got {}", f, o.type_name())),
    }
}

fn type_str(t: &Option<TypeExpr>) -> Value {
    match t {
        Some(t) => Value::str(t.display()),
        None => Value::str("any"),
    }
}

fn reflect_module() -> IndexMap<String, Value> {
    let mut m: IndexMap<String, Value> = IndexMap::new();
    let mut add = |name: &'static str, min: usize, max: Option<usize>, f: Box<dyn Fn(&mut Exec, &[Value]) -> R<Value> + Send + Sync>| {
        m.insert(name.to_string(), Native::value(name, min, max, move |ex, a| f(ex, a)));
    };
    add("class_of", 1, Some(1), Box::new(|ex, a| match &a[0] {
        Value::Object(o) => Ok(Value::Class(o.class.clone())),
        o => {
            let n = o.type_name();
            let found = lk(&ex.interp.classes).get(&n).cloned();
            Ok(found.map(Value::Class).unwrap_or(Value::Null))
        }
    }));
    add("class_name", 1, Some(1), Box::new(|_, a| Ok(Value::str(match &a[0] {
        Value::Class(c) => c.name.clone(),
        o => o.type_name(),
    }))));
    add("class", 1, Some(1), Box::new(|ex, a| Ok(Value::Class(class_arg(ex, &a[0], "reflect.class")?))));
    add("classes", 0, Some(0), Box::new(|ex, _| {
        let mut v: Vec<(String, Arc<Class>)> = lk(&ex.interp.classes).iter().map(|(k, c)| (k.clone(), c.clone())).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Value::list(v.into_iter().map(|(_, c)| Value::Class(c)).collect()))
    }));
    add("new", 1, None, Box::new(|ex, a| {
        let c = class_arg(ex, &a[0], "reflect.new")?;
        let fr = Frame::new(ex.interp.root.clone(), "<reflect>");
        let sp = ex.cur_span;
        ex.instantiate(&c, vec![], a[1..].to_vec(), &fr, sp)
    }));
    add("fields", 1, Some(1), Box::new(|ex, a| {
        let c = class_arg(ex, &a[0], "reflect.fields")?;
        let mut out = vec![];
        for (owner, f) in ex.all_fields(&c) {
            out.push(Value::map_from([
                ("name", Value::str(&f.name)),
                ("type", type_str(&f.ty)),
                ("access", Value::str(f.access.as_str())),
                ("static", Value::Bool(f.is_static)),
                ("readonly", Value::Bool(f.readonly)),
                ("declared_in", Value::str(&owner.name)),
            ]));
        }
        Ok(Value::list(out))
    }));
    add("methods", 1, Some(1), Box::new(|ex, a| {
        let c = class_arg(ex, &a[0], "reflect.methods")?;
        let mut names: Vec<String> = vec![];
        for ch in c.chain() {
            for n in ch.methods.keys() {
                if !names.contains(n) {
                    names.push(n.clone());
                }
            }
        }
        let mut out = vec![];
        for n in names {
            let ov = ex.find_methods(&c, &n);
            let count = ov.len() as i64;
            for (owner, d) in ov {
                let params: Vec<Value> = d.params.iter().map(|p| Value::map_from([("name", Value::str(&p.name)), ("type", type_str(&p.ty))])).collect();
                out.push(Value::map_from([
                    ("name", Value::str(&d.name)),
                    ("params", Value::list(params)),
                    ("returns", match &d.ret { Some(t) => Value::str(t.display()), None => Value::str("any") }),
                    ("access", Value::str(d.access.as_str())),
                    ("static", Value::Bool(d.is_static)),
                    ("abstract", Value::Bool(d.body.is_none())),
                    ("overloads", Value::Int(count)),
                    ("declared_in", Value::str(&owner.name)),
                ]));
            }
        }
        Ok(Value::list(out))
    }));
    add("get", 2, Some(2), Box::new(|ex, a| {
        let n = a_str(ex, a, 1, "reflect.get")?;
        let fr = Frame::new(ex.interp.root.clone(), "<reflect>");
        ex.get_member(&a[0], &n, &fr)
    }));
    add("set", 3, Some(3), Box::new(|ex, a| {
        let n = a_str(ex, a, 1, "reflect.set")?;
        let fr = Frame::new(ex.interp.root.clone(), "<reflect>");
        ex.set_member(&a[0], &n, a[2].clone(), &fr)?;
        Ok(Value::Null)
    }));
    add("invoke", 2, None, Box::new(|ex, a| {
        let n = a_str(ex, a, 1, "reflect.invoke")?;
        let fr = Frame::new(ex.interp.root.clone(), "<reflect>");
        let sp = ex.cur_span;
        ex.call_member(a[0].clone(), &n, a[2..].to_vec(), &fr, sp)
    }));
    add("parent", 1, Some(1), Box::new(|ex, a| {
        let c = class_arg(ex, &a[0], "reflect.parent")?;
        Ok(c.parent.clone().map(Value::Class).unwrap_or(Value::Null))
    }));
    add("interfaces", 1, Some(1), Box::new(|ex, a| {
        let c = class_arg(ex, &a[0], "reflect.interfaces")?;
        let mut out: Vec<Value> = vec![];
        let mut seen = vec![];
        let mut stack: Vec<Arc<Class>> = c.chain();
        while let Some(x) = stack.pop() {
            for i in &x.interfaces {
                if !seen.contains(&i.id) {
                    seen.push(i.id);
                    out.push(Value::Class(i.clone()));
                    stack.push(i.clone());
                }
            }
        }
        Ok(Value::list(out))
    }));
    add("is_a", 2, Some(2), Box::new(|ex, a| {
        let target = class_arg(ex, &a[1], "reflect.is_a")?;
        Ok(Value::Bool(match &a[0] {
            Value::Object(o) => o.class.is_subclass_of(&target),
            Value::Class(c) => c.is_subclass_of(&target),
            _ => false,
        }))
    }));
    add("type_args", 1, Some(1), Box::new(|_, a| Ok(match &a[0] {
        Value::Object(o) => Value::list(o.type_args.iter().map(|t| Value::str(t.display())).collect()),
        _ => Value::list(vec![]),
    })));
    add("is_abstract", 1, Some(1), Box::new(|ex, a| Ok(Value::Bool(class_arg(ex, &a[0], "reflect.is_abstract")?.is_abstract))));
    m
}

// ───────────────────────── builtin methods ─────────────────────────

pub const STR_M: &[&str] = &["upper", "lower", "trim", "trim_start", "trim_end", "split", "join", "starts_with", "ends_with", "contains", "replace", "find", "index_of", "substr", "repeat", "pad_left", "pad_right", "chars", "bytes", "lines", "to_int", "to_float", "format", "len", "is_empty", "to_string"];
pub const LIST_M: &[&str] = &["map", "filter", "reduce", "sort", "sort_by", "reverse", "slice", "index_of", "contains", "join", "first", "last", "flat", "any", "all", "each", "unique", "concat", "insert", "remove", "clear", "len", "push", "pop", "find", "sum", "min", "max", "is_empty", "to_string"];
pub const MAP_M: &[&str] = &["keys", "values", "items", "get", "has", "remove", "merge", "each", "map_values", "filter", "len", "clear", "set", "is_empty", "to_string"];
const TASK_M: &[&str] = &["cancel", "wait", "done"];
const NUM_M: &[&str] = &["abs", "floor", "ceil", "round", "to_string"];

fn method_names(v: &Value) -> &'static [&'static str] {
    match v {
        Value::Str(_) => STR_M,
        Value::List(_) => LIST_M,
        Value::Map(_) => MAP_M,
        Value::Task(_) => TASK_M,
        Value::Int(_) | Value::Float(_) => NUM_M,
        Value::Bool(_) | Value::Null => &["to_string"],
        _ => &[],
    }
}

pub fn bound_builtin(recv: &Value, name: &str) -> Option<Value> {
    if !method_names(recv).contains(&name) {
        return None;
    }
    let (r, n) = (recv.clone(), name.to_string());
    Some(Native::value(name, 0, None, move |ex, a| call_builtin(ex, &r, &n, a)))
}

fn nargs(ex: &mut Exec, name: &str, args: &[Value], min: usize, max: usize) -> R<()> {
    if args.len() < min || args.len() > max {
        let exp = if min == max { format!("{}", min) } else { format!("{}..{}", min, max) };
        return ex.err("TypeError", format!("{}() expects {} argument(s), got {}", name, exp, args.len()));
    }
    Ok(())
}

pub fn call_builtin(ex: &mut Exec, recv: &Value, name: &str, args: &[Value]) -> R<Value> {
    if !method_names(recv).contains(&name) {
        let mut msg = format!("{} has no method `{}`", recv.type_name(), name);
        if let Some(s) = suggest(name, method_names(recv).iter().copied()) {
            msg.push_str(&format!(" (did you mean `{}`?)", s));
        }
        return ex.err("NameError", msg);
    }
    if name == "to_string" {
        nargs(ex, name, args, 0, 0)?;
        return Ok(Value::str(ex.to_display(recv)?));
    }
    match recv {
        Value::Str(s) => str_method(ex, s, name, args),
        Value::List(l) => list_method(ex, recv, l, name, args),
        Value::Map(m) => map_method(ex, recv, m, name, args),
        Value::Task(t) => match name {
            "cancel" => {
                t.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(Value::Null)
            }
            "wait" => ex.await_task(t),
            _ => Ok(Value::Bool(lk(&t.state).is_some())),
        },
        Value::Int(i) => Ok(match name {
            "abs" => Value::Int(i.wrapping_abs()),
            _ => Value::Int(*i),
        }),
        Value::Float(f) => Ok(match name {
            "abs" => Value::Float(f.abs()),
            "floor" => Value::Int(f.floor() as i64),
            "ceil" => Value::Int(f.ceil() as i64),
            _ => Value::Int(f.round() as i64),
        }),
        _ => ex.err("NameError", format!("{} has no method `{}`", recv.type_name(), name)),
    }
}

fn char_idx_to_val(i: Option<usize>) -> Value {
    Value::Int(i.map(|x| x as i64).unwrap_or(-1))
}

fn str_method(ex: &mut Exec, s: &Arc<str>, name: &str, a: &[Value]) -> R<Value> {
    match name {
        "upper" => { nargs(ex, name, a, 0, 0)?; Ok(Value::str(s.to_uppercase())) }
        "lower" => { nargs(ex, name, a, 0, 0)?; Ok(Value::str(s.to_lowercase())) }
        "trim" => { nargs(ex, name, a, 0, 0)?; Ok(Value::str(s.trim())) }
        "trim_start" => { nargs(ex, name, a, 0, 0)?; Ok(Value::str(s.trim_start())) }
        "trim_end" => { nargs(ex, name, a, 0, 0)?; Ok(Value::str(s.trim_end())) }
        "len" => Ok(Value::Int(s.chars().count() as i64)),
        "is_empty" => Ok(Value::Bool(s.is_empty())),
        "split" => {
            nargs(ex, name, a, 1, 1)?;
            let sep = a_str(ex, a, 0, "split")?;
            Ok(Value::list(if sep.is_empty() { s.chars().map(|c| Value::str(c.to_string())).collect() } else { s.split(sep.as_str()).map(Value::str).collect() }))
        }
        "join" => {
            nargs(ex, name, a, 1, 1)?;
            let items = a_list(ex, a, 0, "join")?;
            let mut parts = vec![];
            for i in &items {
                parts.push(ex.to_display(i)?);
            }
            Ok(Value::str(parts.join(s)))
        }
        "starts_with" => { let p = a_str(ex, a, 0, name)?; Ok(Value::Bool(s.starts_with(p.as_str()))) }
        "ends_with" => { let p = a_str(ex, a, 0, name)?; Ok(Value::Bool(s.ends_with(p.as_str()))) }
        "contains" => { let p = a_str(ex, a, 0, name)?; Ok(Value::Bool(s.contains(p.as_str()))) }
        "replace" => {
            let f = a_str(ex, a, 0, name)?;
            let t = a_str(ex, a, 1, name)?;
            Ok(Value::str(s.replace(f.as_str(), &t)))
        }
        "find" | "index_of" => {
            let p = a_str(ex, a, 0, name)?;
            Ok(char_idx_to_val(s.find(p.as_str()).map(|b| s[..b].chars().count())))
        }
        "substr" => {
            nargs(ex, name, a, 1, 2)?;
            let chars: Vec<char> = s.chars().collect();
            let start = a_int(ex, a, 0, name)?;
            let start = if start < 0 { (chars.len() as i64 + start).max(0) } else { start.min(chars.len() as i64) } as usize;
            let len = if a.len() > 1 { a_int(ex, a, 1, name)?.max(0) as usize } else { chars.len() };
            let end = (start + len).min(chars.len());
            Ok(Value::str(chars[start..end].iter().collect::<String>()))
        }
        "repeat" => {
            let n = a_int(ex, a, 0, name)?;
            if n < 0 || (n as usize).saturating_mul(s.len()) > 1 << 28 {
                return ex.err("ValueError", "repeat count out of range");
            }
            Ok(Value::str(s.repeat(n as usize)))
        }
        "pad_left" | "pad_right" => {
            nargs(ex, name, a, 1, 2)?;
            let w = a_int(ex, a, 0, name)?.max(0) as usize;
            let fill = if a.len() > 1 { a_str(ex, a, 1, name)? } else { " ".into() };
            let fc = fill.chars().next().unwrap_or(' ');
            let n = s.chars().count();
            if n >= w {
                return Ok(Value::str(s.as_ref()));
            }
            let pad: String = std::iter::repeat(fc).take(w - n).collect();
            Ok(Value::str(if name == "pad_left" { format!("{}{}", pad, s) } else { format!("{}{}", s, pad) }))
        }
        "chars" => Ok(Value::list(s.chars().map(|c| Value::str(c.to_string())).collect())),
        "bytes" => Ok(Value::list(s.bytes().map(|b| Value::Int(b as i64)).collect())),
        "lines" => Ok(Value::list(s.lines().map(Value::str).collect())),
        "to_int" => match s.trim().parse::<i64>() {
            Ok(i) => Ok(Value::Int(i)),
            Err(_) => ex.err("ValueError", format!("cannot convert {} to int", quote_str(s))),
        },
        "to_float" => match s.trim().parse::<f64>() {
            Ok(i) => Ok(Value::Float(i)),
            Err(_) => ex.err("ValueError", format!("cannot convert {} to float", quote_str(s))),
        },
        "format" => {
            let mut out = String::new();
            let mut next = 0usize;
            let cs: Vec<char> = s.chars().collect();
            let mut i = 0;
            while i < cs.len() {
                if cs[i] == '{' && i + 1 < cs.len() && cs[i + 1] == '{' {
                    out.push('{');
                    i += 2;
                } else if cs[i] == '}' && i + 1 < cs.len() && cs[i + 1] == '}' {
                    out.push('}');
                    i += 2;
                } else if cs[i] == '{' {
                    let mut j = i + 1;
                    let mut idx = String::new();
                    while j < cs.len() && cs[j] != '}' {
                        idx.push(cs[j]);
                        j += 1;
                    }
                    if j >= cs.len() {
                        return ex.err("ValueError", "format: unclosed `{`");
                    }
                    let k = if idx.is_empty() { let k = next; next += 1; k } else {
                        match idx.parse::<usize>() {
                            Ok(k) => k,
                            Err(_) => return ex.err("ValueError", format!("format: bad placeholder {{{}}}", idx)),
                        }
                    };
                    match a.get(k) {
                        Some(v) => out.push_str(&ex.to_display(v)?),
                        None => return ex.err("ValueError", format!("format: missing argument for placeholder #{}", k)),
                    }
                    i = j + 1;
                } else {
                    out.push(cs[i]);
                    i += 1;
                }
            }
            Ok(Value::str(out))
        }
        _ => ex.err("NameError", format!("string has no method `{}`", name)),
    }
}

fn norm_index(i: i64, len: usize) -> usize {
    if i < 0 { (len as i64 + i).max(0) as usize } else { (i as usize).min(len) }
}

fn list_method(ex: &mut Exec, recv: &Value, l: &ListRef, name: &str, a: &[Value]) -> R<Value> {
    let sp = ex.cur_span;
    match name {
        "len" => Ok(Value::Int(lk(l).len() as i64)),
        "is_empty" => Ok(Value::Bool(lk(l).is_empty())),
        "push" => { nargs(ex, name, a, 1, 1)?; lk(l).push(a[0].clone()); Ok(recv.clone()) }
        "pop" => {
            let v = lk(l).pop();
            match v { Some(v) => Ok(v), None => ex.err("IndexError", "pop from empty list") }
        }
        "clear" => { lk(l).clear(); Ok(Value::Null) }
        "first" => Ok(lk(l).first().cloned().unwrap_or(Value::Null)),
        "last" => Ok(lk(l).last().cloned().unwrap_or(Value::Null)),
        "reverse" => { let mut v = lk(l).clone(); v.reverse(); Ok(Value::list(v)) }
        "slice" => {
            nargs(ex, name, a, 1, 2)?;
            let items = lk(l).clone();
            let from = norm_index(a_int(ex, a, 0, name)?, items.len());
            let to = if a.len() > 1 { norm_index(a_int(ex, a, 1, name)?, items.len()) } else { items.len() };
            Ok(Value::list(if from < to { items[from..to].to_vec() } else { vec![] }))
        }
        "index_of" => { nargs(ex, name, a, 1, 1)?; Ok(char_idx_to_val(lk(l).iter().position(|x| values_equal(x, &a[0])))) }
        "contains" => { nargs(ex, name, a, 1, 1)?; Ok(Value::Bool(lk(l).iter().any(|x| values_equal(x, &a[0])))) }
        "join" => {
            let sep = if a.is_empty() { String::new() } else { a_str(ex, a, 0, name)? };
            let items = lk(l).clone();
            let mut parts = vec![];
            for i in &items {
                parts.push(ex.to_display(i)?);
            }
            Ok(Value::str(parts.join(&sep)))
        }
        "concat" => {
            let mut v = lk(l).clone();
            for x in a {
                match x {
                    Value::List(o) => v.extend(lk(o).iter().cloned()),
                    other => v.push(other.clone()),
                }
            }
            Ok(Value::list(v))
        }
        "insert" => {
            nargs(ex, name, a, 2, 2)?;
            let i = a_int(ex, a, 0, name)?;
            let mut g = lk(l);
            let idx = norm_index(i, g.len());
            g.insert(idx, a[1].clone());
            Ok(recv.clone())
        }
        "remove" => {
            nargs(ex, name, a, 1, 1)?;
            let i = a_int(ex, a, 0, name)?;
            let mut g = lk(l);
            let len = g.len() as i64;
            let idx = if i < 0 { i + len } else { i };
            if idx < 0 || idx >= len {
                drop(g);
                return ex.err("IndexError", format!("list index {} out of range (length {})", i, len));
            }
            Ok(g.remove(idx as usize))
        }
        "flat" => {
            let mut out = vec![];
            for x in lk(l).iter() {
                match x {
                    Value::List(i) => out.extend(lk(i).iter().cloned()),
                    o => out.push(o.clone()),
                }
            }
            Ok(Value::list(out))
        }
        "unique" => {
            let mut out: Vec<Value> = vec![];
            for x in lk(l).clone() {
                if !out.iter().any(|y| values_equal(&x, y)) {
                    out.push(x);
                }
            }
            Ok(Value::list(out))
        }
        "sum" => {
            let items = lk(l).clone();
            let mut acc = Value::Int(0);
            for i in items {
                acc = ex.binary(BinOp::Add, acc, i)?;
            }
            Ok(acc)
        }
        "min" | "max" => {
            let items = lk(l).clone();
            let mut best: Option<Value> = None;
            for i in items {
                best = Some(match best {
                    None => i,
                    Some(b) => {
                        let o = cmp_values(ex, &i, &b)?;
                        if (name == "min" && o == Ordering::Less) || (name == "max" && o == Ordering::Greater) { i } else { b }
                    }
                });
            }
            Ok(best.unwrap_or(Value::Null))
        }
        "map" | "filter" | "each" | "any" | "all" | "find" => {
            nargs(ex, name, a, 1, 1)?;
            let f = a_fn(ex, a, 0, name)?;
            let items = lk(l).clone();
            let mut out = vec![];
            for x in items {
                let r = ex.call_value(&f, vec![x.clone()], sp)?;
                match name {
                    "map" => out.push(r),
                    "filter" => if cond(ex, r)? { out.push(x) },
                    "any" => if cond(ex, r)? { return Ok(Value::Bool(true)) },
                    "all" => if !cond(ex, r)? { return Ok(Value::Bool(false)) },
                    "find" => if cond(ex, r)? { return Ok(x) },
                    _ => {}
                }
            }
            Ok(match name {
                "map" | "filter" => Value::list(out),
                "any" => Value::Bool(false),
                "all" => Value::Bool(true),
                "find" => Value::Null,
                _ => Value::Null,
            })
        }
        "reduce" => {
            nargs(ex, name, a, 1, 2)?;
            let f = a_fn(ex, a, 0, name)?;
            let items = lk(l).clone();
            let mut it = items.into_iter();
            let mut acc = if a.len() > 1 {
                a[1].clone()
            } else {
                match it.next() {
                    Some(v) => v,
                    None => return ex.err("ValueError", "reduce of empty list with no initial value"),
                }
            };
            for x in it {
                acc = ex.call_value(&f, vec![acc, x], sp)?;
            }
            Ok(acc)
        }
        "sort" | "sort_by" => {
            nargs(ex, name, a, 0, 1)?;
            let items = lk(l).clone();
            let f = if a.is_empty() { None } else { Some(a_fn(ex, a, 0, name)?) };
            let mut keyed: Vec<(Value, Value)> = vec![];
            for x in items {
                let k = match (&f, name) {
                    (Some(f), "sort_by") => ex.call_value(f, vec![x.clone()], sp)?,
                    _ => x.clone(),
                };
                keyed.push((k, x));
            }
            if name == "sort_by" && f.is_none() {
                return ex.err("TypeError", "sort_by() needs a key function");
            }
            let mut err: Option<Signal> = None;
            let cmp_fn = if name == "sort" { f.clone() } else { None };
            keyed.sort_by(|p, q| {
                if err.is_some() {
                    return Ordering::Equal;
                }
                if let Some(cf) = &cmp_fn {
                    match ex.call_value(cf, vec![p.1.clone(), q.1.clone()], sp) {
                        Ok(Value::Int(n)) => n.cmp(&0),
                        Ok(Value::Bool(b)) => if b { Ordering::Less } else { Ordering::Greater },
                        Ok(o) => {
                            err = Some(match ex.err::<()>("TypeError", format!("sort comparator must return int, got {}", o.type_name())) { Err(e) => e, Ok(_) => unreachable!() });
                            Ordering::Equal
                        }
                        Err(e) => { err = Some(e); Ordering::Equal }
                    }
                } else {
                    match cmp_values(ex, &p.0, &q.0) {
                        Ok(o) => o,
                        Err(e) => { err = Some(e); Ordering::Equal }
                    }
                }
            });
            if let Some(e) = err {
                return Err(e);
            }
            Ok(Value::list(keyed.into_iter().map(|(_, v)| v).collect()))
        }
        _ => ex.err("NameError", format!("list has no method `{}`", name)),
    }
}

fn map_method(ex: &mut Exec, recv: &Value, m: &MapRef, name: &str, a: &[Value]) -> R<Value> {
    let sp = ex.cur_span;
    match name {
        "len" => Ok(Value::Int(lk(m).len() as i64)),
        "is_empty" => Ok(Value::Bool(lk(m).is_empty())),
        "keys" => Ok(Value::list(lk(m).keys().map(Value::str).collect())),
        "values" => Ok(Value::list(lk(m).values().cloned().collect())),
        "items" => Ok(Value::list(lk(m).iter().map(|(k, v)| Value::list(vec![Value::str(k), v.clone()])).collect())),
        "get" => {
            nargs(ex, name, a, 1, 2)?;
            let k = a_str(ex, a, 0, name)?;
            Ok(lk(m).get(&k).cloned().unwrap_or_else(|| a.get(1).cloned().unwrap_or(Value::Null)))
        }
        "has" => { let k = a_str(ex, a, 0, name)?; Ok(Value::Bool(lk(m).contains_key(&k))) }
        "set" => { let k = a_str(ex, a, 0, name)?; nargs(ex, name, a, 2, 2)?; lk(m).insert(k, a[1].clone()); Ok(recv.clone()) }
        "remove" => { let k = a_str(ex, a, 0, name)?; Ok(lk(m).shift_remove(&k).unwrap_or(Value::Null)) }
        "clear" => { lk(m).clear(); Ok(Value::Null) }
        "merge" => {
            let mut out = lk(m).clone();
            for x in a {
                match x {
                    Value::Map(o) => { for (k, v) in lk(o).iter() { out.insert(k.clone(), v.clone()); } }
                    o => return ex.err("TypeError", format!("merge() needs maps, got {}", o.type_name())),
                }
            }
            Ok(Value::map(out))
        }
        "each" | "map_values" | "filter" => {
            nargs(ex, name, a, 1, 1)?;
            let f = a_fn(ex, a, 0, name)?;
            let items = lk(m).clone();
            let mut out = IndexMap::new();
            for (k, v) in items {
                let r = ex.call_value(&f, vec![Value::str(&k), v.clone()], sp)?;
                match name {
                    "map_values" => { out.insert(k, r); }
                    "filter" => if cond(ex, r)? { out.insert(k, v); },
                    _ => {}
                }
            }
            Ok(if name == "each" { Value::Null } else { Value::map(out) })
        }
        _ => ex.err("NameError", format!("map has no method `{}`", name)),
    }
}
