//! Shared plumbing: interpreter construction, script loading, running, reporting.
use crate::config::Config;
use nin_ast::Program;
use nin_runtime::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub static CANCEL: AtomicBool = AtomicBool::new(false);

pub const EXIT_OK: i32 = 0;
pub const EXIT_FAIL: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_INVALID: i32 = 3;
pub const EXIT_CANCEL: i32 = 130;

pub fn color_on(cfg: &Config) -> bool {
    cfg.color && std::env::var_os("NO_COLOR").is_none() && nin_win::is_terminal_stdout()
}

pub fn eprintln_err(msg: &str) {
    eprintln!("error: {}", msg);
}

pub fn make_interp(cfg: &Config, args: Vec<String>) -> Arc<Interp> {
    let mut b = nin_std::builder();
    b.args = args;
    b.pkg_root = cfg.packages.clone();
    b.registries = cfg.registries.clone();
    b.build()
}

pub fn known_std(interp: &Interp) -> Vec<String> {
    let mut v: Vec<String> = interp.natives.keys().cloned().collect();
    v.sort();
    v
}

/// Run semantic analysis; prints diagnostics. Returns false if there are errors.
pub fn check_program(interp: &Interp, prog: &Program, cfg: &Config) -> bool {
    let ds = nin_sema::analyze(prog, &nin_sema::Options { known_std: known_std(interp) });
    if !ds.items.is_empty() {
        eprint!("{}", interp.render_diagnostics(&ds.items, color_on(cfg)));
    }
    !ds.has_errors()
}

pub fn read_script(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    let s = String::from_utf8(bytes).map_err(|_| format!("{} is not valid UTF-8", path.display()))?;
    Ok(s.trim_start_matches('\u{feff}').to_string())
}

pub struct Loaded {
    pub interp: Arc<Interp>,
    pub ex: Exec,
    pub prog: Arc<Program>,
    pub base: PathBuf,
}

/// Parse + analyze a script from text. `Err(code)` after printing diagnostics.
pub fn load_text(cfg: &Config, name: &str, text: &str, base: PathBuf, args: Vec<String>, interp: Option<Arc<Interp>>) -> Result<Loaded, i32> {
    let interp = interp.unwrap_or_else(|| make_interp(cfg, args));
    let mut ex = Exec::new(interp.clone());
    let prog = match ex.parse_source(name, text) {
        Ok(p) => Arc::new(p),
        Err(rendered) => {
            eprint!("{}", rendered);
            return Err(EXIT_INVALID);
        }
    };
    if !check_program(&interp, &prog, cfg) {
        return Err(EXIT_INVALID);
    }
    Ok(Loaded { interp, ex, prog, base })
}

pub fn load_file(cfg: &Config, path: &Path, args: Vec<String>) -> Result<Loaded, i32> {
    let text = match read_script(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln_err(&e);
            return Err(EXIT_FAIL);
        }
    };
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let base = abs.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    load_text(cfg, &path.display().to_string(), &text, base, args, None)
}

/// Ctrl+C → set the root token of `ex` and `extra`.
pub fn watch_cancel(tokens: Vec<Arc<AtomicBool>>) {
    let _ = nin_win::install_ctrlc_handler(&CANCEL);
    std::thread::spawn(move || loop {
        if CANCEL.load(Ordering::SeqCst) {
            for t in &tokens {
                t.store(true, Ordering::SeqCst);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    });
}

pub fn report_signal(interp: &Interp, s: Signal) -> i32 {
    match s {
        Signal::Exit(c) => c,
        Signal::Throw(t) => {
            let (kind, _) = error_parts(&t.value);
            eprint!("{}", interp.render_throw(&t));
            if kind == "CancelledError" && CANCEL.load(Ordering::SeqCst) {
                EXIT_CANCEL
            } else {
                EXIT_FAIL
            }
        }
    }
}

/// Run main (and the workflow if there is no main). Changes cwd to the script directory.
pub fn run_loaded(mut l: Loaded, cfg: &Config) -> i32 {
    let _ = std::env::set_current_dir(&l.base);
    watch_cancel(vec![l.ex.root_token()]);
    let module = "main";
    let has_main = l.prog.main.is_some();
    let env = match l.ex.run_program(&l.prog, &l.base, module) {
        Ok(e) => e,
        Err(s) => return report_signal(&l.interp, s),
    };
    if !has_main && l.prog.workflow.is_some() {
        return run_workflow(&mut l, env, cfg, Default::default());
    }
    if CANCEL.load(Ordering::SeqCst) {
        EXIT_CANCEL
    } else {
        EXIT_OK
    }
}

pub fn run_workflow(l: &mut Loaded, env: Env, cfg: &Config, mut opts: nin_workflow::Options) -> i32 {
    let wf = match &l.prog.workflow {
        Some(w) => w,
        None => {
            eprintln_err("this script has no workflow");
            return EXIT_USAGE;
        }
    };
    if opts.max_parallel.is_none() {
        opts.max_parallel = cfg.max_parallel;
    }
    opts.base_dir = l.base.clone();
    opts.sink = Some(Arc::new(nin_workflow::ConsoleSink));
    watch_cancel(vec![opts.cancel.clone(), l.ex.root_token()]);
    let report_path = REPORT.with(|r| r.borrow().clone());
    match nin_workflow::run(l.interp.clone(), env, wf, opts) {
        Ok(rep) => {
            println!("{}", rep.summary());
            if let Some(p) = report_path {
                let txt = serde_json::to_string_pretty(&rep.to_json()).unwrap_or_default();
                if let Err(e) = std::fs::write(&p, txt) {
                    eprintln_err(&format!("cannot write report {}: {}", p, e));
                }
            }
            if CANCEL.load(Ordering::SeqCst) {
                EXIT_CANCEL
            } else if rep.ok() {
                EXIT_OK
            } else {
                EXIT_FAIL
            }
        }
        Err(e) => {
            eprintln_err(&e);
            EXIT_FAIL
        }
    }
}

thread_local! {
    pub static REPORT: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}
