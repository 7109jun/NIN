use nin_plugin::host::{probe, LoadedPlugin, PluginError};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

fn locate() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let target = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| root.join("target"));
    let (prefix, ext) = if cfg!(windows) { ("", "dll") } else if cfg!(target_os = "macos") { ("lib", "dylib") } else { ("lib", "so") };
    let lib = target.join("debug").join(format!("{prefix}nin_hello.{ext}"));
    // Always (re)build so the artifact is never stale; cheap when up to date.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let st = std::process::Command::new(cargo)
        .args(["build", "-p", "nin-hello"])
        .current_dir(&root)
        .status()
        .expect("spawn cargo");
    assert!(st.success(), "building nin-hello failed");
    assert!(lib.exists(), "missing {}", lib.display());
    lib
}

#[test]
fn all_functions() {
    let p = LoadedPlugin::load(&locate()).unwrap();
    assert_eq!(p.name, "hello");
    assert_eq!(p.functions.len(), 8);
    assert!(p.has_function("greet") && !p.has_function("nope"));
    assert_eq!(p.call("greet", &json!([])).unwrap(), json!("Hello, world!"));
    assert_eq!(p.call("greet", &json!(["룰루"])).unwrap(), json!("Hello, 룰루!"));
    assert!(matches!(p.call("greet", &json!([5])), Err(PluginError::Call { code: 1, .. })));
    assert_eq!(p.call("add", &json!([1, 2, 3])).unwrap(), json!(6));
    assert_eq!(p.call("add", &json!([1, 2.5])).unwrap(), json!(3.5));
    assert!(matches!(p.call("add", &json!([1])), Err(PluginError::Arity { .. })));
    assert!(p.call("add", &json!([1, "x"])).is_err());
    assert_eq!(p.call("reverse", &json!(["abc"])).unwrap(), json!("cba"));
    assert_eq!(p.call("sum", &json!([[1, 2, 3, 4]])).unwrap(), json!(10));
    let info = p.call("info", &json!([])).unwrap();
    assert_eq!(info["plugin"], "hello");
    assert_eq!(info["abi"], 1);
    assert_eq!(info["host"]["name"], "NIN Shell");
    match p.call("fail", &json!(["bad thing"])) {
        Err(PluginError::Call { code: 1, message }) => assert_eq!(message, "bad thing"),
        o => panic!("{o:?}"),
    }
    let v = json!({"a": [1, null, true, "s", {"b": 1.5}], "u": "ü"});
    assert_eq!(p.call("echo", &json!([v.clone()])).unwrap(), v);
    assert!(matches!(p.call("nope", &json!([])), Err(PluginError::NoSuchFunction(_))));
    assert!(matches!(p.call("greet", &json!({})), Err(PluginError::Json(_))));
}

#[test]
fn panic_is_contained() {
    let p = LoadedPlugin::load(&locate()).unwrap();
    for _ in 0..3 {
        match p.call("panic", &json!([])) {
            Err(PluginError::Call { code: 2, message }) => assert!(message.contains("on purpose")),
            o => panic!("{o:?}"),
        }
    }
    assert_eq!(p.call("greet", &json!(["after"])).unwrap(), json!("Hello, after!"));
}

#[test]
fn concurrent_calls() {
    let p = Arc::new(LoadedPlugin::load(&locate()).unwrap());
    let hs: Vec<_> = (0..8)
        .map(|t| {
            let p = p.clone();
            std::thread::spawn(move || {
                for i in 0..500i64 {
                    assert_eq!(p.call("add", &json!([t, i])).unwrap(), json!(t + i));
                    assert_eq!(p.call("reverse", &json!([format!("{t}-{i}")])).unwrap().as_str().unwrap().len(), format!("{t}-{i}").len());
                    if i % 100 == 0 {
                        assert!(p.call("panic", &json!([])).is_err());
                    }
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
}

#[test]
fn probe_and_reload() {
    let path = locate();
    let m = probe(&path).unwrap();
    assert_eq!(m.name, "hello");
    assert!(m.functions.iter().any(|f| f.name == "add" && f.min_args == 2 && f.max_args == -1));
    let big = "x".repeat(1 << 20);
    let p = LoadedPlugin::load(&path).unwrap();
    assert_eq!(p.call("echo", &json!([big.clone()])).unwrap(), json!(big));
}
