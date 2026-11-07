//! Static type model for gradual typing: `Ty`, the class table and assignability.

use nin_ast::{TypeExpr, TypeExprKind};
use nin_diag::Span;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    /// unknown / not statically known — compatible with everything
    Any,
    Null,
    Void,
    Bool,
    Int,
    Float,
    Str,
    List(Box<Ty>),
    Map(Box<Ty>),
    Fn,
    Task,
    Module,
    Class(String),
    Opt(Box<Ty>),
    Union(Vec<Ty>),
}

impl Ty {
    pub fn display(&self) -> String {
        match self {
            Ty::Any => "any".into(),
            Ty::Null => "null".into(),
            Ty::Void => "void".into(),
            Ty::Bool => "bool".into(),
            Ty::Int => "int".into(),
            Ty::Float => "float".into(),
            Ty::Str => "string".into(),
            Ty::List(t) => if **t == Ty::Any { "list".into() } else { format!("list<{}>", t.display()) },
            Ty::Map(t) => if **t == Ty::Any { "map".into() } else { format!("map<string,{}>", t.display()) },
            Ty::Fn => "fn".into(),
            Ty::Task => "task".into(),
            Ty::Module => "module".into(),
            Ty::Class(c) => c.clone(),
            Ty::Opt(t) => format!("{}?", t.display()),
            Ty::Union(ts) => ts.iter().map(|t| t.display()).collect::<Vec<_>>().join("|"),
        }
    }
    pub fn is_any(&self) -> bool {
        matches!(self, Ty::Any)
    }
    pub fn is_numeric(&self) -> bool {
        matches!(self, Ty::Int | Ty::Float)
    }
    /// True if the type is fully known (no `any` inside).
    pub fn is_known(&self) -> bool {
        match self {
            Ty::Any => false,
            Ty::List(t) | Ty::Map(t) => t.is_known() || **t == Ty::Any,
            Ty::Opt(t) => t.is_known(),
            Ty::Union(ts) => ts.iter().all(|t| t.is_known()),
            _ => true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Class,
    Interface,
    Enum,
}

#[derive(Clone, Debug)]
pub struct ClassInfo {
    pub name: String,
    pub kind: Kind,
    pub parent: Option<String>,
    pub interfaces: Vec<String>,
    pub generics: Vec<String>,
    pub is_abstract: bool,
    pub is_final: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ClassTable {
    pub classes: HashMap<String, ClassInfo>,
}

impl ClassTable {
    pub fn get(&self, n: &str) -> Option<&ClassInfo> {
        self.classes.get(n)
    }

    pub fn is_subclass(&self, a: &str, b: &str) -> bool {
        self.is_sub_depth(a, b, 0)
    }

    fn is_sub_depth(&self, a: &str, b: &str, d: usize) -> bool {
        if a == b {
            return true;
        }
        if d > 64 {
            return false;
        }
        match self.classes.get(a) {
            Some(c) => c.parent.iter().chain(c.interfaces.iter()).any(|p| self.is_sub_depth(p, b, d + 1)),
            None => false,
        }
    }
}

pub const BUILTIN_TYPES: &[&str] = &["any", "int", "float", "string", "bool", "null", "void", "list", "map", "task", "fn", "error", "module"];

/// Resolve a syntactic type. `Err((name, span))` names the first unknown type.
pub fn from_type_expr(te: &TypeExpr, table: &ClassTable, generics: &[String]) -> Result<Ty, (String, Span)> {
    Ok(match &te.kind {
        TypeExprKind::Optional(t) => Ty::Opt(Box::new(from_type_expr(t, table, generics)?)),
        TypeExprKind::Union(ts) => Ty::Union(ts.iter().map(|t| from_type_expr(t, table, generics)).collect::<Result<_, _>>()?),
        TypeExprKind::Func { .. } => Ty::Fn,
        TypeExprKind::Named { path, args } => {
            if path.len() > 1 {
                return Ok(Ty::Any); // module-qualified: resolved at run time
            }
            let n = path[0].as_str();
            if generics.iter().any(|g| g == n) {
                return Ok(Ty::Any);
            }
            match n {
                "any" => Ty::Any,
                "int" => Ty::Int,
                "float" => Ty::Float,
                "string" => Ty::Str,
                "bool" => Ty::Bool,
                "null" => Ty::Null,
                "void" => Ty::Void,
                "fn" => Ty::Fn,
                "task" => Ty::Task,
                "module" => Ty::Module,
                "error" => Ty::Class("Error".into()),
                "list" => Ty::List(Box::new(match args.first() {
                    Some(a) => from_type_expr(a, table, generics)?,
                    None => Ty::Any,
                })),
                "map" => Ty::Map(Box::new(match args.get(1) {
                    Some(a) => from_type_expr(a, table, generics)?,
                    None => Ty::Any,
                })),
                other => {
                    for a in args {
                        from_type_expr(a, table, generics)?;
                    }
                    if table.get(other).is_some() {
                        Ty::Class(other.to_string())
                    } else {
                        return Err((other.to_string(), te.span));
                    }
                }
            }
        }
    })
}

/// Can a value of static type `from` be stored where `to` is expected?
pub fn assignable(t: &ClassTable, from: &Ty, to: &Ty) -> bool {
    match (from, to) {
        (Ty::Any, _) | (_, Ty::Any) => true,
        (Ty::Union(fs), _) => fs.iter().all(|f| assignable(t, f, to)),
        (_, Ty::Union(ts)) => ts.iter().any(|x| assignable(t, from, x)),
        (Ty::Opt(f), Ty::Opt(x)) => assignable(t, f, x),
        (Ty::Opt(_), _) => false,
        (_, Ty::Opt(x)) => matches!(from, Ty::Null) || assignable(t, from, x),
        (Ty::Int, Ty::Float) => true,
        (Ty::List(a), Ty::List(b)) | (Ty::Map(a), Ty::Map(b)) => assignable(t, a, b),
        (Ty::Class(a), Ty::Class(b)) => t.is_subclass(a, b),
        (a, b) => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> ClassTable {
        let mut t = ClassTable::default();
        let mk = |n: &str, p: Option<&str>, i: &[&str]| ClassInfo { name: n.into(), kind: Kind::Class, parent: p.map(String::from), interfaces: i.iter().map(|s| s.to_string()).collect(), generics: vec![], is_abstract: false, is_final: false };
        t.classes.insert("Base".into(), mk("Base", None, &["Shape"]));
        t.classes.insert("Circle".into(), mk("Circle", Some("Base"), &[]));
        t.classes.insert("Shape".into(), mk("Shape", None, &[]));
        t
    }

    #[test]
    fn assignability() {
        let t = table();
        assert!(assignable(&t, &Ty::Int, &Ty::Float));
        assert!(!assignable(&t, &Ty::Float, &Ty::Int));
        assert!(assignable(&t, &Ty::Null, &Ty::Opt(Box::new(Ty::Int))));
        assert!(!assignable(&t, &Ty::Null, &Ty::Int));
        assert!(assignable(&t, &Ty::Class("Circle".into()), &Ty::Class("Shape".into())));
        assert!(!assignable(&t, &Ty::Class("Shape".into()), &Ty::Class("Circle".into())));
        assert!(assignable(&t, &Ty::Any, &Ty::Str));
        assert!(assignable(&t, &Ty::Str, &Ty::Union(vec![Ty::Int, Ty::Str])));
        assert!(!assignable(&t, &Ty::Bool, &Ty::Union(vec![Ty::Int, Ty::Str])));
        assert!(assignable(&t, &Ty::List(Box::new(Ty::Int)), &Ty::List(Box::new(Ty::Float))));
        assert!(!assignable(&t, &Ty::List(Box::new(Ty::Str)), &Ty::List(Box::new(Ty::Int))));
    }

    #[test]
    fn resolve_types() {
        let t = table();
        let sp = Span::DUMMY;
        let te = nin_ast::parse_type("list<Circle>?", sp).unwrap();
        assert_eq!(from_type_expr(&te, &t, &[]).unwrap().display(), "list<Circle>?");
        let bad = nin_ast::parse_type("Nope", sp).unwrap();
        assert_eq!(from_type_expr(&bad, &t, &[]).unwrap_err().0, "Nope");
        let g = nin_ast::parse_type("T", sp).unwrap();
        assert_eq!(from_type_expr(&g, &t, &["T".to_string()]).unwrap(), Ty::Any);
    }
}
