//! NIN standard library: native modules registered into the interpreter.

mod data;
mod misc;
mod net;
mod system;
mod util;

use nin_runtime::InterpBuilder;

/// Names of all standard modules (`std.<name>`).
pub const MODULES: &[&str] = &["fs", "path", "process", "net", "sys", "json", "yaml", "toml", "object", "task", "time", "math", "str", "list", "map", "regex", "log", "io", "env", "hash", "random"];

/// Register every `std.*` module.
pub fn register_all(b: &mut InterpBuilder) {
    let all: Vec<(&str, indexmap::IndexMap<String, nin_runtime::Value>)> = vec![
        ("fs", system::fs()),
        ("path", system::path()),
        ("process", system::process()),
        ("sys", system::sys()),
        ("env", system::env()),
        ("io", system::io()),
        ("log", system::log()),
        ("net", net::net()),
        ("json", data::json()),
        ("yaml", data::yaml()),
        ("toml", data::toml()),
        ("object", data::object()),
        ("hash", data::hash()),
        ("regex", data::regex()),
        ("random", data::random()),
        ("task", misc::task()),
        ("time", misc::time()),
        ("math", misc::math()),
        ("str", misc::str_mod()),
        ("list", misc::list_mod()),
        ("map", misc::map_mod()),
    ];
    for (n, m) in all {
        b.add_module(&format!("std.{}", n), m);
    }
}

/// An `InterpBuilder` with the full standard library installed.
pub fn builder() -> InterpBuilder {
    let mut b = InterpBuilder::new();
    register_all(&mut b);
    b
}
