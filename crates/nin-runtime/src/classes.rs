//! Object model: class construction, instantiation, dispatch, overloads, access control.

use crate::interp::*;
use crate::value::*;
use indexmap::IndexMap;
use nin_ast::*;
use nin_diag::{suggest, Span};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

pub fn sig_key(d: &FuncDecl) -> String {
    format!("{}({})", d.name, d.params.iter().map(|p| p.ty.as_ref().map(|t| t.display()).unwrap_or_else(|| "any".into()) + if p.variadic { "..." } else { "" }).collect::<Vec<_>>().join(","))
}

fn arity_ok(d: &FuncDecl, n: usize) -> bool {
    let variadic = d.params.last().map_or(false, |p| p.variadic);
    let fixed = if variadic { d.params.len() - 1 } else { d.params.len() };
    let required = d.params.iter().take(fixed).filter(|p| p.default.is_none()).count();
    n >= required && (variadic || n <= fixed)
}

pub fn value_type_expr(v: &Value) -> TypeExpr {
    TypeExpr::named(&v.type_name(), Span::DUMMY)
}

impl Exec {
    // ───────── class construction ─────────

    fn make_class(&mut self, decl: Arc<ClassDecl>, kind: ClassKind, env: &Env, module: &str) -> R<Arc<Class>> {
        let mut parent = None;
        if let Some(t) = &decl.extends {
            let path = match &t.kind {
                TypeExprKind::Named { path, .. } => path.clone(),
                _ => return self.err_at(t.span, "TypeError", "invalid extends clause"),
            };
            match self.lookup_class(&path, env) {
                Some(p) => {
                    if p.kind == ClassKind::Interface {
                        return self.err_at(t.span, "TypeError", format!("class {} cannot extend interface {} (use implements)", decl.name, p.name));
                    }
                    if p.is_final {
                        return self.err_at(t.span, "TypeError", format!("class {} cannot extend final class {}", decl.name, p.name));
                    }
                    if p.kind == ClassKind::Enum {
                        return self.err_at(t.span, "TypeError", "cannot extend an enum");
                    }
                    parent = Some(p);
                }
                None => return self.err_at(t.span, "NameError", format!("unknown parent class `{}`", path.join("."))),
            }
        }
        let mut interfaces = vec![];
        for t in &decl.implements {
            let path = match &t.kind {
                TypeExprKind::Named { path, .. } => path.clone(),
                _ => return self.err_at(t.span, "TypeError", "invalid implements clause"),
            };
            match self.lookup_class(&path, env) {
                Some(i) if i.kind == ClassKind::Interface => interfaces.push(i),
                Some(i) => return self.err_at(t.span, "TypeError", format!("`{}` is not an interface", i.name)),
                None => return self.err_at(t.span, "NameError", format!("unknown interface `{}`", path.join("."))),
            }
        }
        let mut methods: IndexMap<String, Vec<Arc<FuncDecl>>> = IndexMap::new();
        for m in &decl.methods {
            methods.entry(m.name.clone()).or_default().push(m.clone());
        }
        let cls = Arc::new(Class {
            id: self.interp.fresh_id(),
            name: decl.name.clone(),
            kind,
            decl: Some(decl.clone()),
            parent,
            interfaces,
            generics: decl.generics.clone(),
            is_abstract: decl.is_abstract || kind == ClassKind::Interface,
            is_final: decl.is_final,
            ctors: decl.constructors.clone(),
            methods,
            statics: Mutex::new(IndexMap::new()),
            env: env.clone(),
            module_name: module.to_string(),
            enum_values: Mutex::new(vec![]),
        });
        // override / final checks
        if let Some(p) = &cls.parent {
            for m in &decl.methods {
                for (_, pm) in self.find_methods(p, &m.name) {
                    if pm.is_final && sig_key(&pm) == sig_key(m) {
                        return self.err_at(m.span, "TypeError", format!("method `{}` of {} is final and cannot be overridden", m.name, p.name));
                    }
                }
            }
        }
        if kind == ClassKind::Class && !cls.is_abstract {
            let missing = self.missing_abstract(&cls);
            if let Some(m) = missing.first() {
                return self.err_at(decl.span, "TypeError", format!("class {} must implement abstract method `{}`", cls.name, m));
            }
        }
        Ok(cls)
    }

    fn missing_abstract(&self, cls: &Arc<Class>) -> Vec<String> {
        let mut required: Vec<(String, usize)> = vec![];
        let mut seen: HashSet<u64> = HashSet::new();
        let mut stack: Vec<Arc<Class>> = vec![cls.clone()];
        while let Some(c) = stack.pop() {
            if !seen.insert(c.id) {
                continue;
            }
            for (n, ms) in &c.methods {
                for m in ms {
                    if m.body.is_none() && !m.is_static {
                        required.push((n.clone(), m.params.len()));
                    }
                }
            }
            if let Some(p) = &c.parent {
                stack.push(p.clone());
            }
            stack.extend(c.interfaces.iter().cloned());
        }
        let mut out = vec![];
        for (n, ar) in required {
            let ok = self.find_methods(cls, &n).iter().any(|(_, d)| d.body.is_some() && d.params.len() == ar);
            if !ok {
                out.push(n);
            }
        }
        out.dedup();
        out
    }

    /// Define everything a program declares into `env`.
    pub fn define_program(&mut self, prog: &Program, env: &Env, module: &str) -> R<()> {
        let mfr = Frame::new(env.clone(), module);
        let modname: Arc<str> = Arc::from(module);
        for f in &prog.functions {
            env.define(&f.name, Value::Func(Arc::new(Function { decl: f.clone(), env: env.clone(), class: None, this: None, module_name: modname.clone() })), true);
        }
        for e in &prog.enums {
            let c = self.make_enum(e, env, module, &mfr)?;
            lk(&self.interp.classes).insert(c.name.clone(), c.clone());
            env.define(&e.name, Value::Class(c), true);
        }
        // interfaces and classes: define in dependency order
        enum Pending<'a> {
            I(&'a InterfaceDecl),
            C(&'a ClassDecl),
        }
        let mut pending: Vec<Pending> = prog.interfaces.iter().map(Pending::I).chain(prog.classes.iter().map(Pending::C)).collect();
        let mut statics_todo: Vec<Arc<Class>> = vec![];
        while !pending.is_empty() {
            let before = pending.len();
            let mut next = vec![];
            for p in pending {
                let (decl, kind) = match &p {
                    Pending::I(i) => (
                        Arc::new(ClassDecl { name: i.name.clone(), generics: i.generics.clone(), extends: None, implements: i.extends.clone(), is_abstract: true, is_final: false, fields: vec![], constructors: vec![], methods: i.methods.clone(), doc: i.doc.clone(), span: i.span }),
                        ClassKind::Interface,
                    ),
                    Pending::C(c) => (Arc::new((*c).clone()), ClassKind::Class),
                };
                let deps: Vec<&TypeExpr> = decl.extends.iter().chain(decl.implements.iter()).collect();
                let ready = deps.iter().all(|t| match &t.kind {
                    TypeExprKind::Named { path, .. } => self.lookup_class(path, env).is_some(),
                    _ => true,
                });
                if !ready {
                    next.push(p);
                    continue;
                }
                let cls = self.make_class(decl, kind, env, module)?;
                env.define(&cls.name, Value::Class(cls.clone()), true);
                lk(&self.interp.classes).insert(cls.name.clone(), cls.clone());
                statics_todo.push(cls);
            }
            pending = next;
            if pending.len() == before {
                // unresolved parent: report precisely
                let p = &pending[0];
                let (decl_name, span, deps): (String, Span, Vec<TypeExpr>) = match p {
                    Pending::I(i) => (i.name.clone(), i.span, i.extends.clone()),
                    Pending::C(c) => (c.name.clone(), c.span, c.extends.iter().chain(c.implements.iter()).cloned().collect()),
                };
                for t in deps {
                    if let TypeExprKind::Named { path, .. } = &t.kind {
                        if self.lookup_class(path, env).is_none() {
                            let names = env.all_names();
                            let mut m = format!("class `{}` depends on unknown or cyclic type `{}`", decl_name, path.join("."));
                            if let Some(s) = suggest(&path[0], names.iter().map(|s| s.as_str())) {
                                m.push_str(&format!(" (did you mean `{}`?)", s));
                            }
                            return self.err_at(span, "NameError", m);
                        }
                    }
                }
                return self.err_at(span, "NameError", format!("cannot resolve declarations of `{}`", decl_name));
            }
        }
        for g in &prog.globals {
            let mut v = self.eval(&g.value, &mfr)?;
            if let Some(t) = &g.ty {
                v = self.coerce_to(v, t, &mfr, &format!("global `{}`", g.name))?;
            }
            env.define(&g.name, v, g.is_const);
        }
        for c in statics_todo {
            self.init_statics(&c)?;
        }
        Ok(())
    }

    fn init_statics(&mut self, c: &Arc<Class>) -> R<()> {
        if let Some(d) = c.decl.clone() {
            let fr = Frame { env: Scope::new(Some(c.env.clone())), this: None, class: Some(c.clone()), in_ctor: false, generics: Arc::new(HashMap::new()), fname: Arc::from("<static>"), module: Arc::from(c.module_name.as_str()) };
            for f in d.fields.iter().filter(|f| f.is_static) {
                let mut v = match &f.default {
                    Some(e) => self.eval(e, &fr)?,
                    None => Value::Null,
                };
                if let (Some(t), Some(_)) = (&f.ty, &f.default) {
                    v = self.coerce_to(v, t, &fr, &format!("static field `{}.{}`", c.name, f.name))?;
                }
                lk(&c.statics).insert(f.name.clone(), v);
            }
        }
        Ok(())
    }

    fn make_enum(&mut self, e: &EnumDecl, env: &Env, module: &str, fr: &Frame) -> R<Arc<Class>> {
        let decl = Arc::new(ClassDecl { name: e.name.clone(), generics: vec![], extends: None, implements: vec![], is_abstract: false, is_final: true, fields: vec![], constructors: vec![], methods: vec![], doc: None, span: e.span });
        let cls = Arc::new(Class {
            id: self.interp.fresh_id(),
            name: e.name.clone(),
            kind: ClassKind::Enum,
            decl: Some(decl),
            parent: None,
            interfaces: vec![],
            generics: vec![],
            is_abstract: false,
            is_final: true,
            ctors: vec![],
            methods: IndexMap::new(),
            statics: Mutex::new(IndexMap::new()),
            env: env.clone(),
            module_name: module.to_string(),
            enum_values: Mutex::new(vec![]),
        });
        for (i, (vn, ve)) in e.variants.iter().enumerate() {
            let val = match ve {
                Some(x) => self.eval(x, fr)?,
                None => Value::Int(i as i64),
            };
            let mut fields = IndexMap::new();
            fields.insert("name".to_string(), Value::str(vn));
            fields.insert("value".to_string(), val);
            fields.insert("ordinal".to_string(), Value::Int(i as i64));
            let o = Value::Object(Arc::new(Object { id: self.interp.fresh_id(), class: cls.clone(), fields: Mutex::new(fields), type_args: vec![] }));
            lk(&cls.statics).insert(vn.clone(), o.clone());
            lk(&cls.enum_values).push(o);
        }
        Ok(cls)
    }

    // ───────── lookup ─────────

    /// All overloads named `name` visible on `class` (most derived first, overridden ones removed).
    pub fn find_methods(&self, class: &Arc<Class>, name: &str) -> Vec<(Arc<Class>, Arc<FuncDecl>)> {
        let mut out: Vec<(Arc<Class>, Arc<FuncDecl>)> = vec![];
        let mut seen_sig: HashSet<String> = HashSet::new();
        let mut seen_cls: HashSet<u64> = HashSet::new();
        let mut queue: std::collections::VecDeque<Arc<Class>> = std::collections::VecDeque::new();
        queue.push_back(class.clone());
        while let Some(c) = queue.pop_front() {
            if !seen_cls.insert(c.id) {
                continue;
            }
            if let Some(ms) = c.methods.get(name) {
                for m in ms {
                    if seen_sig.insert(sig_key(m)) {
                        out.push((c.clone(), m.clone()));
                    }
                }
            }
            if let Some(p) = &c.parent {
                queue.push_back(p.clone());
            }
            for i in &c.interfaces {
                queue.push_back(i.clone());
            }
        }
        // concrete implementations win over abstract duplicates with the same arity
        let has_body: Vec<usize> = out.iter().filter(|(_, d)| d.body.is_some()).map(|(_, d)| d.params.len()).collect();
        out.retain(|(_, d)| d.body.is_some() || !has_body.contains(&d.params.len()));
        out
    }

    pub fn find_field(&self, class: &Arc<Class>, name: &str) -> Option<(Arc<Class>, FieldDecl)> {
        for c in class.chain() {
            if let Some(d) = &c.decl {
                if let Some(f) = d.fields.iter().find(|f| f.name == name) {
                    return Some((c.clone(), f.clone()));
                }
            }
        }
        None
    }

    pub fn all_fields(&self, class: &Arc<Class>) -> Vec<(Arc<Class>, FieldDecl)> {
        let mut out = vec![];
        for c in class.chain().iter().rev() {
            if let Some(d) = &c.decl {
                for f in &d.fields {
                    out.push((c.clone(), f.clone()));
                }
            }
        }
        out
    }

    pub fn can_access(&self, access: Access, owner: &Arc<Class>, fr: &Frame) -> bool {
        match access {
            Access::Public => true,
            Access::Private => fr.class.as_ref().map_or(false, |c| c.id == owner.id),
            Access::Protected => fr.class.as_ref().map_or(false, |c| c.is_subclass_of(owner)),
        }
    }

    pub fn obj_generics(&self, o: &Object) -> Arc<HashMap<String, TypeExpr>> {
        let mut m = HashMap::new();
        for (g, a) in o.class.generics.iter().zip(o.type_args.iter()) {
            m.insert(g.name.clone(), a.clone());
        }
        Arc::new(m)
    }

    // ───────── overload resolution ─────────

    fn type_score(&mut self, v: &Value, t: &TypeExpr, fr: &Frame) -> Option<u32> {
        if !self.conforms(v, t, fr) {
            return None;
        }
        Some(match &t.kind {
            TypeExprKind::Named { path, .. } if path.len() == 1 => match (path[0].as_str(), v) {
                ("any", _) => 4,
                ("float", Value::Int(_)) => 2,
                (n, _) if fr.generics.contains_key(n) => 3,
                (_, Value::Object(o)) => {
                    let mut d = 0;
                    for c in o.class.chain() {
                        if c.name == path[0] {
                            break;
                        }
                        d += 1;
                    }
                    d.min(2)
                }
                _ => 0,
            },
            TypeExprKind::Named { .. } => match v {
                Value::Object(_) => 1,
                _ => 0,
            },
            TypeExprKind::Optional(_) | TypeExprKind::Union(_) => 1,
            TypeExprKind::Func { .. } => 0,
        })
    }

    pub fn select_overload(&mut self, cands: &[(Arc<Class>, Arc<FuncDecl>)], args: &[Value], fr: &Frame, name: &str, span: Span) -> R<(Arc<Class>, Arc<FuncDecl>)> {
        let by_arity: Vec<&(Arc<Class>, Arc<FuncDecl>)> = cands.iter().filter(|(_, d)| arity_ok(d, args.len())).collect();
        if by_arity.is_empty() {
            let sigs: Vec<String> = cands.iter().map(|(_, d)| format!("{}", sig_key(d))).collect();
            return self.err_at(span, "TypeError", format!("no overload of `{}` accepts {} argument(s); candidates: {}", name, args.len(), sigs.join(", ")));
        }
        if by_arity.len() == 1 {
            return Ok(by_arity[0].clone());
        }
        let mut scored: Vec<(u32, &(Arc<Class>, Arc<FuncDecl>))> = vec![];
        'next: for c in by_arity {
            let d = &c.1;
            let variadic = d.params.last().map_or(false, |p| p.variadic);
            let mut total = 0;
            for (i, a) in args.iter().enumerate() {
                let p = if i < d.params.len() { &d.params[i] } else if variadic { d.params.last().unwrap() } else { continue };
                match &p.ty {
                    None => total += 3,
                    Some(t) => match self.type_score(a, t, fr) {
                        Some(s) => total += s,
                        None => continue 'next,
                    },
                }
            }
            if variadic {
                total += 1;
            }
            total += d.params.len().saturating_sub(args.len()) as u32;
            scored.push((total, c));
        }
        if scored.is_empty() {
            let at: Vec<String> = args.iter().map(|a| a.type_name()).collect();
            let sigs: Vec<String> = cands.iter().map(|(_, d)| sig_key(d)).collect();
            return self.err_at(span, "TypeError", format!("no overload of `{}` matches ({}); candidates: {}", name, at.join(", "), sigs.join(", ")));
        }
        scored.sort_by_key(|(s, _)| *s);
        if scored.len() > 1 && scored[0].0 == scored[1].0 {
            let sigs = format!("{} and {}", sig_key(&scored[0].1 .1), sig_key(&scored[1].1 .1));
            return self.err_at(span, "TypeError", format!("ambiguous call to `{}`: {}", name, sigs));
        }
        Ok(scored[0].1.clone())
    }

    // ───────── invocation ─────────

    pub fn invoke_method(&mut self, recv: Value, owner: Option<&Arc<Class>>, d: &Arc<FuncDecl>, args: Vec<Value>, span: Span) -> R<Value> {
        let env = owner.map(|c| c.env.clone()).unwrap_or_else(|| self.interp.root.clone());
        let module: Arc<str> = Arc::from(owner.map(|c| c.module_name.as_str()).unwrap_or("<native>"));
        let generics = match &recv {
            Value::Object(o) => self.obj_generics(o),
            _ => Arc::new(HashMap::new()),
        };
        let this = if d.is_static { None } else { Some(recv) };
        let name = match owner {
            Some(c) => format!("{}.{}", c.name, d.name),
            None => d.name.clone(),
        };
        if d.is_async {
            return Ok(self.spawn_async(d.clone(), env, this, owner.cloned(), module, args, span, generics, name));
        }
        self.invoke(d, &env, this, owner.cloned(), &module, args, span, generics, false, &name)
    }

    pub fn call_bound(&mut self, b: &Arc<BoundMethod>, args: Vec<Value>, span: Span) -> R<Value> {
        let first = b.overloads.first().map(|(c, _)| c.clone());
        let fr = match &first {
            Some(c) => Frame::new(c.env.clone(), &c.module_name),
            None => Frame::new(self.interp.root.clone(), "<native>"),
        };
        let (owner, d) = self.select_overload(&b.overloads, &args, &fr, &b.name, span)?;
        self.invoke_method(b.recv.clone(), Some(&owner), &d, args, span)
    }

    fn accessible_overloads(&mut self, cands: Vec<(Arc<Class>, Arc<FuncDecl>)>, fr: &Frame, what: &str, name: &str) -> R<Vec<(Arc<Class>, Arc<FuncDecl>)>> {
        let ok: Vec<_> = cands.iter().filter(|(o, d)| self.can_access(d.access, o, fr)).cloned().collect();
        if ok.is_empty() && !cands.is_empty() {
            let acc = cands[0].1.access.as_str();
            return self.err("AccessError", format!("cannot call {} {} `{}` of class {} from here", acc, what, name, cands[0].0.name));
        }
        Ok(ok)
    }

    pub fn call_member(&mut self, recv: Value, name: &str, args: Vec<Value>, fr: &Frame, span: Span) -> R<Value> {
        match &recv {
            Value::Object(o) => {
                let cands = self.find_methods(&o.class, name);
                if !cands.is_empty() {
                    let ok = self.accessible_overloads(cands, fr, "method", name)?;
                    let (owner, d) = self.select_overload(&ok, &args, fr, name, span)?;
                    return self.invoke_method(recv.clone(), Some(&owner), &d, args, span);
                }
                let fv = lk(&o.fields).get(name).cloned();
                if let Some(f) = fv {
                    if f.is_callable() {
                        if let Some((owner, fd)) = self.find_field(&o.class, name) {
                            if !self.can_access(fd.access, &owner, fr) {
                                return self.err("AccessError", format!("cannot access {} field `{}` of class {}", fd.access.as_str(), name, owner.name));
                            }
                        }
                        return self.call_value(&f, args, span);
                    }
                }
                if let Some(r) = self.object_builtin(&recv, name, &args) {
                    return r;
                }
                self.no_member(&recv, name)
            }
            Value::Class(c) => {
                let cands: Vec<_> = self.find_methods(c, name).into_iter().filter(|(_, d)| d.is_static).collect();
                if !cands.is_empty() {
                    let ok = self.accessible_overloads(cands, fr, "static method", name)?;
                    let (owner, d) = self.select_overload(&ok, &args, fr, name, span)?;
                    return self.invoke_method(recv.clone(), Some(&owner), &d, args, span);
                }
                let sv = lk(&c.statics).get(name).cloned();
                if let Some(f) = sv {
                    if f.is_callable() {
                        return self.call_value(&f, args, span);
                    }
                }
                if c.kind == ClassKind::Enum {
                    return self.enum_static(c, name, &args);
                }
                self.no_member(&recv, name)
            }
            Value::Module(m) => {
                let f = self.module_member(m, name)?;
                self.call_value(&f, args, span)
            }
            Value::Map(m) => {
                let f = lk(m).get(name).cloned();
                if let Some(f) = f {
                    if f.is_callable() {
                        return self.call_value(&f, args, span);
                    }
                }
                crate::builtins::call_builtin(self, &recv, name, &args)
            }
            Value::Null => self.err_at(span, "TypeError", format!("cannot call method `{}` on null", name)),
            _ => crate::builtins::call_builtin(self, &recv, name, &args),
        }
    }

    fn enum_static(&mut self, c: &Arc<Class>, name: &str, args: &[Value]) -> R<Value> {
        match name {
            "values" => Ok(Value::list(lk(&c.enum_values).clone())),
            "from_name" | "from_value" => {
                let a = match args.first() {
                    Some(a) => a.clone(),
                    None => return self.err("TypeError", format!("{}() expects 1 argument", name)),
                };
                let key = if name == "from_name" { "name" } else { "value" };
                for v in lk(&c.enum_values).clone() {
                    if let Value::Object(o) = &v {
                        if lk(&o.fields).get(key).map_or(false, |x| values_equal(x, &a)) {
                            return Ok(v);
                        }
                    }
                }
                self.err("ValueError", format!("{} has no variant with {} {}", c.name, key, repr(&a)))
            }
            _ => self.no_member(&Value::Class(c.clone()), name),
        }
    }

    pub fn module_member(&mut self, m: &Arc<Module>, name: &str) -> R<Value> {
        let found = match &m.kind {
            ModuleKind::Native(map) => map.get(name).cloned(),
            ModuleKind::Script { env, .. } => env.get_local(name),
        };
        match found {
            Some(v) => Ok(v),
            None => {
                let names: Vec<String> = match &m.kind {
                    ModuleKind::Native(map) => map.keys().cloned().collect(),
                    ModuleKind::Script { env, .. } => env.local_names(),
                };
                let mut msg = format!("module `{}` has no member `{}`", m.name, name);
                if let Some(s) = suggest(name, names.iter().map(|s| s.as_str())) {
                    msg.push_str(&format!(" (did you mean `{}`?)", s));
                }
                self.err("NameError", msg)
            }
        }
    }

    fn no_member<T>(&mut self, recv: &Value, name: &str) -> R<T> {
        let mut names: Vec<String> = vec![];
        match recv {
            Value::Object(o) => {
                for (_, f) in self.all_fields(&o.class) {
                    names.push(f.name);
                }
                for c in o.class.chain() {
                    names.extend(c.methods.keys().cloned());
                }
            }
            Value::Class(c) => {
                names.extend(lk(&c.statics).keys().cloned());
                for ch in c.chain() {
                    names.extend(ch.methods.keys().cloned());
                }
            }
            _ => {}
        }
        let tn = match recv {
            Value::Class(c) => format!("class {}", c.name),
            other => other.type_name(),
        };
        let mut msg = format!("{} has no member `{}`", tn, name);
        if let Some(s) = suggest(name, names.iter().map(|s| s.as_str())) {
            msg.push_str(&format!(" (did you mean `{}`?)", s));
        }
        self.err("NameError", msg)
    }

    /// `to_string`, `equals`, `hash` available on every object.
    pub fn object_builtin(&mut self, recv: &Value, name: &str, args: &[Value]) -> Option<R<Value>> {
        let o = match recv {
            Value::Object(o) => o,
            _ => return None,
        };
        match (name, args.len()) {
            ("to_string", 0) => Some(Ok(Value::str(repr(recv)))),
            ("equals", 1) => Some(Ok(Value::Bool(values_equal(recv, &args[0])))),
            ("hash", 0) => Some(Ok(Value::Int(o.id as i64))),
            _ => None,
        }
    }

    pub fn get_member(&mut self, o: &Value, name: &str, fr: &Frame) -> R<Value> {
        match o {
            Value::Object(obj) => {
                if let Some((owner, fd)) = self.find_field(&obj.class, name) {
                    if !self.can_access(fd.access, &owner, fr) {
                        return self.err("AccessError", format!("cannot access {} field `{}` of class {}", fd.access.as_str(), name, owner.name));
                    }
                    if fd.is_static {
                        return Ok(lk(&owner.statics).get(name).cloned().unwrap_or(Value::Null));
                    }
                    return Ok(lk(&obj.fields).get(name).cloned().unwrap_or(Value::Null));
                }
                let cands = self.find_methods(&obj.class, name);
                if !cands.is_empty() {
                    let ok = self.accessible_overloads(cands, fr, "method", name)?;
                    return Ok(Value::Bound(Arc::new(BoundMethod { recv: o.clone(), name: name.to_string(), overloads: ok })));
                }
                let v = lk(&obj.fields).get(name).cloned();
                if let Some(v) = v {
                    return Ok(v);
                }
                if matches!(name, "to_string" | "equals" | "hash") {
                    let (r, n) = (o.clone(), name.to_string());
                    return Ok(Native::value(name, 0, Some(1), move |ex, a| ex.object_builtin(&r, &n, a).unwrap_or(Ok(Value::Null))));
                }
                self.no_member(o, name)
            }
            Value::Class(c) => {
                if let Some((owner, fd)) = self.find_field(c, name) {
                    if fd.is_static {
                        if !self.can_access(fd.access, &owner, fr) {
                            return self.err("AccessError", format!("cannot access {} static field `{}` of class {}", fd.access.as_str(), name, owner.name));
                        }
                        return Ok(lk(&owner.statics).get(name).cloned().unwrap_or(Value::Null));
                    }
                }
                let sv = lk(&c.statics).get(name).cloned();
                if let Some(v) = sv {
                    return Ok(v);
                }
                let cands: Vec<_> = self.find_methods(c, name).into_iter().filter(|(_, d)| d.is_static).collect();
                if !cands.is_empty() {
                    let ok = self.accessible_overloads(cands, fr, "static method", name)?;
                    return Ok(Value::Bound(Arc::new(BoundMethod { recv: o.clone(), name: name.to_string(), overloads: ok })));
                }
                if name == "name" {
                    return Ok(Value::str(&c.name));
                }
                if c.kind == ClassKind::Enum && matches!(name, "values" | "from_name" | "from_value") {
                    let (cc, n) = (c.clone(), name.to_string());
                    return Ok(Native::value(name, 0, Some(1), move |ex, a| ex.enum_static(&cc, &n, a)));
                }
                self.no_member(o, name)
            }
            Value::Module(m) => self.module_member(m, name),
            Value::Map(m) => {
                let v = lk(m).get(name).cloned();
                match v {
                    Some(v) => Ok(v),
                    None => Ok(crate::builtins::bound_builtin(o, name).unwrap_or(Value::Null)),
                }
            }
            Value::Null => self.err("TypeError", format!("cannot read `{}` of null (use `?.` to propagate null)", name)),
            Value::Task(t) => match name {
                "name" => Ok(Value::str(&t.name)),
                "done" => Ok(Value::Bool(lk(&t.state).is_some())),
                _ => crate::builtins::bound_builtin(o, name).map_or_else(|| self.no_member(o, name), Ok),
            },
            _ => match crate::builtins::bound_builtin(o, name) {
                Some(v) => Ok(v),
                None => self.no_member(o, name),
            },
        }
    }

    pub fn set_member(&mut self, o: &Value, name: &str, v: Value, fr: &Frame) -> R<()> {
        match o {
            Value::Object(obj) => {
                match self.find_field(&obj.class, name) {
                    Some((owner, fd)) => {
                        if !self.can_access(fd.access, &owner, fr) {
                            return self.err("AccessError", format!("cannot modify {} field `{}` of class {}", fd.access.as_str(), name, owner.name));
                        }
                        if fd.readonly {
                            let in_own_ctor = fr.in_ctor && matches!(&fr.this, Some(Value::Object(t)) if Arc::ptr_eq(t, obj));
                            if !in_own_ctor {
                                return self.err("StateError", format!("field `{}` of {} is readonly", name, owner.name));
                            }
                        }
                        let mut v = v;
                        if let Some(t) = &fd.ty {
                            let gf = Frame { env: owner.env.clone(), this: None, class: Some(owner.clone()), in_ctor: false, generics: self.obj_generics(obj), fname: Arc::from("<set>"), module: Arc::from(owner.module_name.as_str()) };
                            v = self.coerce_to(v, t, &gf, &format!("field `{}.{}`", owner.name, name))?;
                        }
                        if fd.is_static {
                            lk(&owner.statics).insert(name.to_string(), v);
                        } else {
                            lk(&obj.fields).insert(name.to_string(), v);
                        }
                        Ok(())
                    }
                    None => {
                        if obj.class.kind == ClassKind::Enum {
                            return self.err("StateError", "enum values are immutable");
                        }
                        let has = lk(&obj.fields).contains_key(name);
                        if has {
                            lk(&obj.fields).insert(name.to_string(), v);
                            Ok(())
                        } else {
                            self.no_member(o, name)
                        }
                    }
                }
            }
            Value::Class(c) => match self.find_field(c, name) {
                Some((owner, fd)) if fd.is_static => {
                    if !self.can_access(fd.access, &owner, fr) {
                        return self.err("AccessError", format!("cannot modify {} static field `{}`", fd.access.as_str(), name));
                    }
                    if fd.readonly {
                        return self.err("StateError", format!("static field `{}` is readonly", name));
                    }
                    let mut v = v;
                    if let Some(t) = &fd.ty {
                        let gf = Frame::new(owner.env.clone(), &owner.module_name);
                        v = self.coerce_to(v, t, &gf, &format!("static field `{}.{}`", owner.name, name))?;
                    }
                    lk(&owner.statics).insert(name.to_string(), v);
                    Ok(())
                }
                _ => self.err("StateError", format!("class {} has no static field `{}`", c.name, name)),
            },
            Value::Map(m) => {
                lk(m).insert(name.to_string(), v);
                Ok(())
            }
            Value::Null => self.err("TypeError", format!("cannot set `{}` on null", name)),
            other => self.err("TypeError", format!("cannot set member `{}` on {}", name, other.type_name())),
        }
    }

    // ───────── instantiation ─────────

    pub fn new_object(&mut self, ty: &TypeExpr, args: Vec<Value>, fr: &Frame, span: Span) -> R<Value> {
        let (path, targs) = match &ty.kind {
            TypeExprKind::Named { path, args } => (path.clone(), args.clone()),
            _ => return self.err_at(span, "TypeError", "`new` needs a class name"),
        };
        let cls = match self.lookup_class(&path, &fr.env) {
            Some(c) => c,
            None => {
                let names = fr.env.all_names();
                let mut m = format!("unknown class `{}`", path.join("."));
                if let Some(s) = suggest(&path[0], names.iter().map(|s| s.as_str())) {
                    m.push_str(&format!(" (did you mean `{}`?)", s));
                }
                return self.err_at(span, "NameError", m);
            }
        };
        // resolve generic names used as type arguments through the current frame
        let targs: Vec<TypeExpr> = targs.into_iter().map(|t| self.subst(&t, fr)).collect();
        self.instantiate(&cls, targs, args, fr, span)
    }

    pub fn subst(&self, t: &TypeExpr, fr: &Frame) -> TypeExpr {
        match &t.kind {
            TypeExprKind::Named { path, args } if path.len() == 1 && args.is_empty() => fr.generics.get(&path[0]).cloned().unwrap_or_else(|| t.clone()),
            TypeExprKind::Named { path, args } => TypeExpr::new(TypeExprKind::Named { path: path.clone(), args: args.iter().map(|a| self.subst(a, fr)).collect() }, t.span),
            _ => t.clone(),
        }
    }

    pub fn instantiate(&mut self, cls: &Arc<Class>, mut targs: Vec<TypeExpr>, args: Vec<Value>, fr: &Frame, span: Span) -> R<Value> {
        match cls.kind {
            ClassKind::Interface => return self.err_at(span, "TypeError", format!("cannot instantiate interface {}", cls.name)),
            ClassKind::Enum => return self.err_at(span, "TypeError", format!("cannot instantiate enum {}", cls.name)),
            ClassKind::Class => {}
        }
        if cls.is_abstract {
            return self.err_at(span, "TypeError", format!("cannot instantiate abstract class {}", cls.name));
        }
        if !targs.is_empty() && targs.len() != cls.generics.len() {
            return self.err_at(span, "TypeError", format!("class {} takes {} type argument(s), got {}", cls.name, cls.generics.len(), targs.len()));
        }
        for (g, a) in cls.generics.iter().zip(targs.iter()) {
            if let (Some(b), TypeExprKind::Named { path, .. }) = (&g.bound, &a.kind) {
                if let TypeExprKind::Named { path: bp, .. } = &b.kind {
                    if let (Some(bc), Some(ac)) = (self.lookup_class(bp, &cls.env), self.lookup_class(path, &fr.env)) {
                        if !ac.is_subclass_of(&bc) {
                            return self.err_at(span, "TypeError", format!("type argument {} does not satisfy bound {} of {}", a.display(), b.display(), g.name));
                        }
                    }
                }
            }
        }
        if targs.is_empty() && !cls.generics.is_empty() {
            targs = self.infer_generics(cls, &args, fr);
        }
        let obj = Arc::new(Object { id: self.interp.fresh_id(), class: cls.clone(), fields: Mutex::new(IndexMap::new()), type_args: targs });
        self.run_ctor(cls, &obj, args, span)?;
        Ok(Value::Object(obj))
    }

    fn effective_ctors(&self, cls: &Arc<Class>) -> Vec<(Arc<Class>, Arc<FuncDecl>)> {
        for c in cls.chain() {
            if !c.ctors.is_empty() {
                return c.ctors.iter().map(|d| (c.clone(), d.clone())).collect();
            }
        }
        vec![]
    }

    fn infer_generics(&mut self, cls: &Arc<Class>, args: &[Value], fr: &Frame) -> Vec<TypeExpr> {
        let mut inferred: HashMap<String, TypeExpr> = HashMap::new();
        let ctors = self.effective_ctors(cls);
        let pick = ctors.iter().find(|(_, d)| arity_ok(d, args.len()) && d.params.len() == args.len());
        if let Some((_, d)) = pick {
            for (p, a) in d.params.iter().zip(args.iter()) {
                if let Some(TypeExpr { kind: TypeExprKind::Named { path, args: ta }, .. }) = &p.ty {
                    if path.len() == 1 && ta.is_empty() && cls.generics.iter().any(|g| g.name == path[0]) && !matches!(a, Value::Null) {
                        inferred.entry(path[0].clone()).or_insert_with(|| value_type_expr(a));
                    }
                }
            }
        }
        let _ = fr;
        cls.generics.iter().map(|g| inferred.get(&g.name).cloned().unwrap_or_else(|| TypeExpr::named("any", Span::DUMMY))).collect()
    }

    fn run_ctor(&mut self, cls: &Arc<Class>, obj: &Arc<Object>, args: Vec<Value>, span: Span) -> R<()> {
        if cls.ctors.is_empty() {
            if let Some(p) = &cls.parent {
                self.run_ctor(p, obj, args, span)?;
            } else if !args.is_empty() {
                return self.err_at(span, "TypeError", format!("class {} has no constructor but got {} argument(s)", cls.name, args.len()));
            }
            return self.init_fields(cls, obj);
        }
        let cands: Vec<(Arc<Class>, Arc<FuncDecl>)> = cls.ctors.iter().map(|d| (cls.clone(), d.clone())).collect();
        let fr = Frame { env: cls.env.clone(), this: None, class: Some(cls.clone()), in_ctor: true, generics: self.obj_generics(obj), fname: Arc::from("constructor"), module: Arc::from(cls.module_name.as_str()) };
        let (_, d) = self.select_overload(&cands, &args, &fr, &format!("{}.constructor", cls.name), span)?;
        let explicit_super = matches!(d.body.as_ref().and_then(|b| b.first()).map(|s| &s.kind), Some(StmtKind::SuperCall(_)));
        if !explicit_super {
            if let Some(p) = &cls.parent {
                self.run_ctor(p, obj, vec![], span)?;
            }
            self.init_fields(cls, obj)?;
        }
        let name = format!("{}.constructor", cls.name);
        let module: Arc<str> = Arc::from(cls.module_name.as_str());
        self.invoke(&d, &cls.env, Some(Value::Object(obj.clone())), Some(cls.clone()), &module, args, span, self.obj_generics(obj), true, &name)?;
        Ok(())
    }

    fn init_fields(&mut self, cls: &Arc<Class>, obj: &Arc<Object>) -> R<()> {
        let d = match &cls.decl {
            Some(d) => d.clone(),
            None => return Ok(()),
        };
        let fr = Frame { env: Scope::new(Some(cls.env.clone())), this: Some(Value::Object(obj.clone())), class: Some(cls.clone()), in_ctor: true, generics: self.obj_generics(obj), fname: Arc::from("<init>"), module: Arc::from(cls.module_name.as_str()) };
        for f in d.fields.iter().filter(|f| !f.is_static) {
            let mut v = match &f.default {
                Some(e) => self.eval(e, &fr)?,
                None => Value::Null,
            };
            if let (Some(t), Some(_)) = (&f.ty, &f.default) {
                v = self.coerce_to(v, t, &fr, &format!("field `{}.{}`", cls.name, f.name))?;
            }
            lk(&obj.fields).insert(f.name.clone(), v);
        }
        Ok(())
    }

    pub fn super_ctor(&mut self, args: Vec<Value>, fr: &Frame, span: Span) -> R<()> {
        let (cls, obj) = match (&fr.class, &fr.this) {
            (Some(c), Some(Value::Object(o))) if fr.in_ctor => (c.clone(), o.clone()),
            _ => return self.err_at(span, "StateError", "`super(...)` is only valid inside a constructor"),
        };
        match &cls.parent {
            Some(p) => self.run_ctor(p, &obj, args, span)?,
            None => {
                if !args.is_empty() {
                    return self.err_at(span, "StateError", format!("class {} has no parent constructor", cls.name));
                }
            }
        }
        self.init_fields(&cls, &obj)
    }

    pub fn call_super(&mut self, method: &str, args: Vec<Value>, fr: &Frame, span: Span) -> R<Value> {
        let (cls, this) = match (&fr.class, &fr.this) {
            (Some(c), Some(t)) => (c.clone(), t.clone()),
            _ => return self.err_at(span, "StateError", "`super` is only valid inside a method"),
        };
        let mut cands = vec![];
        if let Some(p) = &cls.parent {
            cands = self.find_methods(p, method).into_iter().filter(|(_, d)| d.body.is_some()).collect();
        }
        if cands.is_empty() {
            for i in &cls.interfaces {
                cands.extend(self.find_methods(i, method).into_iter().filter(|(_, d)| d.body.is_some()));
            }
        }
        if cands.is_empty() {
            if let Some(r) = self.object_builtin(&this, method, &args) {
                return r;
            }
            return self.err_at(span, "NameError", format!("parent of {} has no method `{}`", cls.name, method));
        }
        let ok = self.accessible_overloads(cands, fr, "method", method)?;
        let (owner, d) = self.select_overload(&ok, &args, fr, method, span)?;
        self.invoke_method(this, Some(&owner), &d, args, span)
    }
}
