//! nin.exe — NIN Shell command line.
mod cmds;
mod config;
mod core;
mod repl;

use crate::core::*;
use std::path::PathBuf;

fn main() {
    let h = std::thread::Builder::new().stack_size(256 << 20).spawn(real_main).unwrap();
    let code = h.join().unwrap_or(EXIT_FAIL);
    std::process::exit(code);
}

fn real_main() -> i32 {
    nin_win::set_utf8_codepage();
    nin_win::enable_vt();
    let (cfg, warn) = config::load();
    if let Some(w) = warn {
        eprintln!("warning: bad config: {}", w);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    // compiled bundle?
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(Some(b)) = nin_compile::read_embedded(&exe) {
            let code = cmds::run_bundle(&cfg, b, args);
            return finish(&cfg, code, true);
        }
    }
    if args.is_empty() {
        return repl::run(&cfg);
    }
    let code = cmds::dispatch(&cfg, args);
    let interactive_run = std::env::args().nth(1).map_or(false, |a| a == "run" || a.ends_with(".nin"));
    finish(&cfg, code, interactive_run)
}

/// Keep the console open when launched by double-click.
fn finish(cfg: &config::Config, code: i32, may_pause: bool) -> i32 {
    let pause = match cfg.pause_on_exit.as_str() {
        "always" => true,
        "never" => false,
        _ => nin_win::is_exclusive_console(),
    };
    if pause && may_pause {
        nin_win::pause_for_key(&format!("\n[exit code {}] Press any key to close...", code));
    }
    code
}

pub fn exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("nin.exe"))
}
