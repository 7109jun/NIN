//! Runtime values, scopes and error signals.

use indexmap::IndexMap;
use nin_ast::{ClassDecl, FuncDecl, GenericParam, Program, TypeExpr};
use nin_diag::Span;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

pub fn lk<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub type ListRef = Arc<Mutex<Vec<Value>>>;
pub type MapRef = Arc<Mutex<IndexMap<String, Value>>>;
pub type R<T> = Result<T, Signal>;

/// Non-local exits: a thrown NIN value or process exit.
pub enum Signal {
    Throw(Box<Throw>),
    Exit(i32),
}

pub struct Throw {
    pub value: Value,
    /// `(function name, span of the call/throw site)`, innermost first
    pub trace: Vec<(String, Span)>,
}

#[derive(Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Arc<str>),
    List(ListRef),
    Map(MapRef),
    Object(Arc<Object>),
    Class(Arc<Class>),
    Func(Arc<Function>),
    Native(Arc<Native>),
    Bound(Arc<BoundMethod>),
    Task(Arc<Task>),
    Module(Arc<Module>),
}

impl Value {
    pub fn str(s: impl AsRef<str>) -> Value {
        Value::Str(Arc::from(s.as_ref()))
    }
    pub fn list(v: Vec<Value>) -> Value {
        Value::List(Arc::new(Mutex::new(v)))
    }
    pub fn map(m: IndexMap<String, Value>) -> Value {
        Value::Map(Arc::new(Mutex::new(m)))
    }
    pub fn map_from<I: IntoIterator<Item = (&'static str, Value)>>(it: I) -> Value {
        Value::map(it.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
    pub fn type_name(&self) -> String {
        match self {
            Value::Null => "null".into(),
            Value::Bool(_) => "bool".into(),
            Value::Int(_) => "int".into(),
            Value::Float(_) => "float".into(),
            Value::Str(_) => "string".into(),
            Value::List(_) => "list".into(),
            Value::Map(_) => "map".into(),
            Value::Object(o) => o.class.name.clone(),
            Value::Class(c) => format!("class {}", c.name),
            Value::Func(_) | Value::Native(_) | Value::Bound(_) => "function".into(),
            Value::Task(_) => "task".into(),
            Value::Module(_) => "module".into(),
        }
    }
    pub fn is_callable(&self) -> bool {
        matches!(self, Value::Func(_) | Value::Native(_) | Value::Bound(_))
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
}

pub struct Object {
    pub id: u64,
    pub class: Arc<Class>,
    pub fields: Mutex<IndexMap<String, Value>>,
    /// reified generic arguments (parallel to `class.generics`)
    pub type_args: Vec<TypeExpr>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClassKind {
    Class,
    Interface,
    Enum,
}

pub struct Class {
    pub id: u64,
    pub name: String,
    pub kind: ClassKind,
    pub decl: Option<Arc<ClassDecl>>,
    pub parent: Option<Arc<Class>>,
    pub interfaces: Vec<Arc<Class>>,
    pub generics: Vec<GenericParam>,
    pub is_abstract: bool,
    pub is_final: bool,
    pub ctors: Vec<Arc<FuncDecl>>,
    /// own methods (including abstract ones and interface signatures)
    pub methods: IndexMap<String, Vec<Arc<FuncDecl>>>,
    pub statics: Mutex<IndexMap<String, Value>>,
    pub env: Env,
    pub module_name: String,
    pub enum_values: Mutex<Vec<Value>>,
}

impl Class {
    pub fn is_subclass_of(&self, other: &Class) -> bool {
        if self.id == other.id {
            return true;
        }
        if let Some(p) = &self.parent {
            if p.is_subclass_of(other) {
                return true;
            }
        }
        self.interfaces.iter().any(|i| i.is_subclass_of(other))
    }
    /// Chain from this class up to the root (self first).
    pub fn chain(self: &Arc<Class>) -> Vec<Arc<Class>> {
        let mut v = vec![self.clone()];
        let mut cur = self.parent.clone();
        while let Some(c) = cur {
            cur = c.parent.clone();
            v.push(c);
        }
        v
    }
}

pub struct Function {
    pub decl: Arc<FuncDecl>,
    pub env: Env,
    /// declaring class (for `super`, `this` access checks)
    pub class: Option<Arc<Class>>,
    pub this: Option<Value>,
    pub module_name: Arc<str>,
}

pub struct BoundMethod {
    pub recv: Value,
    pub name: String,
    pub overloads: Vec<(Arc<Class>, Arc<FuncDecl>)>,
}

pub type NativeFnBox = Box<dyn Fn(&mut crate::Exec, &[Value]) -> R<Value> + Send + Sync>;

pub struct Native {
    pub name: String,
    pub min: usize,
    pub max: Option<usize>,
    pub f: NativeFnBox,
}

impl Native {
    pub fn value(name: &str, min: usize, max: Option<usize>, f: impl Fn(&mut crate::Exec, &[Value]) -> R<Value> + Send + Sync + 'static) -> Value {
        Value::Native(Arc::new(Native { name: name.to_string(), min, max, f: Box::new(f) }))
    }
}

pub struct Task {
    pub name: String,
    pub state: Mutex<Option<R<Value>>>,
    pub cv: Condvar,
    pub cancel: Arc<AtomicBool>,
}

pub enum ModuleKind {
    Native(IndexMap<String, Value>),
    Script { env: Env, program: Arc<Program> },
}

pub struct Module {
    pub name: String,
    pub kind: ModuleKind,
    pub path: Option<PathBuf>,
}

// ───────────────────────────── scopes ─────────────────────────────

#[derive(Clone)]
pub struct Binding {
    pub value: Value,
    pub is_const: bool,
}

pub struct Scope {
    vars: Mutex<HashMap<String, Binding>>,
    parent: Option<Env>,
}

pub type Env = Arc<Scope>;

pub enum AssignError {
    NotFound,
    Const,
}

impl Scope {
    pub fn new(parent: Option<Env>) -> Env {
        Arc::new(Scope { vars: Mutex::new(HashMap::new()), parent })
    }
    pub fn get(&self, name: &str) -> Option<Value> {
        if let Some(b) = lk(&self.vars).get(name) {
            return Some(b.value.clone());
        }
        match &self.parent {
            Some(p) => p.get(name),
            None => None,
        }
    }
    pub fn get_local(&self, name: &str) -> Option<Value> {
        lk(&self.vars).get(name).map(|b| b.value.clone())
    }
    pub fn define(&self, name: &str, value: Value, is_const: bool) {
        lk(&self.vars).insert(name.to_string(), Binding { value, is_const });
    }
    pub fn assign(&self, name: &str, value: Value) -> Result<(), AssignError> {
        {
            let mut g = lk(&self.vars);
            if let Some(b) = g.get_mut(name) {
                if b.is_const {
                    return Err(AssignError::Const);
                }
                b.value = value;
                return Ok(());
            }
        }
        match &self.parent {
            Some(p) => p.assign(name, value),
            None => Err(AssignError::NotFound),
        }
    }
    pub fn local_names(&self) -> Vec<String> {
        let mut v: Vec<String> = lk(&self.vars).keys().cloned().collect();
        v.sort();
        v
    }
    pub fn all_names(&self) -> Vec<String> {
        let mut v = self.local_names();
        if let Some(p) = &self.parent {
            v.extend(p.all_names());
        }
        v.sort();
        v.dedup();
        v
    }
}

// ───────────────────────────── formatting & equality ─────────────────────────────

pub fn fmt_float(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{:.1}", f)
    } else {
        format!("{}", f)
    }
}

pub fn quote_str(s: &str) -> String {
    nin_syntax::escape_json_string(s)
}

/// Structural equality for plain data; identity for objects and callables.
pub fn values_equal(a: &Value, b: &Value) -> bool {
    values_equal_depth(a, b, 0)
}

fn values_equal_depth(a: &Value, b: &Value, d: usize) -> bool {
    if d > 100 {
        return false;
    }
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Int(x), Value::Float(y)) | (Value::Float(y), Value::Int(x)) => (*x as f64) == *y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::List(x), Value::List(y)) => {
            if Arc::ptr_eq(x, y) {
                return true;
            }
            let (xv, yv) = (lk(x).clone(), lk(y).clone());
            xv.len() == yv.len() && xv.iter().zip(yv.iter()).all(|(p, q)| values_equal_depth(p, q, d + 1))
        }
        (Value::Map(x), Value::Map(y)) => {
            if Arc::ptr_eq(x, y) {
                return true;
            }
            let (xv, yv) = (lk(x).clone(), lk(y).clone());
            xv.len() == yv.len() && xv.iter().all(|(k, v)| yv.get(k).map_or(false, |w| values_equal_depth(v, w, d + 1)))
        }
        (Value::Object(x), Value::Object(y)) => Arc::ptr_eq(x, y),
        (Value::Class(x), Value::Class(y)) => x.id == y.id,
        (Value::Func(x), Value::Func(y)) => Arc::ptr_eq(x, y),
        (Value::Native(x), Value::Native(y)) => Arc::ptr_eq(x, y),
        (Value::Task(x), Value::Task(y)) => Arc::ptr_eq(x, y),
        (Value::Module(x), Value::Module(y)) => Arc::ptr_eq(x, y),
        _ => false,
    }
}

/// Debug-style representation used inside collections (strings quoted).
pub fn repr(v: &Value) -> String {
    repr_depth(v, 0)
}

fn repr_depth(v: &Value, d: usize) -> String {
    if d > 8 {
        return "...".into();
    }
    match v {
        Value::Str(s) => quote_str(s),
        Value::List(l) => {
            let items = lk(l).clone();
            format!("[{}]", items.iter().map(|x| repr_depth(x, d + 1)).collect::<Vec<_>>().join(", "))
        }
        Value::Map(m) => {
            let items = lk(m).clone();
            if items.is_empty() {
                return "{}".into();
            }
            format!("{{{}}}", items.iter().map(|(k, x)| format!("{}: {}", quote_str(k), repr_depth(x, d + 1))).collect::<Vec<_>>().join(", "))
        }
        Value::Object(o) => {
            let f = lk(&o.fields).clone();
            if o.class.kind == ClassKind::Enum {
                return format!("{}.{}", o.class.name, f.get("name").map(|n| display_simple(n)).unwrap_or_default());
            }
            format!("{}{{{}}}", o.class.name, f.iter().map(|(k, x)| format!("{}: {}", k, repr_depth(x, d + 1))).collect::<Vec<_>>().join(", "))
        }
        other => display_simple(other),
    }
}

/// Display without invoking user code (`to_string` overrides need an `Exec`; see `Exec::to_display`).
pub fn display_simple(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => fmt_float(*f),
        Value::Str(s) => s.to_string(),
        Value::List(_) | Value::Map(_) | Value::Object(_) => repr(v),
        Value::Class(c) => format!("<class {}>", c.name),
        Value::Func(f) => format!("<fn {}>", f.decl.name),
        Value::Native(n) => format!("<native {}>", n.name),
        Value::Bound(b) => format!("<method {}>", b.name),
        Value::Task(t) => format!("<task {}>", t.name),
        Value::Module(m) => format!("<module {}>", m.name),
    }
}
