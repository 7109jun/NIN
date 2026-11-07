//! Lowering: validated JSON document → typed AST (`Program`).
//!
//! Value positions use `${}` templates (`parse_template`), expression positions parse
//! the string as an expression (`parse_expression`); see docs/SPEC.md §2.
//! The input must already have passed `nin_schema::validate`, but this pass is
//! defensive and never panics: structural surprises become diagnostics.

use crate::expr::{parse_expression, parse_template};
use crate::types::parse_type;
use crate::*;
use nin_diag::{Diagnostic, Diagnostics, Span};
use nin_syntax::{Json, JsonKind};
use std::sync::Arc;

/// Parse + validate + lower a `.nin` source. Warnings are dropped; use
/// [`parse_program_diag`] to keep them.
pub fn parse_program(src: &str, file: u32) -> Result<Program, Diagnostics> {
    let (p, ds) = parse_program_diag(src, file);
    match p {
        Some(p) if !ds.has_errors() => Ok(p),
        _ => Err(ds),
    }
}

/// Like [`parse_program`] but returns all diagnostics (errors and warnings).
pub fn parse_program_diag(src: &str, file: u32) -> (Option<Program>, Diagnostics) {
    let doc = match nin_syntax::parse_script(src, file) {
        Ok(d) => d,
        Err(d) => return (None, Diagnostics { items: vec![d] }),
    };
    let mut ds = nin_schema::validate(&doc);
    if ds.has_errors() {
        return (None, ds);
    }
    match lower(&doc, file) {
        Ok(p) => (Some(p), ds),
        Err(more) => {
            ds.extend(more.items);
            (None, ds)
        }
    }
}

/// Lower an already validated document.
pub fn lower(doc: &Json, file: u32) -> Result<Program, Diagnostics> {
    let mut l = L { ds: Diagnostics::new(), file };
    let p = l.program(doc);
    if l.ds.has_errors() {
        Err(l.ds)
    } else {
        Ok(p)
    }
}

struct L {
    ds: Diagnostics,
    file: u32,
}

#[derive(Clone, Copy, PartialEq)]
enum FKind {
    Func,
    Method,
    Ctor,
    IfaceMethod,
}

const OPS: &[(&str, BinOp)] = &[
    ("+", BinOp::Add),
    ("-", BinOp::Sub),
    ("*", BinOp::Mul),
    ("/", BinOp::Div),
    ("%", BinOp::Mod),
    ("**", BinOp::Pow),
    ("??", BinOp::Coalesce),
];

fn sub_base(span: Span, whole_len: usize, off: usize, len: usize) -> Span {
    if !span.is_dummy() && span.len() == whole_len + 2 {
        Span { file: span.file, start: span.start + off as u32, end: span.start + (off + len + 2) as u32 }
    } else {
        span
    }
}

impl L {
    fn err(&mut self, code: &'static str, msg: impl Into<String>, span: Span, label: &str) {
        self.ds.push(Diagnostic::error(code, msg).at(span, label));
    }

    fn str_of(j: &Json) -> String {
        j.as_str().unwrap_or("").to_string()
    }

    fn get_str(j: &Json, k: &str) -> Option<String> {
        j.get(k).and_then(|v| v.as_str()).map(|s| s.to_string())
    }

    fn get_bool(j: &Json, k: &str) -> bool {
        j.get(k).and_then(|v| v.as_bool()).unwrap_or(false)
    }

    // ───────── values / expressions / types ─────────

    fn value(&mut self, j: &Json) -> Expr {
        let span = j.span;
        match &j.kind {
            JsonKind::Null => Expr::null(span),
            JsonKind::Bool(b) => Expr::new(ExprKind::Bool(*b), span),
            JsonKind::Int(i) => Expr::new(ExprKind::Int(*i), span),
            JsonKind::Float(f) => Expr::new(ExprKind::Float(*f), span),
            JsonKind::Str(s) => match parse_template(s, span) {
                Ok(e) => e,
                Err(d) => {
                    self.ds.push(d);
                    Expr::str(s.clone(), span)
                }
            },
            JsonKind::Array(a) => {
                let items = a.iter().map(|x| self.value(x)).collect();
                Expr::new(ExprKind::List(items), span)
            }
            JsonKind::Object(ms) => {
                let mut entries = Vec::new();
                for m in ms {
                    let v = self.value(&m.value);
                    entries.push((Expr::str(m.key.clone(), m.key_span), v));
                }
                Expr::new(ExprKind::Map(entries), span)
            }
        }
    }

    fn opt_value(&mut self, j: &Json, k: &str) -> Option<Expr> {
        j.get(k).map(|v| self.value(v))
    }

    fn cond(&mut self, j: &Json) -> Expr {
        let span = j.span;
        match &j.kind {
            JsonKind::Str(s) => {
                let t = s.trim();
                if t.starts_with("${") && t.ends_with('}') {
                    if let Ok(e) = parse_template(s, span) {
                        if let ExprKind::Template(parts) = &e.kind {
                            if parts.len() == 1 {
                                if let TemplatePart::Expr(inner) = &parts[0] {
                                    return inner.clone();
                                }
                            }
                        }
                    }
                }
                match parse_expression(s, span) {
                    Ok(e) => e,
                    Err(d) => {
                        self.ds.push(d);
                        Expr::null(span)
                    }
                }
            }
            JsonKind::Bool(b) => Expr::new(ExprKind::Bool(*b), span),
            JsonKind::Int(i) => Expr::new(ExprKind::Int(*i), span),
            JsonKind::Float(f) => Expr::new(ExprKind::Float(*f), span),
            _ => {
                self.err("E0206", format!("expected an expression, found {}", j.type_name()), span, "expression strings look like \"x > 3\"");
                Expr::null(span)
            }
        }
    }

    fn opt_cond(&mut self, j: &Json, k: &str) -> Option<Expr> {
        j.get(k).map(|v| self.cond(v))
    }

    fn type_of(&mut self, j: &Json) -> Option<TypeExpr> {
        let s = j.as_str()?;
        match parse_type(s, j.span) {
            Ok(t) => Some(t),
            Err(d) => {
                self.ds.push(d);
                None
            }
        }
    }

    fn opt_type(&mut self, j: &Json, k: &str) -> Option<TypeExpr> {
        j.get(k).and_then(|v| self.type_of(v))
    }

    fn type_text(&mut self, text: &str, base: Span) -> Option<TypeExpr> {
        match parse_type(text, base) {
            Ok(t) => Some(t),
            Err(d) => {
                self.ds.push(d);
                None
            }
        }
    }

    fn kv_map(&mut self, j: Option<&Json>) -> Vec<(String, Expr)> {
        let mut out = Vec::new();
        if let Some(ms) = j.and_then(|x| x.as_object()) {
            for m in ms {
                let v = self.value(&m.value);
                out.push((m.key.clone(), v));
            }
        }
        out
    }

    // ───────── declarations ─────────

    fn generics(&mut self, j: Option<&Json>) -> Vec<GenericParam> {
        let mut out = Vec::new();
        if let Some(items) = j.and_then(|x| x.as_array()) {
            for it in items {
                if let Some(s) = it.as_str() {
                    match s.split_once(':') {
                        Some((n, b)) => {
                            let off = s.find(':').unwrap_or(0) + 1;
                            let bound_text = b.trim();
                            let lead = b.len() - b.trim_start().len();
                            let bound = self.type_text(bound_text, sub_base(it.span, s.len(), off + lead, bound_text.len()));
                            out.push(GenericParam { name: n.trim().to_string(), bound });
                        }
                        None => out.push(GenericParam { name: s.trim().to_string(), bound: None }),
                    }
                }
            }
        }
        out
    }

    fn param_from_str(&mut self, s: &str, span: Span) -> Param {
        let (variadic, rest_off) = if s.trim_start().starts_with("...") { (true, s.find("...").unwrap_or(0) + 3) } else { (false, 0) };
        let body = &s[rest_off..];
        let name_end = body.find(|c| c == ':' || c == '=').unwrap_or(body.len());
        let name = body[..name_end].trim().to_string();
        let mut ty = None;
        let mut default = None;
        let mut cursor = rest_off + name_end;
        if body[name_end..].starts_with(':') {
            let after = &s[cursor + 1..];
            let tend = after.find('=').unwrap_or(after.len());
            let ttext = after[..tend].trim();
            let lead = after[..tend].len() - after[..tend].trim_start().len();
            ty = self.type_text(ttext, sub_base(span, s.len(), cursor + 1 + lead, ttext.len()));
            cursor += 1 + tend;
        }
        if s[cursor..].starts_with('=') {
            let after = &s[cursor + 1..];
            let dtext = after.trim();
            let lead = after.len() - after.trim_start().len();
            match parse_expression(dtext, sub_base(span, s.len(), cursor + 1 + lead, dtext.len())) {
                Ok(e) => default = Some(e),
                Err(d) => self.ds.push(d),
            }
        }
        Param { name, ty, default, variadic, span }
    }

    fn params(&mut self, j: Option<&Json>) -> Vec<Param> {
        let mut out = Vec::new();
        if let Some(items) = j.and_then(|x| x.as_array()) {
            for it in items {
                match &it.kind {
                    JsonKind::Str(s) => out.push(self.param_from_str(s, it.span)),
                    JsonKind::Object(_) => {
                        let name = Self::get_str(it, "name").unwrap_or_default();
                        let ty = self.opt_type(it, "type");
                        let default = self.opt_cond(it, "default");
                        out.push(Param { name, ty, default, variadic: Self::get_bool(it, "variadic"), span: it.span });
                    }
                    _ => {}
                }
            }
        }
        out
    }

    fn access(&mut self, j: &Json) -> Access {
        match j.get("access").and_then(|a| a.as_str()) {
            Some("private") => Access::Private,
            Some("protected") => Access::Protected,
            _ => Access::Public,
        }
    }

    fn func(&mut self, name: &str, j: &Json, kind: FKind) -> FuncDecl {
        let body = j.get("body").map(|b| self.block(b));
        let is_abstract = Self::get_bool(j, "abstract") || (kind == FKind::IfaceMethod);
        FuncDecl {
            name: name.to_string(),
            generics: self.generics(j.get("generics")),
            params: self.params(j.get("params")),
            ret: self.opt_type(j, "returns"),
            body: if kind == FKind::IfaceMethod { None } else { body },
            is_async: Self::get_bool(j, "async"),
            is_static: kind == FKind::Method && Self::get_bool(j, "static"),
            is_abstract: is_abstract && kind != FKind::Ctor && kind != FKind::Func,
            is_override: Self::get_bool(j, "override"),
            is_final: Self::get_bool(j, "final"),
            access: if kind == FKind::Method || kind == FKind::Ctor { self.access(j) } else { Access::Public },
            doc: Self::get_str(j, "doc"),
            span: j.span,
        }
    }

    fn methods(&mut self, j: Option<&Json>, kind: FKind) -> Vec<Arc<FuncDecl>> {
        let mut out = Vec::new();
        if let Some(ms) = j.and_then(|x| x.as_object()) {
            for m in ms {
                match &m.value.kind {
                    JsonKind::Array(items) => {
                        for it in items {
                            out.push(Arc::new(self.func(&m.key, it, kind)));
                        }
                    }
                    _ => out.push(Arc::new(self.func(&m.key, &m.value, kind))),
                }
            }
        }
        out
    }

    fn class(&mut self, name: &str, j: &Json) -> ClassDecl {
        let mut fields = Vec::new();
        if let Some(ms) = j.get("fields").and_then(|x| x.as_object()) {
            for m in ms {
                let f = match &m.value.kind {
                    JsonKind::Str(_) => FieldDecl { name: m.key.clone(), ty: self.type_of(&m.value), default: None, access: Access::Public, is_static: false, readonly: false, span: m.value.span },
                    _ => {
                        let v = &m.value;
                        let ty = self.opt_type(v, "type");
                        let default = self.opt_value(v, "default");
                        let access = self.access(v);
                        FieldDecl { name: m.key.clone(), ty, default, access, is_static: Self::get_bool(v, "static"), readonly: Self::get_bool(v, "readonly"), span: v.span }
                    }
                };
                fields.push(f);
            }
        }
        let mut constructors = Vec::new();
        if let Some(c) = j.get("constructor") {
            constructors.push(Arc::new(self.func("constructor", c, FKind::Ctor)));
        }
        if let Some(items) = j.get("constructors").and_then(|x| x.as_array()) {
            for it in items {
                constructors.push(Arc::new(self.func("constructor", it, FKind::Ctor)));
            }
        }
        let extends = j.get("extends").and_then(|e| self.type_of(e));
        let mut implements = Vec::new();
        if let Some(items) = j.get("implements").and_then(|x| x.as_array()) {
            for it in items {
                if let Some(t) = self.type_of(it) {
                    implements.push(t);
                }
            }
        }
        ClassDecl {
            name: name.to_string(),
            generics: self.generics(j.get("generics")),
            extends,
            implements,
            is_abstract: Self::get_bool(j, "abstract"),
            is_final: Self::get_bool(j, "final"),
            fields,
            constructors,
            methods: self.methods(j.get("methods"), FKind::Method),
            doc: Self::get_str(j, "doc"),
            span: j.span,
        }
    }

    fn interface(&mut self, name: &str, j: &Json) -> InterfaceDecl {
        let mut extends = Vec::new();
        match j.get("extends") {
            Some(e) if e.as_str().is_some() => {
                if let Some(t) = self.type_of(e) {
                    extends.push(t);
                }
            }
            Some(e) => {
                if let Some(items) = e.as_array() {
                    for it in items {
                        if let Some(t) = self.type_of(it) {
                            extends.push(t);
                        }
                    }
                }
            }
            None => {}
        }
        InterfaceDecl { name: name.to_string(), generics: self.generics(j.get("generics")), extends, methods: self.methods(j.get("methods"), FKind::IfaceMethod), doc: Self::get_str(j, "doc"), span: j.span }
    }

    fn import(&mut self, j: &Json) -> Option<Import> {
        let (kind, raw) = if let Some(u) = Self::get_str(j, "use") {
            (ImportKind::Std(u.clone()), u)
        } else if let Some(f) = Self::get_str(j, "file") {
            (ImportKind::File(f.clone()), f)
        } else if let Some(p) = Self::get_str(j, "package") {
            (ImportKind::Package(p.clone()), p)
        } else {
            return None;
        };
        let alias = Self::get_str(j, "as").unwrap_or_else(|| match &kind {
            ImportKind::File(f) => {
                let name = f.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(f);
                name.strip_suffix(".nin").unwrap_or(name).to_string()
            }
            _ => raw.rsplit(|c| c == '.' || c == ':' || c == '/' || c == '\\').next().unwrap_or(&raw).trim_end_matches(".dll").to_string(),
        });
        if !nin_schema::is_identifier(&alias) {
            self.err("E0106", format!("import alias `{}` is not a valid identifier", alias), j.span, "add an explicit `\"as\": \"name\"`");
        }
        Some(Import { kind, alias, span: j.span })
    }

    // ───────── statements ─────────

    fn block(&mut self, j: &Json) -> Block {
        let mut out = Vec::new();
        if let Some(items) = j.as_array() {
            for it in items {
                if let Some(s) = self.stmt(it) {
                    out.push(s);
                }
            }
        }
        out
    }

    fn opt_block(&mut self, j: &Json, k: &str) -> Block {
        match j.get(k) {
            Some(b) => self.block(b),
            None => Vec::new(),
        }
    }

    fn redirect(&mut self, j: Option<&Json>) -> Option<Redirect> {
        let j = j?;
        match &j.kind {
            JsonKind::Str(s) => Some(match s.as_str() {
                "null" => Redirect::Null,
                "capture" => Redirect::Capture,
                "stdout" => Redirect::Merge,
                _ => Redirect::Inherit,
            }),
            JsonKind::Object(_) => {
                let path = j.get("file").map(|f| self.value(f)).unwrap_or_else(|| Expr::null(j.span));
                Some(Redirect::File { path, append: Self::get_bool(j, "append") })
            }
            _ => None,
        }
    }

    /// `head` is the value of the `run` key. In steps, `env` and `timeout` belong to the step.
    fn run_stmt(&mut self, j: &Json, head: &Json, in_step: bool) -> RunStmt {
        let mut args = Vec::new();
        if let Some(a) = j.get("args").and_then(|x| x.as_array()) {
            for it in a {
                args.push(self.value(it));
            }
        }
        RunStmt {
            program: self.value(head),
            args,
            shell: Self::get_bool(j, "shell"),
            cwd: self.opt_value(j, "cwd"),
            env: if in_step { Vec::new() } else { self.kv_map(j.get("env")) },
            stdin: self.opt_value(j, "stdin"),
            stdin_file: self.opt_value(j, "stdin_file"),
            stdout: self.redirect(j.get("stdout")),
            stderr: self.redirect(j.get("stderr")),
            into: Self::get_str(j, "into"),
            check: Self::get_bool(j, "check"),
            timeout: if in_step { None } else { self.opt_value(j, "timeout") },
            span: j.span,
        }
    }

    fn place(&mut self, j: &Json) -> Expr {
        let e = self.cond(j);
        if !e.is_place() && !matches!(e.kind, ExprKind::Null) {
            self.err("E0205", "invalid assignment target", j.span, "expected a variable, `a.b` or `a[i]`");
        }
        e
    }

    fn stmt(&mut self, j: &Json) -> Option<Stmt> {
        const HEADS: &[&str] = &[
            "let", "const", "set", "call", "print", "eprint", "if", "while", "for", "match", "break", "continue", "return", "throw", "try", "run", "parallel", "spawn", "await", "block", "assert", "output", "super", "exit",
        ];
        let ms = j.as_object()?;
        let head = ms.iter().find(|m| HEADS.contains(&m.key.as_str()))?;
        let hv = &head.value;
        let span = j.span;
        let kind = match head.key.as_str() {
            "let" | "const" => StmtKind::Let { name: Self::str_of(hv), ty: self.opt_type(j, "type"), value: self.opt_value(j, "value"), is_const: head.key == "const" },
            "set" => {
                let target = self.place(hv);
                let op = match j.get("op").and_then(|o| o.as_str()) {
                    Some(o) => OPS.iter().find(|x| x.0 == o).map(|x| x.1),
                    None => None,
                };
                let value = self.opt_value(j, "value").unwrap_or_else(|| Expr::null(span));
                StmtKind::Assign { target, op, value }
            }
            "call" => StmtKind::Expr(self.cond(hv)),
            "print" | "eprint" => {
                let mut value = self.value(hv);
                let mut newline = true;
                if let Some(end) = Self::get_str(j, "end") {
                    if end.is_empty() {
                        newline = false;
                    } else if end != "\n" {
                        newline = false;
                        let sp = value.span;
                        value = Expr::new(ExprKind::Template(vec![TemplatePart::Expr(value), TemplatePart::Text(end)]), sp);
                    }
                }
                StmtKind::Print { value, newline, stderr: head.key == "eprint" }
            }
            "if" => StmtKind::If { cond: self.cond(hv), then: self.opt_block(j, "then"), otherwise: self.opt_block(j, "else") },
            "while" => StmtKind::While { cond: self.cond(hv), body: self.opt_block(j, "do") },
            "for" => {
                let vars = match &hv.kind {
                    JsonKind::Array(a) => a.iter().map(Self::str_of).collect(),
                    _ => vec![Self::str_of(hv)],
                };
                let iter = match j.get("in") {
                    Some(i) => self.cond(i),
                    None => Expr::null(span),
                };
                StmtKind::For { vars, iter, body: self.opt_block(j, "do") }
            }
            "match" => {
                let subject = self.cond(hv);
                let mut cases = Vec::new();
                if let Some(items) = j.get("cases").and_then(|c| c.as_array()) {
                    for c in items {
                        let when = match c.get("when") {
                            Some(w) => match w.as_array() {
                                Some(a) => a.iter().map(|x| self.value(x)).collect(),
                                None => vec![self.value(w)],
                            },
                            None => vec![],
                        };
                        cases.push(MatchCase { when, body: self.opt_block(c, "do") });
                    }
                }
                StmtKind::Match { subject, cases, default: self.opt_block(j, "default") }
            }
            "break" => StmtKind::Break,
            "continue" => StmtKind::Continue,
            "return" => {
                if hv.is_null() {
                    StmtKind::Return(None)
                } else {
                    StmtKind::Return(Some(self.value(hv)))
                }
            }
            "throw" => StmtKind::Throw(self.value(hv)),
            "try" => {
                let body = self.block(hv);
                let mut catches = Vec::new();
                if let Some(c) = j.get("catch") {
                    let cbody = self.block(c);
                    catches.push(CatchClause { ty: self.opt_type(j, "type"), var: Self::get_str(j, "as"), body: cbody, span: c.span });
                }
                if let Some(items) = j.get("catches").and_then(|c| c.as_array()) {
                    for c in items {
                        catches.push(CatchClause { ty: self.opt_type(c, "type"), var: Self::get_str(c, "as"), body: self.opt_block(c, "do"), span: c.span });
                    }
                }
                StmtKind::Try { body, catches, finally: self.opt_block(j, "finally") }
            }
            "run" => StmtKind::Run(Box::new(self.run_stmt(j, hv, false))),
            "parallel" => {
                let mut branches = Vec::new();
                if let Some(items) = hv.as_array() {
                    for b in items {
                        branches.push(self.block(b));
                    }
                }
                StmtKind::Parallel { branches, max: self.opt_value(j, "max") }
            }
            "spawn" => StmtKind::Spawn { body: self.block(hv), name: Self::get_str(j, "as") },
            "await" => StmtKind::Await { task: self.cond(hv), into: Self::get_str(j, "as") },
            "block" => StmtKind::Block(self.block(hv)),
            "assert" => StmtKind::Assert { cond: self.cond(hv), message: self.opt_value(j, "message") },
            "output" => StmtKind::Output { name: Self::str_of(hv), value: self.opt_value(j, "value").unwrap_or_else(|| Expr::null(span)) },
            "super" => {
                let args = match hv.as_array() {
                    Some(a) => a.iter().map(|x| self.value(x)).collect(),
                    None => vec![],
                };
                StmtKind::SuperCall(args)
            }
            "exit" => {
                if hv.is_null() {
                    StmtKind::Exit(None)
                } else {
                    StmtKind::Exit(Some(self.value(hv)))
                }
            }
            _ => return None,
        };
        Some(Stmt::new(kind, span))
    }

    // ───────── workflow ─────────

    fn step(&mut self, j: &Json) -> Step {
        let action = if let Some(r) = j.get("run") {
            StepAction::Run(Box::new(self.run_stmt(j, r, true)))
        } else if let Some(s) = j.get("script") {
            StepAction::Script(self.block(s))
        } else {
            StepAction::Uses { function: Self::get_str(j, "uses").unwrap_or_default(), with: self.kv_map(j.get("with")) }
        };
        Step {
            id: Self::get_str(j, "id"),
            name: self.opt_value(j, "name"),
            cond: self.opt_cond(j, "if"),
            action,
            env: self.kv_map(j.get("env")),
            workdir: self.opt_value(j, "workdir"),
            timeout: self.opt_value(j, "timeout"),
            retries: self.opt_value(j, "retries"),
            retry_delay: self.opt_value(j, "retry_delay"),
            continue_on_error: self.opt_cond(j, "continue_on_error"),
            outputs: self.kv_map(j.get("outputs")),
            on_error: self.opt_block(j, "on_error"),
            span: j.span,
        }
    }

    fn steps(&mut self, j: Option<&Json>) -> Vec<Step> {
        let mut out = Vec::new();
        if let Some(items) = j.and_then(|x| x.as_array()) {
            for it in items {
                out.push(self.step(it));
            }
        }
        out
    }

    fn job(&mut self, id: &str, j: &Json) -> Job {
        let needs = match j.get("needs") {
            Some(n) => match &n.kind {
                JsonKind::Str(s) => vec![s.clone()],
                JsonKind::Array(a) => a.iter().map(Self::str_of).collect(),
                _ => vec![],
            },
            None => vec![],
        };
        let mut matrix = None;
        let mut fail_fast_matrix = true;
        let mut matrix_max = None;
        if let Some(st) = j.get("strategy") {
            fail_fast_matrix = st.get("fail_fast").and_then(|b| b.as_bool()).unwrap_or(true);
            matrix_max = self.opt_value(st, "max_parallel");
            if let Some(m) = st.get("matrix").and_then(|m| m.as_object()) {
                let mut dims = Vec::new();
                let mut include = Vec::new();
                let mut exclude = Vec::new();
                for mem in m {
                    match mem.key.as_str() {
                        "include" | "exclude" => {
                            let list: Vec<Expr> = mem.value.as_array().map(|a| a.iter().map(|x| self.value(x)).collect()).unwrap_or_default();
                            if mem.key == "include" {
                                include = list;
                            } else {
                                exclude = list;
                            }
                        }
                        _ => {
                            let v = self.value(&mem.value);
                            dims.push((mem.key.clone(), v));
                        }
                    }
                }
                matrix = Some(Matrix { dims, include, exclude, fail_fast: fail_fast_matrix, max_parallel: matrix_max.clone() });
            }
        }
        let limits = j.get("limits").map(|l| Limits { memory: self.opt_value(l, "memory"), processes: self.opt_value(l, "processes"), cpu_time: self.opt_value(l, "cpu_time") });
        Job {
            id: id.to_string(),
            name: self.opt_value(j, "name"),
            needs,
            cond: self.opt_cond(j, "if"),
            env: self.kv_map(j.get("env")),
            workdir: self.opt_value(j, "workdir"),
            timeout: self.opt_value(j, "timeout"),
            retries: self.opt_value(j, "retries"),
            retry_delay: self.opt_value(j, "retry_delay"),
            continue_on_error: self.opt_cond(j, "continue_on_error"),
            matrix,
            limits,
            steps: self.steps(j.get("steps")),
            outputs: self.kv_map(j.get("outputs")),
            finally: self.steps(j.get("finally")),
            span: j.span,
        }
    }

    fn workflow(&mut self, j: &Json) -> Workflow {
        let mut inputs = Vec::new();
        if let Some(ms) = j.get("inputs").and_then(|i| i.as_object()) {
            for m in ms {
                let decl = match &m.value.kind {
                    JsonKind::Str(_) => InputDecl { name: m.key.clone(), ty: self.type_of(&m.value), default: None, required: false, description: None },
                    _ => {
                        let v = &m.value;
                        InputDecl {
                            name: m.key.clone(),
                            ty: self.opt_type(v, "type"),
                            default: self.opt_value(v, "default"),
                            required: Self::get_bool(v, "required"),
                            description: Self::get_str(v, "description"),
                        }
                    }
                };
                inputs.push(decl);
            }
        }
        let mut jobs = Vec::new();
        if let Some(ms) = j.get("jobs").and_then(|i| i.as_object()) {
            for m in ms {
                jobs.push(self.job(&m.key, &m.value));
            }
        }
        let defaults = j.get("defaults");
        Workflow {
            name: Self::get_str(j, "name"),
            env: self.kv_map(j.get("env")),
            inputs,
            jobs,
            max_parallel: self.opt_value(j, "max_parallel"),
            default_timeout: defaults.and_then(|d| self.opt_value(d, "timeout")),
            default_retries: defaults.and_then(|d| self.opt_value(d, "retries")),
            span: j.span,
        }
    }

    // ───────── program ─────────

    fn program(&mut self, doc: &Json) -> Program {
        let mut p = Program::empty(self.file);
        p.version = Self::get_str(doc, "nin").unwrap_or_else(|| "1".to_string());
        p.name = Self::get_str(doc, "name");
        p.description = Self::get_str(doc, "description");
        if let Some(items) = doc.get("imports").and_then(|i| i.as_array()) {
            for it in items {
                if let Some(i) = self.import(it) {
                    p.imports.push(i);
                }
            }
        }
        for (key, is_const) in [("const", true), ("vars", false)] {
            if let Some(ms) = doc.get(key).and_then(|c| c.as_object()) {
                for m in ms {
                    let value = self.value(&m.value);
                    p.globals.push(GlobalDecl { name: m.key.clone(), ty: None, value, is_const, span: m.value.span });
                }
            }
        }
        if let Some(ms) = doc.get("enums").and_then(|c| c.as_object()) {
            for m in ms {
                let mut variants = Vec::new();
                match &m.value.kind {
                    JsonKind::Array(a) => {
                        for v in a {
                            variants.push((Self::str_of(v), None));
                        }
                    }
                    JsonKind::Object(o) => {
                        for v in o {
                            let val = self.value(&v.value);
                            variants.push((v.key.clone(), Some(val)));
                        }
                    }
                    _ => {}
                }
                p.enums.push(EnumDecl { name: m.key.clone(), variants, span: m.value.span });
            }
        }
        if let Some(ms) = doc.get("interfaces").and_then(|c| c.as_object()) {
            for m in ms {
                let i = self.interface(&m.key, &m.value);
                p.interfaces.push(i);
            }
        }
        if let Some(ms) = doc.get("classes").and_then(|c| c.as_object()) {
            for m in ms {
                let c = self.class(&m.key, &m.value);
                p.classes.push(c);
            }
        }
        if let Some(ms) = doc.get("functions").and_then(|c| c.as_object()) {
            for m in ms {
                let f = self.func(&m.key, &m.value, FKind::Func);
                p.functions.push(Arc::new(f));
            }
        }
        if let Some(m) = doc.get("main") {
            p.main = Some(self.block(m));
        }
        if let Some(w) = doc.get("workflow") {
            p.workflow = Some(self.workflow(w));
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nin_diag::SourceMap;

    fn prog(src: &str) -> Program {
        match parse_program(src, 0) {
            Ok(p) => p,
            Err(ds) => {
                let mut sm = SourceMap::new();
                sm.add_file("t.nin", src);
                panic!("{}", sm.render_all(&ds.items, false));
            }
        }
    }

    fn errs(src: &str) -> Vec<&'static str> {
        match parse_program(src, 0) {
            Ok(_) => vec![],
            Err(ds) => ds.items.iter().filter(|d| d.is_error()).map(|d| d.code).collect(),
        }
    }

    #[test]
    fn minimal() {
        let p = prog(r#"{"nin":"1","main":[{"print":"hi"}]}"#);
        assert_eq!(p.version, "1");
        let main = p.main.unwrap();
        assert!(matches!(&main[0].kind, StmtKind::Print { newline: true, stderr: false, .. }));
    }

    #[test]
    fn value_vs_expression_positions() {
        let p = prog(r#"{"nin":"1","main":[
            {"let":"x","value":"${1 + 2}"},
            {"let":"y","value":"plain"},
            {"if":"x > 2","then":[{"print":"v=${x}"}]},
            {"while":"${x < 10}","do":[{"set":"x","op":"+","value":1}]},
            {"let":"l","value":[1,"${x}",{"k":"v"}]}
        ]}"#);
        let m = p.main.unwrap();
        match &m[0].kind {
            StmtKind::Let { value: Some(v), .. } => assert!(matches!(&v.kind, ExprKind::Template(parts) if parts.len() == 1)),
            _ => panic!(),
        }
        match &m[1].kind {
            StmtKind::Let { value: Some(v), .. } => assert!(matches!(&v.kind, ExprKind::Str(s) if s == "plain")),
            _ => panic!(),
        }
        match &m[2].kind {
            StmtKind::If { cond, then, otherwise } => {
                assert!(matches!(&cond.kind, ExprKind::Binary(BinOp::Gt, _, _)));
                assert_eq!(then.len(), 1);
                assert!(otherwise.is_empty());
            }
            _ => panic!(),
        }
        match &m[3].kind {
            StmtKind::While { cond, body } => {
                assert!(matches!(&cond.kind, ExprKind::Binary(BinOp::Lt, _, _)));
                assert!(matches!(&body[0].kind, StmtKind::Assign { op: Some(BinOp::Add), .. }));
            }
            _ => panic!(),
        }
        match &m[4].kind {
            StmtKind::Let { value: Some(v), .. } => assert!(matches!(&v.kind, ExprKind::List(items) if items.len() == 3 && matches!(items[2].kind, ExprKind::Map(_)))),
            _ => panic!(),
        }
    }

    #[test]
    fn functions_and_params() {
        let p = prog(r#"{"nin":"1","functions":{"f":{"params":["a: int","b: int = 1 + 1","...rest",{"name":"c","type":"list<string>","default":"[]"}],"returns":"int","async":true,"generics":["T","U: Cmp"],"body":[{"return":"${a}"}]}}}"#);
        let f = &p.functions[0];
        assert_eq!(f.name, "f");
        assert!(f.is_async);
        assert_eq!(f.params.len(), 4);
        assert_eq!(f.params[0].ty.as_ref().unwrap().display(), "int");
        assert!(f.params[1].default.is_some());
        assert!(f.params[2].variadic);
        assert_eq!(f.params[3].ty.as_ref().unwrap().display(), "list<string>");
        assert_eq!(f.generics.len(), 2);
        assert_eq!(f.generics[1].bound.as_ref().unwrap().display(), "Cmp");
        assert_eq!(f.ret.as_ref().unwrap().display(), "int");
    }

    #[test]
    fn classes_and_interfaces() {
        let p = prog(r#"{"nin":"1",
          "interfaces":{"Shape":{"extends":"Named","methods":{"area":{"returns":"float"}}}},
          "enums":{"C":["A","B"],"D":{"X":1}},
          "classes":{"Circle":{"extends":"Base","implements":["Shape","Printable"],"abstract":false,"generics":["T"],
            "fields":{"r":{"type":"float","default":1.5,"access":"private","readonly":true},"n":"int"},
            "constructors":[{"params":["r: float"],"body":[{"super":[]},{"set":"this.r","value":"${r}"}]}],
            "methods":{"area":{"returns":"float","access":"protected","body":[]},
                       "scale":[{"params":["k: int"],"body":[]},{"params":["k: float"],"static":true,"body":[]}],
                       "draw":{"abstract":true}}}}}"#);
        let c = &p.classes[0];
        assert_eq!(c.extends.as_ref().unwrap().display(), "Base");
        assert_eq!(c.implements.len(), 2);
        assert_eq!(c.fields.len(), 2);
        assert_eq!(c.fields[0].access, Access::Private);
        assert!(c.fields[0].readonly);
        assert_eq!(c.fields[1].ty.as_ref().unwrap().display(), "int");
        assert_eq!(c.constructors.len(), 1);
        assert!(matches!(&c.constructors[0].body.as_ref().unwrap()[0].kind, StmtKind::SuperCall(a) if a.is_empty()));
        assert_eq!(c.methods.len(), 4);
        assert_eq!(c.methods[0].access, Access::Protected);
        assert_eq!(c.methods[1].name, "scale");
        assert!(c.methods[2].is_static);
        assert!(c.methods[3].is_abstract && c.methods[3].body.is_none());
        let i = &p.interfaces[0];
        assert_eq!(i.extends.len(), 1);
        assert!(i.methods[0].is_abstract && i.methods[0].body.is_none());
        assert_eq!(p.enums.len(), 2);
        assert!(p.enums[1].variants[0].1.is_some());
    }

    #[test]
    fn statements_all_heads() {
        let p = prog(r#"{"nin":"1","main":[
          {"const":"a","value":1,"type":"int"},
          {"call":"f(1)"},
          {"eprint":"e","end":""},
          {"for":["k","v"],"in":"m","do":[{"break":true}]},
          {"match":"x","cases":[{"when":[1,2],"do":[{"print":"a"}]},{"when":3,"do":[]}],"default":[{"print":"d"}]},
          {"return":null},
          {"throw":"new Error('x')"},
          {"try":[{"call":"g()"}],"catch":[],"as":"e","type":"IOError","finally":[{"print":"f"}]},
          {"try":[],"catches":[{"type":"A","as":"x","do":[]},{"do":[]}]},
          {"run":"git","args":["status","${x}"],"shell":false,"cwd":".","env":{"A":"1"},"stdin":"in","stdout":"capture","stderr":{"file":"e.txt","append":true},"into":"r","check":true,"timeout":"5s"},
          {"parallel":[[{"call":"a()"}],[{"call":"b()"}]],"max":2},
          {"spawn":[{"call":"w()"}],"as":"t"},
          {"await":"t","as":"r"},
          {"block":[]},
          {"assert":"x == 1","message":"bad"},
          {"exit":3}
        ]}"#);
        let m = p.main.unwrap();
        assert_eq!(m.len(), 16);
        assert!(matches!(&m[0].kind, StmtKind::Let { is_const: true, ty: Some(_), .. }));
        assert!(matches!(&m[2].kind, StmtKind::Print { newline: false, stderr: true, .. }));
        assert!(matches!(&m[3].kind, StmtKind::For { vars, .. } if vars == &vec!["k".to_string(), "v".to_string()]));
        assert!(matches!(&m[4].kind, StmtKind::Match { cases, default, .. } if cases.len() == 2 && cases[0].when.len() == 2 && cases[1].when.len() == 1 && default.len() == 1));
        assert!(matches!(&m[5].kind, StmtKind::Return(None)));
        assert!(matches!(&m[7].kind, StmtKind::Try { catches, finally, .. } if catches.len() == 1 && catches[0].ty.is_some() && finally.len() == 1));
        assert!(matches!(&m[8].kind, StmtKind::Try { catches, .. } if catches.len() == 2));
        match &m[9].kind {
            StmtKind::Run(r) => {
                assert_eq!(r.args.len(), 2);
                assert!(r.check && r.into.as_deref() == Some("r"));
                assert!(matches!(r.stdout, Some(Redirect::Capture)));
                assert!(matches!(r.stderr, Some(Redirect::File { append: true, .. })));
                assert_eq!(r.env.len(), 1);
                assert!(r.timeout.is_some());
            }
            _ => panic!(),
        }
        assert!(matches!(&m[10].kind, StmtKind::Parallel { branches, max: Some(_) } if branches.len() == 2));
        assert!(matches!(&m[11].kind, StmtKind::Spawn { name: Some(n), .. } if n == "t"));
        assert!(matches!(&m[12].kind, StmtKind::Await { into: Some(n), .. } if n == "r"));
        assert!(matches!(&m[15].kind, StmtKind::Exit(Some(_))));
    }

    #[test]
    fn workflow_lowering() {
        let p = prog(r#"{"nin":"1","workflow":{"name":"ci","max_parallel":2,"env":{"CI":"1"},
          "inputs":{"profile":{"type":"string","default":"debug","required":false},"fast":"bool"},
          "defaults":{"timeout":"10m","retries":1},
          "jobs":{"build":{"needs":"lint","if":"success()","timeout":"5m","retries":2,"continue_on_error":false,
             "strategy":{"matrix":{"t":["a","b"],"include":[{"t":"c"}],"exclude":[{"t":"b"}]},"fail_fast":false,"max_parallel":2},
             "limits":{"memory":"512MB","processes":8,"cpu_time":"2m"},
             "steps":[{"id":"cc","name":"compile","run":"cargo","args":["build"],"env":{"A":"1"},"timeout":"1m","outputs":{"o":"${stdout}"},"on_error":[{"print":"x"}]},
                      {"script":[{"output":"k","value":1}]},{"uses":"fn1","with":{"a":1}}],
             "outputs":{"r":"${steps.cc.outputs.o}"},"finally":[{"run":"cleanup"}]},
             "lint":{"steps":[{"run":"l"}]}}}}"#);
        let w = p.workflow.unwrap();
        assert_eq!(w.name.as_deref(), Some("ci"));
        assert_eq!(w.inputs.len(), 2);
        assert!(w.default_timeout.is_some() && w.default_retries.is_some());
        assert_eq!(w.jobs.len(), 2);
        let b = &w.jobs[0];
        assert_eq!(b.id, "build");
        assert_eq!(b.needs, vec!["lint".to_string()]);
        let m = b.matrix.as_ref().unwrap();
        assert_eq!(m.dims.len(), 1);
        assert_eq!(m.include.len(), 1);
        assert_eq!(m.exclude.len(), 1);
        assert!(!m.fail_fast);
        assert!(b.limits.as_ref().unwrap().memory.is_some());
        assert_eq!(b.steps.len(), 3);
        match &b.steps[0].action {
            StepAction::Run(r) => {
                assert!(r.env.is_empty() && r.timeout.is_none());
            }
            _ => panic!(),
        }
        assert_eq!(b.steps[0].env.len(), 1);
        assert!(b.steps[0].timeout.is_some());
        assert_eq!(b.steps[0].on_error.len(), 1);
        assert!(matches!(&b.steps[1].action, StepAction::Script(s) if s.len() == 1));
        assert!(matches!(&b.steps[2].action, StepAction::Uses { function, with } if function == "fn1" && with.len() == 1));
        assert_eq!(b.finally.len(), 1);
    }

    #[test]
    fn imports_aliases() {
        let p = prog(r#"{"nin":"1","imports":[{"use":"std.fs"},{"file":"./lib/util.nin"},{"package":"my-lib","as":"ml"},{"use":"std.json","as":"j"}]}"#);
        let a: Vec<&str> = p.imports.iter().map(|i| i.alias.as_str()).collect();
        assert_eq!(a, vec!["fs", "util", "ml", "j"]);
    }

    #[test]
    fn globals_order() {
        let p = prog(r#"{"nin":"1","vars":{"v":1},"const":{"c":2}}"#);
        assert_eq!(p.globals[0].name, "c");
        assert!(p.globals[0].is_const);
        assert_eq!(p.globals[1].name, "v");
    }

    #[test]
    fn bad_expressions_report_errors() {
        assert_eq!(errs(r#"{"nin":"1","main":[{"if":"1 +"}]}"#), vec!["E0203"]);
        assert_eq!(errs(r#"{"nin":"1","main":[{"print":"${1 +}"}]}"#), vec!["E0203"]);
        assert_eq!(errs(r#"{"nin":"1","main":[{"let":"x","type":"list<","value":1}]}"#), vec!["E0207"]);
        assert_eq!(errs(r#"{"nin":"1","main":[{"set":"1 + 2","value":1}]}"#), vec!["E0205"]);
        assert_eq!(errs(r#"{"nin":"1","functions":{"f":{"params":["a: ","b"]}}}"#), vec!["E0207"]);
    }

    #[test]
    fn end_to_end_errors_render_with_json_positions() {
        let src = "{\n  \"nin\": \"1\",\n  \"main\": [\n    {\"if\": \"x > * 3\", \"then\": []}\n  ]\n}\n";
        let ds = parse_program(src, 0).unwrap_err();
        let mut sm = SourceMap::new();
        sm.add_file("t.nin", src);
        let out = sm.render(&ds.items[0], false);
        assert!(out.contains("t.nin:4:"), "{}", out);
        assert!(out.contains("x > * 3"), "{}", out);
        // syntax error
        let ds = parse_program("{\"nin\": }", 0).unwrap_err();
        assert_eq!(ds.items[0].code, "E0001");
        // schema error
        let ds = parse_program("{\"nin\":\"1\",\"mian\":[]}", 0).unwrap_err();
        assert_eq!(ds.items[0].code, "E0101");
    }

    #[test]
    fn warnings_survive() {
        let (p, ds) = parse_program_diag(r#"{"nin":"1","main":[]}"#, 0);
        assert!(p.is_some());
        assert_eq!(ds.warning_count(), 1);
    }

    #[test]
    fn custom_print_end() {
        let p = prog(r#"{"nin":"1","main":[{"print":"a","end":"!"}]}"#);
        match &p.main.unwrap()[0].kind {
            StmtKind::Print { value, newline: false, .. } => assert!(matches!(value.kind, ExprKind::Template(_))),
            _ => panic!(),
        }
    }
}
