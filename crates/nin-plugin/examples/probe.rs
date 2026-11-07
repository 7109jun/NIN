//! probe <plugin.dll|.so>: load a plugin, list functions, call greet/add.
use nin_plugin::host::LoadedPlugin;
use nin_plugin::serde_json::json;
use std::path::Path;

fn main() {
    let path = std::env::args().nth(1).expect("usage: probe <plugin>");
    let p = match LoadedPlugin::load(Path::new(&path)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("load failed: {e}");
            std::process::exit(1);
        }
    };
    println!("plugin {} v{} - {}", p.name, p.version, p.description);
    for f in &p.functions {
        println!("  {}({}..{}) {}", f.name, f.min_args, f.max_args, f.doc);
    }
    if p.has_function("greet") {
        println!("greet() = {:?}", p.call("greet", &json!([])));
        println!("greet(NIN) = {:?}", p.call("greet", &json!(["NIN"])));
    }
    if p.has_function("add") {
        println!("add(1,2,3) = {:?}", p.call("add", &json!([1, 2, 3])));
    }
    println!("info() = {:?}", p.call("info", &json!([])));
    println!("panic() = {:?}", p.call("panic", &json!([])));
    println!("add(1) = {:?}", p.call("add", &json!([1])));
}
