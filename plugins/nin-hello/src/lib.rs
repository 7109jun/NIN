//! nin-hello: sample NIN plugin built with `export_plugin!`.

use nin_plugin::sdk;
use nin_plugin::serde_json::{json, Value};

fn args(v: &Value) -> &[Value] {
    v.as_array().map(|a| a.as_slice()).unwrap_or(&[])
}

fn greet(v: &Value) -> Result<Value, String> {
    let name = match args(v).first() {
        None | Some(Value::Null) => "world".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(o) => return Err(format!("greet: name must be a string, got {o}")),
    };
    sdk::log(0, &format!("greet called for {name}"));
    Ok(Value::String(format!("Hello, {name}!")))
}

/// Sums numbers; integers stay integers (checked), otherwise f64.
fn sum_values(items: &[Value]) -> Result<Value, String> {
    let mut int: Option<i64> = Some(0);
    let mut fl = 0.0f64;
    for it in items {
        let n = it.as_number().ok_or_else(|| format!("expected number, got {it}"))?;
        fl += n.as_f64().unwrap_or(0.0);
        int = match (int, n.as_i64()) {
            (Some(a), Some(b)) => a.checked_add(b),
            _ => None,
        };
    }
    Ok(match int {
        Some(i) => json!(i),
        None => json!(fl),
    })
}

fn add(v: &Value) -> Result<Value, String> {
    sum_values(args(v))
}

fn sum(v: &Value) -> Result<Value, String> {
    match args(v).first() {
        Some(Value::Array(a)) => sum_values(a),
        _ => Err("sum: expected a list".into()),
    }
}

fn reverse(v: &Value) -> Result<Value, String> {
    match args(v).first() {
        Some(Value::String(s)) => Ok(Value::String(s.chars().rev().collect())),
        _ => Err("reverse: expected a string".into()),
    }
}

fn info(_: &Value) -> Result<Value, String> {
    let host = match sdk::host_info() {
        Some((n, ver)) => json!({"name": n, "version": ver}),
        None => Value::Null,
    };
    Ok(json!({
        "plugin": "hello",
        "version": env!("CARGO_PKG_VERSION"),
        "abi": nin_plugin::NIN_PLUGIN_ABI_VERSION,
        "host": host,
    }))
}

fn fail(v: &Value) -> Result<Value, String> {
    match args(v).first() {
        Some(Value::String(s)) => Err(s.clone()),
        Some(o) => Err(o.to_string()),
        None => Err("fail called".into()),
    }
}

fn do_panic(_: &Value) -> Result<Value, String> {
    panic!("hello plugin panicked on purpose")
}

fn echo(v: &Value) -> Result<Value, String> {
    Ok(args(v).first().cloned().unwrap_or(Value::Null))
}

nin_plugin::export_plugin! {
    name: "hello",
    version: "0.1.0",
    description: "Sample NIN plugin demonstrating the stable plugin ABI",
    functions: [
        ("greet", "greet(name?) -> string", 0, 1, greet),
        ("add", "add(a, b, ...) -> number", 2, -1, add),
        ("reverse", "reverse(str) -> string", 1, 1, reverse),
        ("sum", "sum(list) -> number", 1, 1, sum),
        ("info", "info() -> map with plugin/host info", 0, 0, info),
        ("fail", "fail(msg?) -> always errors", 0, 1, fail),
        ("panic", "panic() -> panics (contained by the ABI layer)", 0, 0, do_panic),
        ("echo", "echo(x) -> x", 1, 1, echo),
    ]
}
