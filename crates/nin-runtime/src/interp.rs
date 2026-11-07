//! Tree-walking interpreter: expressions, statements, calls, tasks, processes.

use crate::value::*;
use indexmap::IndexMap;
use nin_ast::*;
use nin_diag::{suggest, Span};
use nin_win::{OutSpec, ProcSpec, StdinSpec};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MAX_DEPTH: usize = 1000;
pub const TASK_STACK: usize = 64 * 1024 * 1024;

pub trait Sink: Send + Sync {
    fn write(&self, text: &str, is_err: bool);
}

pub struct InterpBuilder {
    pub natives: IndexMap<String, IndexMap<String, Value>>,
    pub args: Vec<String>,
    pub pkg_root: Option<PathBuf>,
    pub registries: Vec<String>,
}

impl Default for InterpBuilder {
    fn default() -> Self {
        InterpBuilder { natives: IndexMap::new(), args: Vec::new(), pkg_root: None, registries: Vec::new() }
    }
}

impl InterpBuilder {
    pub fn new() -> InterpBuilder {
        InterpBuilder::default()
    }
    pub fn add_module(&mut self, name: &str, members: IndexMap<String, Value>) -> &mut Self {
        self.natives.insert(name.to_string(), members);
        self
    }
    pub fn args(&mut self, a: Vec<String>) -> &mut Self {
        self.args = a;
        self
    }
    pub fn build(self) -> Arc<Interp> {
        let root = Scope::new(None);
        let interp = Arc::new(Interp {
            sources: Mutex::new(nin_diag::SourceMap::new()),
            modules: Mutex::new(HashMap::new()),
            loading: Mutex::new(Vec::new()),
            natives: self.natives.into_iter().map(|(k, v)| (k.clone(), Arc::new(Module { name: k, kind: ModuleKind::Native(v), path: None }))).collect(),
            plugins: Mutex::new(HashMap::new()),
            classes: Mutex::new(HashMap::new()),
            root: root.clone(),
            args: self.args,
            next_id: AtomicU64::new(1),
            out_lock: Mutex::new(()),
            pkg_root: self.pkg_root,
            registries: self.registries,
            vfs: Mutex::new(HashMap::new()),
            vfs_pkgs: Mutex::new(HashMap::new()),
        });
        crate::builtins::install_globals(&interp);
        let mut ex = Exec::new(interp.clone());
        if let Err(Signal::Throw(t)) = crate::modules::load_prelude(&mut ex) {
            panic!("internal error: prelude failed to load: {}", display_simple(&t.value));
        }
        interp
    }
}

pub struct Interp {
    pub sources: Mutex<nin_diag::SourceMap>,
    pub modules: Mutex<HashMap<PathBuf, Arc<Module>>>,
    pub loading: Mutex<Vec<PathBuf>>,
    pub natives: HashMap<String, Arc<Module>>,
    pub plugins: Mutex<HashMap<String, Arc<Module>>>,
    pub classes: Mutex<HashMap<String, Arc<Class>>>,
    pub root: Env,
    pub args: Vec<String>,
    pub next_id: AtomicU64,
    pub out_lock: Mutex<()>,
    pub pkg_root: Option<PathBuf>,
    pub registries: Vec<String>,
    /// embedded files of a compiled bundle: virtual path -> source text
    pub vfs: Mutex<HashMap<String, String>>,
    /// embedded packages: name -> virtual entry path
    pub vfs_pkgs: Mutex<HashMap<String, String>>,
}

impl Interp {
    pub fn fresh_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }
    pub fn add_source(&self, name: &str, text: &str) -> u32 {
        lk(&self.sources).add_file(name, text)
    }
    /// Render an uncaught throw as a multi-line report with the NIN call stack.
    pub fn render_throw(&self, t: &Throw) -> String {
        let (kind, msg) = error_parts(&t.value);
        let sm = lk(&self.sources);
        let mut out = String::new();
        let first = t.trace.first().map(|(_, s)| *s).unwrap_or(Span::DUMMY);
        let d = nin_diag::Diagnostic::error("E0500", format!("{}: {}", kind, msg)).at(first, "thrown here");
        out.push_str(&sm.render(&d, false));
        if t.trace.len() > 1 {
            out.push_str("stack:\n");
            for (name, sp) in &t.trace {
                out.push_str(&format!("  at {} ({})\n", name, sm.location(*sp)));
            }
        }
        out
    }
}

/// `(class name, message)` of a thrown value.
pub fn error_parts(v: &Value) -> (String, String) {
    match v {
        Value::Object(o) => {
            let f = lk(&o.fields);
            let msg = f.get("message").map(display_simple).unwrap_or_default();
            (o.class.name.clone(), msg)
        }
        other => ("Error".to_string(), display_simple(other)),
    }
}

#[derive(Clone)]
pub struct Frame {
    pub env: Env,
    pub this: Option<Value>,
    pub class: Option<Arc<Class>>,
    pub in_ctor: bool,
    pub generics: Arc<HashMap<String, TypeExpr>>,
    pub fname: Arc<str>,
    pub module: Arc<str>,
}

impl Frame {
    pub fn new(env: Env, module: &str) -> Frame {
        Frame { env, this: None, class: None, in_ctor: false, generics: Arc::new(HashMap::new()), fname: Arc::from("<main>"), module: Arc::from(module) }
    }
    pub fn child(&self) -> Frame {
        let mut f = self.clone();
        f.env = Scope::new(Some(self.env.clone()));
        f
    }
}

pub enum Flow {
    Normal,
    Break,
    Continue,
    Return(Value),
}

pub struct Exec {
    pub interp: Arc<Interp>,
    pub cancel: Vec<Arc<AtomicBool>>,
    pub trace: Vec<(String, Span)>,
    pub cur_span: Span,
    pub sink: Option<Arc<dyn Sink>>,
    pub group: Option<Arc<nin_win::ProcessGroup>>,
    pub depth: usize,
    pub cur_class: Option<Arc<Class>>,
}

fn clone_result(r: &R<Value>) -> R<Value> {
    match r {
        Ok(v) => Ok(v.clone()),
        Err(Signal::Exit(c)) => Err(Signal::Exit(*c)),
        Err(Signal::Throw(t)) => Err(Signal::Throw(Box::new(Throw { value: t.value.clone(), trace: t.trace.clone() }))),
    }
}

impl Exec {
    pub fn new(interp: Arc<Interp>) -> Exec {
        Exec { interp, cancel: vec![Arc::new(AtomicBool::new(false))], trace: Vec::new(), cur_span: Span::DUMMY, sink: None, group: None, depth: 0, cur_class: None }
    }

    /// Child executor for another thread: shares cancellation (plus `extra`), sink and process group.
    pub fn child_exec(&self, extra: Option<Arc<AtomicBool>>) -> Exec {
        let mut cancel = self.cancel.clone();
        if let Some(e) = extra {
            cancel.push(e);
        }
        Exec { interp: self.interp.clone(), cancel, trace: self.trace.clone(), cur_span: self.cur_span, sink: self.sink.clone(), group: self.group.clone(), depth: self.depth, cur_class: None }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.iter().any(|c| c.load(Ordering::SeqCst))
    }

    pub fn root_token(&self) -> Arc<AtomicBool> {
        self.cancel[0].clone()
    }

    // ───────── errors ─────────

    pub fn make_error(&mut self, kind: &str, msg: impl Into<String>) -> Value {
        let class = match self.interp.root.get(kind) {
            Some(Value::Class(c)) => c,
            _ => match self.interp.root.get("Error") {
                Some(Value::Class(c)) => c,
                _ => return Value::str(msg.into()),
            },
        };
        let mut fields: IndexMap<String, Value> = IndexMap::new();
        for c in class.chain().iter().rev() {
            if let Some(d) = &c.decl {
                for f in d.fields.iter().filter(|f| !f.is_static) {
                    fields.insert(f.name.clone(), Value::Null);
                }
            }
        }
        fields.insert("message".into(), Value::str(msg.into()));
        fields.insert("code".into(), Value::str(kind.to_lowercase()));
        fields.insert("stack".into(), Value::list(vec![]));
        Value::Object(Arc::new(Object { id: self.interp.fresh_id(), class, fields: Mutex::new(fields), type_args: vec![] }))
    }

    pub fn throw_value(&mut self, v: Value) -> Signal {
        let mut trace = vec![(self.trace.last().map(|t| t.0.clone()).unwrap_or_else(|| "<main>".into()), self.cur_span)];
        trace.extend(self.trace.iter().rev().cloned());
        if let Value::Object(o) = &v {
            let mut f = lk(&o.fields);
            let empty = matches!(f.get("stack"), Some(Value::List(l)) if lk(l).is_empty());
            if empty && f.contains_key("message") {
                let sm = lk(&self.interp.sources);
                let lines: Vec<Value> = trace.iter().map(|(n, s)| Value::str(format!("at {} ({})", n, sm.location(*s)))).collect();
                f.insert("stack".into(), Value::list(lines));
            }
        }
        Signal::Throw(Box::new(Throw { value: v, trace }))
    }

    pub fn err<T>(&mut self, kind: &str, msg: impl Into<String>) -> R<T> {
        let e = self.make_error(kind, msg);
        Err(self.throw_value(e))
    }

    pub fn err_at<T>(&mut self, span: Span, kind: &str, msg: impl Into<String>) -> R<T> {
        if !span.is_dummy() {
            self.cur_span = span;
        }
        self.err(kind, msg)
    }

    // ───────── output ─────────

    pub fn out(&mut self, s: &str, is_err: bool) {
        if let Some(sink) = &self.sink {
            sink.write(s, is_err);
            return;
        }
        use std::io::Write;
        let _g = lk(&self.interp.out_lock);
        if is_err {
            let mut e = std::io::stderr().lock();
            let _ = e.write_all(s.as_bytes());
            let _ = e.flush();
        } else {
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(s.as_bytes());
            let _ = o.flush();
        }
    }

    // ───────── display ─────────

    pub fn to_display(&mut self, v: &Value) -> R<String> {
        match v {
            Value::Str(s) => Ok(s.to_string()),
            Value::Object(o) => {
                if o.class.kind == ClassKind::Enum {
                    return Ok(repr(v));
                }
                let found = self.find_methods(&o.class, "to_string");
                if let Some((_, d)) = found.iter().find(|(_, d)| d.params.is_empty() && !d.is_static) {
                    let owner = found.iter().find(|(_, x)| Arc::ptr_eq(x, d)).map(|x| x.0.clone());
                    let r = self.invoke_method(v.clone(), owner.as_ref(), d, vec![], self.cur_span)?;
                    return self.to_display(&r);
                }
                Ok(repr(v))
            }
            Value::List(_) | Value::Map(_) => Ok(self.repr_deep(v, 0)?),
            other => Ok(display_simple(other)),
        }
    }

    /// Like `repr` but honours user `to_string` of objects nested in collections.
    fn repr_deep(&mut self, v: &Value, d: usize) -> R<String> {
        if d > 8 {
            return Ok("...".into());
        }
        Ok(match v {
            Value::Str(s) => quote_str(s),
            Value::List(l) => {
                let items = lk(l).clone();
                let mut parts = vec![];
                for i in &items {
                    parts.push(self.repr_deep(i, d + 1)?);
                }
                format!("[{}]", parts.join(", "))
            }
            Value::Map(m) => {
                let items = lk(m).clone();
                let mut parts = vec![];
                for (k, i) in &items {
                    parts.push(format!("{}: {}", quote_str(k), self.repr_deep(i, d + 1)?));
                }
                if parts.is_empty() { "{}".into() } else { format!("{{{}}}", parts.join(", ")) }
            }
            Value::Object(_) => self.to_display(v)?,
            other => display_simple(other),
        })
    }

    // ───────── type checks ─────────

    pub fn lookup_class(&self, path: &[String], env: &Env) -> Option<Arc<Class>> {
        let mut cur = env.get(path.first()?)?;
        for seg in &path[1..] {
            cur = match &cur {
                Value::Module(m) => match &m.kind {
                    ModuleKind::Native(map) => map.get(seg)?.clone(),
                    ModuleKind::Script { env, .. } => env.get_local(seg)?,
                },
                _ => return None,
            };
        }
        match cur {
            Value::Class(c) => Some(c),
            _ => None,
        }
    }

    pub fn conforms(&mut self, v: &Value, t: &TypeExpr, fr: &Frame) -> bool {
        match &t.kind {
            TypeExprKind::Optional(inner) => matches!(v, Value::Null) || self.conforms(v, inner, fr),
            TypeExprKind::Union(alts) => alts.iter().any(|a| self.conforms(v, a, fr)),
            TypeExprKind::Func { .. } => v.is_callable(),
            TypeExprKind::Named { path, args } => {
                if path.len() == 1 {
                    let n = path[0].as_str();
                    if let Some(g) = fr.generics.get(n) {
                        let g = g.clone();
                        return self.conforms(v, &g, fr);
                    }
                    match n {
                        "any" => return true,
                        "int" => return matches!(v, Value::Int(_)),
                        "float" => return matches!(v, Value::Float(_) | Value::Int(_)),
                        "string" => return matches!(v, Value::Str(_)),
                        "bool" => return matches!(v, Value::Bool(_)),
                        "null" | "void" => return matches!(v, Value::Null),
                        "fn" => return v.is_callable(),
                        "task" => return matches!(v, Value::Task(_)),
                        "module" => return matches!(v, Value::Module(_)),
                        "list" => {
                            if let Value::List(l) = v {
                                if let Some(el) = args.first() {
                                    let items = lk(l).clone();
                                    return items.iter().all(|x| self.conforms(x, el, fr));
                                }
                                return true;
                            }
                            return false;
                        }
                        "map" => {
                            if let Value::Map(m) = v {
                                if let Some(el) = args.get(1) {
                                    let items: Vec<Value> = lk(m).values().cloned().collect();
                                    return items.iter().all(|x| self.conforms(x, el, fr));
                                }
                                return true;
                            }
                            return false;
                        }
                        _ => {}
                    }
                }
                match self.lookup_class(path, &fr.env) {
                    Some(c) => match v {
                        Value::Object(o) => o.class.is_subclass_of(&c),
                        _ => false,
                    },
                    None => false,
                }
            }
        }
    }

    pub fn type_known(&self, t: &TypeExpr, fr: &Frame) -> bool {
        match &t.kind {
            TypeExprKind::Optional(i) => self.type_known(i, fr),
            TypeExprKind::Union(a) => a.iter().all(|x| self.type_known(x, fr)),
            TypeExprKind::Func { .. } => true,
            TypeExprKind::Named { path, args } => {
                if !args.iter().all(|a| self.type_known(a, fr)) {
                    return false;
                }
                if path.len() == 1 && (fr.generics.contains_key(&path[0]) || matches!(path[0].as_str(), "any" | "int" | "float" | "string" | "bool" | "null" | "void" | "fn" | "task" | "module" | "list" | "map")) {
                    return true;
                }
                self.lookup_class(path, &fr.env).is_some()
            }
        }
    }

    /// Check `v` against `t`, converting `int` → `float` where required.
    pub fn coerce_to(&mut self, v: Value, t: &TypeExpr, fr: &Frame, what: &str) -> R<Value> {
        if self.conforms(&v, t, fr) {
            if let (Value::Int(i), true) = (&v, type_is_float(t)) {
                return Ok(Value::Float(*i as f64));
            }
            return Ok(v);
        }
        if !self.type_known(t, fr) {
            return self.err("TypeError", format!("unknown type `{}` in {}", t.display(), what));
        }
        let got = match &v {
            Value::List(_) => repr(&v),
            _ => v.type_name(),
        };
        let shown = self.subst(t, fr).display();
        self.err("TypeError", format!("{}: expected {}, got {}", what, shown, got))
    }

    // ───────── expressions ─────────

    pub fn truthy(&mut self, v: Value, span: Span) -> R<bool> {
        match v {
            Value::Bool(b) => Ok(b),
            Value::Null => Ok(false),
            other => self.err_at(span, "TypeError", format!("condition must be bool, got {}", other.type_name())),
        }
    }

    pub fn eval(&mut self, e: &Expr, fr: &Frame) -> R<Value> {
        match &e.kind {
            ExprKind::Null => Ok(Value::Null),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Int(i) => Ok(Value::Int(*i)),
            ExprKind::Float(f) => Ok(Value::Float(*f)),
            ExprKind::Str(s) => Ok(Value::str(s)),
            ExprKind::Template(parts) => {
                if parts.len() == 1 {
                    if let TemplatePart::Expr(x) = &parts[0] {
                        return self.eval(x, fr);
                    }
                }
                let mut s = String::new();
                for p in parts {
                    match p {
                        TemplatePart::Text(t) => s.push_str(t),
                        TemplatePart::Expr(x) => {
                            let v = self.eval(x, fr)?;
                            s.push_str(&self.to_display(&v)?);
                        }
                    }
                }
                Ok(Value::str(s))
            }
            ExprKind::List(items) => {
                let mut v = Vec::with_capacity(items.len());
                for i in items {
                    v.push(self.eval(i, fr)?);
                }
                Ok(Value::list(v))
            }
            ExprKind::Map(entries) => {
                let mut m = IndexMap::new();
                for (k, v) in entries {
                    let kv = self.eval(k, fr)?;
                    let key = match kv {
                        Value::Str(s) => s.to_string(),
                        other => return self.err_at(k.span, "TypeError", format!("map keys must be strings, got {}", other.type_name())),
                    };
                    let val = self.eval(v, fr)?;
                    m.insert(key, val);
                }
                Ok(Value::map(m))
            }
            ExprKind::Ident(name) => match fr.env.get(name) {
                Some(v) => Ok(v),
                None => {
                    let names = fr.env.all_names();
                    let mut msg = format!("undefined variable `{}`", name);
                    if let Some(s) = suggest(name, names.iter().map(|s| s.as_str())) {
                        msg.push_str(&format!(" (did you mean `{}`?)", s));
                    }
                    self.err_at(e.span, "NameError", msg)
                }
            },
            ExprKind::This => match &fr.this {
                Some(t) => Ok(t.clone()),
                None => self.err_at(e.span, "StateError", "`this` is not available here"),
            },
            ExprKind::Unary(op, x) => {
                let v = self.eval(x, fr)?;
                match (op, v) {
                    (UnOp::Neg, Value::Int(i)) => match i.checked_neg() {
                        Some(n) => Ok(Value::Int(n)),
                        None => self.err_at(e.span, "ArithmeticError", "integer overflow"),
                    },
                    (UnOp::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
                    (UnOp::Neg, o) => self.err_at(e.span, "TypeError", format!("cannot negate {}", o.type_name())),
                    (UnOp::Not, v) => Ok(Value::Bool(!self.truthy(v, e.span)?)),
                }
            }
            ExprKind::Binary(op, l, r) => self.eval_binary(*op, l, r, e.span, fr),
            ExprKind::Ternary(c, a, b) => {
                let cv = self.eval(c, fr)?;
                if self.truthy(cv, c.span)? {
                    self.eval(a, fr)
                } else {
                    self.eval(b, fr)
                }
            }
            ExprKind::Member { obj, name, optional } => {
                let o = self.eval(obj, fr)?;
                if *optional && matches!(o, Value::Null) {
                    return Ok(Value::Null);
                }
                self.cur_span = e.span;
                self.get_member(&o, name, fr)
            }
            ExprKind::Index { obj, index } => {
                let o = self.eval(obj, fr)?;
                let i = self.eval(index, fr)?;
                self.cur_span = e.span;
                self.index_value(&o, &i)
            }
            ExprKind::Slice { obj, from, to } => {
                let o = self.eval(obj, fr)?;
                let f = match from {
                    Some(x) => Some(self.eval(x, fr)?),
                    None => None,
                };
                let t = match to {
                    Some(x) => Some(self.eval(x, fr)?),
                    None => None,
                };
                self.cur_span = e.span;
                self.slice_value(&o, f, t)
            }
            ExprKind::Call { callee, args } => self.eval_call(callee, args, e.span, fr),
            ExprKind::New { class, args } => {
                let mut av = Vec::with_capacity(args.len());
                for a in args {
                    av.push(self.eval(a, fr)?);
                }
                self.cur_span = e.span;
                self.new_object(class, av, fr, e.span)
            }
            ExprKind::SuperMethod { method, args } => {
                let mut av = Vec::with_capacity(args.len());
                for a in args {
                    av.push(self.eval(a, fr)?);
                }
                self.cur_span = e.span;
                self.call_super(method, av, fr, e.span)
            }
            ExprKind::Lambda(decl) => Ok(Value::Func(Arc::new(Function { decl: decl.clone(), env: fr.env.clone(), class: fr.class.clone(), this: fr.this.clone(), module_name: fr.module.clone() }))),
            ExprKind::Is(x, t) => {
                let v = self.eval(x, fr)?;
                Ok(Value::Bool(self.conforms(&v, t, fr)))
            }
            ExprKind::As(x, t) => {
                let v = self.eval(x, fr)?;
                self.cur_span = e.span;
                self.cast(v, t, fr)
            }
            ExprKind::Await(x) => {
                let v = self.eval(x, fr)?;
                self.cur_span = e.span;
                self.await_value(v)
            }
        }
    }

    fn cast(&mut self, v: Value, t: &TypeExpr, fr: &Frame) -> R<Value> {
        if let TypeExprKind::Named { path, .. } = &t.kind {
            if path.len() == 1 {
                match (path[0].as_str(), &v) {
                    ("int", Value::Float(f)) if f.fract() == 0.0 && f.abs() < 9.2e18 => return Ok(Value::Int(*f as i64)),
                    ("int", Value::Str(s)) => {
                        return match s.trim().parse::<i64>() {
                            Ok(i) => Ok(Value::Int(i)),
                            Err(_) => self.err("ValueError", format!("cannot convert {} to int", quote_str(s))),
                        }
                    }
                    ("float", Value::Str(s)) => {
                        return match s.trim().parse::<f64>() {
                            Ok(i) => Ok(Value::Float(i)),
                            Err(_) => self.err("ValueError", format!("cannot convert {} to float", quote_str(s))),
                        }
                    }
                    ("string", _) => return Ok(Value::str(self.to_display(&v)?)),
                    ("bool", Value::Str(s)) => {
                        return match s.as_ref() {
                            "true" => Ok(Value::Bool(true)),
                            "false" => Ok(Value::Bool(false)),
                            _ => self.err("ValueError", format!("cannot convert {} to bool", quote_str(s))),
                        }
                    }
                    _ => {}
                }
            }
        }
        if self.conforms(&v, t, fr) {
            return self.coerce_to(v, t, fr, "cast");
        }
        self.err("TypeError", format!("cannot cast {} to {}", v.type_name(), t.display()))
    }

    fn eval_binary(&mut self, op: BinOp, l: &Expr, r: &Expr, span: Span, fr: &Frame) -> R<Value> {
        match op {
            BinOp::And => {
                let lv = self.eval(l, fr)?;
                if !self.truthy(lv, l.span)? {
                    return Ok(Value::Bool(false));
                }
                let rv = self.eval(r, fr)?;
                return Ok(Value::Bool(self.truthy(rv, r.span)?));
            }
            BinOp::Or => {
                let lv = self.eval(l, fr)?;
                if self.truthy(lv, l.span)? {
                    return Ok(Value::Bool(true));
                }
                let rv = self.eval(r, fr)?;
                return Ok(Value::Bool(self.truthy(rv, r.span)?));
            }
            BinOp::Coalesce => {
                let lv = self.eval(l, fr)?;
                if !matches!(lv, Value::Null) {
                    return Ok(lv);
                }
                return self.eval(r, fr);
            }
            _ => {}
        }
        let lv = self.eval(l, fr)?;
        let rv = self.eval(r, fr)?;
        self.cur_span = span;
        self.binary(op, lv, rv)
    }

    pub fn binary(&mut self, op: BinOp, l: Value, r: Value) -> R<Value> {
        use Value::*;
        match op {
            BinOp::Eq => return self.equals(&l, &r).map(Bool),
            BinOp::Ne => return self.equals(&l, &r).map(|b| Bool(!b)),
            BinOp::In => {
                return match &r {
                    List(items) => {
                        let items = lk(items).clone();
                        Ok(Bool(items.iter().any(|x| values_equal(x, &l))))
                    }
                    Map(m) => match &l {
                        Str(k) => Ok(Bool(lk(m).contains_key(k.as_ref()))),
                        _ => Ok(Bool(false)),
                    },
                    Str(s) => match &l {
                        Str(sub) => Ok(Bool(s.contains(sub.as_ref()))),
                        _ => self.err("TypeError", "`in` on a string needs a string on the left"),
                    },
                    other => self.err("TypeError", format!("`in` is not supported for {}", other.type_name())),
                };
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let ord = match (&l, &r) {
                    (Int(a), Int(b)) => a.partial_cmp(b),
                    (Int(a), Float(b)) => (*a as f64).partial_cmp(b),
                    (Float(a), Int(b)) => a.partial_cmp(&(*b as f64)),
                    (Float(a), Float(b)) => a.partial_cmp(b),
                    (Str(a), Str(b)) => a.partial_cmp(b),
                    _ => return self.err("TypeError", format!("cannot compare {} {} {}", l.type_name(), op.symbol(), r.type_name())),
                };
                let o = match ord {
                    Some(o) => o,
                    None => return Ok(Bool(false)), // NaN
                };
                use std::cmp::Ordering::*;
                return Ok(Bool(match op {
                    BinOp::Lt => o == Less,
                    BinOp::Le => o != Greater,
                    BinOp::Gt => o == Greater,
                    _ => o != Less,
                }));
            }
            _ => {}
        }
        match (op, &l, &r) {
            (BinOp::Add, Int(a), Int(b)) => a.checked_add(*b).map(Int).map_or_else(|| self.err("ArithmeticError", "integer overflow in +"), Ok),
            (BinOp::Sub, Int(a), Int(b)) => a.checked_sub(*b).map(Int).map_or_else(|| self.err("ArithmeticError", "integer overflow in -"), Ok),
            (BinOp::Mul, Int(a), Int(b)) => a.checked_mul(*b).map(Int).map_or_else(|| self.err("ArithmeticError", "integer overflow in *"), Ok),
            (BinOp::Div, Int(a), Int(b)) => {
                if *b == 0 {
                    self.err("ArithmeticError", "division by zero")
                } else if a % b == 0 {
                    Ok(Int(a / b))
                } else {
                    Ok(Float(*a as f64 / *b as f64))
                }
            }
            (BinOp::Mod, Int(a), Int(b)) => {
                if *b == 0 {
                    self.err("ArithmeticError", "modulo by zero")
                } else {
                    Ok(Int(a.wrapping_rem(*b)))
                }
            }
            (BinOp::Pow, Int(a), Int(b)) if *b >= 0 => match u32::try_from(*b).ok().and_then(|e| a.checked_pow(e)) {
                Some(v) => Ok(Int(v)),
                None => Ok(Float((*a as f64).powf(*b as f64))),
            },
            (BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::Pow, Int(_) | Float(_), Int(_) | Float(_)) => {
                let a = num(&l);
                let b = num(&r);
                match op {
                    BinOp::Add => Ok(Float(a + b)),
                    BinOp::Sub => Ok(Float(a - b)),
                    BinOp::Mul => Ok(Float(a * b)),
                    BinOp::Div => {
                        if b == 0.0 {
                            self.err("ArithmeticError", "division by zero")
                        } else {
                            Ok(Float(a / b))
                        }
                    }
                    BinOp::Mod => {
                        if b == 0.0 {
                            self.err("ArithmeticError", "modulo by zero")
                        } else {
                            Ok(Float(a % b))
                        }
                    }
                    _ => Ok(Float(a.powf(b))),
                }
            }
            (BinOp::Add, Str(a), _) => {
                let rs = self.to_display(&r)?;
                Ok(Value::str(format!("{}{}", a, rs)))
            }
            (BinOp::Add, _, Str(b)) => {
                let ls = self.to_display(&l)?;
                Ok(Value::str(format!("{}{}", ls, b)))
            }
            (BinOp::Add, List(a), List(b)) => {
                let mut v = lk(a).clone();
                v.extend(lk(b).iter().cloned());
                Ok(Value::list(v))
            }
            _ => self.err("TypeError", format!("unsupported operands: {} {} {}", l.type_name(), op.symbol(), r.type_name())),
        }
    }

    /// `==` honouring user-defined `equals(other)` on objects.
    pub fn equals(&mut self, a: &Value, b: &Value) -> R<bool> {
        if let Value::Object(o) = a {
            if o.class.kind != ClassKind::Enum {
                let found = self.find_methods(&o.class, "equals");
                if let Some((owner, d)) = found.iter().find(|(_, d)| d.params.len() == 1 && !d.is_static) {
                    let r = self.invoke_method(a.clone(), Some(owner), d, vec![b.clone()], self.cur_span)?;
                    return Ok(matches!(r, Value::Bool(true)));
                }
            }
        }
        Ok(values_equal(a, b))
    }

    pub fn index_value(&mut self, o: &Value, i: &Value) -> R<Value> {
        match (o, i) {
            (Value::List(l), Value::Int(n)) => {
                let items = lk(l);
                let len = items.len() as i64;
                let idx = if *n < 0 { n + len } else { *n };
                if idx < 0 || idx >= len {
                    drop(items);
                    return self.err("IndexError", format!("list index {} out of range (length {})", n, len));
                }
                Ok(items[idx as usize].clone())
            }
            (Value::Str(s), Value::Int(n)) => {
                let chars: Vec<char> = s.chars().collect();
                let len = chars.len() as i64;
                let idx = if *n < 0 { n + len } else { *n };
                if idx < 0 || idx >= len {
                    return self.err("IndexError", format!("string index {} out of range (length {})", n, len));
                }
                Ok(Value::str(chars[idx as usize].to_string()))
            }
            (Value::Map(m), Value::Str(k)) => {
                let found = lk(m).get(k.as_ref()).cloned();
                match found {
                    Some(v) => Ok(v),
                    None => self.err("KeyError", format!("key {} not found", quote_str(k))),
                }
            }
            (Value::Object(_), Value::Str(k)) => {
                let fr = Frame::new(self.interp.root.clone(), "<index>");
                self.get_member(o, k, &fr)
            }
            _ => self.err("TypeError", format!("cannot index {} with {}", o.type_name(), i.type_name())),
        }
    }

    fn slice_value(&mut self, o: &Value, from: Option<Value>, to: Option<Value>) -> R<Value> {
        let bound = |ex: &mut Exec, v: Option<Value>, default: i64, len: i64| -> R<i64> {
            match v {
                None | Some(Value::Null) => Ok(default),
                Some(Value::Int(n)) => Ok(if n < 0 { (n + len).max(0) } else { n.min(len) }),
                Some(other) => ex.err("TypeError", format!("slice bounds must be int, got {}", other.type_name())),
            }
        };
        match o {
            Value::List(l) => {
                let items = lk(l).clone();
                let len = items.len() as i64;
                let a = bound(self, from, 0, len)?;
                let b = bound(self, to, len, len)?;
                Ok(Value::list(if a < b { items[a as usize..b as usize].to_vec() } else { vec![] }))
            }
            Value::Str(s) => {
                let chars: Vec<char> = s.chars().collect();
                let len = chars.len() as i64;
                let a = bound(self, from, 0, len)?;
                let b = bound(self, to, len, len)?;
                Ok(Value::str(if a < b { chars[a as usize..b as usize].iter().collect::<String>() } else { String::new() }))
            }
            other => self.err("TypeError", format!("cannot slice {}", other.type_name())),
        }
    }

    // ───────── calls ─────────

    fn eval_args(&mut self, args: &[Expr], fr: &Frame) -> R<Vec<Value>> {
        let mut v = Vec::with_capacity(args.len());
        for a in args {
            v.push(self.eval(a, fr)?);
        }
        Ok(v)
    }

    fn eval_call(&mut self, callee: &Expr, args: &[Expr], span: Span, fr: &Frame) -> R<Value> {
        if let ExprKind::Member { obj, name, optional } = &callee.kind {
            let recv = self.eval(obj, fr)?;
            if *optional && matches!(recv, Value::Null) {
                return Ok(Value::Null);
            }
            let av = self.eval_args(args, fr)?;
            self.cur_span = span;
            return self.call_member(recv, name, av, fr, span);
        }
        let f = self.eval(callee, fr)?;
        let av = self.eval_args(args, fr)?;
        self.cur_span = span;
        self.call_value(&f, av, span)
    }

    pub fn call_value(&mut self, f: &Value, args: Vec<Value>, span: Span) -> R<Value> {
        match f {
            Value::Func(func) => self.call_function(func, args, span),
            Value::Native(n) => {
                if args.len() < n.min || n.max.map_or(false, |m| args.len() > m) {
                    let exp = match n.max {
                        Some(m) if m == n.min => format!("{}", m),
                        Some(m) => format!("{}..{}", n.min, m),
                        None => format!("at least {}", n.min),
                    };
                    return self.err_at(span, "TypeError", format!("{}() expects {} argument(s), got {}", n.name, exp, args.len()));
                }
                self.check_cancel()?;
                (n.f)(self, &args)
            }
            Value::Bound(b) => {
                let b = b.clone();
                self.call_bound(&b, args, span)
            }
            Value::Class(c) => self.err_at(span, "TypeError", format!("class {} must be instantiated with `new`", c.name)),
            other => self.err_at(span, "TypeError", format!("{} is not callable", other.type_name())),
        }
    }

    pub fn check_cancel(&mut self) -> R<()> {
        if self.is_cancelled() {
            return self.err("CancelledError", "operation cancelled");
        }
        Ok(())
    }

    pub fn call_function(&mut self, f: &Arc<Function>, args: Vec<Value>, span: Span) -> R<Value> {
        let generics = Arc::new(HashMap::new());
        let name = match &f.class {
            Some(c) => format!("{}.{}", c.name, f.decl.name),
            None => f.decl.name.clone(),
        };
        if f.decl.is_async {
            return Ok(self.spawn_async(f.decl.clone(), f.env.clone(), f.this.clone(), f.class.clone(), f.module_name.clone(), args, span, generics, name));
        }
        self.invoke(&f.decl, &f.env, f.this.clone(), f.class.clone(), &f.module_name, args, span, generics, false, &name)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_async(&mut self, decl: Arc<FuncDecl>, env: Env, this: Option<Value>, class: Option<Arc<Class>>, module: Arc<str>, args: Vec<Value>, span: Span, generics: Arc<HashMap<String, TypeExpr>>, name: String) -> Value {
        let cancel = Arc::new(AtomicBool::new(false));
        let task = Arc::new(Task { name: name.clone(), state: Mutex::new(None), cv: std::sync::Condvar::new(), cancel: cancel.clone() });
        let mut child = self.child_exec(Some(cancel));
        let t2 = task.clone();
        let spawn = std::thread::Builder::new().name(format!("nin-task-{}", name)).stack_size(TASK_STACK).spawn(move || {
            let r = child.invoke_async_body(&decl, &env, this, class, &module, args, span, generics, &name);
            *lk(&t2.state) = Some(r);
            t2.cv.notify_all();
        });
        if let Err(e) = spawn {
            *lk(&task.state) = Some(Err(Signal::Throw(Box::new(Throw { value: Value::str(format!("cannot start task: {}", e)), trace: vec![] }))));
        }
        Value::Task(task)
    }

    #[allow(clippy::too_many_arguments)]
    fn invoke_async_body(&mut self, decl: &Arc<FuncDecl>, env: &Env, this: Option<Value>, class: Option<Arc<Class>>, module: &Arc<str>, args: Vec<Value>, span: Span, generics: Arc<HashMap<String, TypeExpr>>, name: &str) -> R<Value> {
        self.invoke(decl, env, this, class, module, args, span, generics, false, name)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn invoke(&mut self, decl: &Arc<FuncDecl>, env: &Env, this: Option<Value>, class: Option<Arc<Class>>, module: &Arc<str>, args: Vec<Value>, span: Span, generics: Arc<HashMap<String, TypeExpr>>, in_ctor: bool, name: &str) -> R<Value> {
        if self.depth >= MAX_DEPTH {
            return self.err_at(span, "StateError", "stack overflow (call depth exceeded)");
        }
        self.check_cancel()?;
        let body = match &decl.body {
            Some(b) => b,
            None => return self.err_at(span, "StateError", format!("method `{}` is abstract and has no body", decl.name)),
        };
        let scope = Scope::new(Some(env.clone()));
        let mut gen = (*generics).clone();
        for g in &decl.generics {
            gen.entry(g.name.clone()).or_insert_with(|| g.bound.clone().unwrap_or_else(|| TypeExpr::named("any", Span::DUMMY)));
        }
        let fr = Frame { env: scope.clone(), this, class, in_ctor, generics: Arc::new(gen), fname: Arc::from(name), module: module.clone() };
        self.bind_params(decl, args, &fr, name, span)?;
        self.trace.push((name.to_string(), span));
        self.depth += 1;
        let saved_class = std::mem::replace(&mut self.cur_class, fr.class.clone());
        let res = self.exec_stmts(body, &fr);
        self.cur_class = saved_class;
        self.depth -= 1;
        let flow = match res {
            Ok(f) => {
                self.trace.pop();
                f
            }
            Err(e) => {
                self.trace.pop();
                return Err(e);
            }
        };
        let v = match flow {
            Flow::Return(v) => v,
            _ => Value::Null,
        };
        match &decl.ret {
            Some(t) => {
                if decl.is_async {
                    Ok(v)
                } else {
                    self.coerce_to(v, t, &fr, &format!("return value of `{}`", name))
                }
            }
            None => Ok(v),
        }
    }

    fn bind_params(&mut self, decl: &FuncDecl, mut args: Vec<Value>, fr: &Frame, name: &str, span: Span) -> R<()> {
        let variadic = decl.params.last().map_or(false, |p| p.variadic);
        let fixed = if variadic { decl.params.len() - 1 } else { decl.params.len() };
        let required = decl.params.iter().take(fixed).filter(|p| p.default.is_none()).count();
        if args.len() < required || (!variadic && args.len() > fixed) {
            let exp = if variadic { format!("at least {}", required) } else if required == fixed { format!("{}", fixed) } else { format!("{}..{}", required, fixed) };
            return self.err_at(span, "TypeError", format!("{}() expects {} argument(s), got {}", name, exp, args.len()));
        }
        let rest: Vec<Value> = if variadic && args.len() > fixed { args.split_off(fixed) } else { Vec::new() };
        let mut it = args.into_iter();
        for (i, p) in decl.params.iter().take(fixed).enumerate() {
            let v = match it.next() {
                Some(v) => v,
                None => match &p.default {
                    Some(d) => self.eval(d, fr)?,
                    None => Value::Null,
                },
            };
            let v = match &p.ty {
                Some(t) => self.coerce_to(v, t, fr, &format!("parameter `{}` (#{}) of `{}`", p.name, i + 1, name))?,
                None => v,
            };
            fr.env.define(&p.name, v, false);
        }
        if variadic {
            if let Some(p) = decl.params.last() {
                if let Some(t) = &p.ty {
                    for r in &rest {
                        let _ = self.coerce_to(r.clone(), t, fr, &format!("variadic parameter `{}` of `{}`", p.name, name))?;
                    }
                }
                fr.env.define(&p.name, Value::list(rest), false);
            }
        }
        Ok(())
    }

    // ───────── tasks ─────────

    pub fn await_task(&mut self, t: &Arc<Task>) -> R<Value> {
        loop {
            {
                let g = lk(&t.state);
                if let Some(r) = g.as_ref() {
                    return clone_result(r);
                }
                let (g2, _) = t.cv.wait_timeout(g, Duration::from_millis(20)).unwrap_or_else(|e| e.into_inner());
                if let Some(r) = g2.as_ref() {
                    return clone_result(r);
                }
            }
            if self.is_cancelled() {
                t.cancel.store(true, Ordering::SeqCst);
                return self.err("CancelledError", "cancelled while waiting for a task");
            }
        }
    }

    pub fn await_value(&mut self, v: Value) -> R<Value> {
        match v {
            Value::Task(t) => self.await_task(&t),
            Value::List(l) => {
                let items = lk(&l).clone();
                let mut out = Vec::with_capacity(items.len());
                for i in items {
                    out.push(self.await_value(i)?);
                }
                Ok(Value::list(out))
            }
            other => Ok(other),
        }
    }

    fn spawn_block(&mut self, body: &Block, fr: &Frame, name: &str) -> Arc<Task> {
        let cancel = Arc::new(AtomicBool::new(false));
        let task = Arc::new(Task { name: name.to_string(), state: Mutex::new(None), cv: std::sync::Condvar::new(), cancel: cancel.clone() });
        let mut child = self.child_exec(Some(cancel));
        let t2 = task.clone();
        let body = body.clone();
        let fr2 = fr.child();
        let spawn = std::thread::Builder::new().name(format!("nin-{}", name)).stack_size(TASK_STACK).spawn(move || {
            let r = match child.exec_stmts(&body, &fr2) {
                Ok(Flow::Return(v)) => Ok(v),
                Ok(_) => Ok(Value::Null),
                Err(e) => Err(e),
            };
            *lk(&t2.state) = Some(r);
            t2.cv.notify_all();
        });
        if spawn.is_err() {
            *lk(&task.state) = Some(Err(Signal::Throw(Box::new(Throw { value: Value::str("cannot start task thread"), trace: vec![] }))));
        }
        task
    }

    /// Run a callable on its own thread; returns the task handle.
    pub fn spawn_callable(&mut self, f: Value, args: Vec<Value>, name: &str) -> Value {
        let cancel = Arc::new(AtomicBool::new(false));
        let task = Arc::new(Task { name: name.to_string(), state: Mutex::new(None), cv: std::sync::Condvar::new(), cancel: cancel.clone() });
        let mut child = self.child_exec(Some(cancel));
        let t2 = task.clone();
        let span = self.cur_span;
        let spawn = std::thread::Builder::new().name(format!("nin-{}", name)).stack_size(TASK_STACK).spawn(move || {
            let r = child.call_value(&f, args, span);
            *lk(&t2.state) = Some(r);
            t2.cv.notify_all();
        });
        if spawn.is_err() {
            *lk(&task.state) = Some(Err(Signal::Throw(Box::new(Throw { value: Value::str("cannot start task thread"), trace: vec![] }))));
        }
        Value::Task(task)
    }

    /// Sleep that wakes up on cancellation.
    pub fn sleep_ms(&mut self, ms: u64) -> R<()> {
        let end = Instant::now() + Duration::from_millis(ms);
        loop {
            self.check_cancel()?;
            let now = Instant::now();
            if now >= end {
                return Ok(());
            }
            std::thread::sleep((end - now).min(Duration::from_millis(5)));
        }
    }

    // ───────── statements ─────────

    pub fn exec_block(&mut self, stmts: &[Stmt], fr: &Frame) -> R<Flow> {
        let fr2 = fr.child();
        self.exec_stmts(stmts, &fr2)
    }

    pub fn exec_stmts(&mut self, stmts: &[Stmt], fr: &Frame) -> R<Flow> {
        for s in stmts {
            match self.exec_stmt(s, fr)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    fn for_items(&mut self, it: Value, two: bool) -> R<Vec<(Value, Value)>> {
        Ok(match it {
            Value::List(l) => lk(&l).clone().into_iter().enumerate().map(|(i, v)| (Value::Int(i as i64), v)).collect(),
            Value::Str(s) => s.chars().enumerate().map(|(i, c)| (Value::Int(i as i64), Value::str(c.to_string()))).collect(),
            Value::Map(m) => lk(&m).clone().into_iter().map(|(k, v)| (Value::str(k), v)).collect(),
            Value::Null => vec![],
            other => {
                let _ = two;
                return self.err("TypeError", format!("cannot iterate over {}", other.type_name()));
            }
        })
    }

    pub fn exec_stmt(&mut self, s: &Stmt, fr: &Frame) -> R<Flow> {
        self.cur_span = s.span;
        match &s.kind {
            StmtKind::Let { name, ty, value, is_const } => {
                let mut v = match value {
                    Some(e) => self.eval(e, fr)?,
                    None => Value::Null,
                };
                if let Some(t) = ty {
                    v = self.coerce_to(v, t, fr, &format!("variable `{}`", name))?;
                }
                fr.env.define(name, v, *is_const);
                Ok(Flow::Normal)
            }
            StmtKind::Assign { target, op, value } => {
                let mut v = self.eval(value, fr)?;
                self.cur_span = s.span;
                if let Some(op) = op {
                    let cur = self.eval(target, fr)?;
                    v = self.binary(*op, cur, v)?;
                }
                self.assign(target, v, fr)?;
                Ok(Flow::Normal)
            }
            StmtKind::Expr(e) => {
                self.eval(e, fr)?;
                Ok(Flow::Normal)
            }
            StmtKind::Print { value, newline, stderr } => {
                let text = match (&value.kind, ) {
                    (ExprKind::List(items), ) => {
                        let mut parts = vec![];
                        for i in items {
                            let v = self.eval(i, fr)?;
                            parts.push(self.to_display(&v)?);
                        }
                        parts.join(" ")
                    }
                    _ => {
                        let v = self.eval(value, fr)?;
                        self.to_display(&v)?
                    }
                };
                let mut t = text;
                if *newline {
                    t.push('\n');
                }
                self.out(&t, *stderr);
                Ok(Flow::Normal)
            }
            StmtKind::If { cond, then, otherwise } => {
                let c = self.eval(cond, fr)?;
                if self.truthy(c, cond.span)? {
                    self.exec_block(then, fr)
                } else {
                    self.exec_block(otherwise, fr)
                }
            }
            StmtKind::While { cond, body } => {
                loop {
                    self.check_cancel()?;
                    let c = self.eval(cond, fr)?;
                    if !self.truthy(c, cond.span)? {
                        break;
                    }
                    match self.exec_block(body, fr)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                }
                Ok(Flow::Normal)
            }
            StmtKind::For { vars, iter, body } => {
                let it = self.eval(iter, fr)?;
                let two = vars.len() == 2;
                let items = self.for_items(it, two)?;
                for (k, v) in items {
                    self.check_cancel()?;
                    let lf = fr.child();
                    if two {
                        lf.env.define(&vars[0], k, false);
                        lf.env.define(&vars[1], v, false);
                    } else {
                        // one variable: list items / chars / map keys
                        let single = match &k {
                            Value::Str(_) => k, // map key
                            _ => v,
                        };
                        lf.env.define(&vars[0], single, false);
                    }
                    match self.exec_stmts(body, &lf)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                }
                Ok(Flow::Normal)
            }
            StmtKind::Match { subject, cases, default } => {
                let sv = self.eval(subject, fr)?;
                for c in cases {
                    for w in &c.when {
                        let wv = self.eval(w, fr)?;
                        if self.equals(&sv, &wv)? {
                            return self.exec_block(&c.body, fr);
                        }
                    }
                }
                self.exec_block(default, fr)
            }
            StmtKind::Break => Ok(Flow::Break),
            StmtKind::Continue => Ok(Flow::Continue),
            StmtKind::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e, fr)?,
                    None => Value::Null,
                };
                Ok(Flow::Return(v))
            }
            StmtKind::Throw(e) => {
                let v = self.eval(e, fr)?;
                self.cur_span = s.span;
                Err(self.throw_value(v))
            }
            StmtKind::Try { body, catches, finally } => {
                let result = self.exec_block(body, fr);
                let result = match result {
                    Err(Signal::Throw(t)) => {
                        let mut handled: Option<R<Flow>> = None;
                        for c in catches {
                            let matches = match &c.ty {
                                None => true,
                                Some(ty) => self.conforms(&t.value, ty, fr),
                            };
                            if matches {
                                let cf = fr.child();
                                if let Some(var) = &c.var {
                                    cf.env.define(var, t.value.clone(), false);
                                }
                                handled = Some(self.exec_stmts(&c.body, &cf));
                                break;
                            }
                        }
                        match handled {
                            Some(r) => r,
                            None => Err(Signal::Throw(t)),
                        }
                    }
                    other => other,
                };
                if !finally.is_empty() && !matches!(result, Err(Signal::Exit(_))) {
                    match self.exec_block(finally, fr)? {
                        Flow::Normal => {}
                        other => return Ok(other),
                    }
                }
                result
            }
            StmtKind::Run(r) => {
                self.exec_run(r, fr, s.span)?;
                Ok(Flow::Normal)
            }
            StmtKind::Parallel { branches, max } => {
                let limit = match max {
                    Some(m) => match self.eval(m, fr)? {
                        Value::Int(n) if n >= 1 => n as usize,
                        _ => branches.len().max(1),
                    },
                    None => branches.len().max(1),
                };
                let mut first_err: Option<Signal> = None;
                let mut idx = 0;
                let mut running: Vec<Arc<Task>> = Vec::new();
                let mut finished = 0usize;
                while finished < branches.len() {
                    while idx < branches.len() && running.len() < limit && first_err.is_none() {
                        running.push(self.spawn_block(&branches[idx], fr, &format!("parallel-{}", idx)));
                        idx += 1;
                    }
                    if running.is_empty() {
                        break;
                    }
                    // wait for the first finished one
                    let mut done_at = None;
                    for (i, t) in running.iter().enumerate() {
                        if lk(&t.state).is_some() {
                            done_at = Some(i);
                            break;
                        }
                    }
                    match done_at {
                        Some(i) => {
                            let t = running.remove(i);
                            finished += 1;
                            if let Err(e) = self.await_task(&t) {
                                if first_err.is_none() {
                                    for r in &running {
                                        r.cancel.store(true, Ordering::SeqCst);
                                    }
                                    first_err = Some(e);
                                }
                            }
                        }
                        None => {
                            if self.is_cancelled() {
                                for r in &running {
                                    r.cancel.store(true, Ordering::SeqCst);
                                }
                                if first_err.is_none() {
                                    return self.err("CancelledError", "parallel block cancelled");
                                }
                            }
                            std::thread::sleep(Duration::from_millis(2));
                        }
                    }
                    if first_err.is_some() && idx < branches.len() {
                        // branches never started count as finished
                        finished += branches.len() - idx;
                        idx = branches.len();
                    }
                }
                match first_err {
                    Some(e) => Err(e),
                    None => Ok(Flow::Normal),
                }
            }
            StmtKind::Spawn { body, name } => {
                let t = self.spawn_block(body, fr, name.as_deref().unwrap_or("spawn"));
                if let Some(n) = name {
                    fr.env.define(n, Value::Task(t), false);
                }
                Ok(Flow::Normal)
            }
            StmtKind::Await { task, into } => {
                let t = self.eval(task, fr)?;
                self.cur_span = s.span;
                let r = self.await_value(t)?;
                if let Some(n) = into {
                    fr.env.define(n, r, false);
                }
                Ok(Flow::Normal)
            }
            StmtKind::Block(b) => self.exec_block(b, fr),
            StmtKind::Assert { cond, message } => {
                let c = self.eval(cond, fr)?;
                if !self.truthy(c, cond.span)? {
                    let msg = match message {
                        Some(m) => {
                            let v = self.eval(m, fr)?;
                            self.to_display(&v)?
                        }
                        None => "assertion failed".to_string(),
                    };
                    return self.err_at(s.span, "AssertionError", msg);
                }
                Ok(Flow::Normal)
            }
            StmtKind::Output { name, value } => {
                let v = self.eval(value, fr)?;
                match fr.env.get("__outputs") {
                    Some(Value::Map(m)) => {
                        lk(&m).insert(name.clone(), v);
                        Ok(Flow::Normal)
                    }
                    _ => self.err_at(s.span, "StateError", "`output` is only valid inside workflow step scripts"),
                }
            }
            StmtKind::SuperCall(args) => {
                let av = self.eval_args(args, fr)?;
                self.cur_span = s.span;
                self.super_ctor(av, fr, s.span)?;
                Ok(Flow::Normal)
            }
            StmtKind::Exit(e) => {
                let code = match e {
                    Some(e) => match self.eval(e, fr)? {
                        Value::Int(i) => i as i32,
                        Value::Bool(b) => if b { 0 } else { 1 },
                        Value::Null => 0,
                        other => return self.err_at(s.span, "TypeError", format!("exit code must be int, got {}", other.type_name())),
                    },
                    None => 0,
                };
                Err(Signal::Exit(code))
            }
        }
    }

    pub fn assign(&mut self, target: &Expr, v: Value, fr: &Frame) -> R<()> {
        match &target.kind {
            ExprKind::Ident(name) => match fr.env.assign(name, v) {
                Ok(()) => Ok(()),
                Err(AssignError::Const) => self.err_at(target.span, "StateError", format!("cannot assign to constant `{}`", name)),
                Err(AssignError::NotFound) => {
                    let names = fr.env.all_names();
                    let mut msg = format!("assignment to undeclared variable `{}` (declare it with `let`)", name);
                    if let Some(s) = suggest(name, names.iter().map(|s| s.as_str())) {
                        msg.push_str(&format!(" — did you mean `{}`?", s));
                    }
                    self.err_at(target.span, "NameError", msg)
                }
            },
            ExprKind::Member { obj, name, .. } => {
                let o = self.eval(obj, fr)?;
                self.cur_span = target.span;
                self.set_member(&o, name, v, fr)
            }
            ExprKind::Index { obj, index } => {
                let o = self.eval(obj, fr)?;
                let i = self.eval(index, fr)?;
                self.cur_span = target.span;
                match (&o, &i) {
                    (Value::List(l), Value::Int(n)) => {
                        let mut items = lk(l);
                        let len = items.len() as i64;
                        let idx = if *n < 0 { n + len } else { *n };
                        if idx < 0 || idx >= len {
                            drop(items);
                            return self.err("IndexError", format!("list index {} out of range (length {})", n, len));
                        }
                        items[idx as usize] = v;
                        Ok(())
                    }
                    (Value::Map(m), Value::Str(k)) => {
                        lk(m).insert(k.to_string(), v);
                        Ok(())
                    }
                    (Value::Object(_), Value::Str(k)) => self.set_member(&o, k, v, fr),
                    _ => self.err("TypeError", format!("cannot assign into {} with index {}", o.type_name(), i.type_name())),
                }
            }
            _ => self.err_at(target.span, "TypeError", "invalid assignment target"),
        }
    }

    // ───────── external processes ─────────

    pub fn duration_ms(&mut self, v: &Value) -> R<u64> {
        match v {
            Value::Int(i) if *i >= 0 => Ok(*i as u64),
            Value::Float(f) if *f >= 0.0 => Ok(*f as u64),
            Value::Str(s) => match parse_duration_ms(s) {
                Some(ms) => Ok(ms),
                None => self.err("ValueError", format!("invalid duration {} (use e.g. 500ms, 30s, 5m, 2h, 1d, 1h30m)", quote_str(s))),
            },
            other => self.err("TypeError", format!("duration must be a number of ms or a string like \"30s\", got {}", other.type_name())),
        }
    }

    pub fn size_bytes(&mut self, v: &Value) -> R<u64> {
        match v {
            Value::Int(i) if *i >= 0 => Ok(*i as u64),
            Value::Str(s) => match parse_size(s) {
                Some(n) => Ok(n),
                None => self.err("ValueError", format!("invalid size {} (use e.g. 512MB, 2GB)", quote_str(s))),
            },
            other => self.err("TypeError", format!("size must be bytes or a string like \"512MB\", got {}", other.type_name())),
        }
    }

    fn out_spec(&mut self, r: &Option<Redirect>, into: bool, fr: &Frame, is_err: bool) -> R<OutSpec> {
        Ok(match r {
            Some(Redirect::Inherit) => OutSpec::Inherit,
            Some(Redirect::Null) => OutSpec::Null,
            Some(Redirect::Capture) => OutSpec::Capture,
            Some(Redirect::Merge) => {
                if is_err {
                    OutSpec::MergeIntoStdout
                } else {
                    OutSpec::Capture
                }
            }
            Some(Redirect::File { path, append }) => {
                let p = self.eval(path, fr)?;
                let ps = self.to_display(&p)?;
                OutSpec::File { path: PathBuf::from(ps), append: *append }
            }
            None => {
                if into || self.sink.is_some() {
                    OutSpec::Capture
                } else {
                    OutSpec::Inherit
                }
            }
        })
    }

    pub fn build_proc_spec(&mut self, r: &RunStmt, fr: &Frame) -> R<(ProcSpec, Option<Duration>)> {
        let prog = self.eval(&r.program, fr)?;
        let prog = self.to_display(&prog)?;
        let mut args = Vec::new();
        for a in &r.args {
            let v = self.eval(a, fr)?;
            args.push(self.to_display(&v)?);
        }
        let mut spec = if r.shell {
            let mut line = prog.clone();
            for a in &args {
                line.push(' ');
                line.push_str(a);
            }
            ProcSpec::shell(line)
        } else {
            ProcSpec::new(prog).args(args)
        };
        if let Some(c) = &r.cwd {
            let v = self.eval(c, fr)?;
            spec.cwd = Some(PathBuf::from(self.to_display(&v)?));
        }
        for (k, e) in &r.env {
            let v = self.eval(e, fr)?;
            spec.env.push((k.clone(), self.to_display(&v)?));
        }
        if let Some(s) = &r.stdin {
            let v = self.eval(s, fr)?;
            spec.stdin = StdinSpec::Text(self.to_display(&v)?);
        } else if let Some(f) = &r.stdin_file {
            let v = self.eval(f, fr)?;
            spec.stdin = StdinSpec::File(PathBuf::from(self.to_display(&v)?));
        } else if self.sink.is_some() {
            spec.stdin = StdinSpec::Null;
        }
        let into = r.into.is_some();
        spec.stdout = self.out_spec(&r.stdout, into, fr, false)?;
        spec.stderr = self.out_spec(&r.stderr, into, fr, true)?;
        let timeout = match &r.timeout {
            Some(t) => {
                let v = self.eval(t, fr)?;
                Some(Duration::from_millis(self.duration_ms(&v)?))
            }
            None => None,
        };
        Ok((spec, timeout))
    }

    /// Run a process honouring cancellation, timeout and the job's process group.
    pub fn run_process(&mut self, spec: &ProcSpec, timeout: Option<Duration>) -> R<nin_win::ProcResult> {
        self.check_cancel()?;
        let group = match &self.group {
            Some(g) => g.clone(),
            None => match nin_win::ProcessGroup::new(nin_win::Limits { kill_on_close: true, ..Default::default() }) {
                Ok(g) => Arc::new(g),
                Err(e) => return self.err("ProcessError", format!("cannot create process group: {}", e)),
            },
        };
        let mut child = match group.spawn(spec) {
            Ok(c) => c,
            Err(e) => {
                let code = if e.kind() == std::io::ErrorKind::NotFound { "not_found" } else { "spawn_failed" };
                let ev = self.make_error("ProcessError", e.to_string());
                if let Value::Object(o) = &ev {
                    lk(&o.fields).insert("code".into(), Value::str(code));
                }
                return Err(self.throw_value(ev));
            }
        };
        let start = Instant::now();
        let mut timed_out = false;
        let mut cancelled = false;
        let res = loop {
            match child.try_wait() {
                Ok(Some(r)) => break r,
                Ok(None) => {}
                Err(e) => return self.err("ProcessError", format!("wait failed: {}", e)),
            }
            if self.is_cancelled() && !cancelled {
                cancelled = true;
                let _ = child.kill();
            }
            if let Some(t) = timeout {
                if !timed_out && start.elapsed() >= t {
                    timed_out = true;
                    let _ = child.kill();
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let mut res = res;
        res.timed_out = res.timed_out || timed_out;
        res.cancelled = res.cancelled || cancelled;
        if res.cancelled {
            return self.err("CancelledError", "process cancelled");
        }
        Ok(res)
    }

    pub fn proc_result_value(&self, r: &nin_win::ProcResult) -> Value {
        Value::map_from([
            ("code", Value::Int(r.code as i64)),
            ("ok", Value::Bool(r.ok())),
            ("stdout", Value::str(r.stdout_text())),
            ("stderr", Value::str(r.stderr_text())),
            ("duration_ms", Value::Int(r.duration.as_millis() as i64)),
            ("timed_out", Value::Bool(r.timed_out)),
            ("pid", Value::Int(r.pid as i64)),
        ])
    }

    fn exec_run(&mut self, r: &RunStmt, fr: &Frame, span: Span) -> R<Value> {
        let (spec, timeout) = self.build_proc_spec(r, fr)?;
        self.cur_span = span;
        let res = self.run_process(&spec, timeout)?;
        // forward captured output to the sink when nothing asked for it
        if r.into.is_none() && self.sink.is_some() {
            if !res.stdout.is_empty() {
                let t = res.stdout_text();
                self.out(&t, false);
            }
            if !res.stderr.is_empty() {
                let t = res.stderr_text();
                self.out(&t, true);
            }
        }
        let rv = self.proc_result_value(&res);
        if let Some(name) = &r.into {
            fr.env.define(name, rv.clone(), false);
        }
        if r.check {
            if res.timed_out {
                return self.err_at(span, "TimeoutError", format!("command `{}` timed out after {} ms", spec.program, res.duration.as_millis()));
            }
            if res.code != 0 {
                let tail: String = res.stderr_text().lines().last().unwrap_or("").to_string();
                let ev = self.make_error("ProcessError", format!("command `{}` exited with code {}{}", if spec.program.is_empty() { spec.shell_line.clone().unwrap_or_default() } else { spec.program.clone() }, res.code, if tail.is_empty() { String::new() } else { format!(": {}", tail) }));
                if let Value::Object(o) = &ev {
                    let mut f = lk(&o.fields);
                    f.insert("result".into(), rv);
                    f.insert("code".into(), Value::str("exit_code"));
                }
                return Err(self.throw_value(ev));
            }
        } else if res.timed_out {
            // keep going: result.timed_out tells the story
        }
        Ok(rv)
    }
}

fn num(v: &Value) -> f64 {
    match v {
        Value::Int(i) => *i as f64,
        Value::Float(f) => *f,
        _ => 0.0,
    }
}

fn type_is_float(t: &TypeExpr) -> bool {
    matches!(&t.kind, TypeExprKind::Named { path, .. } if path.len() == 1 && path[0] == "float")
}

pub fn parse_duration_ms(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<f64>() {
        return if n >= 0.0 { Some(n as u64) } else { None };
    }
    let mut total = 0f64;
    let mut num = String::new();
    let mut unit = String::new();
    let flush = |num: &mut String, unit: &mut String, total: &mut f64| -> Option<()> {
        if num.is_empty() {
            return None;
        }
        let n: f64 = num.parse().ok()?;
        let mult = match unit.as_str() {
            "ms" => 1.0,
            "s" | "sec" => 1000.0,
            "m" | "min" => 60_000.0,
            "h" => 3_600_000.0,
            "d" => 86_400_000.0,
            _ => return None,
        };
        *total += n * mult;
        num.clear();
        unit.clear();
        Some(())
    };
    for c in s.chars() {
        if c.is_ascii_digit() || c == '.' {
            if !unit.is_empty() {
                flush(&mut num, &mut unit, &mut total)?;
            }
            num.push(c);
        } else if c.is_ascii_alphabetic() {
            unit.push(c);
        } else if c.is_whitespace() {
            continue;
        } else {
            return None;
        }
    }
    flush(&mut num, &mut unit, &mut total)?;
    Some(total as u64)
}

pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let n: f64 = s[..split].parse().ok()?;
    let mult: f64 = match s[split..].trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "tib" => 1024.0f64.powi(4),
        _ => return None,
    };
    Some((n * mult) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration_ms("500ms"), Some(500));
        assert_eq!(parse_duration_ms("30s"), Some(30_000));
        assert_eq!(parse_duration_ms("5m"), Some(300_000));
        assert_eq!(parse_duration_ms("1h30m"), Some(5_400_000));
        assert_eq!(parse_duration_ms("2d"), Some(172_800_000));
        assert_eq!(parse_duration_ms("1500"), Some(1500));
        assert_eq!(parse_duration_ms("abc"), None);
        assert_eq!(parse_duration_ms(""), None);
        assert_eq!(parse_duration_ms("5x"), None);
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("512MB"), Some(512 * 1024 * 1024));
        assert_eq!(parse_size("2GB"), Some(2 * 1024 * 1024 * 1024));
        assert_eq!(parse_size("1024"), Some(1024));
        assert_eq!(parse_size("1.5k"), Some(1536));
        assert_eq!(parse_size("zz"), None);
    }
}
