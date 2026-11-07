//! Schema validator for NIN documents (stage 2 of the pipeline).
//!
//! Validates the *shape* of a parsed JSON document against the NIN schema
//! (docs/SPEC.md) and reports as many problems as possible in one pass:
//!
//! * E0101 unknown key (with did-you-mean and the list of valid keys)
//! * E0102 wrong JSON type
//! * E0103 missing required key
//! * E0104 missing / ambiguous statement (or step) head key
//! * E0105 invalid enum value
//! * E0106 invalid identifier
//! * E0107 duplicate step id
//! * E0108 empty `steps`
//! * W0101 suspicious (empty `main`, ...)

use nin_diag::{suggest, Diagnostic, Diagnostics, Span};
use nin_syntax::{Json, JsonKind};

#[derive(Clone, Copy)]
enum K {
    Any,
    Str,
    Bool,
    Int,
    Ident,
    /// expression position: string / bool / number
    Cond,
    /// duration or size: string or number
    StrNum,
    Strs,
    StrOrStrs,
    Block,
    Enum(&'static [&'static str]),
    AnyArr,
    MapAny,
    Obj(&'static [F]),
    Funcs,
    Func,
    Params,
    Fields,
    Methods,
    Ctors,
    Ctor,
    Classes,
    Interfaces,
    Enums,
    Imports,
    Redirect,
    ForVar,
    Cases,
    Catches,
    Branches,
    Jobs,
    Steps,
    Inputs,
    Matrix,
    Generics,
}

/// (key, kind, required)
type F = (&'static str, K, bool);

const VALID_ACCESS: &[&str] = &["public", "protected", "private"];
const VALID_OPS: &[&str] = &["+", "-", "*", "/", "%", "**", "??"];

const TOP: &[F] = &[
    ("nin", K::Str, true),
    ("name", K::Str, false),
    ("description", K::Str, false),
    ("version", K::Str, false),
    ("imports", K::Imports, false),
    ("const", K::MapAny, false),
    ("vars", K::MapAny, false),
    ("enums", K::Enums, false),
    ("interfaces", K::Interfaces, false),
    ("classes", K::Classes, false),
    ("functions", K::Funcs, false),
    ("main", K::Block, false),
    ("workflow", K::Obj(WORKFLOW), false),
];

const IMPORT: &[F] = &[("use", K::Str, false), ("file", K::Str, false), ("package", K::Str, false), ("as", K::Ident, false)];

const FUNC: &[F] = &[
    ("params", K::Params, false),
    ("returns", K::Str, false),
    ("async", K::Bool, false),
    ("generics", K::Generics, false),
    ("doc", K::Str, false),
    ("body", K::Block, false),
];
const PARAM: &[F] = &[("name", K::Ident, true), ("type", K::Str, false), ("default", K::Cond, false), ("variadic", K::Bool, false)];

const METHOD: &[F] = &[
    ("params", K::Params, false),
    ("returns", K::Str, false),
    ("async", K::Bool, false),
    ("generics", K::Generics, false),
    ("doc", K::Str, false),
    ("body", K::Block, false),
    ("access", K::Enum(VALID_ACCESS), false),
    ("static", K::Bool, false),
    ("abstract", K::Bool, false),
    ("override", K::Bool, false),
    ("final", K::Bool, false),
];
const CTOR: &[F] = &[("params", K::Params, false), ("body", K::Block, false), ("access", K::Enum(VALID_ACCESS), false), ("doc", K::Str, false)];
const FIELD: &[F] = &[
    ("type", K::Str, false),
    ("default", K::Any, false),
    ("access", K::Enum(VALID_ACCESS), false),
    ("static", K::Bool, false),
    ("readonly", K::Bool, false),
];
const CLASS: &[F] = &[
    ("extends", K::Str, false),
    ("implements", K::Strs, false),
    ("abstract", K::Bool, false),
    ("final", K::Bool, false),
    ("generics", K::Generics, false),
    ("doc", K::Str, false),
    ("fields", K::Fields, false),
    ("constructor", K::Ctor, false),
    ("constructors", K::Ctors, false),
    ("methods", K::Methods, false),
];
const IFACE: &[F] = &[("extends", K::StrOrStrs, false), ("generics", K::Generics, false), ("doc", K::Str, false), ("methods", K::Methods, false)];

const REDIRECT_FILE: &[F] = &[("file", K::Any, true), ("append", K::Bool, false)];
const CASE: &[F] = &[("when", K::Any, true), ("do", K::Block, false)];
const CATCH: &[F] = &[("type", K::Str, false), ("as", K::Ident, false), ("do", K::Block, false)];

const RUN_OPTS: &[F] = &[
    ("args", K::AnyArr, false),
    ("shell", K::Bool, false),
    ("cwd", K::Any, false),
    ("env", K::MapAny, false),
    ("stdin", K::Any, false),
    ("stdin_file", K::Any, false),
    ("stdout", K::Redirect, false),
    ("stderr", K::Redirect, false),
    ("into", K::Ident, false),
    ("check", K::Bool, false),
    ("timeout", K::StrNum, false),
];

/// Statement heads: the first entry of each spec is the head key itself.
const STMTS: &[(&str, &[F])] = &[
    ("let", &[("let", K::Ident, true), ("type", K::Str, false), ("value", K::Any, false)]),
    ("const", &[("const", K::Ident, true), ("type", K::Str, false), ("value", K::Any, false)]),
    ("set", &[("set", K::Cond, true), ("value", K::Any, false), ("op", K::Enum(VALID_OPS), false)]),
    ("call", &[("call", K::Cond, true)]),
    ("print", &[("print", K::Any, true), ("end", K::Str, false)]),
    ("eprint", &[("eprint", K::Any, true), ("end", K::Str, false)]),
    ("if", &[("if", K::Cond, true), ("then", K::Block, false), ("else", K::Block, false)]),
    ("while", &[("while", K::Cond, true), ("do", K::Block, false)]),
    ("for", &[("for", K::ForVar, true), ("in", K::Cond, true), ("do", K::Block, false)]),
    ("match", &[("match", K::Cond, true), ("cases", K::Cases, false), ("default", K::Block, false)]),
    ("break", &[("break", K::Any, true)]),
    ("continue", &[("continue", K::Any, true)]),
    ("return", &[("return", K::Any, true)]),
    ("throw", &[("throw", K::Any, true)]),
    (
        "try",
        &[("try", K::Block, true), ("catch", K::Block, false), ("as", K::Ident, false), ("type", K::Str, false), ("catches", K::Catches, false), ("finally", K::Block, false)],
    ),
    (
        "run",
        &[
            ("run", K::Any, true),
            ("args", K::AnyArr, false),
            ("shell", K::Bool, false),
            ("cwd", K::Any, false),
            ("env", K::MapAny, false),
            ("stdin", K::Any, false),
            ("stdin_file", K::Any, false),
            ("stdout", K::Redirect, false),
            ("stderr", K::Redirect, false),
            ("into", K::Ident, false),
            ("check", K::Bool, false),
            ("timeout", K::StrNum, false),
        ],
    ),
    ("parallel", &[("parallel", K::Branches, true), ("max", K::Any, false)]),
    ("spawn", &[("spawn", K::Block, true), ("as", K::Ident, false)]),
    ("await", &[("await", K::Cond, true), ("as", K::Ident, false)]),
    ("block", &[("block", K::Block, true)]),
    ("assert", &[("assert", K::Cond, true), ("message", K::Any, false)]),
    ("output", &[("output", K::Ident, true), ("value", K::Any, false)]),
    ("super", &[("super", K::AnyArr, true)]),
    ("exit", &[("exit", K::Any, true)]),
];

const STEP_COMMON: &[F] = &[
    ("id", K::Ident, false),
    ("name", K::Any, false),
    ("if", K::Cond, false),
    ("env", K::MapAny, false),
    ("workdir", K::Any, false),
    ("timeout", K::StrNum, false),
    ("retries", K::Any, false),
    ("retry_delay", K::StrNum, false),
    ("continue_on_error", K::Cond, false),
    ("outputs", K::MapAny, false),
    ("on_error", K::Block, false),
    ("script", K::Block, false),
    ("uses", K::Str, false),
    ("with", K::MapAny, false),
    ("run", K::Any, false),
];

const LIMITS: &[F] = &[("memory", K::StrNum, false), ("processes", K::Any, false), ("cpu_time", K::StrNum, false)];
const STRATEGY: &[F] = &[("matrix", K::Matrix, false), ("fail_fast", K::Bool, false), ("max_parallel", K::Any, false)];
const INPUT: &[F] = &[("type", K::Str, false), ("default", K::Any, false), ("required", K::Bool, false), ("description", K::Str, false)];
const DEFAULTS: &[F] = &[("timeout", K::StrNum, false), ("retries", K::Any, false)];

const JOB: &[F] = &[
    ("name", K::Any, false),
    ("needs", K::StrOrStrs, false),
    ("if", K::Cond, false),
    ("env", K::MapAny, false),
    ("workdir", K::Any, false),
    ("timeout", K::StrNum, false),
    ("retries", K::Any, false),
    ("retry_delay", K::StrNum, false),
    ("continue_on_error", K::Cond, false),
    ("strategy", K::Obj(STRATEGY), false),
    ("limits", K::Obj(LIMITS), false),
    ("steps", K::Steps, true),
    ("outputs", K::MapAny, false),
    ("finally", K::Steps, false),
];

const WORKFLOW: &[F] = &[
    ("name", K::Str, false),
    ("env", K::MapAny, false),
    ("inputs", K::Inputs, false),
    ("defaults", K::Obj(DEFAULTS), false),
    ("max_parallel", K::Any, false),
    ("jobs", K::Jobs, true),
];

pub fn is_identifier(s: &str) -> bool {
    let mut it = s.chars();
    match it.next() {
        Some(c) if c == '_' || c.is_alphabetic() => {}
        _ => return false,
    }
    it.all(|c| c == '_' || c.is_alphanumeric())
}

fn is_job_id(s: &str) -> bool {
    !s.is_empty() && s.chars().next().map_or(false, |c| c == '_' || c.is_alphabetic()) && s.chars().all(|c| c == '_' || c == '-' || c.is_alphanumeric())
}

/// Validate a parsed `.nin` document.
pub fn validate(doc: &Json) -> Diagnostics {
    let mut v = V { ds: Diagnostics::new() };
    v.obj(doc, TOP, "document");
    if let Some(JsonKind::Array(a)) = doc.get("main").map(|m| &m.kind) {
        if a.is_empty() {
            let m = doc.get("main").map(|m| m.span).unwrap_or(Span::DUMMY);
            v.ds.push(Diagnostic::warning("W0101", "`main` is empty").at(m, "nothing will run"));
        }
    }
    if let Some(n) = doc.get("nin") {
        if let Some(s) = n.as_str() {
            if s != "1" {
                v.ds.push(Diagnostic::error("E0105", format!("unsupported NIN format version `{}`", s)).at(n.span, "only version \"1\" is supported"));
            }
        }
    }
    v.ds
}

struct V {
    ds: Diagnostics,
}

impl V {
    fn type_err(&mut self, j: &Json, expected: &str, ctx: &str) {
        self.ds.push(
            Diagnostic::error("E0102", format!("expected {} for `{}`, found {}", expected, ctx, j.type_name())).at(j.span, format!("expected {}", expected)),
        );
    }

    fn ident(&mut self, j: &Json, ctx: &str) {
        match j.as_str() {
            Some(s) if is_identifier(s) => {}
            Some(s) => self.ds.push(
                Diagnostic::error("E0106", format!("`{}` is not a valid identifier", s))
                    .at(j.span, "identifiers match [A-Za-z_][A-Za-z0-9_]*")
                    .note(format!("in `{}`", ctx)),
            ),
            None => self.type_err(j, "a string identifier", ctx),
        }
    }

    fn obj(&mut self, j: &Json, spec: &[F], ctx: &str) {
        let members = match j.as_object() {
            Some(m) => m,
            None => return self.type_err(j, "an object", ctx),
        };
        for m in members {
            match spec.iter().find(|f| f.0 == m.key) {
                Some(f) => self.kind(&m.value, f.1, &format!("{}.{}", ctx, m.key)),
                None => self.unknown_key(&m.key, m.key_span, spec.iter().map(|f| f.0), ctx),
            }
        }
        for f in spec.iter().filter(|f| f.2) {
            if !members.iter().any(|m| m.key == f.0) {
                self.ds.push(Diagnostic::error("E0103", format!("missing required key `{}` in {}", f.0, ctx)).at(j.span, format!("`{}` is required here", f.0)));
            }
        }
    }

    fn unknown_key<'a>(&mut self, key: &str, span: Span, valid: impl Iterator<Item = &'a str> + Clone, ctx: &str) {
        let list: Vec<&str> = valid.clone().collect();
        let mut d = Diagnostic::error("E0101", format!("unknown key `{}` in {}", key, ctx)).at(span, "not a valid key here");
        match suggest(key, valid) {
            Some(s) => d = d.help(format!("did you mean `{}`?", s)),
            None => {
                if !list.is_empty() {
                    d = d.note(format!("valid keys: {}", list.join(", ")));
                }
            }
        }
        self.ds.push(d);
    }

    fn kind(&mut self, j: &Json, k: K, ctx: &str) {
        match k {
            K::Any => {}
            K::Str => {
                if j.as_str().is_none() {
                    self.type_err(j, "a string", ctx)
                }
            }
            K::Bool => {
                if j.as_bool().is_none() {
                    self.type_err(j, "a boolean", ctx)
                }
            }
            K::Int => {
                if j.as_i64().is_none() {
                    self.type_err(j, "an integer", ctx)
                }
            }
            K::Ident => self.ident(j, ctx),
            K::Cond => {
                if !matches!(j.kind, JsonKind::Str(_) | JsonKind::Bool(_) | JsonKind::Int(_) | JsonKind::Float(_)) {
                    self.type_err(j, "an expression string (or boolean/number)", ctx)
                }
            }
            K::StrNum => {
                if !matches!(j.kind, JsonKind::Str(_) | JsonKind::Int(_) | JsonKind::Float(_)) {
                    self.type_err(j, "a string (e.g. \"30s\") or a number", ctx)
                }
            }
            K::Strs => self.arr_of(j, ctx, |v, x, c| v.kind(x, K::Str, c)),
            K::StrOrStrs => {
                if j.as_str().is_none() {
                    self.arr_of(j, ctx, |v, x, c| v.kind(x, K::Str, c))
                }
            }
            K::Block => self.block(j, ctx),
            K::Enum(vals) => match j.as_str() {
                Some(s) if vals.contains(&s) => {}
                Some(s) => {
                    let mut d = Diagnostic::error("E0105", format!("invalid value `{}` for `{}`", s, ctx)).at(j.span, "not allowed here").note(format!("allowed: {}", vals.join(", ")));
                    if let Some(sg) = suggest(s, vals.iter().copied()) {
                        d = d.help(format!("did you mean `{}`?", sg));
                    }
                    self.ds.push(d)
                }
                None => self.type_err(j, "a string", ctx),
            },
            K::AnyArr => {
                if j.as_array().is_none() {
                    self.type_err(j, "an array", ctx)
                }
            }
            K::MapAny => match j.as_object() {
                Some(ms) => {
                    for m in ms {
                        if !m.key.is_empty() && m.key.chars().any(|c| c.is_control()) {
                            self.ds.push(Diagnostic::error("E0106", "invalid key").at(m.key_span, "control characters are not allowed"));
                        }
                    }
                }
                None => self.type_err(j, "an object", ctx),
            },
            K::Obj(spec) => self.obj(j, spec, ctx),
            K::Funcs => self.map_of(j, ctx, true, |v, x, c| v.kind(x, K::Func, c)),
            K::Func => self.obj(j, FUNC, ctx),
            K::Params => self.arr_of(j, ctx, |v, x, c| match x.kind {
                JsonKind::Str(_) => {}
                JsonKind::Object(_) => v.obj(x, PARAM, c),
                _ => v.type_err(x, "a parameter string (\"x: int\") or object", c),
            }),
            K::Fields => self.map_of(j, ctx, true, |v, x, c| match x.kind {
                JsonKind::Str(_) => {}
                JsonKind::Object(_) => v.obj(x, FIELD, c),
                _ => v.type_err(x, "a type string or a field object", c),
            }),
            K::Methods => self.map_of(j, ctx, true, |v, x, c| match &x.kind {
                JsonKind::Object(_) => v.obj(x, METHOD, c),
                JsonKind::Array(items) => {
                    for (i, it) in items.iter().enumerate() {
                        v.obj(it, METHOD, &format!("{}[{}]", c, i));
                    }
                }
                _ => v.type_err(x, "a method object or an array of overloads", c),
            }),
            K::Ctor => self.obj(j, CTOR, ctx),
            K::Ctors => self.arr_of(j, ctx, |v, x, c| v.obj(x, CTOR, c)),
            K::Classes => self.map_of(j, ctx, true, |v, x, c| v.obj(x, CLASS, c)),
            K::Interfaces => self.map_of(j, ctx, true, |v, x, c| v.obj(x, IFACE, c)),
            K::Enums => self.map_of(j, ctx, true, |v, x, c| match &x.kind {
                JsonKind::Array(items) => {
                    for it in items {
                        v.ident(it, c);
                    }
                }
                JsonKind::Object(ms) => {
                    for m in ms {
                        if !is_identifier(&m.key) {
                            v.ds.push(Diagnostic::error("E0106", format!("`{}` is not a valid enum variant name", m.key)).at(m.key_span, ""));
                        }
                    }
                }
                _ => v.type_err(x, "an array of names or an object name → value", c),
            }),
            K::Imports => self.arr_of(j, ctx, |v, x, c| {
                v.obj(x, IMPORT, c);
                if let Some(ms) = x.as_object() {
                    let heads = ms.iter().filter(|m| matches!(m.key.as_str(), "use" | "file" | "package")).count();
                    if heads != 1 {
                        v.ds.push(Diagnostic::error("E0104", "an import needs exactly one of `use`, `file` or `package`").at(x.span, format!("found {}", heads)));
                    }
                }
            }),
            K::Redirect => match &j.kind {
                JsonKind::Str(s) => {
                    let allowed = ["inherit", "null", "capture", "stdout"];
                    if !allowed.contains(&s.as_str()) {
                        self.ds.push(Diagnostic::error("E0105", format!("invalid redirect `{}`", s)).at(j.span, "").note("allowed: inherit, null, capture, stdout, or {\"file\": path}"));
                    }
                }
                JsonKind::Object(_) => self.obj(j, REDIRECT_FILE, ctx),
                _ => self.type_err(j, "a redirect string or {\"file\": ...}", ctx),
            },
            K::ForVar => match &j.kind {
                JsonKind::Str(_) => self.ident(j, ctx),
                JsonKind::Array(a) => {
                    if a.is_empty() || a.len() > 2 {
                        self.ds.push(Diagnostic::error("E0102", "`for` takes one or two variable names").at(j.span, format!("found {}", a.len())));
                    }
                    for x in a {
                        self.ident(x, ctx);
                    }
                }
                _ => self.type_err(j, "a variable name or [key, value]", ctx),
            },
            K::Cases => self.arr_of(j, ctx, |v, x, c| v.obj(x, CASE, c)),
            K::Catches => self.arr_of(j, ctx, |v, x, c| v.obj(x, CATCH, c)),
            K::Branches => self.arr_of(j, ctx, |v, x, c| v.block(x, c)),
            K::Jobs => match j.as_object() {
                Some(ms) => {
                    if ms.is_empty() {
                        self.ds.push(Diagnostic::error("E0108", "a workflow needs at least one job").at(j.span, "`jobs` is empty"));
                    }
                    for m in ms {
                        if !is_job_id(&m.key) {
                            self.ds.push(Diagnostic::error("E0106", format!("`{}` is not a valid job id", m.key)).at(m.key_span, "job ids match [A-Za-z_][A-Za-z0-9_-]*"));
                        }
                        self.obj(&m.value, JOB, &format!("{}.{}", ctx, m.key));
                    }
                }
                None => self.type_err(j, "an object", ctx),
            },
            K::Steps => match j.as_array() {
                Some(items) => {
                    if items.is_empty() && ctx.ends_with(".steps") {
                        self.ds.push(Diagnostic::error("E0108", "`steps` must not be empty").at(j.span, "add at least one step"));
                    }
                    let mut seen: Vec<(String, Span)> = Vec::new();
                    for (i, it) in items.iter().enumerate() {
                        let c = format!("{}[{}]", ctx, i);
                        self.step(it, &c);
                        if let Some(id) = it.get("id").and_then(|x| x.as_str()) {
                            if let Some((_, first)) = seen.iter().find(|(s, _)| s == id) {
                                let first = *first;
                                let sp = it.get_member("id").map(|m| m.value.span).unwrap_or(it.span);
                                self.ds.push(Diagnostic::error("E0107", format!("duplicate step id `{}`", id)).at(sp, "already used").also(first, "first used here"));
                            } else {
                                seen.push((id.to_string(), it.get_member("id").map(|m| m.value.span).unwrap_or(it.span)));
                            }
                        }
                    }
                }
                None => self.type_err(j, "an array of steps", ctx),
            },
            K::Inputs => self.map_of(j, ctx, true, |v, x, c| match x.kind {
                JsonKind::Str(_) => {}
                JsonKind::Object(_) => v.obj(x, INPUT, c),
                _ => v.type_err(x, "a type string or an input object", c),
            }),
            K::Matrix => match j.as_object() {
                Some(ms) => {
                    for m in ms {
                        match m.key.as_str() {
                            "include" | "exclude" => self.arr_of(&m.value, &format!("{}.{}", ctx, m.key), |v, x, c| {
                                if x.as_object().is_none() {
                                    v.type_err(x, "an object", c)
                                }
                            }),
                            _ => {
                                if !is_identifier(&m.key) {
                                    self.ds.push(Diagnostic::error("E0106", format!("`{}` is not a valid matrix dimension name", m.key)).at(m.key_span, ""));
                                }
                                if m.value.as_array().is_none() {
                                    self.type_err(&m.value, "an array of values", &format!("{}.{}", ctx, m.key));
                                }
                            }
                        }
                    }
                }
                None => self.type_err(j, "an object", ctx),
            },
            K::Generics => self.arr_of(j, ctx, |v, x, c| match x.as_str() {
                Some(s) => {
                    let name = s.split(':').next().unwrap_or("").trim();
                    if !is_identifier(name) {
                        v.ds.push(Diagnostic::error("E0106", format!("`{}` is not a valid type parameter", s)).at(x.span, "use `T` or `T: Bound`"));
                    }
                }
                None => v.type_err(x, "a string (`T` or `T: Bound`)", c),
            }),
        }
    }

    fn arr_of(&mut self, j: &Json, ctx: &str, mut f: impl FnMut(&mut V, &Json, &str)) {
        match j.as_array() {
            Some(items) => {
                for (i, it) in items.iter().enumerate() {
                    f(self, it, &format!("{}[{}]", ctx, i));
                }
            }
            None => self.type_err(j, "an array", ctx),
        }
    }

    fn map_of(&mut self, j: &Json, ctx: &str, ident_keys: bool, mut f: impl FnMut(&mut V, &Json, &str)) {
        match j.as_object() {
            Some(ms) => {
                for m in ms {
                    if ident_keys && !is_identifier(&m.key) {
                        self.ds.push(Diagnostic::error("E0106", format!("`{}` is not a valid identifier", m.key)).at(m.key_span, "names match [A-Za-z_][A-Za-z0-9_]*").note(format!("in `{}`", ctx)));
                    }
                    f(self, &m.value, &format!("{}.{}", ctx, m.key));
                }
            }
            None => self.type_err(j, "an object", ctx),
        }
    }

    fn block(&mut self, j: &Json, ctx: &str) {
        match j.as_array() {
            Some(items) => {
                for (i, it) in items.iter().enumerate() {
                    self.stmt(it, &format!("{}[{}]", ctx, i));
                }
            }
            None => {
                let mut d = Diagnostic::error("E0102", format!("expected an array of statements for `{}`, found {}", ctx, j.type_name())).at(j.span, "expected `[ ... ]`");
                if j.as_object().is_some() {
                    d = d.help("wrap a single statement in an array: [ { ... } ]");
                }
                self.ds.push(d);
            }
        }
    }

    fn stmt(&mut self, j: &Json, ctx: &str) {
        let members = match j.as_object() {
            Some(m) => m,
            None => {
                let mut d = Diagnostic::error("E0102", format!("expected a statement object, found {}", j.type_name())).at(j.span, "a statement is an object like {\"print\": \"hi\"}");
                if j.as_str().is_some() {
                    d = d.help("bare strings are not statements; use {\"call\": \"...\"} or {\"print\": \"...\"}");
                }
                self.ds.push(d);
                return;
            }
        };
        let heads: Vec<&nin_syntax::Member> = members.iter().filter(|m| STMTS.iter().any(|s| s.0 == m.key)).collect();
        if heads.is_empty() {
            let names: Vec<&str> = STMTS.iter().map(|s| s.0).collect();
            let mut d = Diagnostic::error("E0104", "statement has no recognised head key").at(j.span, "expected one of the statement keys");
            if let Some(first) = members.first() {
                if let Some(s) = suggest(&first.key, names.iter().copied()) {
                    d = d.help(format!("did you mean `{}`?", s));
                }
            }
            d = d.note(format!("statement keys: {}", names.join(", ")));
            self.ds.push(d);
            return;
        }
        // `for`'s `in` / `try`'s `type`/`as` are modifiers, never heads; heads are only those in STMTS.
        let (head, spec) = {
            let h = heads[0];
            let spec = STMTS.iter().find(|s| s.0 == h.key).map(|s| s.1).unwrap_or(&[]);
            (h, spec)
        };
        if heads.len() > 1 {
            let mut d = Diagnostic::error("E0104", format!("statement has several head keys: {}", heads.iter().map(|h| format!("`{}`", h.key)).collect::<Vec<_>>().join(", "))).at(heads[1].key_span, "only one statement per object").also(head.key_span, "first head here");
            d = d.help("put each statement in its own object");
            self.ds.push(d);
        }
        for m in members {
            match spec.iter().find(|f| f.0 == m.key) {
                Some(f) => self.kind(&m.value, f.1, &format!("{}.{}", ctx, m.key)),
                None => {
                    if heads.iter().any(|h| h.key == m.key) {
                        continue; // reported as multiple heads
                    }
                    self.unknown_key(&m.key, m.key_span, spec.iter().map(|f| f.0), &format!("`{}` statement", head.key));
                }
            }
        }
        if head.key == "try" && members.iter().all(|m| m.key != "catch" && m.key != "catches" && m.key != "finally") {
            self.ds.push(Diagnostic::error("E0103", "`try` needs `catch`, `catches` or `finally`").at(j.span, "nothing handles errors here"));
        }
    }

    fn step(&mut self, j: &Json, ctx: &str) {
        let members = match j.as_object() {
            Some(m) => m,
            None => return self.type_err(j, "a step object", ctx),
        };
        let mut spec: Vec<F> = STEP_COMMON.to_vec();
        spec.extend_from_slice(RUN_OPTS);
        for m in members {
            match spec.iter().find(|f| f.0 == m.key) {
                Some(f) => self.kind(&m.value, f.1, &format!("{}.{}", ctx, m.key)),
                None => self.unknown_key(&m.key, m.key_span, spec.iter().map(|f| f.0), "step"),
            }
        }
        let kinds = members.iter().filter(|m| matches!(m.key.as_str(), "run" | "script" | "uses")).count();
        if kinds != 1 {
            self.ds.push(
                Diagnostic::error("E0104", if kinds == 0 { "step has no action" } else { "step has several actions" })
                    .at(j.span, "a step needs exactly one of `run`, `script` or `uses`"),
            );
        }
        if let Some(w) = members.iter().find(|m| m.key == "with") {
            if members.iter().all(|m| m.key != "uses") {
                self.ds.push(Diagnostic::error("E0101", "`with` is only valid together with `uses`").at(w.key_span, ""));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nin_syntax::parse_script;

    fn check(src: &str) -> Diagnostics {
        validate(&parse_script(src, 0).expect("json"))
    }
    fn codes(src: &str) -> Vec<&'static str> {
        check(src).items.iter().map(|d| d.code).collect()
    }

    #[test]
    fn minimal_ok() {
        assert!(check(r#"{"nin":"1","main":[{"print":"hi"}]}"#).items.is_empty());
    }

    #[test]
    fn full_document_ok() {
        let src = r#"{
          "nin":"1","name":"x","imports":[{"use":"std.fs","as":"fs"},{"file":"./a.nin"}],
          "const":{"PI":3.14},"vars":{"n":0},"enums":{"C":["A","B"],"D":{"X":1}},
          "interfaces":{"Shape":{"methods":{"area":{"params":[],"returns":"float"}}}},
          "classes":{"Circle":{"extends":"Base","implements":["Shape"],"generics":["T","U: Cmp"],
             "fields":{"r":{"type":"float","default":1.0,"access":"private"},"n":"int"},
             "constructors":[{"params":["r: float"],"body":[{"super":[]},{"set":"this.r","value":"${r}"}]}],
             "methods":{"area":{"returns":"float","body":[{"return":"this.r"}]},"s":[{"params":["k: int"],"body":[]},{"params":["k: float"],"body":[]}]}}},
          "functions":{"f":{"params":["a: int","...rest",{"name":"b","type":"int","default":"1"}],"body":[
             {"let":"x","value":1},{"set":"x","op":"+","value":2},
             {"if":"x > 1","then":[{"print":["a","b"],"end":""}],"else":[{"break":true}]},
             {"for":["k","v"],"in":"m","do":[{"continue":true}]},
             {"match":"x","cases":[{"when":[1,2],"do":[]}],"default":[]},
             {"try":[{"throw":"e"}],"catch":[],"as":"e","finally":[]},
             {"try":[],"catches":[{"type":"IOError","as":"e","do":[]}]},
             {"run":"git","args":["status"],"stdout":"capture","stderr":{"file":"e.txt","append":true},"into":"r","check":true,"timeout":"5s"},
             {"parallel":[[{"call":"a()"}],[{"call":"b()"}]],"max":2},
             {"spawn":[],"as":"t"},{"await":"t","as":"r"},{"assert":"x","message":"m"},{"exit":0}]}},
          "main":[{"call":"f()"}],
          "workflow":{"name":"w","env":{"A":"1"},"inputs":{"p":{"type":"string","default":"d"},"q":"bool"},"defaults":{"timeout":"1m"},
            "jobs":{"build-1":{"needs":["lint"],"if":"success()","strategy":{"matrix":{"t":["a","b"],"include":[{"t":"c"}],"exclude":[{"t":"b"}]},"fail_fast":true},
               "limits":{"memory":"512MB","processes":4},"steps":[{"id":"s1","run":"x","args":[]},{"id":"s2","script":[{"output":"o","value":1}]},{"uses":"f","with":{"a":1}}],
               "outputs":{"o":"${steps.s1.code}"},"finally":[{"run":"c"}]},"lint":{"steps":[{"run":"l"}]}}}}"#;
        let ds = check(src);
        assert!(ds.items.is_empty(), "{:?}", ds.items.iter().map(|d| d.to_string()).collect::<Vec<_>>());
    }

    #[test]
    fn unknown_key_suggests() {
        let ds = check(r#"{"nin":"1","mian":[]}"#);
        assert_eq!(ds.items[0].code, "E0101");
        assert_eq!(ds.items[0].help.as_deref(), Some("did you mean `main`?"));
    }

    #[test]
    fn type_errors() {
        assert_eq!(codes(r#"{"nin":1}"#), vec!["E0102"]);
        assert_eq!(codes(r#"{"nin":"1","main":{"print":"x"}}"#), vec!["E0102"]);
        assert_eq!(codes(r#"{"nin":"1","functions":{"f":{"params":[1]}}}"#), vec!["E0102"]);
    }

    #[test]
    fn missing_required() {
        assert_eq!(codes(r#"{}"#), vec!["E0103"]);
        assert_eq!(codes(r#"{"nin":"1","workflow":{"jobs":{"a":{}}}}"#), vec!["E0103"]);
    }

    #[test]
    fn statement_heads() {
        assert_eq!(codes(r#"{"nin":"1","main":[{"foo":1}]}"#), vec!["E0104"]);
        assert_eq!(codes(r#"{"nin":"1","main":[{"print":"a","call":"b"}]}"#), vec!["E0104"]);
        assert_eq!(codes(r#"{"nin":"1","main":["echo hi"]}"#), vec!["E0102"]);
        assert_eq!(codes(r#"{"nin":"1","main":[{"print":"a","bogus":1}]}"#), vec!["E0101"]);
        assert_eq!(codes(r#"{"nin":"1","main":[{"try":[]}]}"#), vec!["E0103"]);
    }

    #[test]
    fn enums_and_idents() {
        assert_eq!(codes(r#"{"nin":"1","classes":{"A":{"fields":{"x":{"access":"pubic"}}}}}"#), vec!["E0105"]);
        assert_eq!(codes(r#"{"nin":"1","functions":{"1bad":{}}}"#), vec!["E0106"]);
        assert_eq!(codes(r#"{"nin":"1","main":[{"let":"a-b"}]}"#), vec!["E0106"]);
        assert_eq!(codes(r#"{"nin":"2"}"#), vec!["E0105"]);
    }

    #[test]
    fn workflow_rules() {
        assert_eq!(codes(r#"{"nin":"1","workflow":{"jobs":{"a":{"steps":[]}}}}"#), vec!["E0108"]);
        assert_eq!(codes(r#"{"nin":"1","workflow":{"jobs":{"a":{"steps":[{"id":"x","run":"a"},{"id":"x","run":"b"}]}}}}"#), vec!["E0107"]);
        assert_eq!(codes(r#"{"nin":"1","workflow":{"jobs":{"a":{"steps":[{"id":"x"}]}}}}"#), vec!["E0104"]);
        assert_eq!(codes(r#"{"nin":"1","workflow":{"jobs":{"a":{"steps":[{"run":"a","script":[]}]}}}}"#), vec!["E0104"]);
        assert_eq!(codes(r#"{"nin":"1","workflow":{"jobs":{}}}"#), vec!["E0108"]);
        assert_eq!(codes(r#"{"nin":"1","workflow":{"jobs":{"a":{"steps":[{"run":"a","with":{}}]}}}}"#), vec!["E0101"]);
    }

    #[test]
    fn many_errors_in_one_pass() {
        let ds = check(r#"{"nin":"1","main":[{"foo":1},{"print":"a","zz":2}],"mian":1}"#);
        assert!(ds.error_count() >= 3);
    }

    #[test]
    fn empty_main_warns() {
        let ds = check(r#"{"nin":"1","main":[]}"#);
        assert!(!ds.has_errors());
        assert_eq!(ds.warning_count(), 1);
    }

    #[test]
    fn spans_point_at_values() {
        let src = r#"{"nin":"1","main":[{"print":"a","zz":2}]}"#;
        let ds = check(src);
        let sp = ds.items[0].primary_span().unwrap();
        assert_eq!(&src[sp.start as usize..sp.end as usize], "\"zz\"");
    }

    #[test]
    fn identifier_rule() {
        assert!(is_identifier("a_1"));
        assert!(is_identifier("한글"));
        assert!(!is_identifier("1a"));
        assert!(!is_identifier(""));
        assert!(!is_identifier("a-b"));
    }
}
