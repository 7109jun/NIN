//! fs, path, process, sys, env, io, log.

use crate::util::*;
use indexmap::IndexMap;
use nin_runtime::builtins::{a_int, a_str};
use nin_runtime::*;
use std::io::{BufRead, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

fn p(ex: &mut Exec, a: &[Value], i: usize, f: &str) -> R<PathBuf> {
    Ok(PathBuf::from(a_str(ex, a, i, f)?))
}

fn pv(p: &Path) -> Value {
    Value::str(p.to_string_lossy())
}

pub fn fs() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    m.f("read_text", 1, Some(1), |ex, a| {
        let path = p(ex, a, 0, "read_text")?;
        match std::fs::read(&path) {
            Ok(b) => Ok(Value::str(String::from_utf8_lossy(&b).trim_start_matches('\u{feff}'))),
            Err(e) => io_err(ex, e, &format!("cannot read {}", path.display())),
        }
    });
    m.f("write_text", 2, Some(2), |ex, a| {
        let path = p(ex, a, 0, "write_text")?;
        let t = ex.to_display(&a[1])?;
        match std::fs::write(&path, t) {
            Ok(_) => Ok(Value::Null),
            Err(e) => io_err(ex, e, &format!("cannot write {}", path.display())),
        }
    });
    m.f("append_text", 2, Some(2), |ex, a| {
        let path = p(ex, a, 0, "append_text")?;
        let t = ex.to_display(&a[1])?;
        let r = std::fs::OpenOptions::new().create(true).append(true).open(&path).and_then(|mut f| f.write_all(t.as_bytes()));
        match r {
            Ok(_) => Ok(Value::Null),
            Err(e) => io_err(ex, e, &format!("cannot append to {}", path.display())),
        }
    });
    m.f("read_bytes", 1, Some(1), |ex, a| {
        let path = p(ex, a, 0, "read_bytes")?;
        match std::fs::read(&path) {
            Ok(b) => Ok(bytes_value(&b)),
            Err(e) => io_err(ex, e, &format!("cannot read {}", path.display())),
        }
    });
    m.f("write_bytes", 2, Some(2), |ex, a| {
        let path = p(ex, a, 0, "write_bytes")?;
        let b = bytes_of(ex, &a[1], "write_bytes")?;
        match std::fs::write(&path, b) {
            Ok(_) => Ok(Value::Null),
            Err(e) => io_err(ex, e, &format!("cannot write {}", path.display())),
        }
    });
    m.f("exists", 1, Some(1), |ex, a| Ok(Value::Bool(p(ex, a, 0, "exists")?.exists())));
    m.f("is_file", 1, Some(1), |ex, a| Ok(Value::Bool(p(ex, a, 0, "is_file")?.is_file())));
    m.f("is_dir", 1, Some(1), |ex, a| Ok(Value::Bool(p(ex, a, 0, "is_dir")?.is_dir())));
    m.f("list_dir", 1, Some(1), |ex, a| {
        let path = p(ex, a, 0, "list_dir")?;
        match std::fs::read_dir(&path) {
            Ok(rd) => {
                let mut names: Vec<String> = rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).collect();
                names.sort();
                Ok(Value::list(names.into_iter().map(Value::str).collect()))
            }
            Err(e) => io_err(ex, e, &format!("cannot list {}", path.display())),
        }
    });
    m.f("mkdir", 1, Some(1), |ex, a| {
        let path = p(ex, a, 0, "mkdir")?;
        std::fs::create_dir(&path).or_else(|e| io_err(ex, e, &format!("cannot create {}", path.display()))).map(|_| Value::Null)
    });
    m.f("mkdir_all", 1, Some(1), |ex, a| {
        let path = p(ex, a, 0, "mkdir_all")?;
        std::fs::create_dir_all(&path).or_else(|e| io_err(ex, e, &format!("cannot create {}", path.display()))).map(|_| Value::Null)
    });
    m.f("remove", 1, Some(1), |ex, a| {
        let path = p(ex, a, 0, "remove")?;
        let r = if path.is_dir() { std::fs::remove_dir(&path) } else { std::fs::remove_file(&path) };
        r.or_else(|e| io_err(ex, e, &format!("cannot remove {}", path.display()))).map(|_| Value::Null)
    });
    m.f("remove_all", 1, Some(1), |ex, a| {
        let path = p(ex, a, 0, "remove_all")?;
        if !path.exists() {
            return Ok(Value::Null);
        }
        let r = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
        r.or_else(|e| io_err(ex, e, &format!("cannot remove {}", path.display()))).map(|_| Value::Null)
    });
    m.f("copy", 2, Some(2), |ex, a| {
        let (s, d) = (p(ex, a, 0, "copy")?, p(ex, a, 1, "copy")?);
        std::fs::copy(&s, &d).or_else(|e| io_err(ex, e, &format!("cannot copy {} to {}", s.display(), d.display()))).map(|n| Value::Int(n as i64))
    });
    m.f("rename", 2, Some(2), |ex, a| {
        let (s, d) = (p(ex, a, 0, "rename")?, p(ex, a, 1, "rename")?);
        std::fs::rename(&s, &d).or_else(|e| io_err(ex, e, &format!("cannot rename {} to {}", s.display(), d.display()))).map(|_| Value::Null)
    });
    m.f("metadata", 1, Some(1), |ex, a| {
        let path = p(ex, a, 0, "metadata")?;
        match std::fs::metadata(&path) {
            Ok(md) => {
                let mt = md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0);
                Ok(Value::map_from([("size", Value::Int(md.len() as i64)), ("is_file", Value::Bool(md.is_file())), ("is_dir", Value::Bool(md.is_dir())), ("modified_ms", Value::Int(mt)), ("readonly", Value::Bool(md.permissions().readonly()))]))
            }
            Err(e) => io_err(ex, e, &format!("cannot stat {}", path.display())),
        }
    });
    m.f("glob", 1, Some(1), |ex, a| {
        let pat = a_str(ex, a, 0, "glob")?.replace('\\', "/");
        Ok(Value::list(glob(&pat).into_iter().map(Value::str).collect()))
    });
    m.f("walk", 1, Some(1), |ex, a| {
        let path = p(ex, a, 0, "walk")?;
        let mut out = vec![];
        walk(&path, &mut out, 0);
        out.sort();
        Ok(Value::list(out.into_iter().map(Value::str).collect()))
    });
    m.f("temp_file", 0, Some(1), |ex, a| {
        let prefix = if a.is_empty() { "nin".to_string() } else { a_str(ex, a, 0, "temp_file")? };
        let path = unique_temp(&prefix);
        match std::fs::write(&path, b"") {
            Ok(_) => Ok(pv(&path)),
            Err(e) => io_err(ex, e, "cannot create temp file"),
        }
    });
    m.f("temp_dir", 0, Some(1), |ex, a| {
        if a.is_empty() {
            return Ok(pv(&std::env::temp_dir()));
        }
        let prefix = a_str(ex, a, 0, "temp_dir")?;
        let path = unique_temp(&prefix);
        match std::fs::create_dir_all(&path) {
            Ok(_) => Ok(pv(&path)),
            Err(e) => io_err(ex, e, "cannot create temp dir"),
        }
    });
    m.f("canonicalize", 1, Some(1), |ex, a| {
        let path = p(ex, a, 0, "canonicalize")?;
        match std::fs::canonicalize(&path) {
            Ok(c) => Ok(pv(&strip_unc(c))),
            Err(e) => io_err(ex, e, &format!("cannot resolve {}", path.display())),
        }
    });
    m.f("cwd", 0, Some(0), |ex, _| match std::env::current_dir() {
        Ok(d) => Ok(pv(&d)),
        Err(e) => io_err(ex, e, "cannot get cwd"),
    });
    m.f("chdir", 1, Some(1), |ex, a| {
        let path = p(ex, a, 0, "chdir")?;
        std::env::set_current_dir(&path).or_else(|e| io_err(ex, e, &format!("cannot chdir to {}", path.display()))).map(|_| Value::Null)
    });
    m.done()
}

fn strip_unc(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}

fn unique_temp(prefix: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    std::env::temp_dir().join(format!("{}-{}-{}-{}", prefix, std::process::id(), t, N.fetch_add(1, Ordering::SeqCst)))
}

fn walk(dir: &Path, out: &mut Vec<String>, depth: usize) {
    if depth > 64 {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let path = e.path();
            out.push(path.to_string_lossy().replace('\\', "/"));
            if path.is_dir() && !e.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
                walk(&path, out, depth + 1);
            }
        }
    }
}

fn seg_match(p: &[char], s: &[char]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some('*') => (0..=s.len()).any(|i| seg_match(&p[1..], &s[i..])),
        Some('?') => !s.is_empty() && seg_match(&p[1..], &s[1..]),
        Some(c) => !s.is_empty() && s[0].eq_ignore_ascii_case(c) && seg_match(&p[1..], &s[1..]),
    }
}

fn path_match(pat: &[&str], path: &[&str]) -> bool {
    match pat.first() {
        None => path.is_empty(),
        Some(&"**") => (0..=path.len()).any(|i| path_match(&pat[1..], &path[i..])),
        Some(seg) => {
            !path.is_empty() && seg_match(&seg.chars().collect::<Vec<_>>(), &path[0].chars().collect::<Vec<_>>()) && path_match(&pat[1..], &path[1..])
        }
    }
}

pub fn glob(pattern: &str) -> Vec<String> {
    let segs: Vec<&str> = pattern.split('/').collect();
    let lit = segs.iter().take_while(|s| !s.contains(['*', '?'])).count().min(segs.len().saturating_sub(1));
    let base = if lit == 0 { if pattern.starts_with('/') { "/".to_string() } else { ".".to_string() } } else { segs[..lit].join("/") };
    let mut all = vec![];
    walk(Path::new(&base), &mut all, 0);
    let pat_segs: Vec<&str> = segs.iter().copied().filter(|s| !s.is_empty() || false).collect();
    let mut out: Vec<String> = all
        .into_iter()
        .filter(|f| {
            let rel = f.strip_prefix("./").unwrap_or(f);
            let psegs: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
            path_match(&pat_segs, &psegs)
        })
        .map(|f| f.strip_prefix("./").map(|s| s.to_string()).unwrap_or(f))
        .collect();
    out.sort();
    out
}

fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out: Vec<Component> = vec![];
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(c),
            },
            other => out.push(other),
        }
    }
    if out.is_empty() {
        return PathBuf::from(".");
    }
    out.iter().collect()
}

pub fn path() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    m.f("join", 1, None, |ex, a| {
        let mut pb = PathBuf::new();
        for (i, _) in a.iter().enumerate() {
            pb.push(a_str(ex, a, i, "join")?);
        }
        Ok(pv(&pb))
    });
    m.f("parent", 1, Some(1), |ex, a| Ok(p(ex, a, 0, "parent")?.parent().map(|x| pv(x)).unwrap_or(Value::Null)));
    m.f("file_name", 1, Some(1), |ex, a| Ok(p(ex, a, 0, "file_name")?.file_name().map(|x| Value::str(x.to_string_lossy())).unwrap_or(Value::Null)));
    m.f("stem", 1, Some(1), |ex, a| Ok(p(ex, a, 0, "stem")?.file_stem().map(|x| Value::str(x.to_string_lossy())).unwrap_or(Value::Null)));
    m.f("ext", 1, Some(1), |ex, a| Ok(p(ex, a, 0, "ext")?.extension().map(|x| Value::str(x.to_string_lossy())).unwrap_or(Value::str(""))));
    m.f("with_ext", 2, Some(2), |ex, a| {
        let e = a_str(ex, a, 1, "with_ext")?;
        Ok(pv(&p(ex, a, 0, "with_ext")?.with_extension(e.trim_start_matches('.'))))
    });
    m.f("is_absolute", 1, Some(1), |ex, a| Ok(Value::Bool(p(ex, a, 0, "is_absolute")?.is_absolute())));
    m.f("normalize", 1, Some(1), |ex, a| Ok(pv(&lexical_normalize(&p(ex, a, 0, "normalize")?))));
    m.f("relative_to", 2, Some(2), |ex, a| {
        let (x, b) = (lexical_normalize(&p(ex, a, 0, "relative_to")?), lexical_normalize(&p(ex, a, 1, "relative_to")?));
        Ok(match x.strip_prefix(&b) {
            Ok(r) if r.as_os_str().is_empty() => Value::str("."),
            Ok(r) => pv(r),
            Err(_) => Value::Null,
        })
    });
    m.c("sep", Value::str(std::path::MAIN_SEPARATOR.to_string()));
    m.done()
}

// ───────── process ─────────

fn proc_spec(ex: &mut Exec, program: String, args: Vec<String>, shell: bool, opts: &IndexMap<String, Value>) -> R<(nin_win::ProcSpec, Option<std::time::Duration>, bool)> {
    let mut spec = if shell {
        nin_win::ProcSpec::shell(program)
    } else {
        nin_win::ProcSpec::new(program).args(args)
    };
    if let Some(v) = opts.get("cwd") {
        if !matches!(v, Value::Null) {
            spec.cwd = Some(PathBuf::from(ex.to_display(v)?));
        }
    }
    if let Some(Value::Map(e)) = opts.get("env") {
        for (k, v) in lk(e).clone() {
            spec.env.push((k, ex.to_display(&v)?));
        }
    }
    if let Some(v) = opts.get("stdin") {
        if !matches!(v, Value::Null) {
            spec.stdin = nin_win::StdinSpec::Text(ex.to_display(v)?);
        }
    }
    let capture = !matches!(opts.get("capture"), Some(Value::Bool(false)));
    if capture {
        spec.stdout = nin_win::OutSpec::Capture;
        spec.stderr = nin_win::OutSpec::Capture;
        if !opts.contains_key("stdin") {
            spec.stdin = nin_win::StdinSpec::Null;
        }
    }
    let timeout = match opts.get("timeout") {
        Some(v) if !matches!(v, Value::Null) => Some(std::time::Duration::from_millis(ex.duration_ms(v)?)),
        _ => None,
    };
    let check = matches!(opts.get("check"), Some(Value::Bool(true)));
    Ok((spec, timeout, check))
}

fn run_with(ex: &mut Exec, program: String, args: Vec<String>, shell: bool, opts: IndexMap<String, Value>) -> R<Value> {
    let (spec, timeout, check) = proc_spec(ex, program.clone(), args, shell, &opts)?;
    let res = ex.run_process(&spec, timeout)?;
    let rv = ex.proc_result_value(&res);
    if res.timed_out && check {
        return ex.err("TimeoutError", format!("command `{}` timed out", program));
    }
    if check && res.code != 0 {
        let ev = ex.make_error("ProcessError", format!("command `{}` exited with code {}", program, res.code));
        if let Value::Object(o) = &ev {
            lk(&o.fields).insert("result".into(), rv);
        }
        return Err(ex.throw_value(ev));
    }
    Ok(rv)
}

pub fn process() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    m.f("run", 1, Some(3), |ex, a| {
        let prog = a_str(ex, a, 0, "run")?;
        let (args, opts) = match a.get(1) {
            Some(Value::List(_)) => (strs(ex, a, 1, "run")?, opt_map(ex, a, 2, "run")?),
            Some(Value::Map(_)) => (vec![], opt_map(ex, a, 1, "run")?),
            None | Some(Value::Null) => (vec![], IndexMap::new()),
            Some(o) => return ex.err("TypeError", format!("run: arguments must be a list, got {}", o.type_name())),
        };
        run_with(ex, prog, args, false, opts)
    });
    m.f("shell", 1, Some(2), |ex, a| {
        let line = a_str(ex, a, 0, "shell")?;
        let opts = opt_map(ex, a, 1, "shell")?;
        run_with(ex, line, vec![], true, opts)
    });
    m.f("spawn", 1, Some(3), |ex, a| {
        let prog = a_str(ex, a, 0, "spawn")?;
        let args = if a.len() > 1 { strs(ex, a, 1, "spawn")? } else { vec![] };
        let mut cmd = std::process::Command::new(&prog);
        cmd.args(&args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        let opts = opt_map(ex, a, 2, "spawn")?;
        if let Some(v) = opts.get("cwd") {
            cmd.current_dir(ex.to_display(v)?);
        }
        match cmd.spawn() {
            Ok(c) => Ok(Value::Int(c.id() as i64)),
            Err(e) => io_err(ex, e, &format!("cannot start {}", prog)),
        }
    });
    m.f("pid", 0, Some(0), |_, _| Ok(Value::Int(std::process::id() as i64)));
    m.f("kill", 1, Some(1), |ex, a| {
        let pid = a_int(ex, a, 0, "kill")?;
        match nin_win::kill_pid(pid as u32) {
            Ok(_) => Ok(Value::Bool(true)),
            Err(e) => io_err(ex, e, &format!("cannot kill {}", pid)),
        }
    });
    let info = |pi: &nin_win::ProcInfo| Value::map_from([("pid", Value::Int(pi.pid as i64)), ("ppid", Value::Int(pi.ppid as i64)), ("name", Value::str(&pi.name)), ("threads", Value::Int(pi.threads as i64))]);
    m.f("list", 0, Some(0), move |ex, _| match nin_win::list_processes() {
        Ok(v) => Ok(Value::list(v.iter().map(info).collect())),
        Err(e) => io_err(ex, e, "cannot list processes"),
    });
    m.f("find", 1, Some(1), move |ex, a| {
        let n = a_str(ex, a, 0, "find")?;
        Ok(Value::list(nin_win::find_processes(&n).iter().map(info).collect()))
    });
    m.f("exit", 0, Some(1), |ex, a| {
        let c = if a.is_empty() { 0 } else { a_int(ex, a, 0, "exit")? };
        Err(Signal::Exit(c as i32))
    });
    m.f("which", 1, Some(1), |ex, a| {
        let n = a_str(ex, a, 0, "which")?;
        Ok(nin_win::process::resolve_program(&n, None).map(|p| pv(&p)).unwrap_or(Value::Null))
    });
    m.f("env", 1, Some(2), |ex, a| {
        let n = a_str(ex, a, 0, "env")?;
        Ok(std::env::var(&n).map(Value::str).unwrap_or_else(|_| a.get(1).cloned().unwrap_or(Value::Null)))
    });
    m.f("set_env", 2, Some(2), |ex, a| {
        let (k, v) = (a_str(ex, a, 0, "set_env")?, ex.to_display(&a[1])?);
        std::env::set_var(k, v);
        Ok(Value::Null)
    });
    m.done()
}

// ───────── sys / env ─────────

pub fn sys() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    m.f("info", 0, Some(0), |_, _| {
        let i = nin_win::sys_info();
        Ok(Value::map_from([
            ("os", Value::str(&i.os)),
            ("arch", Value::str(&i.arch)),
            ("hostname", Value::str(&i.hostname)),
            ("username", Value::str(&i.username)),
            ("cpu_count", Value::Int(i.cpu_count as i64)),
            ("total_memory", Value::Int(i.total_memory as i64)),
            ("free_memory", Value::Int(i.free_memory as i64)),
            ("os_version", Value::str(&i.os_version)),
            ("uptime_secs", Value::Int(i.uptime_secs as i64)),
        ]))
    });
    m.f("os", 0, Some(0), |_, _| Ok(Value::str(nin_win::sys_info().os)));
    m.f("arch", 0, Some(0), |_, _| Ok(Value::str(nin_win::sys_info().arch)));
    m.f("hostname", 0, Some(0), |_, _| Ok(Value::str(nin_win::sys_info().hostname)));
    m.f("username", 0, Some(0), |_, _| Ok(Value::str(nin_win::sys_info().username)));
    m.f("cpu_count", 0, Some(0), |_, _| Ok(Value::Int(nin_win::sys_info().cpu_count as i64)));
    m.f("uptime", 0, Some(0), |_, _| Ok(Value::Int(nin_win::sys_info().uptime_secs as i64)));
    m.f("memory", 0, Some(0), |_, _| {
        let i = nin_win::sys_info();
        Ok(Value::map_from([("total", Value::Int(i.total_memory as i64)), ("free", Value::Int(i.free_memory as i64))]))
    });
    m.f("known_dir", 1, Some(1), |ex, a| {
        use nin_win::KnownDir::*;
        let n = a_str(ex, a, 0, "known_dir")?;
        let k = match n.as_str() {
            "home" => Home,
            "appdata" => AppData,
            "localappdata" => LocalAppData,
            "desktop" => Desktop,
            "temp" => Temp,
            "documents" => Documents,
            "downloads" => Downloads,
            _ => return ex.err("ValueError", format!("unknown directory `{}` (home, appdata, localappdata, desktop, temp, documents, downloads)", n)),
        };
        Ok(nin_win::known_dir(k).map(|p| pv(&p)).unwrap_or(Value::Null))
    });
    m.done()
}

pub fn env() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    m.f("get", 1, Some(2), |ex, a| {
        let n = a_str(ex, a, 0, "get")?;
        Ok(std::env::var(&n).map(Value::str).unwrap_or_else(|_| a.get(1).cloned().unwrap_or(Value::Null)))
    });
    m.f("set", 2, Some(2), |ex, a| {
        let (k, v) = (a_str(ex, a, 0, "set")?, ex.to_display(&a[1])?);
        std::env::set_var(k, v);
        Ok(Value::Null)
    });
    m.f("remove", 1, Some(1), |ex, a| {
        std::env::remove_var(a_str(ex, a, 0, "remove")?);
        Ok(Value::Null)
    });
    m.f("all", 0, Some(0), |_, _| Ok(Value::map(nin_win::env_vars().into_iter().map(|(k, v)| (k, Value::str(v))).collect())));
    m.f("expand", 1, Some(1), |ex, a| {
        let s = a_str(ex, a, 0, "expand")?;
        let mut out = String::new();
        let cs: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < cs.len() {
            let c = cs[i];
            let close = match c { '%' => Some('%'), '$' if cs.get(i + 1) == Some(&'{') => Some('}'), _ => None };
            if let Some(cl) = close {
                let start = if c == '%' { i + 1 } else { i + 2 };
                if let Some(off) = cs[start..].iter().position(|x| *x == cl) {
                    let name: String = cs[start..start + off].iter().collect();
                    if !name.is_empty() {
                        out.push_str(&std::env::var(&name).unwrap_or_default());
                        i = start + off + 1;
                        continue;
                    }
                }
            }
            out.push(c);
            i += 1;
        }
        Ok(Value::str(out))
    });
    m.done()
}

pub fn io() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    m.f("read_line", 0, Some(0), |ex, _| {
        let mut s = String::new();
        match std::io::stdin().lock().read_line(&mut s) {
            Ok(0) => Ok(Value::Null),
            Ok(_) => Ok(Value::str(s.trim_end_matches(['\r', '\n']))),
            Err(e) => io_err(ex, e, "cannot read stdin"),
        }
    });
    m.f("stdin_all", 0, Some(0), |ex, _| {
        let mut s = String::new();
        match std::io::stdin().lock().read_to_string(&mut s) {
            Ok(_) => Ok(Value::str(s)),
            Err(e) => io_err(ex, e, "cannot read stdin"),
        }
    });
    m.f("prompt", 1, Some(1), |ex, a| {
        let t = ex.to_display(&a[0])?;
        ex.out(&t, false);
        let mut s = String::new();
        match std::io::stdin().lock().read_line(&mut s) {
            Ok(0) => Ok(Value::Null),
            Ok(_) => Ok(Value::str(s.trim_end_matches(['\r', '\n']))),
            Err(e) => io_err(ex, e, "cannot read stdin"),
        }
    });
    m.f("confirm", 1, Some(1), |ex, a| {
        let t = ex.to_display(&a[0])?;
        ex.out(&format!("{} [y/N] ", t), false);
        let mut s = String::new();
        let _ = std::io::stdin().lock().read_line(&mut s);
        Ok(Value::Bool(matches!(s.trim().to_ascii_lowercase().as_str(), "y" | "yes")))
    });
    m.f("write", 1, None, |ex, a| {
        let mut t = String::new();
        for v in a {
            t.push_str(&ex.to_display(v)?);
        }
        ex.out(&t, false);
        Ok(Value::Null)
    });
    m.f("ewrite", 1, None, |ex, a| {
        let mut t = String::new();
        for v in a {
            t.push_str(&ex.to_display(v)?);
        }
        ex.out(&t, true);
        Ok(Value::Null)
    });
    m.done()
}

fn level_num(s: &str) -> i32 {
    match s.to_ascii_lowercase().as_str() {
        "debug" => 0,
        "info" => 1,
        "warn" | "warning" => 2,
        "error" => 3,
        "off" | "none" => 9,
        _ => 1,
    }
}

pub fn log() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    for (name, lvl) in [("debug", 0), ("info", 1), ("warn", 2), ("error", 3)] {
        m.f(name, 1, None, move |ex, a| {
            let min = level_num(&std::env::var("NIN_LOG").unwrap_or_else(|_| "info".into()));
            if lvl < min {
                return Ok(Value::Null);
            }
            let mut parts = vec![];
            for v in a {
                parts.push(ex.to_display(v)?);
            }
            let ts = chrono::Local::now().format("%H:%M:%S%.3f");
            ex.out(&format!("{} [{:<5}] {}\n", ts, name.to_uppercase(), parts.join(" ")), true);
            Ok(Value::Null)
        });
    }
    m.done()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn glob_matching() {
        assert!(path_match(&["src", "**", "*.nin"], &["src", "a", "b", "x.nin"]));
        assert!(path_match(&["src", "**", "*.nin"], &["src", "x.nin"]));
        assert!(!path_match(&["src", "*.nin"], &["src", "a", "x.nin"]));
        assert!(path_match(&["?.txt"], &["a.txt"]));
        assert!(!path_match(&["?.txt"], &["ab.txt"]));
    }
    #[test]
    fn normalize() {
        assert_eq!(lexical_normalize(Path::new("a/./b/../c")), PathBuf::from("a/c"));
        assert_eq!(lexical_normalize(Path::new("../a")), PathBuf::from("../a"));
        assert_eq!(lexical_normalize(Path::new("a/..")), PathBuf::from("."));
    }
}
