//! Semantic analysis (docs/SPEC.md §5): names, classes, calls, gradual type checks, reachability,
//! and the workflow graph. Produces errors (E03xx structure, E04xx types, E06xx workflow) and warnings (W03xx).

use nin_ast::*;
use nin_diag::{suggest, Diagnostic, Diagnostics, Span};
use nin_types::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub struct Options {
    /// names accepted in `{"use":"std.NAME"}` (full names, e.g. `std.fs`)
    pub known_std: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options { known_std: vec![] }
    }
}

const BUILTINS: &[&str] = &["len", "str", "int", "float", "bool", "typeof", "range", "keys", "values", "has", "push", "pop", "error", "panic", "assert_eq", "clone", "deep_clone", "env", "args", "reflect"];
const PRELUDE_ERRORS: &[&str] = &["TypeError", "ValueError", "KeyError", "IndexError", "ArithmeticError", "AssertionError", "IOError", "ProcessError", "TimeoutError", "CancelledError", "StateError", "AccessError", "ImportError", "NameError"];
const WF_CTX: &[&str] = &["inputs", "env", "matrix", "steps", "jobs", "workflow", "job", "error", "stdout", "stderr", "code", "outputs", "success", "failure", "always", "cancelled"];

pub fn analyze(prog: &Program, opts: &Options) -> Diagnostics {
    let mut s = Sema::new(prog, opts);
    s.run();
    s.ds
}

#[derive(Clone)]
struct Var {
    ty: Ty,
    is_const: bool,
    span: Span,
    used: bool,
    warn_unused: bool,
}

#[derive(Default, Clone)]
struct Ctx {
    loops: usize,
    in_func: bool,
    ret: Option<Ty>,
    is_async: bool,
    class: Option<String>,
    in_ctor: bool,
    in_static: bool,
    wf_script: bool,
    fname: String,
}

struct Sema<'a> {
    prog: &'a Program,
    opts: &'a Options,
    ds: Diagnostics,
    table: ClassTable,
    classes: HashMap<String, &'a ClassDecl>,
    ifaces: HashMap<String, &'a InterfaceDecl>,
    funcs: HashMap<String, &'a Arc<FuncDecl>>,
    enums: HashMap<String, &'a EnumDecl>,
    globals: HashMap<String, (Ty, bool)>,
    imports: HashSet<String>,
    scopes: Vec<HashMap<String, Var>>,
    ctx: Ctx,
    generics: Vec<String>,
}

fn arity_ok(d: &FuncDecl, n: usize) -> bool {
    let variadic = d.params.last().map_or(false, |p| p.variadic);
    let fixed = if variadic { d.params.len() - 1 } else { d.params.len() };
    let required = d.params.iter().take(fixed).filter(|p| p.default.is_none()).count();
    n >= required && (variadic || n <= fixed)
}

fn arity_text(d: &FuncDecl) -> String {
    let variadic = d.params.last().map_or(false, |p| p.variadic);
    let fixed = if variadic { d.params.len() - 1 } else { d.params.len() };
    let required = d.params.iter().take(fixed).filter(|p| p.default.is_none()).count();
    if variadic {
        format!("at least {}", required)
    } else if required == fixed {
        format!("{}", fixed)
    } else {
        format!("{} to {}", required, fixed)
    }
}

fn always_returns(b: &Block) -> bool {
    match b.last().map(|s| &s.kind) {
        Some(StmtKind::Return(_)) | Some(StmtKind::Throw(_)) | Some(StmtKind::Exit(_)) => true,
        Some(StmtKind::If { then, otherwise, .. }) => always_returns(then) && always_returns(otherwise),
        Some(StmtKind::Match { cases, default, .. }) => !default.is_empty() && always_returns(default) && cases.iter().all(|c| always_returns(&c.body)),
        Some(StmtKind::Try { body, catches, finally }) => (!finally.is_empty() && always_returns(finally)) || (always_returns(body) && catches.iter().all(|c| always_returns(&c.body))),
        Some(StmtKind::Block(b)) => always_returns(b),
        _ => false,
    }
}

impl<'a> Sema<'a> {
    fn new(prog: &'a Program, opts: &'a Options) -> Sema<'a> {
        Sema { prog, opts, ds: Diagnostics::new(), table: ClassTable::default(), classes: HashMap::new(), ifaces: HashMap::new(), funcs: HashMap::new(), enums: HashMap::new(), globals: HashMap::new(), imports: HashSet::new(), scopes: vec![], ctx: Ctx::default(), generics: vec![] }
    }

    fn err(&mut self, code: &'static str, span: Span, msg: impl Into<String>, label: impl Into<String>) {
        self.ds.push(Diagnostic::error(code, msg).at(span, label));
    }
    fn warn(&mut self, code: &'static str, span: Span, msg: impl Into<String>, label: impl Into<String>) {
        self.ds.push(Diagnostic::warning(code, msg).at(span, label));
    }
    fn err_help(&mut self, code: &'static str, span: Span, msg: impl Into<String>, label: impl Into<String>, help: String) {
        self.ds.push(Diagnostic::error(code, msg).at(span, label).help(help));
    }

    // ───────── declarations ─────────

    fn run(&mut self) {
        let prog = self.prog;
        // 1. collect + duplicates
        let mut seen: HashMap<String, Span> = HashMap::new();
        let mut declare = |me: &mut Sema, name: &str, span: Span, what: &str| {
            if let Some(prev) = seen.get(name) {
                me.ds.push(Diagnostic::error("E0301", format!("duplicate declaration of `{}`", name)).at(span, format!("{} `{}` is declared again here", what, name)).also(*prev, "first declared here"));
            } else {
                seen.insert(name.to_string(), span);
            }
        };
        for i in &prog.imports {
            declare(self, &i.alias, i.span, "import");
            self.imports.insert(i.alias.clone());
            if let ImportKind::Std(n) = &i.kind {
                if !n.starts_with("plugin:") && !self.opts.known_std.is_empty() && !self.opts.known_std.contains(n) {
                    let names: Vec<&str> = self.opts.known_std.iter().map(|s| s.as_str()).collect();
                    let mut d = Diagnostic::error("E0307", format!("unknown standard module `{}`", n)).at(i.span, "no such module");
                    if let Some(s) = suggest(n, names.iter().copied()) {
                        d = d.help(format!("did you mean `{}`?", s));
                    }
                    self.ds.push(d);
                }
            }
        }
        for f in &prog.functions {
            declare(self, &f.name, f.span, "function");
            self.funcs.insert(f.name.clone(), f);
        }
        for e in &prog.enums {
            declare(self, &e.name, e.span, "enum");
            self.enums.insert(e.name.clone(), e);
            self.table.classes.insert(e.name.clone(), ClassInfo { name: e.name.clone(), kind: Kind::Enum, parent: None, interfaces: vec![], generics: vec![], is_abstract: false, is_final: true });
        }
        for c in &prog.interfaces {
            declare(self, &c.name, c.span, "interface");
            self.ifaces.insert(c.name.clone(), c);
            let ifs = c.extends.iter().filter_map(|t| named_path(t)).collect();
            self.table.classes.insert(c.name.clone(), ClassInfo { name: c.name.clone(), kind: Kind::Interface, parent: None, interfaces: ifs, generics: c.generics.iter().map(|g| g.name.clone()).collect(), is_abstract: true, is_final: false });
        }
        // prelude errors
        self.table.classes.insert("Error".into(), ClassInfo { name: "Error".into(), kind: Kind::Class, parent: None, interfaces: vec![], generics: vec![], is_abstract: false, is_final: false });
        for n in PRELUDE_ERRORS {
            self.table.classes.insert(n.to_string(), ClassInfo { name: n.to_string(), kind: Kind::Class, parent: Some("Error".into()), interfaces: vec![], generics: vec![], is_abstract: false, is_final: false });
        }
        for c in &prog.classes {
            declare(self, &c.name, c.span, "class");
            self.classes.insert(c.name.clone(), c);
            self.table.classes.insert(
                c.name.clone(),
                ClassInfo { name: c.name.clone(), kind: Kind::Class, parent: c.extends.as_ref().and_then(named_path), interfaces: c.implements.iter().filter_map(named_path).collect(), generics: c.generics.iter().map(|g| g.name.clone()).collect(), is_abstract: c.is_abstract, is_final: c.is_final },
            );
        }
        for g in &prog.globals {
            declare(self, &g.name, g.span, "global");
        }
        // globals' types
        for g in &prog.globals {
            let ty = match &g.ty {
                Some(t) => self.resolve(t),
                None => Ty::Any,
            };
            self.globals.insert(g.name.clone(), (ty, g.is_const));
        }

        // 2. inheritance structure
        for c in &prog.classes {
            self.check_class_header(c);
        }
        for i in &prog.interfaces {
            for t in &i.extends {
                if let Some(n) = named_path(t) {
                    if !matches!(self.table.get(&n).map(|c| c.kind), Some(Kind::Interface)) {
                        self.err("E0303", t.span, format!("interface `{}` can only extend interfaces", i.name), format!("`{}` is not a known interface", n));
                    }
                }
            }
        }

        // 3. bodies
        self.scopes.push(HashMap::new()); // module scope for global initialisers
        for g in &prog.globals {
            self.ctx = Ctx::default();
            let t = self.expr(&g.value);
            if let Some(dt) = &g.ty {
                let want = self.resolve(dt);
                self.check_assign(&t, &want, g.value.span, &format!("global `{}`", g.name));
            }
        }
        self.scopes.pop();
        for f in &prog.functions {
            self.function(f, None);
        }
        for c in &prog.classes {
            self.class_body(c);
        }
        if let Some(main) = &prog.main {
            self.ctx = Ctx::default();
            self.scopes.push(HashMap::new());
            self.stmts(main);
            self.pop_scope();
        }
        if let Some(wf) = &prog.workflow {
            self.workflow(wf);
        }
    }

    fn check_class_header(&mut self, c: &'a ClassDecl) {
        // parent
        if let Some(t) = &c.extends {
            if let Some(n) = named_path(t) {
                match self.table.get(&n).cloned() {
                    None => {
                        let names: Vec<&str> = self.table.classes.keys().map(|s| s.as_str()).collect();
                        let mut d = Diagnostic::error("E0302", format!("unknown parent class `{}`", n)).at(t.span, "not declared");
                        if let Some(s) = suggest(&n, names.iter().copied()) {
                            d = d.help(format!("did you mean `{}`?", s));
                        }
                        self.ds.push(d);
                    }
                    Some(p) => {
                        if p.kind == Kind::Interface {
                            self.err_help("E0303", t.span, format!("class `{}` cannot extend interface `{}`", c.name, n), "interfaces are implemented", "use `\"implements\"` instead of `\"extends\"`".into());
                        } else if p.kind == Kind::Enum {
                            self.err("E0303", t.span, "cannot extend an enum", "enums are final");
                        } else if p.is_final {
                            self.err("E0305", t.span, format!("class `{}` cannot extend final class `{}`", c.name, n), "parent is final");
                        }
                    }
                }
            }
            // cycle
            let mut cur = named_path(t);
            let mut steps = 0;
            while let Some(n) = cur {
                if n == c.name {
                    self.err("E0304", c.span, format!("inheritance cycle through `{}`", c.name), "class inherits from itself");
                    break;
                }
                steps += 1;
                if steps > 64 {
                    break;
                }
                cur = self.table.get(&n).and_then(|i| i.parent.clone());
            }
        }
        for t in &c.implements {
            if let Some(n) = named_path(t) {
                match self.table.get(&n).map(|i| i.kind) {
                    Some(Kind::Interface) => {}
                    Some(_) => self.err("E0303", t.span, format!("`{}` is not an interface", n), "cannot be implemented"),
                    None => {
                        let names: Vec<&str> = self.ifaces.keys().map(|s| s.as_str()).collect();
                        let mut d = Diagnostic::error("E0302", format!("unknown interface `{}`", n)).at(t.span, "not declared");
                        if let Some(s) = suggest(&n, names.iter().copied()) {
                            d = d.help(format!("did you mean `{}`?", s));
                        }
                        self.ds.push(d);
                    }
                }
            }
        }
    }

    /// All methods visible on `class` (own first, then parents, then interfaces).
    fn methods_of(&self, class: &str) -> Vec<(String, &'a Arc<FuncDecl>)> {
        let mut out = vec![];
        let mut seen = HashSet::new();
        let mut queue = vec![class.to_string()];
        while let Some(n) = queue.pop() {
            if !seen.insert(n.clone()) {
                continue;
            }
            let decl_methods: Option<&'a Vec<Arc<FuncDecl>>> = if let Some(c) = self.classes.get(&n) { Some(&c.methods) } else { self.ifaces.get(&n).map(|i| &i.methods) };
            if let Some(ms) = decl_methods {
                for m in ms {
                    out.push((n.clone(), m));
                }
            }
            if let Some(i) = self.table.get(&n) {
                queue.extend(i.interfaces.iter().cloned());
                if let Some(p) = &i.parent {
                    queue.push(p.clone());
                }
            }
        }
        out
    }

    fn fields_of(&self, class: &str) -> Vec<(String, &'a FieldDecl)> {
        let mut out = vec![];
        let mut cur = Some(class.to_string());
        let mut n = 0;
        while let Some(c) = cur {
            if let Some(d) = self.classes.get(&c) {
                for f in &d.fields {
                    out.push((c.clone(), f));
                }
            }
            cur = self.table.get(&c).and_then(|i| i.parent.clone());
            n += 1;
            if n > 64 {
                break;
            }
        }
        out
    }

    fn sig(d: &FuncDecl) -> String {
        d.params.iter().map(|p| p.ty.as_ref().map(|t| t.display()).unwrap_or_else(|| "any".into())).collect::<Vec<_>>().join(",")
    }

    fn class_body(&mut self, c: &'a ClassDecl) {
        let saved_generics = std::mem::replace(&mut self.generics, c.generics.iter().map(|g| g.name.clone()).collect());
        // abstract implementation
        if !c.is_abstract {
            let all = self.methods_of(&c.name);
            let mut reported: HashSet<String> = HashSet::new();
            for (owner, m) in &all {
                if m.body.is_none() && !m.is_static && owner != &c.name {
                    let implemented = all.iter().any(|(_, o)| o.name == m.name && o.body.is_some() && o.params.len() == m.params.len());
                    if !implemented && reported.insert(m.name.clone()) {
                        let kind = if self.ifaces.contains_key(owner) { "interface" } else { "abstract class" };
                        self.ds.push(Diagnostic::error("E0320", format!("class `{}` must implement `{}` required by {} `{}`", c.name, m.name, kind, owner)).at(c.span, format!("missing method `{}`", m.name)).also(m.span, "declared here"));
                    }
                }
            }
            for m in &c.methods {
                if m.body.is_none() && !m.is_abstract {
                    // fine: parser marks bodiless as abstract; keep silent
                }
                if m.is_abstract || m.body.is_none() {
                    self.err("E0321", m.span, format!("concrete class `{}` declares abstract method `{}`", c.name, m.name), "mark the class `\"abstract\": true` or give the method a body");
                }
            }
        }
        // override rules
        let parent_methods: Vec<(String, &'a Arc<FuncDecl>)> = match c.extends.as_ref().and_then(named_path) {
            Some(p) if self.classes.contains_key(&p) => self.methods_of(&p),
            _ => vec![],
        };
        for m in &c.methods {
            let sig = Self::sig(m);
            let base = parent_methods.iter().find(|(_, p)| p.name == m.name && Self::sig(p) == sig);
            match base {
                Some((owner, p)) => {
                    if p.is_final {
                        self.err("E0322", m.span, format!("method `{}` is final in `{}` and cannot be overridden", m.name, owner), "overrides a final method");
                    }
                    if p.body.is_some() && !m.is_override {
                        self.ds.push(Diagnostic::error("E0323", format!("method `{}` overrides `{}.{}` without `override`", m.name, owner, m.name)).at(m.span, "add `\"override\": true`").also(p.span, "overridden method"));
                    }
                    if p.access != m.access && (m.access == Access::Private || (p.access == Access::Public && m.access == Access::Protected)) {
                        self.err("E0324", m.span, format!("override of `{}` reduces visibility ({} → {})", m.name, p.access.as_str(), m.access.as_str()), "less visible than the overridden method");
                    }
                }
                None => {
                    if m.is_override && !self.methods_of(&c.name).iter().any(|(o, x)| o != &c.name && x.name == m.name) {
                        self.err("E0325", m.span, format!("`{}` is marked `override` but overrides nothing", m.name), "no such method in a parent class or interface");
                    }
                }
            }
        }
        // fields: types and defaults
        self.ctx = Ctx { class: Some(c.name.clone()), ..Ctx::default() };
        self.scopes.push(HashMap::new());
        for f in &c.fields {
            let want = f.ty.as_ref().map(|t| self.resolve(t));
            if let Some(d) = &f.default {
                self.ctx.in_static = f.is_static;
                let t = self.expr(d);
                if let Some(w) = want {
                    self.check_assign(&t, &w, d.span, &format!("field `{}`", f.name));
                }
            }
        }
        self.pop_scope();
        for ctor in &c.constructors {
            self.function(ctor, Some((c, true)));
        }
        for m in &c.methods {
            self.function(m, Some((c, false)));
        }
        self.generics = saved_generics;
    }

    fn resolve(&mut self, t: &TypeExpr) -> Ty {
        match from_type_expr(t, &self.table, &self.generics) {
            Ok(ty) => ty,
            Err((name, span)) => {
                let names: Vec<&str> = BUILTIN_TYPES.iter().copied().chain(self.table.classes.keys().map(|s| s.as_str())).chain(self.generics.iter().map(|s| s.as_str())).collect();
                let mut d = Diagnostic::error("E0402", format!("unknown type `{}`", name)).at(span, "not a known type");
                if let Some(s) = suggest(&name, names.iter().copied()) {
                    d = d.help(format!("did you mean `{}`?", s));
                }
                self.ds.push(d);
                Ty::Any
            }
        }
    }

    fn function(&mut self, f: &'a FuncDecl, class: Option<(&'a ClassDecl, bool)>) {
        let saved_gen = self.generics.clone();
        self.generics.extend(f.generics.iter().map(|g| g.name.clone()));
        let ret = f.ret.as_ref().map(|t| self.resolve(t));
        self.ctx = Ctx {
            in_func: true,
            ret: ret.clone(),
            is_async: f.is_async,
            class: class.map(|(c, _)| c.name.clone()),
            in_ctor: class.map_or(false, |(_, k)| k),
            in_static: f.is_static,
            fname: f.name.clone(),
            ..Ctx::default()
        };
        self.scopes.push(HashMap::new());
        let mut names: HashSet<&str> = HashSet::new();
        for (i, p) in f.params.iter().enumerate() {
            if !names.insert(p.name.as_str()) {
                self.err("E0301", p.span, format!("duplicate parameter `{}`", p.name), "already declared");
            }
            if p.variadic && i + 1 != f.params.len() {
                self.err("E0326", p.span, "a variadic parameter must be last", "move it to the end");
            }
            let mut ty = match &p.ty {
                Some(t) => self.resolve(t),
                None => Ty::Any,
            };
            if p.variadic {
                ty = Ty::List(Box::new(ty));
            }
            if let Some(d) = &p.default {
                let dt = self.expr(d);
                if let Some(pt) = &p.ty {
                    let want = self.resolve(pt);
                    self.check_assign(&dt, &want, d.span, &format!("default of parameter `{}`", p.name));
                }
            }
            self.define(&p.name, ty, false, p.span, false);
        }
        if let Some(body) = &f.body {
            self.stmts(body);
            if let Some(r) = &ret {
                if !f.is_async && !matches!(r, Ty::Void | Ty::Any) && !always_returns(body) && !self.ctx.in_ctor {
                    self.warn("W0303", f.span, format!("function `{}` may not return a value on every path", f.name), format!("declared to return `{}`", r.display()));
                }
            }
        }
        self.pop_scope();
        self.generics = saved_gen;
        self.ctx = Ctx::default();
    }

    // ───────── scopes ─────────

    fn define(&mut self, name: &str, ty: Ty, is_const: bool, span: Span, warn_unused: bool) {
        if let Some(prev) = self.scopes.last().and_then(|s| s.get(name)) {
            if warn_unused {
                let ps = prev.span;
                self.ds.push(Diagnostic::error("E0301", format!("`{}` is already declared in this scope", name)).at(span, "redeclared here").also(ps, "first declared here"));
            }
        }
        if let Some(s) = self.scopes.last_mut() {
            s.insert(name.to_string(), Var { ty, is_const, span, used: false, warn_unused: warn_unused && !name.starts_with('_') });
        }
    }

    fn pop_scope(&mut self) {
        if let Some(s) = self.scopes.pop() {
            let mut unused: Vec<(Span, String)> = s.iter().filter(|(_, v)| v.warn_unused && !v.used).map(|(k, v)| (v.span, k.clone())).collect();
            unused.sort_by_key(|(sp, _)| (sp.file, sp.start));
            for (sp, n) in unused {
                self.warn("W0301", sp, format!("unused variable `{}`", n), "never read");
            }
        }
    }

    fn lookup(&mut self, name: &str) -> Option<(Ty, bool)> {
        for s in self.scopes.iter_mut().rev() {
            if let Some(v) = s.get_mut(name) {
                v.used = true;
                return Some((v.ty.clone(), v.is_const));
            }
        }
        None
    }

    fn mark_used(&mut self, name: &str) {
        for s in self.scopes.iter_mut().rev() {
            if let Some(v) = s.get_mut(name) {
                v.used = true;
                return;
            }
        }
    }

    fn visible_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.scopes.iter().flat_map(|s| s.keys().cloned()).collect();
        v.extend(self.funcs.keys().cloned());
        v.extend(self.classes.keys().cloned());
        v.extend(self.enums.keys().cloned());
        v.extend(self.ifaces.keys().cloned());
        v.extend(self.globals.keys().cloned());
        v.extend(self.imports.iter().cloned());
        v.extend(BUILTINS.iter().map(|s| s.to_string()));
        v.extend(PRELUDE_ERRORS.iter().map(|s| s.to_string()));
        v.push("Error".into());
        v
    }

    fn check_assign(&mut self, from: &Ty, to: &Ty, span: Span, what: &str) {
        if !assignable(&self.table, from, to) {
            self.err("E0401", span, format!("type mismatch in {}: expected `{}`, found `{}`", what, to.display(), from.display()), format!("this is `{}`", from.display()));
        }
    }

    // ───────── statements ─────────

    fn stmts(&mut self, b: &'a Block) {
        let mut dead = false;
        let mut reported = false;
        for s in b {
            if dead && !reported {
                self.warn("W0302", s.span, "unreachable code", "this statement can never run");
                reported = true;
            }
            self.stmt(s);
            if matches!(s.kind, StmtKind::Return(_) | StmtKind::Throw(_) | StmtKind::Break | StmtKind::Continue | StmtKind::Exit(_)) {
                dead = true;
            }
        }
    }

    fn block(&mut self, b: &'a Block) {
        self.scopes.push(HashMap::new());
        self.stmts(b);
        self.pop_scope();
    }

    fn cond(&mut self, e: &'a Expr) {
        let t = self.expr(e);
        if t.is_known() && !matches!(t, Ty::Bool | Ty::Null | Ty::Opt(_) | Ty::Union(_)) {
            self.err("E0405", e.span, format!("condition must be bool, found `{}`", t.display()), "not a boolean");
        }
    }

    fn stmt(&mut self, s: &'a Stmt) {
        match &s.kind {
            StmtKind::Let { name, ty, value, is_const } => {
                let vt = value.as_ref().map(|v| self.expr(v));
                let declared = ty.as_ref().map(|t| self.resolve(t));
                let final_ty = match (&declared, &vt) {
                    (Some(d), Some(v)) => {
                        let sp = value.as_ref().map(|v| v.span).unwrap_or(s.span);
                        self.check_assign(v, d, sp, &format!("variable `{}`", name));
                        d.clone()
                    }
                    (Some(d), None) => d.clone(),
                    (None, Some(v)) if *is_const => v.clone(),
                    _ => Ty::Any,
                };
                self.define(name, final_ty, *is_const, s.span, true);
            }
            StmtKind::Assign { target, op, value } => {
                let vt = self.expr(value);
                match &target.kind {
                    ExprKind::Ident(n) => {
                        let found = self.lookup_for_assign(n);
                        match found {
                            Some((t, is_const)) => {
                                if is_const {
                                    self.err("E0310", target.span, format!("cannot assign to constant `{}`", n), "declared with `const`");
                                } else if op.is_none() {
                                    self.check_assign(&vt, &t, value.span, &format!("assignment to `{}`", n));
                                }
                            }
                            None => {
                                if self.funcs.contains_key(n) || self.classes.contains_key(n) || self.enums.contains_key(n) || self.imports.contains(n) {
                                    self.err("E0310", target.span, format!("cannot assign to `{}`", n), "not a variable");
                                } else {
                                    let names = self.visible_names();
                                    let mut d = Diagnostic::error("E0311", format!("assignment to undeclared variable `{}`", n)).at(target.span, "declare it with `let` first");
                                    if let Some(sg) = suggest(n, names.iter().map(|s| s.as_str())) {
                                        d = d.help(format!("did you mean `{}`?", sg));
                                    }
                                    self.ds.push(d);
                                }
                            }
                        }
                        if op.is_some() {
                            self.mark_used(n);
                        }
                    }
                    _ => {
                        self.expr(target);
                    }
                }
            }
            StmtKind::Expr(e) => {
                self.expr(e);
            }
            StmtKind::Print { value, .. } => {
                self.expr(value);
            }
            StmtKind::If { cond, then, otherwise } => {
                self.cond(cond);
                self.block(then);
                self.block(otherwise);
            }
            StmtKind::While { cond, body } => {
                self.cond(cond);
                self.ctx.loops += 1;
                self.block(body);
                self.ctx.loops -= 1;
            }
            StmtKind::For { vars, iter, body } => {
                let it = self.expr(iter);
                if it.is_known() && !matches!(it, Ty::List(_) | Ty::Map(_) | Ty::Str | Ty::Null | Ty::Opt(_)) {
                    self.err("E0406", iter.span, format!("cannot iterate over `{}`", it.display()), "expected a list, map or string");
                }
                self.scopes.push(HashMap::new());
                for v in vars {
                    self.define(v, Ty::Any, false, s.span, false);
                }
                self.ctx.loops += 1;
                self.stmts(body);
                self.ctx.loops -= 1;
                self.pop_scope();
            }
            StmtKind::Match { subject, cases, default } => {
                self.expr(subject);
                for c in cases {
                    for w in &c.when {
                        self.expr(w);
                    }
                    self.block(&c.body);
                }
                self.block(default);
            }
            StmtKind::Break | StmtKind::Continue => {
                if self.ctx.loops == 0 {
                    let w = if matches!(s.kind, StmtKind::Break) { "break" } else { "continue" };
                    self.err("E0312", s.span, format!("`{}` outside of a loop", w), "only valid inside `for`/`while`");
                }
            }
            StmtKind::Return(e) => {
                let t = e.as_ref().map(|e| self.expr(e)).unwrap_or(Ty::Null);
                if let Some(r) = self.ctx.ret.clone() {
                    if !self.ctx.is_async {
                        if matches!(r, Ty::Void) {
                            if e.is_some() && !matches!(t, Ty::Null | Ty::Any) {
                                self.err("E0401", s.span, "function declared `void` returns a value", "remove the value");
                            }
                        } else {
                            let sp = e.as_ref().map(|e| e.span).unwrap_or(s.span);
                            self.check_assign(&t, &r, sp, &format!("return value of `{}`", self.ctx.fname.clone()));
                        }
                    }
                }
            }
            StmtKind::Throw(e) => {
                self.expr(e);
            }
            StmtKind::Try { body, catches, finally } => {
                self.block(body);
                for c in catches {
                    if let Some(t) = &c.ty {
                        self.resolve(t);
                    }
                    self.scopes.push(HashMap::new());
                    if let Some(v) = &c.var {
                        self.define(v, Ty::Any, false, c.span, false);
                    }
                    self.stmts(&c.body);
                    self.pop_scope();
                }
                self.block(finally);
            }
            StmtKind::Run(r) => self.run_stmt(r),
            StmtKind::Parallel { branches, max } => {
                if let Some(m) = max {
                    self.expr(m);
                }
                for b in branches {
                    let saved = self.ctx.loops;
                    self.ctx.loops = 0;
                    self.block(b);
                    self.ctx.loops = saved;
                }
            }
            StmtKind::Spawn { body, name } => {
                let saved = self.ctx.loops;
                self.ctx.loops = 0;
                self.block(body);
                self.ctx.loops = saved;
                if let Some(n) = name {
                    self.define(n, Ty::Task, false, s.span, false);
                }
            }
            StmtKind::Await { task, into } => {
                self.expr(task);
                if let Some(n) = into {
                    self.define(n, Ty::Any, false, s.span, false);
                }
            }
            StmtKind::Block(b) => self.block(b),
            StmtKind::Assert { cond, message } => {
                self.cond(cond);
                if let Some(m) = message {
                    self.expr(m);
                }
            }
            StmtKind::Output { value, .. } => {
                if !self.ctx.wf_script {
                    self.err("E0313", s.span, "`output` is only valid inside workflow step scripts", "move it into a workflow `script` step");
                }
                self.expr(value);
            }
            StmtKind::SuperCall(args) => {
                for a in args {
                    self.expr(a);
                }
                let ok_class = self.ctx.class.as_ref().and_then(|c| self.table.get(c)).map_or(false, |c| c.parent.is_some());
                if !self.ctx.in_ctor {
                    self.err("E0314", s.span, "`super` call is only valid inside a constructor", "move it into `constructors`");
                } else if !ok_class {
                    self.err("E0314", s.span, "this class has no parent class to call", "remove the `super` call or add `extends`");
                }
            }
            StmtKind::Exit(e) => {
                if let Some(e) = e {
                    self.expr(e);
                }
            }
        }
    }

    fn lookup_for_assign(&mut self, n: &str) -> Option<(Ty, bool)> {
        for s in self.scopes.iter().rev() {
            if let Some(v) = s.get(n) {
                return Some((v.ty.clone(), v.is_const));
            }
        }
        self.globals.get(n).cloned()
    }

    fn run_stmt(&mut self, r: &'a RunStmt) {
        self.expr(&r.program);
        for a in &r.args {
            self.expr(a);
        }
        for e in [&r.cwd, &r.stdin, &r.stdin_file, &r.timeout].into_iter().flatten() {
            self.expr(e);
        }
        for (_, e) in &r.env {
            self.expr(e);
        }
        for rd in [&r.stdout, &r.stderr].into_iter().flatten() {
            if let Redirect::File { path, .. } = rd {
                self.expr(path);
            }
        }
        if let Some(n) = &r.into {
            self.define(n, Ty::Map(Box::new(Ty::Any)), false, r.span, false);
        }
    }

    // ───────── expressions ─────────

    fn class_name_of(&self, name: &str) -> Option<&'a ClassDecl> {
        self.classes.get(name).copied()
    }

    fn expr(&mut self, e: &'a Expr) -> Ty {
        match &e.kind {
            ExprKind::Null => Ty::Null,
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Int(_) => Ty::Int,
            ExprKind::Float(_) => Ty::Float,
            ExprKind::Str(_) => Ty::Str,
            ExprKind::Template(parts) => {
                let mut single = None;
                for p in parts {
                    if let TemplatePart::Expr(x) = p {
                        let t = self.expr(x);
                        if parts.len() == 1 {
                            single = Some(t);
                        }
                    }
                }
                single.unwrap_or(Ty::Str)
            }
            ExprKind::List(items) => {
                let ts: Vec<Ty> = items.iter().map(|i| self.expr(i)).collect();
                let elem = match ts.first() {
                    Some(f) if ts.iter().all(|t| t == f) => f.clone(),
                    _ => Ty::Any,
                };
                Ty::List(Box::new(elem))
            }
            ExprKind::Map(entries) => {
                for (k, v) in entries {
                    let kt = self.expr(k);
                    if kt.is_known() && !matches!(kt, Ty::Str) {
                        self.err("E0407", k.span, format!("map keys must be strings, found `{}`", kt.display()), "wrap it with `str(...)`");
                    }
                    self.expr(v);
                }
                Ty::Map(Box::new(Ty::Any))
            }
            ExprKind::Ident(n) => self.ident(n, e.span),
            ExprKind::This => {
                if self.ctx.class.is_none() || self.ctx.in_static {
                    self.err("E0314", e.span, "`this` is only available inside instance methods and constructors", "no instance here");
                    Ty::Any
                } else {
                    Ty::Class(self.ctx.class.clone().unwrap())
                }
            }
            ExprKind::Unary(op, x) => {
                let t = self.expr(x);
                match op {
                    UnOp::Neg => {
                        if t.is_known() && !t.is_numeric() {
                            self.err("E0403", e.span, format!("cannot negate `{}`", t.display()), "expected a number");
                            Ty::Any
                        } else {
                            t
                        }
                    }
                    UnOp::Not => {
                        if t.is_known() && !matches!(t, Ty::Bool | Ty::Null | Ty::Opt(_) | Ty::Union(_)) {
                            self.err("E0405", x.span, format!("operand of `!` must be bool, found `{}`", t.display()), "not a boolean");
                        }
                        Ty::Bool
                    }
                }
            }
            ExprKind::Binary(op, l, r) => self.binary(*op, l, r, e.span),
            ExprKind::Ternary(c, a, b) => {
                self.cond(c);
                let (ta, tb) = (self.expr(a), self.expr(b));
                if ta == tb { ta } else { Ty::Any }
            }
            ExprKind::Member { obj, name, .. } => self.member(obj, name, e.span),
            ExprKind::Index { obj, index } => {
                let ot = self.expr(obj);
                let it = self.expr(index);
                match (&ot, &it) {
                    (Ty::List(_), t) | (Ty::Str, t) if t.is_known() && !matches!(t, Ty::Int) => {
                        self.err("E0407", index.span, format!("index must be int, found `{}`", t.display()), "lists and strings are indexed by int");
                        Ty::Any
                    }
                    (Ty::Map(_), t) if t.is_known() && !matches!(t, Ty::Str) => {
                        self.err("E0407", index.span, format!("map keys must be strings, found `{}`", t.display()), "maps are indexed by string");
                        Ty::Any
                    }
                    (Ty::List(el), _) => (**el).clone(),
                    (Ty::Map(el), _) => (**el).clone(),
                    (Ty::Str, _) => Ty::Str,
                    _ => Ty::Any,
                }
            }
            ExprKind::Slice { obj, from, to } => {
                let t = self.expr(obj);
                for x in [from, to].into_iter().flatten() {
                    self.expr(x);
                }
                match t {
                    Ty::List(_) | Ty::Str => t,
                    _ => Ty::Any,
                }
            }
            ExprKind::Call { callee, args } => self.call(callee, args, e.span),
            ExprKind::New { class, args } => self.new_expr(class, args, e.span),
            ExprKind::SuperMethod { method, args } => {
                for a in args {
                    self.expr(a);
                }
                match self.ctx.class.clone() {
                    None => {
                        self.err("E0314", e.span, "`super` is only valid inside a method", "no enclosing class");
                    }
                    Some(c) => {
                        let parent = self.table.get(&c).and_then(|i| i.parent.clone());
                        let known = parent.as_ref().map_or(false, |p| self.methods_of(p).iter().any(|(_, m)| &m.name == method && m.body.is_some()));
                        if parent.is_none() || (!known && !matches!(method.as_str(), "to_string" | "equals" | "hash") && parent.as_ref().map_or(false, |p| self.classes.contains_key(p))) {
                            let msg = match &parent {
                                None => format!("`super.{}` used in class `{}` that has no parent", method, c),
                                Some(p) => format!("parent class `{}` has no method `{}`", p, method),
                            };
                            self.err("E0314", e.span, msg, "no such parent method");
                        }
                    }
                }
                Ty::Any
            }
            ExprKind::Lambda(decl) => {
                let saved = self.ctx.clone();
                self.scopes.push(HashMap::new());
                for p in &decl.params {
                    self.define(&p.name, Ty::Any, false, p.span, false);
                }
                self.ctx.loops = 0;
                self.ctx.ret = None;
                self.ctx.in_func = true;
                if let Some(b) = &decl.body {
                    self.stmts(b);
                }
                self.pop_scope();
                self.ctx = saved;
                Ty::Fn
            }
            ExprKind::Is(x, t) => {
                self.expr(x);
                self.resolve(t);
                Ty::Bool
            }
            ExprKind::As(x, t) => {
                self.expr(x);
                self.resolve(t)
            }
            ExprKind::Await(x) => {
                self.expr(x);
                Ty::Any
            }
        }
    }

    fn ident(&mut self, n: &str, span: Span) -> Ty {
        if let Some((t, _)) = self.lookup(n) {
            return t;
        }
        if let Some((t, _)) = self.globals.get(n) {
            return t.clone();
        }
        if self.funcs.contains_key(n) {
            return Ty::Fn;
        }
        if self.classes.contains_key(n) || self.enums.contains_key(n) || self.ifaces.contains_key(n) {
            return Ty::Any;
        }
        if self.imports.contains(n) {
            return Ty::Module;
        }
        if BUILTINS.contains(&n) || PRELUDE_ERRORS.contains(&n) || n == "Error" {
            return Ty::Any;
        }
        let names = self.visible_names();
        let mut d = Diagnostic::error("E0301", format!("undefined name `{}`", n)).at(span, "not found in this scope");
        if let Some(s) = suggest(n, names.iter().map(|s| s.as_str())) {
            d = d.help(format!("did you mean `{}`?", s));
        }
        self.ds.push(d);
        Ty::Any
    }

    fn binary(&mut self, op: BinOp, l: &'a Expr, r: &'a Expr, span: Span) -> Ty {
        let (a, b) = (self.expr(l), self.expr(r));
        let known = a.is_known() && b.is_known();
        let bad = |me: &mut Sema, a: &Ty, b: &Ty| {
            me.err("E0403", span, format!("cannot apply `{}` to `{}` and `{}`", op.symbol(), a.display(), b.display()), "incompatible operand types");
        };
        match op {
            BinOp::And | BinOp::Or => {
                for (t, e) in [(&a, l), (&b, r)] {
                    if t.is_known() && !matches!(t, Ty::Bool | Ty::Null | Ty::Opt(_) | Ty::Union(_)) {
                        self.err("E0405", e.span, format!("operand of `{}` must be bool, found `{}`", op.symbol(), t.display()), "not a boolean");
                    }
                }
                Ty::Bool
            }
            BinOp::Eq | BinOp::Ne | BinOp::In => Ty::Bool,
            BinOp::Coalesce => if a == b { a } else { Ty::Any },
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                if known && !((a.is_numeric() && b.is_numeric()) || (a == Ty::Str && b == Ty::Str)) {
                    bad(self, &a, &b);
                }
                Ty::Bool
            }
            BinOp::Add => {
                if a == Ty::Str || b == Ty::Str {
                    Ty::Str
                } else if a.is_numeric() && b.is_numeric() {
                    if a == Ty::Float || b == Ty::Float { Ty::Float } else { Ty::Int }
                } else if matches!((&a, &b), (Ty::List(_), Ty::List(_))) {
                    a
                } else {
                    if known && !matches!(a, Ty::Union(_) | Ty::Opt(_)) && !matches!(b, Ty::Union(_) | Ty::Opt(_)) {
                        bad(self, &a, &b);
                    }
                    Ty::Any
                }
            }
            BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::Pow => {
                if known && !(a.is_numeric() && b.is_numeric()) && !matches!(a, Ty::Union(_) | Ty::Opt(_)) && !matches!(b, Ty::Union(_) | Ty::Opt(_)) {
                    bad(self, &a, &b);
                    return Ty::Any;
                }
                match (op, &a, &b) {
                    (BinOp::Sub | BinOp::Mul | BinOp::Mod, Ty::Int, Ty::Int) => Ty::Int,
                    (_, x, y) if x.is_numeric() && y.is_numeric() && (*x == Ty::Float || *y == Ty::Float) => Ty::Float,
                    _ => Ty::Any,
                }
            }
        }
    }

    fn member(&mut self, obj: &'a Expr, name: &str, span: Span) -> Ty {
        // Enum.Variant / Class.static
        if let ExprKind::Ident(n) = &obj.kind {
            let shadowed = self.scopes.iter().any(|s| s.contains_key(n.as_str()));
            if !shadowed {
                if let Some(en) = self.enums.get(n.as_str()).copied() {
                    if en.variants.iter().any(|(v, _)| v == name) {
                        return Ty::Class(n.clone());
                    }
                    if matches!(name, "values" | "from_name" | "from_value") {
                        return Ty::Fn;
                    }
                    let vs: Vec<&str> = en.variants.iter().map(|(v, _)| v.as_str()).collect();
                    let mut d = Diagnostic::error("E0315", format!("enum `{}` has no variant `{}`", n, name)).at(span, "unknown variant");
                    if let Some(s) = suggest(name, vs.iter().copied()) {
                        d = d.help(format!("did you mean `{}`?", s));
                    }
                    self.ds.push(d);
                    return Ty::Any;
                }
            }
        }
        let ot = self.expr(obj);
        if let ExprKind::This = &obj.kind {
            if let Some(cn) = self.ctx.class.clone() {
                if self.classes.contains_key(&cn) {
                    return self.this_member(&cn, name, span, false);
                }
            }
        }
        match ot {
            Ty::Class(c) if self.classes.contains_key(&c) && !matches!(obj.kind, ExprKind::This) => {
                // access violations on other instances of program classes
                if let Some((owner, f)) = self.fields_of(&c).into_iter().find(|(_, f)| f.name == name) {
                    let cur = self.ctx.class.clone();
                    let ok = match f.access {
                        Access::Public => true,
                        Access::Private => cur.as_deref() == Some(owner.as_str()),
                        Access::Protected => cur.as_ref().map_or(false, |cc| self.table.is_subclass(cc, &owner)),
                    };
                    if !ok {
                        self.err("E0316", span, format!("{} field `{}` of class `{}` is not accessible here", f.access.as_str(), name, owner), "access violation");
                    }
                    return f.ty.as_ref().map(|t| from_type_expr(t, &self.table, &[]).unwrap_or(Ty::Any)).unwrap_or(Ty::Any);
                }
                Ty::Any
            }
            _ => Ty::Any,
        }
    }

    fn this_member(&mut self, cn: &str, name: &str, span: Span, is_call: bool) -> Ty {
        if let Some((owner, f)) = self.fields_of(cn).into_iter().find(|(_, f)| f.name == name) {
            if f.access == Access::Private && owner != cn {
                self.err("E0316", span, format!("private field `{}` of class `{}` is not accessible from `{}`", name, owner, cn), "access violation");
            }
            return f.ty.as_ref().map(|t| from_type_expr(t, &self.table, &self.generics).unwrap_or(Ty::Any)).unwrap_or(Ty::Any);
        }
        let ms = self.methods_of(cn);
        if let Some((owner, m)) = ms.iter().find(|(_, m)| m.name == name) {
            if m.access == Access::Private && owner != cn {
                self.err("E0316", span, format!("private method `{}` of class `{}` is not accessible from `{}`", name, owner, cn), "access violation");
            }
            return Ty::Fn;
        }
        if matches!(name, "to_string" | "equals" | "hash") {
            return Ty::Fn;
        }
        let _ = is_call;
        // inherited from prelude (Error): message/code/cause/stack
        let mut cur = Some(cn.to_string());
        while let Some(c) = cur {
            if c == "Error" || PRELUDE_ERRORS.contains(&c.as_str()) {
                if matches!(name, "message" | "code" | "cause" | "stack" | "result") {
                    return Ty::Any;
                }
            }
            cur = self.table.get(&c).and_then(|i| i.parent.clone());
        }
        let mut names: Vec<String> = self.fields_of(cn).into_iter().map(|(_, f)| f.name.clone()).collect();
        names.extend(self.methods_of(cn).into_iter().map(|(_, m)| m.name.clone()));
        let mut d = Diagnostic::error("E0317", format!("class `{}` has no member `{}`", cn, name)).at(span, "unknown field or method");
        if let Some(s) = suggest(name, names.iter().map(|s| s.as_str())) {
            d = d.help(format!("did you mean `{}`?", s));
        }
        self.ds.push(d);
        Ty::Any
    }

    fn check_args(&mut self, decl: &FuncDecl, args: &'a [Expr], arg_tys: &[Ty], span: Span, what: &str) {
        if !arity_ok(decl, args.len()) {
            self.err("E0308", span, format!("{} expects {} argument(s), got {}", what, arity_text(decl), args.len()), "wrong number of arguments");
            return;
        }
        let gen: Vec<String> = decl.generics.iter().map(|g| g.name.clone()).collect();
        for (i, (p, t)) in decl.params.iter().zip(arg_tys.iter()).enumerate() {
            if p.variadic {
                break;
            }
            if let Some(pt) = &p.ty {
                if let Ok(want) = from_type_expr(pt, &self.table, &[self.generics.clone(), gen.clone()].concat()) {
                    if !assignable(&self.table, t, &want) {
                        self.err("E0401", args[i].span, format!("argument {} of {}: expected `{}`, found `{}`", i + 1, what, want.display(), t.display()), format!("parameter `{}` is `{}`", p.name, want.display()));
                    }
                }
            }
        }
    }

    fn call(&mut self, callee: &'a Expr, args: &'a [Expr], span: Span) -> Ty {
        let tys: Vec<Ty> = Vec::new();
        let _ = tys;
        match &callee.kind {
            ExprKind::Ident(n) => {
                let ct = self.ident(n, callee.span);
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(a)).collect();
                let local = self.scopes.iter().any(|s| s.contains_key(n.as_str()));
                if !local {
                    if let Some(f) = self.funcs.get(n.as_str()).copied() {
                        let f: &FuncDecl = f;
                        self.check_args(f, args, &arg_tys, span, &format!("function `{}`", n));
                        if f.is_async {
                            return Ty::Task;
                        }
                        return f.ret.as_ref().map(|t| from_type_expr(t, &self.table, &f.generics.iter().map(|g| g.name.clone()).collect::<Vec<_>>()).unwrap_or(Ty::Any)).unwrap_or(Ty::Any);
                    }
                    if self.classes.contains_key(n.as_str()) || self.enums.contains_key(n.as_str()) {
                        self.err_help("E0318", callee.span, format!("`{}` is a class and must be created with `new`", n), "not callable", format!("write `new {}(...)`", n));
                    }
                    return match n.as_str() {
                        "len" | "int" => Ty::Int,
                        "str" | "typeof" => Ty::Str,
                        "float" => Ty::Float,
                        "bool" | "has" => Ty::Bool,
                        _ => Ty::Any,
                    };
                }
                if ct.is_known() && !matches!(ct, Ty::Fn) {
                    self.err("E0318", callee.span, format!("`{}` is `{}`, not a function", n, ct.display()), "not callable");
                }
                Ty::Any
            }
            ExprKind::Member { obj, name, .. } => {
                let is_this = matches!(obj.kind, ExprKind::This);
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(a)).collect();
                if is_this && self.ctx.class.is_some() && !self.ctx.in_static {
                    let cn = self.ctx.class.clone().unwrap();
                    if self.classes.contains_key(&cn) {
                        self.this_member(&cn, name, callee.span, true);
                        let ms: Vec<&FuncDecl> = self.methods_of(&cn).into_iter().filter(|(_, m)| &m.name == name).map(|(_, m)| &**m).collect();
                        if !ms.is_empty() {
                            if !ms.iter().any(|m| arity_ok(m, args.len())) {
                                let ex: Vec<String> = ms.iter().map(|m| arity_text(m)).collect();
                                self.err("E0308", span, format!("method `{}` expects {} argument(s), got {}", name, ex.join(" or "), args.len()), "wrong number of arguments");
                            } else if ms.len() == 1 {
                                let m = ms[0];
                                self.check_args(m, args, &arg_tys, span, &format!("method `{}`", name));
                                return m.ret.as_ref().map(|t| from_type_expr(t, &self.table, &[self.generics.clone(), m.generics.iter().map(|g| g.name.clone()).collect()].concat()).unwrap_or(Ty::Any)).unwrap_or(Ty::Any);
                            }
                        }
                    }
                    return Ty::Any;
                }
                // evaluate receiver (enum static calls etc.)
                self.member_callee(obj, name, span);
                Ty::Any
            }
            _ => {
                self.expr(callee);
                for a in args {
                    self.expr(a);
                }
                Ty::Any
            }
        }
    }

    fn member_callee(&mut self, obj: &'a Expr, name: &str, span: Span) {
        if let ExprKind::Ident(n) = &obj.kind {
            let shadowed = self.scopes.iter().any(|s| s.contains_key(n.as_str()));
            if !shadowed && self.enums.contains_key(n.as_str()) {
                if !matches!(name, "values" | "from_name" | "from_value") {
                    self.err("E0315", span, format!("enum `{}` has no function `{}`", n, name), "available: values, from_name, from_value");
                }
                return;
            }
        }
        self.expr(obj);
    }

    fn new_expr(&mut self, class: &'a TypeExpr, args: &'a [Expr], span: Span) -> Ty {
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(a)).collect();
        let (path, targs) = match &class.kind {
            TypeExprKind::Named { path, args } => (path, args),
            _ => {
                self.err("E0318", class.span, "`new` needs a class name", "not a class");
                return Ty::Any;
            }
        };
        for t in targs {
            self.resolve(t);
        }
        if path.len() > 1 {
            return Ty::Any;
        }
        let n = &path[0];
        let info = match self.table.get(n).cloned() {
            Some(i) => i,
            None => {
                let names: Vec<&str> = self.table.classes.keys().map(|s| s.as_str()).collect();
                let mut d = Diagnostic::error("E0302", format!("unknown class `{}`", n)).at(class.span, "not declared");
                if let Some(s) = suggest(n, names.iter().copied()) {
                    d = d.help(format!("did you mean `{}`?", s));
                }
                self.ds.push(d);
                return Ty::Any;
            }
        };
        match info.kind {
            Kind::Interface => {
                self.err("E0319", span, format!("cannot instantiate interface `{}`", n), "interfaces have no instances");
                return Ty::Class(n.clone());
            }
            Kind::Enum => {
                self.err("E0319", span, format!("cannot instantiate enum `{}`", n), "use its variants instead");
                return Ty::Class(n.clone());
            }
            Kind::Class => {}
        }
        if info.is_abstract {
            self.err("E0319", span, format!("cannot instantiate abstract class `{}`", n), "abstract classes need a concrete subclass");
        }
        if !targs.is_empty() && targs.len() != info.generics.len() {
            self.err("E0327", class.span, format!("class `{}` takes {} type argument(s), got {}", n, info.generics.len(), targs.len()), "wrong number of type arguments");
        }
        // constructor arity (first class in the chain that declares constructors)
        if self.classes.contains_key(n) {
            let mut cur = Some(n.clone());
            let mut found: Option<&'a ClassDecl> = None;
            let mut depth = 0;
            while let Some(c) = cur {
                match self.classes.get(&c) {
                    Some(d) if !d.constructors.is_empty() => {
                        found = Some(d);
                        break;
                    }
                    _ => {}
                }
                if PRELUDE_ERRORS.contains(&c.as_str()) || c == "Error" {
                    if args.len() > 2 {
                        self.err("E0308", span, format!("constructor of `{}` expects 0 to 2 arguments, got {}", n, args.len()), "wrong number of arguments");
                    }
                    return Ty::Class(n.clone());
                }
                cur = self.table.get(&c).and_then(|i| i.parent.clone());
                depth += 1;
                if depth > 64 {
                    break;
                }
            }
            match found {
                Some(d) => {
                    if !d.constructors.iter().any(|c| arity_ok(c, args.len())) {
                        let ex: Vec<String> = d.constructors.iter().map(|c| arity_text(c)).collect();
                        self.err("E0308", span, format!("constructor of `{}` expects {} argument(s), got {}", n, ex.join(" or "), args.len()), "wrong number of arguments");
                    } else if d.constructors.len() == 1 && info.generics.is_empty() {
                        let c: &FuncDecl = &d.constructors[0];
                        self.check_args(c, args, &arg_tys, span, &format!("constructor of `{}`", n));
                    }
                }
                None => {
                    if !args.is_empty() {
                        self.err("E0308", span, format!("class `{}` has no constructor but got {} argument(s)", n, args.len()), "remove the arguments or declare a constructor");
                    }
                }
            }
        } else if PRELUDE_ERRORS.contains(&n.as_str()) || n == "Error" {
            if args.len() > 2 {
                self.err("E0308", span, format!("constructor of `{}` expects 0 to 2 arguments, got {}", n, args.len()), "wrong number of arguments");
            }
        }
        let _ = self.class_name_of(n);
        Ty::Class(n.clone())
    }

    // ───────── workflow ─────────

    fn workflow(&mut self, wf: &'a Workflow) {
        self.ctx = Ctx::default();
        self.scopes.push(HashMap::new());
        for n in WF_CTX {
            self.define(n, Ty::Any, false, Span::DUMMY, false);
        }
        for (_, e) in &wf.env {
            self.expr(e);
        }
        for i in &wf.inputs {
            if let Some(d) = &i.default {
                self.expr(d);
            }
            if let Some(t) = &i.ty {
                self.resolve(t);
            }
        }
        for e in [&wf.max_parallel, &wf.default_timeout, &wf.default_retries].into_iter().flatten() {
            self.expr(e);
        }
        // graph
        let ids: HashSet<&str> = wf.jobs.iter().map(|j| j.id.as_str()).collect();
        for j in &wf.jobs {
            for n in &j.needs {
                if !ids.contains(n.as_str()) {
                    let names: Vec<&str> = ids.iter().copied().collect();
                    let mut d = Diagnostic::error("E0602", format!("job `{}` needs unknown job `{}`", j.id, n)).at(j.span, "unknown dependency");
                    if let Some(s) = suggest(n, names.iter().copied()) {
                        d = d.help(format!("did you mean `{}`?", s));
                    }
                    self.ds.push(d);
                }
                if n == &j.id {
                    self.err("E0603", j.span, format!("job `{}` depends on itself", j.id), "cycle");
                }
            }
        }
        if let Some(cycle) = find_cycle(wf) {
            let sp = wf.jobs.iter().find(|j| j.id == cycle[0]).map(|j| j.span).unwrap_or(wf.span);
            self.err("E0603", sp, format!("dependency cycle: {}", cycle.join(" -> ")), "jobs must form a DAG");
        }
        for j in &wf.jobs {
            if let Some(m) = &j.matrix {
                for (_, e) in &m.dims {
                    self.expr(e);
                }
                for e in m.include.iter().chain(m.exclude.iter()) {
                    self.expr(e);
                }
            }
            for e in [&j.name, &j.cond, &j.workdir, &j.timeout, &j.retries, &j.retry_delay, &j.continue_on_error].into_iter().flatten() {
                self.expr(e);
            }
            for (_, e) in j.env.iter().chain(j.outputs.iter()) {
                self.expr(e);
            }
            if let Some(l) = &j.limits {
                for e in [&l.memory, &l.processes, &l.cpu_time].into_iter().flatten() {
                    self.expr(e);
                }
            }
            let mut step_ids: HashSet<&str> = HashSet::new();
            for s in j.steps.iter().chain(j.finally.iter()) {
                if let Some(id) = &s.id {
                    if !step_ids.insert(id.as_str()) {
                        self.err("E0601", s.span, format!("duplicate step id `{}` in job `{}`", id, j.id), "step ids must be unique within a job");
                    }
                }
                self.step(s);
            }
        }
        self.pop_scope();
    }

    fn step(&mut self, s: &'a Step) {
        for e in [&s.name, &s.cond, &s.workdir, &s.timeout, &s.retries, &s.retry_delay, &s.continue_on_error].into_iter().flatten() {
            self.expr(e);
        }
        for (_, e) in s.env.iter().chain(s.outputs.iter()) {
            self.expr(e);
        }
        match &s.action {
            StepAction::Run(r) => self.run_stmt(r),
            StepAction::Script(b) => {
                let saved = self.ctx.wf_script;
                self.ctx.wf_script = true;
                self.block(b);
                self.ctx.wf_script = saved;
            }
            StepAction::Uses { function, with } => {
                for (_, e) in with {
                    self.expr(e);
                }
                let first = function.split('.').next().unwrap_or("");
                if function.contains('.') {
                    if !self.imports.contains(first) {
                        self.err("E0604", s.span, format!("`uses` refers to unknown module `{}`", first), "not imported");
                    }
                } else {
                    match self.funcs.get(function.as_str()).copied() {
                        None => {
                            let names: Vec<&str> = self.funcs.keys().map(|s| s.as_str()).collect();
                            let mut d = Diagnostic::error("E0604", format!("`uses` refers to unknown function `{}`", function)).at(s.span, "not declared in `functions`");
                            if let Some(sg) = suggest(function, names.iter().copied()) {
                                d = d.help(format!("did you mean `{}`?", sg));
                            }
                            self.ds.push(d);
                        }
                        Some(f) => {
                            for (k, _) in with {
                                if !f.params.iter().any(|p| &p.name == k) {
                                    let pn: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
                                    let mut d = Diagnostic::error("E0604", format!("function `{}` has no parameter `{}`", function, k)).at(s.span, "unknown `with` key");
                                    if let Some(sg) = suggest(k, pn.iter().copied()) {
                                        d = d.help(format!("did you mean `{}`?", sg));
                                    }
                                    self.ds.push(d);
                                }
                            }
                            for p in &f.params {
                                if p.default.is_none() && !p.variadic && !with.iter().any(|(k, _)| k == &p.name) {
                                    self.err("E0604", s.span, format!("missing `with` argument `{}` for `{}`", p.name, function), "required parameter not provided");
                                }
                            }
                        }
                    }
                }
            }
        }
        if !s.on_error.is_empty() {
            self.scopes.push(HashMap::new());
            self.stmts(&s.on_error);
            self.pop_scope();
        }
    }
}

fn named_path(t: &TypeExpr) -> Option<String> {
    match &t.kind {
        TypeExprKind::Named { path, .. } if path.len() == 1 => Some(path[0].clone()),
        _ => None,
    }
}

fn find_cycle(wf: &Workflow) -> Option<Vec<String>> {
    let idx: HashMap<&str, &Job> = wf.jobs.iter().map(|j| (j.id.as_str(), j)).collect();
    fn dfs<'a>(id: &'a str, idx: &HashMap<&'a str, &'a Job>, stack: &mut Vec<&'a str>, done: &mut HashSet<&'a str>) -> Option<Vec<String>> {
        if let Some(p) = stack.iter().position(|s| *s == id) {
            let mut c: Vec<String> = stack[p..].iter().map(|s| s.to_string()).collect();
            c.push(id.to_string());
            return Some(c);
        }
        if done.contains(id) {
            return None;
        }
        stack.push(id);
        if let Some(j) = idx.get(id) {
            for n in &j.needs {
                if n != id && idx.contains_key(n.as_str()) {
                    if let Some(c) = dfs(n.as_str(), idx, stack, done) {
                        return Some(c);
                    }
                }
            }
        }
        stack.pop();
        done.insert(id);
        None
    }
    let mut done = HashSet::new();
    for j in &wf.jobs {
        if let Some(c) = dfs(j.id.as_str(), &idx, &mut vec![], &mut done) {
            return Some(c);
        }
    }
    None
}
