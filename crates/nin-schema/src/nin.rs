//! The concrete NIN document schema (docs/SPEC.md sections 1-7).

use crate::*;
use nin_diag::{Diagnostic, Diagnostics};
use nin_syntax::{Json, JsonKind};
use std::collections::BTreeMap;

const OPS: &[&str] = &["+", "-", "*", "/", "%", "**", "??"];
const ACCESS: &[&str] = &["public", "protected", "private"];

/// Value position: any JSON value.
fn value() -> Node {
    Node::Any
}

/// Expression position: string (parsed as expression) or a boolean/number literal.
fn expr_pos() -> Node {
    Node::OneOf(vec![Node::Str, Node::Bool, Node::Number])
}

fn type_str() -> Node {
    Node::Str
}

fn block() -> Node {
    Node::Ref("block")
}

fn f(key: &'static str, node: Node) -> Field {
    Field { key, node, required: false }
}
fn rq(key: &'static str, node: Node) -> Field {
    Field { key, node, required: true }
}

/// Generic parameter: `T` or `K: Bound`.
fn check_generic(s: &str) -> Option<StrProblem> {
    let name_part = s.split(':').next().unwrap_or("");
    let name = name_part.trim();
    let lead = name_part.len() - name_part.trim_start().len();
    if !is_ident(name, false) {
        return Some(StrProblem {
            code: "E0106",
            message: format!("invalid generic parameter name `{}`", name),
            label: "not a valid identifier".into(),
            help: Some("write `T` or `K: Bound`".into()),
            range: Some((lead, lead + name.len())),
        });
    }
    None
}

/// Parameter shorthand: `name`, `name: Type`, `name: Type = default`, `...name`.
fn check_param(s: &str) -> Option<StrProblem> {
    let t = s.trim_start();
    let lead = s.len() - t.len();
    let (rest, lead) = match t.strip_prefix("...") {
        Some(r) => (r, lead + 3),
        None => (t, lead),
    };
    let end = rest.find(|c: char| c == ':' || c == '=' || c.is_whitespace()).unwrap_or(rest.len());
    let name = &rest[..end];
    if !is_ident(name, false) {
        return Some(StrProblem {
            code: "E0106",
            message: format!("invalid parameter name `{}`", name),
            label: "not a valid identifier".into(),
            help: Some("parameters look like `x`, `x: int`, `x: int = 5` or `...rest`".into()),
            range: Some((lead, lead + name.len().max(1).min(rest.len().max(1)))),
        });
    }
    None
}

fn param_node() -> Node {
    Node::OneOf(vec![
        Node::StrCheck { what: "parameter", check: check_param },
        Node::object(
            ObjectSchema::new("parameter")
                .required("name", Node::ident())
                .field("type", type_str())
                .field("default", Node::Any)
                .field("variadic", Node::Bool),
        ),
    ])
}

fn generics_node() -> Node {
    Node::array_of("array of generic parameters", Node::StrCheck { what: "generic parameter", check: check_generic })
}

/// Fields shared by functions / methods / constructors.
fn func_fields(o: ObjectSchema, returns: bool) -> ObjectSchema {
    let mut o = o.field("params", Node::array_of("array of parameters", param_node())).field("doc", Node::Str);
    if returns {
        o = o.field("returns", type_str()).field("generics", generics_node()).field("async", Node::Bool);
    }
    o
}

fn flags(o: ObjectSchema) -> ObjectSchema {
    o.field("access", Node::Enum(ACCESS.to_vec()))
        .field("static", Node::Bool)
        .field("abstract", Node::Bool)
        .field("override", Node::Bool)
        .field("final", Node::Bool)
}

fn is_true(j: &Json, key: &str) -> bool {
    matches!(j.get(key).map(|v| &v.kind), Some(JsonKind::Bool(true)))
}

fn check_method(v: &Json, out: &mut Diagnostics) {
    let abs = is_true(v, "abstract");
    match (abs, v.get_member("body")) {
        (false, None) => out.push(
            Diagnostic::error("E0103", "missing required key `body`")
                .at(open_span(v), "this method has no `body`")
                .note("only `abstract` methods may omit the body")
                .help("add `\"body\": []` or mark the method `\"abstract\": true`"),
        ),
        (true, Some(b)) => out.push(Diagnostic::warning("W0104", "abstract method has a body").at(b.key_span, "an abstract method must not have a body")),
        _ => {}
    }
    if abs && is_true(v, "final") {
        if let Some(m) = v.get_member("final") {
            out.push(Diagnostic::warning("W0105", "method is both `abstract` and `final`").at(m.key_span, "nothing could ever implement it"));
        }
    }
}

fn func_node() -> Node {
    Node::object(func_fields(ObjectSchema::new("function"), true).required("body", block()))
}

fn method_obj() -> Node {
    Node::object(flags(func_fields(ObjectSchema::new("method"), true)).field("body", block()).check(check_method))
}

fn methods_map() -> Node {
    Node::ident_map("methods", false, Node::OneOf(vec![method_obj(), Node::array_of("array of overloads", method_obj())]))
}

fn iface_method() -> Node {
    Node::object(
        ObjectSchema::new("interface method")
            .field("params", Node::array_of("array of parameters", param_node()))
            .field("returns", type_str())
            .field("generics", generics_node())
            .field("async", Node::Bool)
            .field("static", Node::Bool)
            .field("doc", Node::Str),
    )
}

fn constructor_node() -> Node {
    Node::object(
        ObjectSchema::new("constructor")
            .field("params", Node::array_of("array of parameters", param_node()))
            .field("access", Node::Enum(ACCESS.to_vec()))
            .field("doc", Node::Str)
            .required("body", block()),
    )
}

fn check_class(v: &Json, out: &mut Diagnostics) {
    if is_true(v, "abstract") && is_true(v, "final") {
        if let Some(m) = v.get_member("final") {
            out.push(Diagnostic::warning("W0105", "class is both `abstract` and `final`").at(m.key_span, "an abstract class can never be instantiated or extended"));
        }
    }
}

fn field_node() -> Node {
    Node::OneOf(vec![
        Node::Str,
        Node::object(
            ObjectSchema::new("field")
                .field("type", type_str())
                .field("default", Node::Any)
                .field("access", Node::Enum(ACCESS.to_vec()))
                .field("readonly", Node::Bool)
                .field("static", Node::Bool)
                .field("doc", Node::Str),
        ),
    ])
}

fn class_node() -> Node {
    Node::object(
        ObjectSchema::new("class")
            .field("extends", Node::Str)
            .field("implements", Node::array_of("array of interface names", Node::Str))
            .field("abstract", Node::Bool)
            .field("final", Node::Bool)
            .field("generics", generics_node())
            .field("doc", Node::Str)
            .field("fields", Node::ident_map("fields", false, field_node()))
            .field("constructor", constructor_node())
            .field("constructors", Node::array_of("array of constructors", constructor_node()))
            .field("methods", methods_map())
            .conflict("constructor", "constructors")
            .check(check_class),
    )
}

fn interface_node() -> Node {
    Node::object(
        ObjectSchema::new("interface")
            .field("generics", generics_node())
            .field("extends", Node::array_of("array of interface names", Node::Str))
            .field("methods", Node::ident_map("methods", false, Node::OneOf(vec![iface_method(), Node::array_of("array of overloads", iface_method())])))
            .field("doc", Node::Str),
    )
}

fn import_node() -> Node {
    Node::object(
        ObjectSchema::new("import")
            .field("use", Node::Str)
            .field("file", Node::Str)
            .field("package", Node::Str)
            .field("as", Node::ident())
            .exactly_one(&["use", "file", "package"]),
    )
}

fn enums_node() -> Node {
    Node::ident_map(
        "enums",
        false,
        Node::OneOf(vec![
            Node::array_warn_empty("enum variant list", Node::ident()),
            Node::ident_map("enum variant map", false, Node::Any),
        ]),
    )
}

// ───────────────────────────── statements ─────────────────────────────

fn redirect(allow_merge: bool) -> Node {
    let mut names = vec!["inherit", "null", "capture"];
    if allow_merge {
        names.push("stdout");
    }
    Node::OneOf(vec![
        Node::Enum(names),
        Node::object(ObjectSchema::new("redirect").required("file", Node::Any).field("append", Node::Bool)),
    ])
}

/// Modifiers of the `run` statement (also valid on workflow `run` steps).
fn run_mods() -> Vec<Field> {
    vec![
        f("args", Node::array_of("array of arguments", Node::Any)),
        f("shell", Node::Bool),
        f("cwd", Node::Any),
        f("env", Node::map("object of environment variables", Node::Any)),
        f("stdin", Node::Any),
        f("stdin_file", Node::Any),
        f("stdout", redirect(false)),
        f("stderr", redirect(true)),
        f("into", Node::ident()),
        f("check", Node::Bool),
        f("timeout", Node::Any),
    ]
}

const RUN_ONLY_KEYS: &[&str] = &["args", "shell", "cwd", "stdin", "stdin_file", "stdout", "stderr", "into", "check"];

fn head(key: &'static str, value: Node, modifiers: Vec<Field>) -> Head {
    Head { key, value, modifiers, checks: vec![] }
}

fn flag_head(key: &'static str) -> Head {
    Head { key, value: Node::Bool, modifiers: vec![], checks: vec![check_flag] }
}

fn check_flag(v: &Json, out: &mut Diagnostic_sink) {
    for k in ["break", "continue"] {
        if let Some(m) = v.get_member(k) {
            if m.value.as_bool() == Some(false) {
                out.push(Diagnostic::warning("W0103", format!("`{}: false` does nothing", k)).at(m.value.span, "this statement is a no-op").help(format!("use `\"{}\": true`", k)));
            }
        }
    }
}

#[allow(non_camel_case_types)]
type Diagnostic_sink = Diagnostics;

fn check_try(v: &Json, out: &mut Diagnostics) {
    let has = |k: &str| v.get_member(k).is_some();
    if !has("catch") && !has("catches") && !has("finally") {
        if let Some(m) = v.get_member("try") {
            out.push(Diagnostic::warning("W0103", "`try` without `catch`, `catches` or `finally` has no effect").at(m.key_span, "no handler"));
        }
    }
    if !has("catch") {
        for k in ["as", "type"] {
            if let Some(m) = v.get_member(k) {
                out.push(Diagnostic::warning("W0103", format!("`{}` has no effect without `catch`", k)).at(m.key_span, "ignored").help("use `catch`, or put `type`/`as` inside a `catches` entry"));
            }
        }
    }
}

fn stmt_node() -> Node {
    let mut try_head = head(
        "try",
        block(),
        vec![
            f("catch", block()),
            f("as", Node::ident()),
            f("type", type_str()),
            f(
                "catches",
                Node::array_of(
                    "array of catch clauses",
                    Node::object(ObjectSchema::new("catch clause").field("type", type_str()).field("as", Node::ident()).required("do", block())),
                ),
            ),
            f("finally", block()),
        ],
    );
    try_head.checks.push(check_try_conflict);
    try_head.checks.push(check_try);

    let heads = vec![
        head("let", Node::ident(), vec![f("type", type_str()), f("value", Node::Any)]),
        head("const", Node::ident(), vec![f("type", type_str()), f("value", Node::Any)]),
        head("set", Node::Str, vec![rq("value", Node::Any), f("op", Node::Enum(OPS.to_vec()))]),
        head("call", Node::Str, vec![]),
        head("print", Node::Any, vec![f("end", Node::Str)]),
        head("eprint", Node::Any, vec![f("end", Node::Str)]),
        head("if", expr_pos(), vec![rq("then", block()), f("else", block())]),
        head("while", expr_pos(), vec![rq("do", block())]),
        head(
            "for",
            Node::OneOf(vec![Node::ident(), Node::array_len("array of 1 or 2 loop variables", Node::ident(), 1, 2)]),
            vec![rq("in", expr_pos()), rq("do", block())],
        ),
        head(
            "match",
            expr_pos(),
            vec![
                rq(
                    "cases",
                    Node::array_warn_empty(
                        "array of match cases",
                        Node::object(ObjectSchema::new("match case").required("when", Node::Any).required("do", block())),
                    ),
                ),
                f("default", block()),
            ],
        ),
        flag_head("break"),
        flag_head("continue"),
        head("return", Node::Any, vec![]),
        head("throw", Node::Any, vec![]),
        try_head,
        {
            let mut mods = run_mods();
            // keep order stable
            mods.shrink_to_fit();
            head("run", Node::Any, mods)
        },
        head("parallel", Node::array_warn_empty("array of branches (each an array of statements)", block()), vec![f("max", Node::Any)]),
        head("spawn", block(), vec![f("as", Node::ident())]),
        head("await", expr_pos(), vec![f("as", Node::ident())]),
        head("block", block(), vec![]),
        head("assert", expr_pos(), vec![f("message", Node::Any)]),
        head("output", Node::ident(), vec![rq("value", Node::Any)]),
        head("super", Node::array_of("array of arguments", Node::Any), vec![]),
        head("exit", Node::Any, vec![]),
    ];
    Node::Tagged(Box::new(TaggedSchema { what: "statement", heads }))
}

fn check_try_conflict(v: &Json, out: &mut Diagnostics) {
    if let (Some(a), Some(b)) = (v.get_member("catch"), v.get_member("catches")) {
        out.push(
            Diagnostic::error("E0109", "`catch` and `catches` cannot be used together")
                .at(b.key_span, "conflicts with `catch`")
                .also(a.key_span, "first given here"),
        );
    }
}

// ───────────────────────────── workflow ─────────────────────────────

fn check_positive(v: &Json, key: &str, allow_zero: bool, out: &mut Diagnostics) {
    if let Some(m) = v.get_member(key) {
        if let Some(i) = m.value.as_i64() {
            if i < 0 || (!allow_zero && i == 0) {
                out.push(
                    Diagnostic::warning("W0107", format!("`{}` should be {}, found {}", key, if allow_zero { "non-negative" } else { "positive" }, i))
                        .at(m.value.span, "suspicious value"),
                );
            }
        }
    }
}

fn check_retry_delay(v: &Json, out: &mut Diagnostics) {
    if let (Some(d), None) = (v.get_member("retry_delay"), v.get_member("retries")) {
        out.push(Diagnostic::warning("W0106", "`retry_delay` without `retries` has no effect").at(d.key_span, "nothing is ever retried"));
    }
}

fn check_workflow(v: &Json, out: &mut Diagnostics) {
    check_positive(v, "max_parallel", false, out);
    if let Some(m) = v.get_member("jobs") {
        if m.value.as_object().map_or(false, |o| o.is_empty()) {
            out.push(Diagnostic::warning("W0108", "workflow has no jobs").at(m.value.span, "empty `jobs`"));
        }
    }
}

fn check_steps_array(v: &Json, out: &mut Diagnostics) {
    let steps = v.get_member("steps");
    if let Some(m) = steps {
        if let Some(a) = m.value.as_array() {
            if a.is_empty() {
                out.push(
                    Diagnostic::error("E0108", "`steps` must contain at least one step")
                        .at(m.value.span, "empty `steps`")
                        .help("add a step such as {\"run\": \"cmd\"} or remove the job"),
                );
            }
        }
    }
    // duplicate ids across steps + finally
    let mut seen: Vec<(String, Span)> = Vec::new();
    for key in ["steps", "finally"] {
        if let Some(a) = v.get(key).and_then(|j| j.as_array()) {
            for s in a {
                if let Some(idm) = s.get_member("id") {
                    if let Some(id) = idm.value.as_str() {
                        if let Some((_, first)) = seen.iter().find(|(n, _)| n == id) {
                            out.push(
                                Diagnostic::error("E0107", format!("duplicate step id `{}`", id))
                                    .at(idm.value.span, "this id is already used in the job")
                                    .also(*first, "first used here"),
                            );
                        } else {
                            seen.push((id.to_string(), idm.value.span));
                        }
                    }
                }
            }
        }
    }
}

fn check_job(v: &Json, out: &mut Diagnostics) {
    check_steps_array(v, out);
    check_retry_delay(v, out);
    check_positive(v, "retries", true, out);
}

fn check_step(v: &Json, out: &mut Diagnostics) {
    check_retry_delay(v, out);
    check_positive(v, "retries", true, out);
    if v.get_member("run").is_none() {
        for k in RUN_ONLY_KEYS {
            if let Some(m) = v.get_member(k) {
                out.push(Diagnostic::warning("W0103", format!("`{}` only applies to `run` steps", k)).at(m.key_span, "ignored on this step"));
            }
        }
    }
    if v.get_member("with").is_some() && v.get_member("uses").is_none() {
        if let Some(m) = v.get_member("with") {
            out.push(Diagnostic::warning("W0103", "`with` only applies to `uses` steps").at(m.key_span, "ignored on this step"));
        }
    }
}

fn step_node() -> Node {
    let mut o = ObjectSchema::new("step")
        .field("id", Node::Ident { dash: true })
        .field("name", Node::Any)
        .field("if", expr_pos())
        .field("run", Node::Any)
        .field("script", block())
        .field("uses", Node::Str)
        .field("with", Node::ident_map("object of named arguments", false, Node::Any))
        .field("workdir", Node::Any)
        .field("retries", Node::Any)
        .field("retry_delay", Node::Any)
        .field("continue_on_error", expr_pos())
        .field("outputs", Node::ident_map("object of outputs", false, Node::Any))
        .field("on_error", block());
    for fl in run_mods() {
        o.fields.push(fl);
    }
    o.exactly_one = vec!["run", "script", "uses"];
    o.checks.push(check_step);
    Node::object(o)
}

fn matrix_node() -> Node {
    let mut o = ObjectSchema::new("matrix")
        .field("include", Node::array_of("array of objects", Node::map("object", Node::Any)))
        .field("exclude", Node::array_of("array of objects", Node::map("object", Node::Any)));
    o.additional = Some(Node::array_of("array of values", Node::Any));
    o.key_ident = Some(false);
    Node::object(o)
}

fn job_node() -> Node {
    Node::object(
        ObjectSchema::new("job")
            .field("name", Node::Any)
            .field("needs", Node::OneOf(vec![Node::array_of("array of job ids", Node::Ident { dash: true }), Node::Ident { dash: true }]))
            .field("if", expr_pos())
            .field("env", Node::map("object of environment variables", Node::Any))
            .field("workdir", Node::Any)
            .field("timeout", Node::Any)
            .field("retries", Node::Any)
            .field("retry_delay", Node::Any)
            .field("continue_on_error", expr_pos())
            .field(
                "strategy",
                Node::object(
                    ObjectSchema::new("strategy")
                        .field("matrix", matrix_node())
                        .field("fail_fast", Node::Bool)
                        .field("max_parallel", Node::Any),
                ),
            )
            .field(
                "limits",
                Node::object(ObjectSchema::new("limits").field("memory", Node::Any).field("processes", Node::Any).field("cpu_time", Node::Any)),
            )
            .required("steps", Node::array_of("array of steps", step_node()))
            .field("outputs", Node::ident_map("object of outputs", false, Node::Any))
            .field("finally", Node::array_of("array of steps", step_node()))
            .check(check_job),
    )
}

fn workflow_node() -> Node {
    Node::object(
        ObjectSchema::new("workflow")
            .field("name", Node::Str)
            .field("max_parallel", Node::Any)
            .field("env", Node::map("object of environment variables", Node::Any))
            .field(
                "inputs",
                Node::ident_map(
                    "object of inputs",
                    false,
                    Node::object(
                        ObjectSchema::new("input")
                            .field("type", type_str())
                            .field("default", Node::Any)
                            .field("required", Node::Bool)
                            .field("description", Node::Str),
                    ),
                ),
            )
            .field("defaults", Node::object(ObjectSchema::new("defaults").field("timeout", Node::Any).field("retries", Node::Any)))
            .required("jobs", Node::ident_map("object of jobs", true, job_node()))
            .check(check_workflow),
    )
}

// ───────────────────────────── document ─────────────────────────────

fn check_doc(v: &Json, out: &mut Diagnostics) {
    if let Some(m) = v.get_member("nin") {
        if let Some(s) = m.value.as_str() {
            if s != "1" {
                out.push(
                    Diagnostic::warning("W0101", format!("unsupported format version `{}`", s))
                        .at(m.value.span, "this shell implements version \"1\"")
                        .help("set \"nin\": \"1\""),
                );
            }
        }
    }
    if let Some(members) = v.as_object() {
        if members.iter().all(|m| m.key == "nin" || m.key == "name" || m.key == "description") {
            out.push(Diagnostic::warning("W0102", "the document defines nothing").at(open_span(v), "no `main`, `workflow`, functions, classes or globals"));
        }
    }
}

/// The schema of a complete `.nin` document.
pub fn nin_schema() -> Schema {
    let mut defs: BTreeMap<&'static str, Node> = BTreeMap::new();
    defs.insert("stmt", stmt_node());
    defs.insert("block", Node::array_of("array of statements", Node::Ref("stmt")));
    let root = Node::object(
        ObjectSchema::new("the document")
            .required("nin", Node::Str)
            .field("name", Node::Str)
            .field("description", Node::Str)
            .field("imports", Node::array_of("array of imports", import_node()))
            .field("const", Node::ident_map("object of constants", false, Node::Any))
            .field("vars", Node::ident_map("object of variables", false, Node::Any))
            .field("enums", enums_node())
            .field("interfaces", Node::ident_map("interfaces", false, interface_node()))
            .field("classes", Node::ident_map("classes", false, class_node()))
            .field("functions", Node::ident_map("functions", false, func_node()))
            .field("main", block())
            .field("workflow", workflow_node())
            .check(check_doc),
    );
    Schema { root, defs }
}
