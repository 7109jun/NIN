use indexmap::IndexMap;
use nin_runtime::builtins::{a_int, a_list, a_str};
use nin_runtime::*;
use std::io;

pub struct Mod(pub IndexMap<String, Value>);

impl Mod {
    pub fn new() -> Mod {
        Mod(IndexMap::new())
    }
    pub fn f(&mut self, name: &'static str, min: usize, max: Option<usize>, f: impl Fn(&mut Exec, &[Value]) -> R<Value> + Send + Sync + 'static) {
        self.0.insert(name.to_string(), Native::value(name, min, max, f));
    }
    pub fn c(&mut self, name: &str, v: Value) {
        self.0.insert(name.to_string(), v);
    }
    pub fn done(self) -> IndexMap<String, Value> {
        self.0
    }
}

pub fn io_err<T>(ex: &mut Exec, e: io::Error, what: &str) -> R<T> {
    let code = match e.kind() {
        io::ErrorKind::NotFound => "not_found",
        io::ErrorKind::PermissionDenied => "permission_denied",
        io::ErrorKind::AlreadyExists => "already_exists",
        io::ErrorKind::TimedOut => "timed_out",
        _ => "io",
    };
    let ev = ex.make_error("IOError", format!("{}: {}", what, e));
    if let Value::Object(o) = &ev {
        lk(&o.fields).insert("code".into(), Value::str(code));
    }
    Err(ex.throw_value(ev))
}

pub fn bytes_of(ex: &mut Exec, v: &Value, f: &str) -> R<Vec<u8>> {
    match v {
        Value::Str(s) => Ok(s.as_bytes().to_vec()),
        Value::List(l) => {
            let items = lk(l).clone();
            let mut out = Vec::with_capacity(items.len());
            for i in items {
                match i {
                    Value::Int(n) if (0..=255).contains(&n) => out.push(n as u8),
                    _ => return ex.err("TypeError", format!("{}: byte lists must contain ints 0..255", f)),
                }
            }
            Ok(out)
        }
        o => ex.err("TypeError", format!("{}: expected string or byte list, got {}", f, o.type_name())),
    }
}

pub fn bytes_value(b: &[u8]) -> Value {
    Value::list(b.iter().map(|x| Value::Int(*x as i64)).collect())
}

pub fn opt_map(ex: &mut Exec, args: &[Value], i: usize, f: &str) -> R<IndexMap<String, Value>> {
    match args.get(i) {
        None | Some(Value::Null) => Ok(IndexMap::new()),
        Some(Value::Map(m)) => Ok(lk(m).clone()),
        Some(o) => ex.err("TypeError", format!("{}: options must be a map, got {}", f, o.type_name())),
    }
}

pub fn strs(ex: &mut Exec, args: &[Value], i: usize, f: &str) -> R<Vec<String>> {
    let items = a_list(ex, args, i, f)?;
    let mut out = vec![];
    for x in &items {
        out.push(ex.to_display(x)?);
    }
    Ok(out)
}

#[allow(unused_imports)]
pub use nin_runtime::builtins::a_fn;
#[allow(dead_code)]
pub fn _keep(_: fn(&mut Exec, &[Value], usize, &str) -> R<i64>, _: fn(&mut Exec, &[Value], usize, &str) -> R<String>) {}
#[allow(dead_code)]
const _: fn() = || {
    let _ = _keep(a_int, a_str);
};
