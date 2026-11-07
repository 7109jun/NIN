use crate::report::*;
use crate::sink::*;
use indexmap::IndexMap;
use nin_ast::*;
use nin_diag::Span;
use nin_runtime::builtins::value_to_json;
use nin_runtime::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct Options {
    /// `name=value` workflow inputs
    pub inputs: Vec<(String, String)>,
    /// run only these jobs (plus their dependencies); empty = all
    pub jobs: Vec<String>,
    pub max_parallel: Option<usize>,
    pub dry_run: bool,
    /// default working directory of run steps (the script directory)
    pub base_dir: PathBuf,
    pub sink: Option<Arc<dyn Sink>>,
    /// set from outside (Ctrl+C) to cancel the whole workflow
    pub cancel: Arc<AtomicBool>,
}

impl Default for Options {
    fn default() -> Self {
        Options { inputs: vec![], jobs: vec![], max_parallel: None, dry_run: false, base_dir: PathBuf::from("."), sink: None, cancel: Arc::new(AtomicBool::new(false)) }
    }
}

// ───────────────────────── planning ─────────────────────────

/// Validate the job graph and return the execution levels (jobs of one level can run in parallel).
pub fn plan(wf: &Workflow) -> Result<Vec<Vec<String>>, String> {
    let ids: Vec<&str> = wf.jobs.iter().map(|j| j.id.as_str()).collect();
    for j in &wf.jobs {
        for n in &j.needs {
            if !ids.contains(&n.as_str()) {
                return Err(format!("job `{}` needs unknown job `{}`", j.id, n));
            }
        }
    }
    let mut level: HashMap<&str, usize> = HashMap::new();
    let mut remaining: Vec<&Job> = wf.jobs.iter().collect();
    let mut levels: Vec<Vec<String>> = vec![];
    while !remaining.is_empty() {
        let (ready, rest): (Vec<&Job>, Vec<&Job>) = remaining.into_iter().partition(|j| j.needs.iter().all(|n| level.contains_key(n.as_str())));
        if ready.is_empty() {
            // find a cycle path for the message
            let start = rest[0];
            let mut path = vec![start.id.as_str()];
            let mut cur = start;
            loop {
                let next = cur.needs.iter().find(|n| rest.iter().any(|r| r.id == **n)).map(|s| s.as_str()).unwrap_or(start.id.as_str());
                if path.contains(&next) {
                    path.push(next);
                    let from = path.iter().position(|p| *p == next).unwrap();
                    return Err(format!("dependency cycle: {}", path[from..].join(" -> ")));
                }
                path.push(next);
                cur = rest.iter().find(|r| r.id == next).copied().unwrap_or(start);
            }
        }
        let lv = levels.len();
        for j in &ready {
            level.insert(j.id.as_str(), lv);
        }
        levels.push(ready.iter().map(|j| j.id.clone()).collect());
        remaining = rest;
    }
    Ok(levels)
}

pub fn graph_text(wf: &Workflow) -> Result<String, String> {
    let levels = plan(wf)?;
    let mut s = String::new();
    for (i, l) in levels.iter().enumerate() {
        s.push_str(&format!("stage {}  (parallel)\n", i + 1));
        for id in l {
            let j = wf.jobs.iter().find(|j| &j.id == id).unwrap();
            let needs = if j.needs.is_empty() { String::new() } else { format!("  <- {}", j.needs.join(", ")) };
            let mx = if j.matrix.is_some() { "  [matrix]" } else { "" };
            s.push_str(&format!("  {}{}{}  ({} step{})\n", id, mx, needs, j.steps.len(), if j.steps.len() == 1 { "" } else { "s" }));
        }
    }
    Ok(s)
}

// ───────────────────────── helpers ─────────────────────────

fn thrown_message(sig: &Signal) -> String {
    match sig {
        Signal::Throw(t) => {
            let (k, m) = error_parts(&t.value);
            format!("{}: {}", k, m)
        }
        Signal::Exit(c) => format!("script called exit({})", c),
    }
}

fn is_cancelled_sig(sig: &Signal) -> bool {
    matches!(sig, Signal::Throw(t) if error_parts(&t.value).0 == "CancelledError")
}

fn to_json_loose(v: &Value) -> serde_json::Value {
    value_to_json(v).unwrap_or_else(|_| serde_json::Value::String(display_simple(v)))
}

fn map_json(m: &IndexMap<String, Value>) -> serde_json::Value {
    serde_json::Value::Object(m.iter().map(|(k, v)| (k.clone(), to_json_loose(v))).collect())
}

fn eval_str(ex: &mut Exec, e: &Expr, fr: &Frame) -> R<String> {
    let v = ex.eval(e, fr)?;
    ex.to_display(&v)
}

fn eval_env(ex: &mut Exec, items: &[(String, Expr)], fr: &Frame) -> R<Vec<(String, String)>> {
    let mut out = vec![];
    for (k, e) in items {
        out.push((k.clone(), eval_str(ex, e, fr)?));
    }
    Ok(out)
}

fn eval_bool(ex: &mut Exec, e: &Expr, fr: &Frame) -> R<bool> {
    let v = ex.eval(e, fr)?;
    ex.truthy(v, e.span)
}

fn eval_opt_dur(ex: &mut Exec, e: &Option<Expr>, fr: &Frame) -> R<Option<Duration>> {
    match e {
        Some(e) => {
            let v = ex.eval(e, fr)?;
            Ok(Some(Duration::from_millis(ex.duration_ms(&v)?)))
        }
        None => Ok(None),
    }
}

fn eval_opt_int(ex: &mut Exec, e: &Option<Expr>, fr: &Frame) -> R<Option<i64>> {
    match e {
        Some(e) => match ex.eval(e, fr)? {
            Value::Int(i) => Ok(Some(i)),
            Value::Null => Ok(None),
            o => ex.err("TypeError", format!("expected int, got {}", o.type_name())),
        },
        None => Ok(None),
    }
}

fn render(ex: &Exec, sig: Signal) -> String {
    match &sig {
        Signal::Throw(t) => ex.interp.render_throw(t).trim_end().to_string(),
        Signal::Exit(c) => format!("exit({})", c),
    }
}

fn convert_input(s: &str, ty: &Option<TypeExpr>) -> Result<Value, String> {
    let tn = ty.as_ref().map(|t| t.display()).unwrap_or_else(|| "string".into());
    Ok(match tn.as_str() {
        "int" => Value::Int(s.parse::<i64>().map_err(|_| format!("`{}` is not an int", s))?),
        "float" => Value::Float(s.parse::<f64>().map_err(|_| format!("`{}` is not a float", s))?),
        "bool" => match s {
            "true" | "1" | "yes" => Value::Bool(true),
            "false" | "0" | "no" => Value::Bool(false),
            _ => return Err(format!("`{}` is not a bool", s)),
        },
        _ => Value::str(s),
    })
}

fn resolve_inputs(ex: &mut Exec, wf: &Workflow, given: &[(String, String)], fr: &Frame) -> Result<IndexMap<String, Value>, String> {
    let mut out = IndexMap::new();
    for (k, _) in given {
        if !wf.inputs.iter().any(|i| &i.name == k) {
            let names: Vec<&str> = wf.inputs.iter().map(|i| i.name.as_str()).collect();
            let hint = nin_diag::suggest(k, names.iter().copied()).map(|s| format!(" (did you mean `{}`?)", s)).unwrap_or_default();
            return Err(format!("unknown input `{}`{}", k, hint));
        }
    }
    for d in &wf.inputs {
        let v = if let Some((_, s)) = given.iter().rev().find(|(k, _)| k == &d.name) {
            convert_input(s, &d.ty).map_err(|e| format!("input `{}`: {}", d.name, e))?
        } else if let Some(def) = &d.default {
            match ex.eval(def, fr) {
                Ok(v) => v,
                Err(s) => return Err(format!("input `{}` default: {}", d.name, render(ex, s))),
            }
        } else if d.required {
            return Err(format!("missing required input `{}` (pass --input {}=VALUE)", d.name, d.name));
        } else {
            Value::Null
        };
        out.insert(d.name.clone(), v);
    }
    Ok(out)
}

fn expand_matrix(ex: &mut Exec, m: &Matrix, fr: &Frame) -> R<Vec<IndexMap<String, Value>>> {
    let mut dims: Vec<(String, Vec<Value>)> = vec![];
    for (name, e) in &m.dims {
        match ex.eval(e, fr)? {
            Value::List(l) => dims.push((name.clone(), lk(&l).clone())),
            o => return ex.err_at(e.span, "TypeError", format!("matrix dimension `{}` must be a list, got {}", name, o.type_name())),
        }
    }
    let mut combos: Vec<IndexMap<String, Value>> = vec![];
    if !dims.is_empty() {
        combos.push(IndexMap::new());
        for (name, vals) in &dims {
            let mut next = vec![];
            for c in &combos {
                for v in vals {
                    let mut c2 = c.clone();
                    c2.insert(name.clone(), v.clone());
                    next.push(c2);
                }
            }
            combos = next;
        }
    }
    let dim_names: Vec<&String> = dims.iter().map(|(n, _)| n).collect();
    let as_map = |ex: &mut Exec, v: Value, what: &str| -> R<IndexMap<String, Value>> {
        match v {
            Value::Map(m) => Ok(lk(&m).clone()),
            o => ex.err("TypeError", format!("matrix {} entries must be maps, got {}", what, o.type_name())),
        }
    };
    for e in &m.include {
        let v = ex.eval(e, fr)?;
        let inc = as_map(ex, v, "include")?;
        let keys: Vec<&String> = inc.keys().filter(|k| dim_names.contains(k)).collect();
        let matches = |c: &IndexMap<String, Value>| !keys.is_empty() && keys.iter().all(|k| c.get(*k).map_or(false, |x| values_equal(x, &inc[*k])));
        if combos.iter().any(|c| matches(c)) {
            for c in combos.iter_mut() {
                if matches(c) {
                    for (k, v) in &inc {
                        c.insert(k.clone(), v.clone());
                    }
                }
            }
        } else {
            combos.push(inc);
        }
    }
    for e in &m.exclude {
        let v = ex.eval(e, fr)?;
        let exc = as_map(ex, v, "exclude")?;
        combos.retain(|c| !exc.iter().all(|(k, v)| c.get(k).map_or(false, |x| values_equal(x, v))));
    }
    Ok(combos)
}

// ───────────────────────── shared state ─────────────────────────

struct Shared {
    interp: Arc<Interp>,
    env: Env,
    wf: Arc<Workflow>,
    inputs: Value,
    wf_env: Vec<(String, String)>,
    base_dir: PathBuf,
    sink: Arc<dyn Sink>,
    user_cancel: Arc<AtomicBool>,
    ff_cancel: Arc<AtomicBool>,
    run_id: String,
    start_ms: i64,
}

struct Inst {
    job: usize,
    key: String,
    matrix: IndexMap<String, Value>,
}

struct Outcome {
    report: JobReport,
    outputs: IndexMap<String, Value>,
    /// counts as success for dependents
    ok: bool,
    /// failed and not continue_on_error
    hard_fail: bool,
}

struct JobState {
    failed: AtomicBool,
}

struct Attempt {
    ok: bool,
    cancelled: bool,
    code: Option<i32>,
    stdout: String,
    stderr: String,
    outputs: IndexMap<String, Value>,
    error: Option<Value>,
    msg: Option<String>,
}

impl Attempt {
    fn failed(msg: String) -> Attempt {
        Attempt { ok: false, cancelled: false, code: None, stdout: String::new(), stderr: String::new(), outputs: IndexMap::new(), error: None, msg: Some(msg) }
    }
}

/// Run `f` with a watchdog that cancels it at `deadline`. Returns `(result, timed_out)`.
fn with_deadline<T>(ex: &mut Exec, deadline: Option<Instant>, f: impl FnOnce(&mut Exec) -> R<T>) -> (R<T>, bool) {
    let Some(dl) = deadline else { return (f(ex), false) };
    let tok = Arc::new(AtomicBool::new(false));
    let timed = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    ex.cancel.push(tok.clone());
    let (t2, ti2, d2) = (tok.clone(), timed.clone(), done.clone());
    let h = std::thread::spawn(move || {
        while !d2.load(Ordering::SeqCst) {
            if Instant::now() >= dl {
                ti2.store(true, Ordering::SeqCst);
                t2.store(true, Ordering::SeqCst);
                return;
            }
            std::thread::sleep(Duration::from_millis(3));
        }
    });
    let r = f(ex);
    done.store(true, Ordering::SeqCst);
    let _ = h.join();
    ex.cancel.pop();
    (r, timed.load(Ordering::SeqCst))
}

struct JobRun<'a> {
    sh: &'a Shared,
    job: &'a Job,
    inst: &'a Inst,
    ex: Exec,
    base: Frame,
    state: Arc<JobState>,
    steps_map: MapRef,
    wf_job_env: Vec<(String, String)>,
    deadline: Option<Instant>,
    prefix: Arc<PrefixSink>,
}

impl<'a> JobRun<'a> {
    fn cancelled(&self) -> bool {
        self.ex.is_cancelled()
    }

    fn remaining(&self) -> Option<Duration> {
        self.deadline.map(|d| d.saturating_duration_since(Instant::now()))
    }

    fn eff_timeout(&self, step: Option<Duration>) -> Option<Duration> {
        match (step, self.remaining()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn step_frame(&mut self, step_env: &[(String, String)]) -> Frame {
        let fr = self.base.child();
        let mut all: IndexMap<String, Value> = self.wf_job_env.iter().map(|(k, v)| (k.clone(), Value::str(v))).collect();
        for (k, v) in step_env {
            all.insert(k.clone(), Value::str(v));
        }
        fr.env.define("env", Value::map(all), false);
        fr
    }

    fn attempt_run(&mut self, step: &Step, r: &RunStmt, fr: &Frame, env_pairs: &[(String, String)], step_tmo: Option<Duration>) -> Attempt {
        let (mut spec, run_tmo) = match self.ex.build_proc_spec(r, fr) {
            Ok(x) => x,
            Err(s) => {
                let cancelled = is_cancelled_sig(&s);
                let mut a = Attempt::failed(thrown_message(&s));
                a.cancelled = cancelled;
                if let Signal::Throw(t) = &s {
                    a.error = Some(t.value.clone());
                }
                return a;
            }
        };
        if matches!(spec.stdout, nin_win::OutSpec::Inherit) {
            spec.stdout = nin_win::OutSpec::Capture;
        }
        if matches!(spec.stderr, nin_win::OutSpec::Inherit) {
            spec.stderr = nin_win::OutSpec::Capture;
        }
        let mut envs: Vec<(String, String)> = env_pairs.to_vec();
        envs.extend(spec.env.clone());
        spec.env = envs;
        if spec.cwd.is_none() {
            let wd = match (&step.workdir, &self.job.workdir) {
                (Some(e), _) | (None, Some(e)) => eval_str(&mut self.ex, e, fr).ok(),
                _ => None,
            };
            spec.cwd = Some(match wd {
                Some(w) => self.sh.base_dir.join(w),
                None => self.sh.base_dir.clone(),
            });
        } else if let Some(c) = &spec.cwd {
            if c.is_relative() {
                spec.cwd = Some(self.sh.base_dir.join(c));
            }
        }
        let tmo = {
            let t = match (step_tmo, run_tmo) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            self.eff_timeout(t)
        };
        match self.ex.run_process(&spec, tmo) {
            Ok(res) => {
                let (so, se) = (res.stdout_text(), res.stderr_text());
                if !so.is_empty() {
                    self.ex.out(&so, false);
                }
                if !se.is_empty() {
                    self.ex.out(&se, true);
                }
                let label = if spec.program.is_empty() { spec.shell_line.clone().unwrap_or_default() } else { spec.program.clone() };
                let mut a = Attempt { ok: res.ok() && !res.limit_exceeded, cancelled: false, code: Some(res.code), stdout: so, stderr: se, outputs: IndexMap::new(), error: None, msg: None };
                if res.limit_exceeded {
                    a.msg = Some(format!("`{}` exceeded the job's resource limits", label));
                } else if res.timed_out {
                    a.msg = Some(format!("`{}` timed out after {} ms", label, res.duration.as_millis()));
                } else if res.code != 0 {
                    a.msg = Some(format!("`{}` exited with code {}", label, res.code));
                }
                if !a.ok {
                    let ev = self.ex.make_error(if res.timed_out { "TimeoutError" } else { "ProcessError" }, a.msg.clone().unwrap_or_default());
                    a.error = Some(ev);
                }
                // outputs
                if a.ok || true {
                    let ofr = fr.child();
                    ofr.env.define("stdout", Value::str(&a.stdout), false);
                    ofr.env.define("stderr", Value::str(&a.stderr), false);
                    ofr.env.define("code", Value::Int(res.code as i64), false);
                    for (k, e) in &step.outputs {
                        match self.ex.eval(e, &ofr) {
                            Ok(v) => {
                                a.outputs.insert(k.clone(), v);
                            }
                            Err(s) => {
                                if a.ok {
                                    a.ok = false;
                                    a.msg = Some(format!("output `{}`: {}", k, thrown_message(&s)));
                                }
                            }
                        }
                    }
                }
                a
            }
            Err(s) => {
                let mut a = Attempt::failed(thrown_message(&s));
                a.cancelled = is_cancelled_sig(&s);
                if let Signal::Throw(t) = &s {
                    a.error = Some(t.value.clone());
                }
                a
            }
        }
    }

    fn attempt_script(&mut self, step: &Step, block: &Block, fr: &Frame, step_tmo: Option<Duration>) -> Attempt {
        let outs: MapRef = Arc::new(std::sync::Mutex::new(IndexMap::new()));
        let sfr = fr.child();
        sfr.env.define("__outputs", Value::Map(outs.clone()), false);
        let tmo = self.eff_timeout(step_tmo);
        let dl = tmo.map(|t| Instant::now() + t);
        let (r, timed) = with_deadline(&mut self.ex, dl, |ex| ex.exec_stmts(block, &sfr));
        match r {
            Ok(_) => {
                let mut a = Attempt { ok: true, cancelled: false, code: None, stdout: String::new(), stderr: String::new(), outputs: lk(&outs).clone(), error: None, msg: None };
                let ofr = sfr.child();
                ofr.env.define("outputs", Value::Map(outs.clone()), false);
                for (k, e) in &step.outputs {
                    match self.ex.eval(e, &ofr) {
                        Ok(v) => {
                            a.outputs.insert(k.clone(), v);
                        }
                        Err(s) => {
                            a.ok = false;
                            a.msg = Some(format!("output `{}`: {}", k, thrown_message(&s)));
                            break;
                        }
                    }
                }
                a
            }
            Err(s) => {
                let mut a = Attempt::failed(thrown_message(&s));
                if let Signal::Throw(t) = &s {
                    a.error = Some(t.value.clone());
                }
                a.outputs = lk(&outs).clone();
                if timed {
                    a.msg = Some(format!("step timed out after {} ms", tmo.map(|t| t.as_millis()).unwrap_or(0)));
                    a.error = Some(self.ex.make_error("TimeoutError", a.msg.clone().unwrap()));
                } else if is_cancelled_sig(&s) {
                    a.cancelled = true;
                }
                if let Signal::Exit(c) = &s {
                    a.code = Some(*c);
                }
                a
            }
        }
    }

    fn attempt_uses(&mut self, function: &str, with: &[(String, Expr)], fr: &Frame, step_tmo: Option<Duration>) -> Attempt {
        let mut parts = function.split('.');
        let first = parts.next().unwrap_or("");
        let mut cur = match fr.env.get(first) {
            Some(v) => v,
            None => return Attempt::failed(format!("`uses`: unknown function `{}`", function)),
        };
        for p in parts {
            cur = match &cur {
                Value::Module(m) => match self.ex.module_member(m, p) {
                    Ok(v) => v,
                    Err(s) => return Attempt::failed(thrown_message(&s)),
                },
                _ => return Attempt::failed(format!("`uses`: `{}` is not a module path", function)),
            };
        }
        let mut named: IndexMap<String, Value> = IndexMap::new();
        for (k, e) in with {
            match self.ex.eval(e, fr) {
                Ok(v) => {
                    named.insert(k.clone(), v);
                }
                Err(s) => return Attempt::failed(format!("`with.{}`: {}", k, thrown_message(&s))),
            }
        }
        let args: Vec<Value> = match &cur {
            Value::Func(f) => {
                for k in named.keys() {
                    if !f.decl.params.iter().any(|p| &p.name == k) {
                        let pn: Vec<&str> = f.decl.params.iter().map(|p| p.name.as_str()).collect();
                        let hint = nin_diag::suggest(k, pn.iter().copied()).map(|s| format!(" (did you mean `{}`?)", s)).unwrap_or_default();
                        return Attempt::failed(format!("`uses`: function `{}` has no parameter `{}`{}", function, k, hint));
                    }
                }
                let last = f.decl.params.iter().rposition(|p| named.contains_key(&p.name)).map(|i| i + 1).unwrap_or(0);
                let mut v = vec![];
                for p in &f.decl.params[..last] {
                    match named.get(&p.name) {
                        Some(x) => v.push(x.clone()),
                        None => match &p.default {
                            Some(d) => match self.ex.eval(d, fr) {
                                Ok(x) => v.push(x),
                                Err(s) => return Attempt::failed(thrown_message(&s)),
                            },
                            None => return Attempt::failed(format!("`uses`: missing argument `{}` for `{}`", p.name, function)),
                        },
                    }
                }
                v
            }
            Value::Native(_) | Value::Bound(_) => named.values().cloned().collect(),
            _ => return Attempt::failed(format!("`uses`: `{}` is not a function", function)),
        };
        let tmo = self.eff_timeout(step_tmo);
        let dl = tmo.map(|t| Instant::now() + t);
        let (r, timed) = with_deadline(&mut self.ex, dl, |ex| ex.call_value(&cur, args, Span::DUMMY));
        match r {
            Ok(mut v) => {
                if let Value::Task(t) = &v {
                    let t = t.clone();
                    match self.ex.await_task(&t) {
                        Ok(x) => v = x,
                        Err(s) => return Attempt::failed(thrown_message(&s)),
                    }
                }
                let mut outputs = IndexMap::new();
                if let Value::Map(m) = &v {
                    outputs.extend(lk(m).clone());
                }
                outputs.insert("result".into(), v);
                Attempt { ok: true, cancelled: false, code: None, stdout: String::new(), stderr: String::new(), outputs, error: None, msg: None }
            }
            Err(s) => {
                let mut a = Attempt::failed(thrown_message(&s));
                if let Signal::Throw(t) = &s {
                    a.error = Some(t.value.clone());
                }
                if timed {
                    a.msg = Some("step timed out".into());
                } else if is_cancelled_sig(&s) {
                    a.cancelled = true;
                }
                a
            }
        }
    }

    fn sleep(&mut self, d: Duration) -> bool {
        self.ex.sleep_ms(d.as_millis() as u64).is_ok()
    }

    fn run_step(&mut self, idx: usize, step: &Step, finally: bool) -> StepReport {
        let t0 = Instant::now();
        let id = step.id.clone().unwrap_or_else(|| format!("{}{}", if finally { "finally-" } else { "step-" }, idx + 1));
        self.prefix.flush();
        let mut name = id.clone();
        let fr0 = self.step_frame(&[]);
        if let Some(n) = &step.name {
            if let Ok(s) = eval_str(&mut self.ex, n, &fr0) {
                name = s;
            }
        }
        let skip = |name: String, id: String, result: &str, t0: Instant, err: Option<String>| StepReport { id, name, result: result.to_string(), code: None, duration_ms: t0.elapsed().as_millis() as u64, attempts: 0, outputs: serde_json::json!({}), error: err };
        let set_entry = |me: &JobRun, id: &str, result: &str, outs: &IndexMap<String, Value>, code: Option<i32>, so: &str, se: &str| {
            if step.id.is_some() {
                let e = Value::map_from([("result", Value::str(result)), ("outputs", Value::map(outs.clone())), ("code", code.map(|c| Value::Int(c as i64)).unwrap_or(Value::Null)), ("stdout", Value::str(so)), ("stderr", Value::str(se))]);
                lk(&me.steps_map).insert(id.to_string(), e);
            }
        };
        // condition
        let saved_cancel = self.ex.cancel.clone();
        let was_cancelled = self.cancelled();
        let run_it = if finally {
            true
        } else {
            match &step.cond {
                Some(c) => match { let saved = std::mem::take(&mut self.ex.cancel); let r = eval_bool(&mut self.ex, c, &fr0); self.ex.cancel = saved; r } {
                    Ok(b) => b,
                    Err(s) => {
                        let m = format!("`if`: {}", thrown_message(&s));
                        self.state.failed.store(true, Ordering::SeqCst);
                        set_entry(self, &id, "failure", &IndexMap::new(), None, "", "");
                        return skip(name, id, "failure", t0, Some(m));
                    }
                },
                None => !self.state.failed.load(Ordering::SeqCst) && !was_cancelled,
            }
        };
        if !run_it {
            set_entry(self, &id, "skipped", &IndexMap::new(), None, "", "");
            return skip(name, id, if was_cancelled && step.cond.is_none() { "cancelled" } else { "skipped" }, t0, None);
        }
        if (step.cond.is_some() || finally) && was_cancelled {
            self.ex.cancel = vec![];
        }
        // sink prefix per step
        let step_prefix = Arc::new(PrefixSink::new(format!("{}/{}", self.inst.key, id), self.sh.sink.clone()));
        let sink_dyn: Arc<dyn Sink> = step_prefix.clone();
        self.ex.sink = Some(sink_dyn);

        let (env_pairs, step_tmo, retries, delay, coe) = {
            let fr = &fr0;
            let step_env = eval_env(&mut self.ex, &step.env, fr).unwrap_or_default();
            let mut ep = self.wf_job_env.clone();
            ep.extend(step_env);
            let tmo = eval_opt_dur(&mut self.ex, &step.timeout, fr).unwrap_or(None);
            let retries = eval_opt_int(&mut self.ex, &step.retries, fr).unwrap_or(None).unwrap_or(0).max(0) as u32;
            let delay = eval_opt_dur(&mut self.ex, &step.retry_delay, fr).unwrap_or(None).unwrap_or(Duration::ZERO);
            let coe = match &step.continue_on_error {
                Some(e) => eval_bool(&mut self.ex, e, fr).unwrap_or(false),
                None => false,
            };
            (ep, tmo, retries, delay, coe)
        };
        let step_only_env: Vec<(String, String)> = env_pairs[self.wf_job_env.len()..].to_vec();
        let fr = self.step_frame(&step_only_env);

        let mut attempts = 0;
        let last = loop {
            attempts += 1;
            let a = match &step.action {
                StepAction::Run(r) => self.attempt_run(step, r, &fr, &env_pairs, step_tmo),
                StepAction::Script(b) => self.attempt_script(step, b, &fr, step_tmo),
                StepAction::Uses { function, with } => self.attempt_uses(function, with, &fr, step_tmo),
            };
            if a.ok || a.cancelled || self.cancelled() && !finally || attempts > retries {
                break a;
            }
            step_prefix.write(&format!("attempt {} failed: {} — retrying\n", attempts, a.msg.clone().unwrap_or_default()), true);
            if !self.sleep(delay) {
                break a;
            }
        };
        step_prefix.flush();
        self.ex.cancel = saved_cancel;
        self.ex.sink = Some(self.prefix.clone());

        let result = if last.ok {
            "success"
        } else if last.cancelled {
            "cancelled"
        } else {
            "failure"
        };
        if !last.ok && !last.cancelled {
            // on_error handler
            if !step.on_error.is_empty() {
                let efr = fr.child();
                let ev = last.error.clone().unwrap_or_else(|| self.ex.make_error("Error", last.msg.clone().unwrap_or_default()));
                efr.env.define("error", ev, false);
                if let Err(s) = self.ex.exec_stmts(&step.on_error, &efr) {
                    let m = thrown_message(&s);
                    self.ex.out(&format!("on_error handler failed: {}\n", m), true);
                }
            }
            if !coe && !finally {
                self.state.failed.store(true, Ordering::SeqCst);
            }
        }
        self.prefix.flush();
        set_entry(self, &id, result, &last.outputs, last.code, &last.stdout, &last.stderr);
        let mut rep = StepReport { id, name, result: result.to_string(), code: last.code, duration_ms: t0.elapsed().as_millis() as u64, attempts, outputs: map_json(&last.outputs), error: last.msg.clone() };
        if coe && result == "failure" {
            rep.error = last.msg.map(|m| format!("{} (continue_on_error)", m));
        }
        rep
    }
}

fn run_instance(sh: &Shared, inst: &Inst, jobs_ctx: Value, needs_failed: bool, cancelled_start: bool) -> Outcome {
    let job = &sh.wf.jobs[inst.job];
    let t0 = Instant::now();
    let mut ex = Exec::new(sh.interp.clone());
    ex.cancel = vec![sh.user_cancel.clone(), sh.ff_cancel.clone()];
    let prefix = Arc::new(PrefixSink::new(inst.key.clone(), sh.sink.clone()));
    ex.sink = Some(prefix.clone());

    let env = Scope::new(Some(sh.env.clone()));
    let state = Arc::new(JobState { failed: std::sync::atomic::AtomicBool::new(needs_failed) });
    let steps_map: MapRef = Arc::new(std::sync::Mutex::new(IndexMap::new()));
    env.define("steps", Value::Map(steps_map.clone()), false);
    env.define("jobs", jobs_ctx, false);
    env.define("matrix", Value::map(inst.matrix.clone()), false);
    env.define("workflow", Value::map_from([("name", Value::str(sh.wf.name.clone().unwrap_or_default())), ("run_id", Value::str(&sh.run_id)), ("start_time", Value::Int(sh.start_ms))]), false);
    {
        let s = state.clone();
        env.define("success", Native::value("success", 0, Some(0), move |_, _| Ok(Value::Bool(!s.failed.load(Ordering::SeqCst)))), true);
        let s = state.clone();
        env.define("failure", Native::value("failure", 0, Some(0), move |_, _| Ok(Value::Bool(s.failed.load(Ordering::SeqCst)))), true);
        env.define("always", Native::value("always", 0, Some(0), |_, _| Ok(Value::Bool(true))), true);
        let c = sh.user_cancel.clone();
        env.define("cancelled", Native::value("cancelled", 0, Some(0), move |_, _| Ok(Value::Bool(c.load(Ordering::SeqCst)))), true);
    }
    let base = Frame::new(env.clone(), "workflow");
    let mut name = inst.key.clone();
    if let Some(n) = &job.name {
        if let Ok(s) = eval_str(&mut ex, n, &base) {
            name = s;
        }
    }
    base.env.define("job", Value::map_from([("id", Value::str(&job.id)), ("name", Value::str(&name)), ("status", Value::str("running"))]), false);

    let coe = match &job.continue_on_error {
        Some(e) => eval_bool(&mut ex, e, &base).unwrap_or(false),
        None => false,
    };
    let mk = |result: &str, err: Option<String>, steps: Vec<StepReport>, outputs: IndexMap<String, Value>, attempts: u32| {
        let ok = result == "success" || (result == "failure" && coe);
        Outcome {
            report: JobReport { id: inst.key.clone(), name: name.clone(), result: result.to_string(), duration_ms: t0.elapsed().as_millis() as u64, attempts, outputs: map_json(&outputs), continue_on_error: coe, steps, error: err },
            outputs,
            ok,
            hard_fail: result == "failure" && !coe,
        }
    };

    // job-level condition
    let run_it = match &job.cond {
        Some(c) => match { let saved = std::mem::take(&mut ex.cancel); let r = eval_bool(&mut ex, c, &base); ex.cancel = saved; r } {
            Ok(b) => b,
            Err(s) => return mk("failure", Some(format!("`if`: {}", thrown_message(&s))), vec![], IndexMap::new(), 0),
        },
        None => !needs_failed && !cancelled_start,
    };
    if !run_it {
        let r = if cancelled_start && job.cond.is_none() && !needs_failed { "cancelled" } else { "skipped" };
        return mk(r, if needs_failed && job.cond.is_none() { Some("a dependency did not succeed".into()) } else { None }, vec![], IndexMap::new(), 0);
    }
    if cancelled_start {
        ex.cancel = vec![];
    }
    state.failed.store(false, Ordering::SeqCst);

    // timeouts, retries, limits
    let wf_fr = Frame::new(sh.env.clone(), "workflow");
    let job_timeout = eval_opt_dur(&mut ex, &job.timeout, &base).ok().flatten().or_else(|| eval_opt_dur(&mut ex, &sh.wf.default_timeout, &wf_fr).ok().flatten());
    let retries = eval_opt_int(&mut ex, &job.retries, &base).ok().flatten().or_else(|| eval_opt_int(&mut ex, &sh.wf.default_retries, &wf_fr).ok().flatten()).unwrap_or(0).max(0) as u32;
    let retry_delay = eval_opt_dur(&mut ex, &job.retry_delay, &base).ok().flatten().unwrap_or(Duration::ZERO);
    let mut limits = nin_win::Limits { kill_on_close: true, ..Default::default() };
    if let Some(l) = &job.limits {
        if let Some(e) = &l.memory {
            match ex.eval(e, &base).and_then(|v| ex.size_bytes(&v)) {
                Ok(n) => limits.memory_bytes = Some(n),
                Err(s) => return mk("failure", Some(format!("limits.memory: {}", thrown_message(&s))), vec![], IndexMap::new(), 0),
            }
        }
        if let Some(e) = &l.processes {
            match ex.eval(e, &base) {
                Ok(Value::Int(n)) if n > 0 => limits.max_processes = Some(n as u32),
                _ => return mk("failure", Some("limits.processes must be a positive int".into()), vec![], IndexMap::new(), 0),
            }
        }
        if let Some(e) = &l.cpu_time {
            match ex.eval(e, &base).and_then(|v| ex.duration_ms(&v)) {
                Ok(n) => limits.cpu_time_ms = Some(n),
                Err(s) => return mk("failure", Some(format!("limits.cpu_time: {}", thrown_message(&s))), vec![], IndexMap::new(), 0),
            }
        }
    }
    match nin_win::ProcessGroup::new(limits) {
        Ok(g) => ex.group = Some(Arc::new(g)),
        Err(e) => return mk("failure", Some(format!("cannot create process group: {}", e)), vec![], IndexMap::new(), 0),
    }
    let mut wf_job_env = sh.wf_env.clone();
    match eval_env(&mut ex, &job.env, &base) {
        Ok(e) => wf_job_env.extend(e),
        Err(s) => return mk("failure", Some(format!("env: {}", thrown_message(&s))), vec![], IndexMap::new(), 0),
    }
    let deadline = job_timeout.map(|t| Instant::now() + t);
    let mut jr = JobRun { sh, job, inst, ex, base, state: state.clone(), steps_map: steps_map.clone(), wf_job_env, deadline, prefix: prefix.clone() };

    let mut attempts = 0;
    let (mut reports, mut outputs);
    loop {
        attempts += 1;
        state.failed.store(false, Ordering::SeqCst);
        lk(&steps_map).clear();
        reports = vec![];
        for (i, s) in job.steps.iter().enumerate() {
            reports.push(jr.run_step(i, s, false));
        }
        // job outputs
        outputs = IndexMap::new();
        let ofr = jr.step_frame(&[]);
        for (k, e) in &job.outputs {
            match jr.ex.eval(e, &ofr) {
                Ok(v) => {
                    outputs.insert(k.clone(), v);
                }
                Err(_) => {
                    outputs.insert(k.clone(), Value::Null);
                }
            }
        }
        let failed = state.failed.load(Ordering::SeqCst);
        let cancelled = jr.cancelled();
        if !failed || cancelled || attempts > retries {
            break;
        }
        jr.prefix.write(&format!("job attempt {} failed — retrying\n", attempts), true);
        if !jr.sleep(retry_delay) {
            break;
        }
    }
    let body_failed = state.failed.load(Ordering::SeqCst);
    let was_cancelled = jr.cancelled();
    // finally steps always run
    let mut fin_failed = false;
    for (i, s) in job.finally.iter().enumerate() {
        let r = jr.run_step(i, s, true);
        if r.result == "failure" {
            fin_failed = true;
        }
        reports.push(r);
    }
    jr.prefix.flush();
    let result = if body_failed || fin_failed {
        "failure"
    } else if was_cancelled && reports.iter().any(|r| r.result == "cancelled") {
        "cancelled"
    } else {
        "success"
    };
    let err = reports.iter().find(|r| r.result == "failure").and_then(|r| r.error.clone());
    mk(result, err, reports, outputs, attempts)
}

// ───────────────────────── the scheduler ─────────────────────────

fn aggregate(results: &[(String, bool)]) -> &'static str {
    // (result, _) pairs → aggregate result string
    let rs: Vec<&str> = results.iter().map(|(r, _)| r.as_str()).collect();
    if rs.iter().all(|r| *r == "success") {
        "success"
    } else if rs.contains(&"failure") {
        "failure"
    } else if rs.iter().all(|r| *r == "skipped") {
        "skipped"
    } else {
        "cancelled"
    }
}

/// Run a workflow to completion and collect the report.
pub fn run(interp: Arc<Interp>, module_env: Env, wf: &Workflow, opts: Options) -> Result<Report, String> {
    let started = Instant::now();
    plan(wf)?;
    let mut ex = Exec::new(interp.clone());
    ex.cancel = vec![opts.cancel.clone()];
    let ctx = Scope::new(Some(module_env.clone()));
    let mfr = Frame::new(ctx.clone(), "workflow");
    let inputs = resolve_inputs(&mut ex, wf, &opts.inputs, &mfr)?;
    let inputs_v = Value::map(inputs);
    ctx.define("inputs", inputs_v.clone(), false);
    let wf_env = eval_env(&mut ex, &wf.env, &mfr).map_err(|s| format!("workflow env: {}", render(&ex, s)))?;
    ctx.define("env", Value::map(wf_env.iter().map(|(k, v)| (k.clone(), Value::str(v))).collect()), false);

    // job selection (+ transitive needs)
    let mut selected: HashSet<String> = HashSet::new();
    if opts.jobs.is_empty() {
        selected.extend(wf.jobs.iter().map(|j| j.id.clone()));
    } else {
        let mut stack: Vec<String> = vec![];
        for id in &opts.jobs {
            if !wf.jobs.iter().any(|j| &j.id == id) {
                let names: Vec<&str> = wf.jobs.iter().map(|j| j.id.as_str()).collect();
                let hint = nin_diag::suggest(id, names.iter().copied()).map(|s| format!(" (did you mean `{}`?)", s)).unwrap_or_default();
                return Err(format!("unknown job `{}`{}", id, hint));
            }
            stack.push(id.clone());
        }
        while let Some(id) = stack.pop() {
            if selected.insert(id.clone()) {
                if let Some(j) = wf.jobs.iter().find(|j| j.id == id) {
                    stack.extend(j.needs.iter().cloned());
                }
            }
        }
    }

    // instances
    let mut insts: Vec<Inst> = vec![];
    let mut job_insts: HashMap<String, Vec<usize>> = HashMap::new();
    for (ji, j) in wf.jobs.iter().enumerate() {
        if !selected.contains(&j.id) {
            continue;
        }
        let combos = match &j.matrix {
            Some(m) => expand_matrix(&mut ex, m, &mfr).map_err(|s| format!("job `{}` matrix: {}", j.id, render(&ex, s)))?,
            None => vec![IndexMap::new()],
        };
        if j.matrix.is_some() && combos.is_empty() {
            return Err(format!("job `{}`: matrix expands to zero combinations", j.id));
        }
        for c in combos {
            let key = if j.matrix.is_some() {
                let mut parts = vec![];
                for (k, v) in &c {
                    parts.push(format!("{}={}", k, ex.to_display(v).unwrap_or_default()));
                }
                format!("{}[{}]", j.id, parts.join(","))
            } else {
                j.id.clone()
            };
            job_insts.entry(j.id.clone()).or_default().push(insts.len());
            insts.push(Inst { job: ji, key, matrix: c });
        }
    }
    let deps: Vec<Vec<usize>> = insts.iter().map(|i| wf.jobs[i.job].needs.iter().flat_map(|n| job_insts.get(n).cloned().unwrap_or_default()).collect()).collect();

    let sink: Arc<dyn Sink> = opts.sink.clone().unwrap_or_else(|| Arc::new(ConsoleSink));
    let wf_name = wf.name.clone().unwrap_or_else(|| "workflow".to_string());

    if opts.dry_run {
        let mut jobs = vec![];
        for i in &insts {
            let j = &wf.jobs[i.job];
            sink.write(&format!("[dry-run] job {}{}\n", i.key, if j.needs.is_empty() { String::new() } else { format!(" (after {})", j.needs.join(", ")) }), false);
            let steps = j.steps.iter().enumerate().map(|(n, s)| StepReport { id: s.id.clone().unwrap_or_else(|| format!("step-{}", n + 1)), name: s.id.clone().unwrap_or_else(|| format!("step-{}", n + 1)), result: "skipped".into(), code: None, duration_ms: 0, attempts: 0, outputs: serde_json::json!({}), error: None }).collect();
            jobs.push(JobReport { id: i.key.clone(), name: i.key.clone(), result: "skipped".into(), duration_ms: 0, attempts: 0, outputs: serde_json::json!({}), continue_on_error: false, steps, error: None });
        }
        return Ok(Report { name: wf_name, outcome: "success".into(), duration_ms: started.elapsed().as_millis() as u64, jobs });
    }

    let max_par = match opts.max_parallel {
        Some(n) => n.max(1),
        None => match &wf.max_parallel {
            Some(e) => match ex.eval(e, &mfr) {
                Ok(Value::Int(n)) if n >= 1 => n as usize,
                _ => return Err("workflow max_parallel must be a positive int".into()),
            },
            None => std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4),
        },
    };

    let sh = Arc::new(Shared {
        interp: interp.clone(),
        env: ctx.clone(),
        wf: Arc::new(wf.clone()),
        inputs: inputs_v,
        wf_env,
        base_dir: opts.base_dir.clone(),
        sink: sink.clone(),
        user_cancel: opts.cancel.clone(),
        ff_cancel: Arc::new(AtomicBool::new(false)),
        run_id: format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) as u64 ^ ((std::process::id() as u64) << 40)),
        start_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0),
    });
    let _ = &sh.inputs;

    #[derive(Clone, Copy, PartialEq)]
    enum St {
        Pending,
        Running,
        Done,
    }
    let n = insts.len();
    let insts = Arc::new(insts);
    let mut state = vec![St::Pending; n];
    let mut outcomes: Vec<Option<Outcome>> = (0..n).map(|_| None).collect();
    let (tx, rx) = mpsc::channel::<(usize, Outcome)>();
    let mut running = 0usize;
    let mut running_per_job: HashMap<usize, usize> = HashMap::new();
    let mut done_count = 0;
    let mut handles = vec![];

    while done_count < n {
        // start everything that is ready
        for i in 0..n {
            if state[i] != St::Pending || running >= max_par {
                continue;
            }
            if !deps[i].iter().all(|d| state[*d] == St::Done) {
                continue;
            }
            let ji = insts[i].job;
            if let Some(m) = &wf.jobs[ji].matrix {
                if let Some(e) = &m.max_parallel {
                    if let Ok(Value::Int(lim)) = ex.eval(e, &mfr) {
                        if running_per_job.get(&ji).copied().unwrap_or(0) >= lim.max(1) as usize {
                            continue;
                        }
                    }
                }
            }
            let needs_failed = deps[i].iter().any(|d| !outcomes[*d].as_ref().map(|o| o.ok).unwrap_or(false));
            let cancelled_start = sh.user_cancel.load(Ordering::SeqCst) || sh.ff_cancel.load(Ordering::SeqCst);
            // `jobs` context from finished jobs
            let mut jobs_ctx = IndexMap::new();
            for (jid, idxs) in &job_insts {
                if idxs.iter().all(|x| state[*x] == St::Done) {
                    let rs: Vec<(String, bool)> = idxs.iter().map(|x| (outcomes[*x].as_ref().unwrap().report.result.clone(), true)).collect();
                    let outs = idxs.iter().filter_map(|x| outcomes[*x].as_ref()).find(|o| o.report.result == "success").map(|o| o.outputs.clone()).unwrap_or_default();
                    jobs_ctx.insert(jid.clone(), Value::map_from([("result", Value::str(aggregate(&rs))), ("outputs", Value::map(outs))]));
                }
            }
            state[i] = St::Running;
            running += 1;
            *running_per_job.entry(ji).or_insert(0) += 1;
            let (sh2, insts2, tx2) = (sh.clone(), insts.clone(), tx.clone());
            let jc = Value::map(jobs_ctx);
            let h = std::thread::Builder::new().name(format!("nin-job-{}", insts[i].key)).stack_size(nin_runtime::interp::TASK_STACK).spawn(move || {
                let out = run_instance(&sh2, &insts2[i], jc, needs_failed, cancelled_start);
                let _ = tx2.send((i, out));
            });
            match h {
                Ok(h) => handles.push(h),
                Err(e) => return Err(format!("cannot start job thread: {}", e)),
            }
        }
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok((i, out)) => {
                state[i] = St::Done;
                running -= 1;
                if let Some(c) = running_per_job.get_mut(&insts[i].job) {
                    *c -= 1;
                }
                done_count += 1;
                let ji = insts[i].job;
                if out.hard_fail {
                    let matrix_unit = wf.jobs[ji].matrix.as_ref().map(|m| !m.fail_fast).unwrap_or(false);
                    if !matrix_unit {
                        sh.ff_cancel.store(true, Ordering::SeqCst);
                    }
                }
                outcomes[i] = Some(out);
                if let Some(m) = &wf.jobs[ji].matrix {
                    if !m.fail_fast {
                        let idxs = &job_insts[&wf.jobs[ji].id];
                        if idxs.iter().all(|x| state[*x] == St::Done) && idxs.iter().any(|x| outcomes[*x].as_ref().map_or(false, |o| o.hard_fail)) {
                            sh.ff_cancel.store(true, Ordering::SeqCst);
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    for h in handles {
        let _ = h.join();
    }
    let jobs: Vec<JobReport> = outcomes.into_iter().flatten().map(|o| o.report).collect();
    let cancelled = sh.user_cancel.load(Ordering::SeqCst);
    let failed = jobs.iter().any(|j| j.result == "failure" && !j.continue_on_error);
    let outcome = if failed {
        "failure"
    } else if cancelled || jobs.iter().any(|j| j.result == "cancelled") {
        "cancelled"
    } else {
        "success"
    };
    Ok(Report { name: wf_name, outcome: outcome.into(), duration_ms: started.elapsed().as_millis() as u64, jobs })
}
