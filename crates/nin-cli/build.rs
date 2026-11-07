use std::path::PathBuf;
use std::process::Command;
fn main() {
    println!("cargo:rerun-if-changed=assets");
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let obj = out.join("nin_res.o");
    let tool = if cfg!(windows) { "windres" } else { "x86_64-w64-mingw32-windres" };
    let st = Command::new(tool).current_dir("assets").args(["-O", "coff", "-i", "nin.rc", "-o"]).arg(&obj).status();
    match st {
        Ok(s) if s.success() => println!("cargo:rustc-link-arg-bins={}", obj.display()),
        _ => println!("cargo:warning=windres failed; building without icon resources"),
    }
}
