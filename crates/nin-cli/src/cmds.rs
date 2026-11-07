use crate::config::Config;
use crate::core::*;
use crate::exe_path;
use nin_pkg::PackageManager;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

const HELP: &str = "NIN Shell — JSON-based scripting shell

USAGE:
  nin                          interactive shell (REPL)
  nin run FILE.nin [ARGS...]   run a script
  nin FILE.nin [ARGS...]       same as run
  nin check FILE.nin           parse, validate and analyze
  nin build FILE.nin [-o OUT.exe]   compile to a self-contained executable
  nin fmt FILE.nin [--write]   canonical pretty JSON
  nin eval \"EXPR\"              evaluate an expression
  nin workflow run|list|graph FILE.nin [--job ID]... [--input k=v]... [--report out.json] [--dry-run] [--max-parallel N]
  nin pkg install|remove|update|search|list|info|pack|publish ...
  nin plugin info|call|list ...
  nin schedule add|list|remove|enable|disable|run|daemon|register|unregister ...
  nin assoc install|uninstall|status
  nin version | nin help

Exit codes: 0 ok, 1 failure, 2 usage, 3 validation errors, 130 cancelled.";

fn usage(msg: &str) -> i32 {
    eprintln!("error: {}\n(try `nin help`)", msg);
    EXIT_USAGE
}

pub fn dispatch(cfg: &Config, mut a: Vec<String>) -> i32 {
    let cmd = a.remove(0);
    match cmd.as_str() {
        "help" | "--help" | "-h" | "/?" => {
            println!("{}", HELP);
            0
        }
        "version" | "--version" | "-V" => {
            println!("nin {}", env!("CARGO_PKG_VERSION"));
            0
        }
        "run" => match a.first() {
            None => usage("run needs a FILE"),
            Some(f) => {
                let f = PathBuf::from(f);
                let rest = a[1..].to_vec();
                run_file(cfg, &f, rest)
            }
        },
        "check" => match a.first() {
            None => usage("check needs a FILE"),
            Some(f) => match load_file(cfg, Path::new(f), vec![]) {
                Ok(_) => {
                    println!("{}: ok", f);
                    0
                }
                Err(c) => c,
            },
        },
        "build" => build(cfg, a),
        "fmt" => fmt(a),
        "eval" => eval(cfg, a),
        "workflow" | "wf" => workflow(cfg, a),
        "pkg" => pkg(cfg, a),
        "plugin" => plugin(cfg, a),
        "schedule" | "sched" => schedule(cfg, a),
        "assoc" => assoc(a),
        f if f.ends_with(".nin") => run_file(cfg, Path::new(f), a),
        other => usage(&format!("unknown command `{}`", other)),
    }
}

fn run_file(cfg: &Config, f: &Path, args: Vec<String>) -> i32 {
    match load_file(cfg, f, args) {
        Ok(l) => run_loaded(l, cfg),
        Err(c) => c,
    }
}

pub fn run_bundle(cfg: &Config, b: nin_compile::Bundle, args: Vec<String>) -> i32 {
    let interp = make_interp(cfg, args);
    {
        let mut vfs = interp.vfs.lock().unwrap();
        for (k, v) in &b.files {
            vfs.insert(k.clone(), v.clone());
        }
    }
    let text = match b.files.get(&b.main) {
        Some(t) => t.clone(),
        None => {
            eprintln_err("corrupt bundle: missing main");
            return EXIT_FAIL;
        }
    };
    let base = Path::new(&b.main).parent().map(Path::to_path_buf).unwrap_or_default();
    match load_text(cfg, &b.main, &text, base, vec![], Some(interp)) {
        Ok(mut l) => {
            // default cwd: where the exe was started
            watch_cancel(vec![l.ex.root_token()]);
            let has_main = l.prog.main.is_some();
            match l.ex.run_program(&l.prog, &l.base.clone(), "main") {
                Ok(env) => {
                    if !has_main && l.prog.workflow.is_some() {
                        run_workflow(&mut l, env, cfg, Default::default())
                    } else {
                        0
                    }
                }
                Err(s) => report_signal(&l.interp, s),
            }
        }
        Err(c) => c,
    }
}

fn take_opt(a: &mut Vec<String>, names: &[&str]) -> Option<String> {
    let i = a.iter().position(|x| names.contains(&x.as_str()))?;
    if i + 1 >= a.len() {
        return None;
    }
    a.remove(i);
    Some(a.remove(i))
}
fn take_all(a: &mut Vec<String>, names: &[&str]) -> Vec<String> {
    let mut v = vec![];
    while let Some(x) = take_opt(a, names) {
        v.push(x);
    }
    v
}
fn take_flag(a: &mut Vec<String>, names: &[&str]) -> bool {
    match a.iter().position(|x| names.contains(&x.as_str())) {
        Some(i) => {
            a.remove(i);
            true
        }
        None => false,
    }
}

fn pm(cfg: &Config) -> Result<PackageManager, String> {
    let root = cfg.packages.clone().unwrap_or_else(PackageManager::default_root);
    PackageManager::new(&root, cfg.registries.clone()).map_err(|e| e.to_string())
}

fn build(cfg: &Config, mut a: Vec<String>) -> i32 {
    let out = take_opt(&mut a, &["-o", "--out"]);
    let icon = take_opt(&mut a, &["--icon"]);
    let f = match a.first() {
        Some(f) => PathBuf::from(f),
        None => return usage("build needs a FILE"),
    };
    if load_file(cfg, &f, vec![]).is_err() {
        return EXIT_INVALID;
    }
    let pmg = pm(cfg).ok();
    let resolver = move |n: &str| pmg.as_ref().and_then(|p| p.find_installed(n).ok().flatten()).and_then(|i| i.entry_path());
    let bundle = match nin_compile::collect(&f, &resolver) {
        Ok(b) => b,
        Err(e) => {
            eprintln_err(&e);
            return EXIT_FAIL;
        }
    };
    let out = out.map(PathBuf::from).unwrap_or_else(|| f.with_extension("exe"));
    if let Err(e) = nin_compile::write_bundle(&exe_path(), &bundle, &out) {
        eprintln_err(&e);
        return EXIT_FAIL;
    }
    if icon.is_some() {
        eprintln!("warning: --icon is not supported yet; the default icon is used");
    }
    println!("built {} ({} file(s) embedded)", out.display(), bundle.files.len());
    0
}

fn fmt(mut a: Vec<String>) -> i32 {
    let write = take_flag(&mut a, &["--write", "-w"]);
    let f = match a.first() {
        Some(f) => f,
        None => return usage("fmt needs a FILE"),
    };
    let text = match read_script(Path::new(f)) {
        Ok(t) => t,
        Err(e) => {
            eprintln_err(&e);
            return 1;
        }
    };
    match nin_syntax::parse_script(&text, 0) {
        Ok(j) => {
            let s = j.to_json_pretty(2) + "\n";
            if write {
                if let Err(e) = std::fs::write(f, s) {
                    eprintln_err(&e.to_string());
                    return 1;
                }
                println!("formatted {} (comments are not preserved)", f);
            } else {
                print!("{}", s);
            }
            0
        }
        Err(d) => {
            eprintln!("{}: {}", d.code, d.message);
            EXIT_INVALID
        }
    }
}

fn eval(cfg: &Config, a: Vec<String>) -> i32 {
    if a.is_empty() {
        return usage("eval needs an expression");
    }
    let src = a.join(" ");
    let interp = make_interp(cfg, vec![]);
    let mut ex = nin_runtime::Exec::new(interp.clone());
    let file = interp.add_source("<eval>", &src);
    let span = nin_diag::Span { file, start: 0, end: src.len() as u32 };
    let expr = match nin_ast::expr::parse_expression(&src, span) {
        Ok(e) => e,
        Err(d) => {
            eprint!("{}", interp.render_diagnostics(&[d], false));
            return EXIT_INVALID;
        }
    };
    let env = nin_runtime::Scope::new(Some(interp.root.clone()));
    let fr = nin_runtime::Frame::new(env, "eval");
    match ex.eval(&expr, &fr) {
        Ok(v) => {
            println!("{}", nin_runtime::display_simple(&v));
            0
        }
        Err(s) => report_signal(&interp, s),
    }
}

fn workflow(cfg: &Config, mut a: Vec<String>) -> i32 {
    if a.is_empty() {
        return usage("workflow needs run|list|graph");
    }
    let sub = a.remove(0);
    let jobs = take_all(&mut a, &["--job", "-j"]);
    let inputs: Vec<(String, String)> = take_all(&mut a, &["--input", "-i"]).into_iter().map(|s| match s.split_once('=') { Some((k, v)) => (k.to_string(), v.to_string()), None => (s, String::new()) }).collect();
    let report = take_opt(&mut a, &["--report"]);
    let mp = take_opt(&mut a, &["--max-parallel"]).and_then(|s| s.parse().ok());
    let dry = take_flag(&mut a, &["--dry-run"]);
    let f = match a.first() {
        Some(f) => PathBuf::from(f),
        None => return usage("workflow needs a FILE"),
    };
    let mut l = match load_file(cfg, &f, a[1..].to_vec()) {
        Ok(l) => l,
        Err(c) => return c,
    };
    let wf = match &l.prog.workflow {
        Some(w) => w.clone(),
        None => {
            eprintln_err("this script has no workflow");
            return EXIT_USAGE;
        }
    };
    match sub.as_str() {
        "list" => {
            match nin_workflow::plan(&wf) {
                Ok(levels) => {
                    println!("workflow {}", wf.name.clone().unwrap_or_default());
                    for (i, lv) in levels.iter().enumerate() {
                        println!("  stage {}: {}", i + 1, lv.join(", "));
                    }
                    0
                }
                Err(e) => {
                    eprintln_err(&e);
                    EXIT_INVALID
                }
            }
        }
        "graph" => match nin_workflow::graph_text(&wf) {
            Ok(t) => {
                println!("{}", t);
                0
            }
            Err(e) => {
                eprintln_err(&e);
                EXIT_INVALID
            }
        },
        "run" => {
            let _ = std::env::set_current_dir(&l.base);
            REPORT.with(|r| *r.borrow_mut() = report.map(|p| std::env::current_dir().map(|d| d.join(&p).display().to_string()).unwrap_or(p)));
            let env = match l.ex.load_program(&l.prog, &l.base.clone(), "main") {
                Ok(e) => e,
                Err(s) => return report_signal(&l.interp, s),
            };
            let opts = nin_workflow::Options { inputs, jobs, max_parallel: mp, dry_run: dry, ..Default::default() };
            run_workflow(&mut l, env, cfg, opts)
        }
        _ => usage("workflow: expected run|list|graph"),
    }
}

fn pkg(cfg: &Config, mut a: Vec<String>) -> i32 {
    if a.is_empty() {
        return usage("pkg needs a subcommand");
    }
    let sub = a.remove(0);
    let force = take_flag(&mut a, &["--force", "-f"]);
    let m = match pm(cfg) {
        Ok(m) => m,
        Err(e) => {
            eprintln_err(&e);
            return 1;
        }
    };
    let arg0 = a.first().cloned();
    let r: Result<(), String> = (|| match sub.as_str() {
        "install" | "add" => {
            let spec = arg0.ok_or("install needs a package spec (name, name@version, or a .nnpkg path)")?;
            for p in m.install(&spec).map_err(|e| e.to_string())? {
                println!("installed {} {}", p.name, p.version);
            }
            Ok(())
        }
        "remove" | "uninstall" => {
            let n = arg0.ok_or("remove needs a package name")?;
            let r = m.remove(&n, force).map_err(|e| e.to_string())?;
            println!("removed {} {}", r.removed.name, r.removed.version);
            for o in r.orphans {
                println!("removed unused dependency {} {}", o.name, o.version);
            }
            Ok(())
        }
        "update" | "upgrade" => {
            let rs = m.update(arg0.as_deref()).map_err(|e| e.to_string())?;
            if rs.is_empty() {
                println!("everything is up to date");
            }
            for r in rs {
                println!("updated {} {} -> {}", r.name, r.from, r.to);
            }
            Ok(())
        }
        "search" => {
            for h in m.search(&arg0.unwrap_or_default()).map_err(|e| e.to_string())? {
                println!("{} {}  {}", h.name, h.latest, h.description);
            }
            Ok(())
        }
        "list" | "ls" => {
            for p in m.list().map_err(|e| e.to_string())? {
                println!("{} {}{}", p.name, p.version, if p.explicit { "" } else { "  (dependency)" });
            }
            Ok(())
        }
        "info" => {
            let n = arg0.ok_or("info needs a package name")?;
            match m.info(&n).map_err(|e| e.to_string())? {
                Some(p) => {
                    println!("name:    {}\nversion: {}\ndir:     {}", p.name, p.version, p.dir.display());
                    if let Some(e) = &p.entry {
                        println!("entry:   {}", e);
                    }
                    for (d, r) in &p.dependencies {
                        println!("depends: {} {}", d, r);
                    }
                    Ok(())
                }
                None => Err(format!("package `{}` is not installed", n)),
            }
        }
        "pack" => {
            let dir = arg0.ok_or("pack needs a source directory")?;
            let out = a.get(1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
            let p = nin_pkg::pack(Path::new(&dir), &out).map_err(|e| e.to_string())?;
            println!("packed {}", p.display());
            Ok(())
        }
        "publish" => {
            let f = arg0.ok_or("publish needs a .nnpkg file")?;
            let reg = a.get(1).ok_or("publish needs a registry directory")?;
            m.publish(Path::new(&f), Path::new(reg)).map_err(|e| e.to_string())?;
            println!("published {} to {}", f, reg);
            Ok(())
        }
        _ => Err(format!("unknown pkg command `{}`", sub)),
    })();
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln_err(&e);
            1
        }
    }
}

fn plugin(cfg: &Config, mut a: Vec<String>) -> i32 {
    if a.is_empty() {
        return usage("plugin needs info|call|list");
    }
    let sub = a.remove(0);
    let r: Result<(), String> = (|| match sub.as_str() {
        "info" | "load" => {
            let p = a.first().ok_or("info needs a DLL path")?;
            let meta = nin_plugin::host::probe(Path::new(p)).map_err(|e| e.to_string())?;
            println!("{} {} — {}", meta.name, meta.version, meta.description);
            for f in meta.functions {
                println!("  {}({}..{} args)  {}", f.name, f.min_args, f.max_args, f.doc);
            }
            Ok(())
        }
        "call" => {
            let p = a.first().ok_or("call needs DLL FUNCTION [JSON-ARRAY]")?;
            let func = a.get(1).ok_or("call needs a function name")?;
            let args: serde_json::Value = match a.get(2) {
                Some(s) => serde_json::from_str(s).map_err(|e| format!("bad JSON args: {}", e))?,
                None => serde_json::json!([]),
            };
            let pl = nin_plugin::host::LoadedPlugin::load(Path::new(p)).map_err(|e| e.to_string())?;
            let v = pl.call(func, &args).map_err(|e| e.to_string())?;
            println!("{}", v);
            Ok(())
        }
        "list" => {
            let dir = cfg.plugins.clone().unwrap_or_else(|| crate::config::config_dir().join("plugins"));
            let rd = std::fs::read_dir(&dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().map_or(false, |x| x.eq_ignore_ascii_case("dll")) {
                    match nin_plugin::host::probe(&p) {
                        Ok(m) => println!("{}  {} {}", p.display(), m.name, m.version),
                        Err(er) => println!("{}  (not a NIN plugin: {})", p.display(), er),
                    }
                }
            }
            Ok(())
        }
        _ => Err(format!("unknown plugin command `{}`", sub)),
    })();
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln_err(&e);
            1
        }
    }
}

fn run_task(t: &nin_sched::ScheduledTask) -> i32 {
    let mut spec = nin_win::ProcSpec::new(t.command[0].clone()).args(t.command[1..].iter().cloned());
    if let Some(w) = &t.workdir {
        spec = spec.cwd(w);
    }
    for (k, v) in &t.env {
        spec = spec.env(k, v);
    }
    match nin_win::run(&spec, None, Some(&CANCEL)) {
        Ok(r) => r.code,
        Err(e) => {
            eprintln!("task {}: {}", t.id, e);
            -1
        }
    }
}

fn schedule(_cfg: &Config, mut a: Vec<String>) -> i32 {
    if a.is_empty() {
        return usage("schedule needs a subcommand");
    }
    let sub = a.remove(0);
    let r: Result<i32, String> = (|| {
        let mut store = nin_sched::Store::open(&nin_sched::Store::default_path()).map_err(|e| e.to_string())?;
        let id_arg = a.first().cloned();
        let need_id = || id_arg.clone().ok_or_else(|| "this command needs a task id".to_string());
        match sub.as_str() {
            "add" => {
                // add NAME TRIGGER SCRIPT [ARGS...]   or   add NAME TRIGGER -- PROGRAM [ARGS...]
                let name = a.first().ok_or("usage: schedule add NAME TRIGGER SCRIPT.nin [ARGS...]")?.clone();
                let trig = nin_sched::parse_trigger(a.get(1).ok_or("missing TRIGGER (e.g. \"every 5m\", \"daily 09:30\", \"at 2026-12-01 08:00\", \"logon\", \"boot\")")?).map_err(|e| e.to_string())?;
                let mut command: Vec<String> = vec![];
                let rest = &a[2..];
                if rest.first().map(|s| s.as_str()) == Some("--") {
                    command.extend(rest[1..].iter().cloned());
                } else {
                    let script = rest.first().ok_or("missing SCRIPT")?;
                    let abs = std::fs::canonicalize(script).map_err(|e| format!("{}: {}", script, e))?;
                    command.push(exe_path().display().to_string());
                    command.push("run".into());
                    command.push(abs.display().to_string());
                    command.extend(rest[1..].iter().cloned());
                }
                if command.is_empty() {
                    return Err("empty command".into());
                }
                let id = nin_sched::make_id(&name, store.list());
                let mut t = nin_sched::ScheduledTask::new(&id, &name, command, trig, chrono::Local::now().naive_local());
                t.workdir = std::env::current_dir().ok().map(|p| p.display().to_string());
                store.add(t).map_err(|e| e.to_string())?;
                store.save().map_err(|e| e.to_string())?;
                println!("added task {}", id);
                Ok(0)
            }
            "list" | "ls" => {
                for t in store.list() {
                    println!("{:<20} {:<9} {:<26} runs={} last={}  {}", t.id, if t.enabled { "enabled" } else { "disabled" }, t.trigger.to_spec_string(), t.run_count, t.last_exit.map_or("-".into(), |c| c.to_string()), if t.os_registered { "[os]" } else { "" });
                }
                Ok(0)
            }
            "remove" | "rm" => {
                let id = need_id()?;
                if store.get(&id).map_or(false, |t| t.os_registered) {
                    let _ = nin_sched::os_unregister(&id);
                }
                store.remove(&id).map_err(|e| e.to_string())?;
                store.save().map_err(|e| e.to_string())?;
                println!("removed {}", id);
                Ok(0)
            }
            "enable" | "disable" => {
                let id = need_id()?;
                store.set_enabled(&id, sub == "enable").map_err(|e| e.to_string())?;
                store.save().map_err(|e| e.to_string())?;
                println!("{}d {}", sub, id);
                Ok(0)
            }
            "run" => {
                let id = need_id()?;
                let t = store.get(&id).cloned().ok_or(format!("no such task `{}`", id))?;
                let code = run_task(&t);
                store.record_run(&id, &chrono::Local::now().naive_local(), code);
                store.save().map_err(|e| e.to_string())?;
                Ok(code)
            }
            "daemon" => {
                println!("scheduler daemon running ({} task(s)); Ctrl+C to stop", store.list().len());
                let _ = nin_win::install_ctrlc_handler(&CANCEL);
                let mut runner = nin_sched::Runner::new(store);
                runner.run_startup_tasks(true);
                runner.run_loop(&CANCEL, std::time::Duration::from_secs(1), &mut |t| {
                    println!("[{}] running {}", chrono::Local::now().format("%F %T"), t.id);
                    run_task(t)
                });
                Ok(0)
            }
            "register" => {
                let id = need_id()?;
                let t = store.get(&id).cloned().ok_or(format!("no such task `{}`", id))?;
                nin_sched::os_register(&t).map_err(|e| e.to_string())?;
                store.set_os_registered(&id, true).map_err(|e| e.to_string())?;
                store.save().map_err(|e| e.to_string())?;
                println!("registered {} with Windows Task Scheduler", id);
                Ok(0)
            }
            "unregister" => {
                let id = need_id()?;
                nin_sched::os_unregister(&id).map_err(|e| e.to_string())?;
                store.set_os_registered(&id, false).map_err(|e| e.to_string())?;
                store.save().map_err(|e| e.to_string())?;
                println!("unregistered {}", id);
                Ok(0)
            }
            _ => Err(format!("unknown schedule command `{}`", sub)),
        }
    })();
    match r {
        Ok(c) => c,
        Err(e) => {
            eprintln_err(&e);
            1
        }
    }
}

fn assoc(a: Vec<String>) -> i32 {
    match a.first().map(|s| s.as_str()) {
        Some("install") => match nin_win::install_assoc(&exe_path()) {
            Ok(()) => {
                println!(".nin files are now associated with {}", exe_path().display());
                0
            }
            Err(e) => {
                eprintln_err(&e.to_string());
                1
            }
        },
        Some("uninstall") => match nin_win::uninstall_assoc() {
            Ok(()) => {
                println!("association removed");
                0
            }
            Err(e) => {
                eprintln_err(&e.to_string());
                1
            }
        },
        Some("status") => {
            let s = nin_win::assoc_status();
            println!("installed: {}", s.installed);
            if let Some(c) = s.command {
                println!("command:   {}", c);
            }
            if let Some(i) = s.icon {
                println!("icon:      {}", i);
            }
            let _ = Ordering::SeqCst;
            0
        }
        _ => usage("assoc needs install|uninstall|status"),
    }
}
