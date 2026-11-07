use nin_runtime::*;
use nin_workflow::*;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Cap(Mutex<String>);
impl Sink for Cap {
    fn write(&self, t: &str, e: bool) {
        self.0.lock().unwrap().push_str(&if e { format!("(err){}", t) } else { t.to_string() });
    }
}

fn exec(src: &str, mut opts: Options) -> (Result<Report, String>, String) {
    let interp = nin_std::builder().build();
    let mut ex = Exec::new(interp.clone());
    let prog = ex.parse_source("wf.nin", src).unwrap_or_else(|e| panic!("{}", e));
    let env = match ex.load_program(&prog, Path::new("."), "wf") {
        Ok(e) => e,
        Err(Signal::Throw(t)) => panic!("{}", interp.render_throw(&t)),
        Err(_) => panic!("exit"),
    };
    let cap = Arc::new(Cap(Mutex::new(String::new())));
    opts.sink = Some(cap.clone());
    let r = nin_workflow::run(interp, env, prog.workflow.as_ref().unwrap(), opts);
    let out = cap.0.lock().unwrap().clone();
    (r, out)
}

fn run_ok(src: &str) -> (Report, String) {
    let (r, out) = exec(src, Options::default());
    match r {
        Ok(r) => (r, out),
        Err(e) => panic!("workflow error: {}", e),
    }
}

fn job<'a>(r: &'a Report, id: &str) -> &'a JobReport {
    r.jobs.iter().find(|j| j.id == id).unwrap_or_else(|| panic!("no job {} in {:?}", id, r.jobs.iter().map(|j| &j.id).collect::<Vec<_>>()))
}

fn wf(jobs: &str) -> String {
    format!(r#"{{"nin":"1","imports":[{{"use":"std.time"}}],"vars":{{"counter":0}},"functions":{{"bump":{{"body":[{{"set":"counter","op":"+","value":1}},{{"return":"${{counter}}"}}]}},"add":{{"params":["a: int","b: int = 10"],"body":[{{"return":"${{a+b}}"}}]}}}},"workflow":{{"name":"t","jobs":{}}}}}"#, jobs)
}

#[test]
fn dag_outputs_and_context() {
    let (r, out) = run_ok(&wf(r#"{
      "a":{"steps":[{"id":"s","script":[{"output":"v","value":21},{"print":"hello from a"}]}],"outputs":{"doubled":"${steps.s.outputs.v * 2}"}},
      "b":{"needs":["a"],"steps":[{"id":"t","script":[{"output":"got","value":"${jobs.a.outputs.doubled}"}]}],"outputs":{"final":"${steps.t.outputs.got + 1}"}},
      "c":{"needs":["b"],"steps":[{"script":[{"assert":"jobs.b.outputs.final == 43 && jobs.a.result == 'success'"}]}]}
    }"#));
    assert!(r.ok(), "{}", r.summary());
    assert_eq!(job(&r, "b").outputs["final"], 43);
    assert!(out.contains("[a/s] hello from a"), "{}", out);
    assert_eq!(r.to_json()["outcome"], "success");
}

#[test]
fn independent_jobs_run_in_parallel() {
    let t = Instant::now();
    let (r, _) = run_ok(&wf(r#"{
      "a":{"steps":[{"script":[{"call":"time.sleep(400)"}]}]},
      "b":{"steps":[{"script":[{"call":"time.sleep(400)"}]}]},
      "c":{"steps":[{"script":[{"call":"time.sleep(400)"}]}]}
    }"#));
    assert!(r.ok());
    assert!(t.elapsed() < Duration::from_millis(1000), "took {:?}", t.elapsed());
}

#[test]
fn max_parallel_limits_concurrency() {
    let t = Instant::now();
    let (r, _) = exec(&wf(r#"{
      "a":{"steps":[{"script":[{"call":"time.sleep(300)"}]}]},
      "b":{"steps":[{"script":[{"call":"time.sleep(300)"}]}]}
    }"#), Options { max_parallel: Some(1), ..Default::default() });
    assert!(r.unwrap().ok());
    assert!(t.elapsed() >= Duration::from_millis(580), "{:?}", t.elapsed());
}

#[test]
fn failure_skips_dependents_and_always_runs() {
    let (r, _) = run_ok(&wf(r#"{
      "bad":{"steps":[{"script":[{"throw":"${new Error('nope')}"}]}]},
      "next":{"needs":["bad"],"steps":[{"script":[]}]},
      "cleanup":{"needs":["bad"],"if":"always()","steps":[{"script":[{"print":"cleaned"}]}]},
      "onfail":{"needs":["bad"],"if":"failure()","steps":[{"script":[]}]}
    }"#));
    assert_eq!(r.outcome, "failure");
    assert_eq!(job(&r, "bad").result, "failure");
    assert!(job(&r, "bad").error.as_ref().unwrap().contains("nope"));
    assert_eq!(job(&r, "next").result, "skipped");
    assert_eq!(job(&r, "cleanup").result, "success", "{}", r.summary());
    assert_eq!(job(&r, "onfail").result, "success");
}

#[test]
fn fail_fast_cancels_running_job() {
    let t = Instant::now();
    let (r, _) = run_ok(&wf(r#"{
      "slow":{"steps":[{"script":[{"call":"time.sleep(10000)"}]}]},
      "bad":{"steps":[{"script":[{"call":"time.sleep(100)"},{"throw":"${new Error('x')}"}]}]}
    }"#));
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert_eq!(r.outcome, "failure");
    assert_eq!(job(&r, "slow").result, "cancelled");
}

#[test]
fn continue_on_error_keeps_workflow_green() {
    let (r, _) = run_ok(&wf(r#"{
      "flaky":{"continue_on_error":true,"steps":[{"script":[{"throw":"${new Error('x')}"}]}]},
      "after":{"needs":["flaky"],"steps":[{"script":[]}]},
      "stepjob":{"steps":[{"id":"x","continue_on_error":true,"script":[{"throw":"${new Error('y')}"}]},{"script":[{"print":"still ran"}]}]}
    }"#));
    assert!(r.ok(), "{}", r.summary());
    assert_eq!(job(&r, "flaky").result, "failure");
    assert_eq!(job(&r, "after").result, "success");
    assert_eq!(job(&r, "stepjob").steps[1].result, "success");
}

#[test]
fn step_retries_until_success() {
    let (r, _) = run_ok(&wf(r#"{
      "j":{"steps":[{"id":"r","retries":3,"retry_delay":"10ms","script":[{"call":"bump()"},{"assert":"counter >= 3","message":"not yet"}]}]}
    }"#));
    assert!(r.ok(), "{}", r.summary());
    assert_eq!(job(&r, "j").steps[0].attempts, 3);
}

#[test]
fn retries_exhausted_fails() {
    let (r, _) = run_ok(&wf(r#"{"j":{"steps":[{"retries":1,"script":[{"throw":"${new Error('always')}"}]}]}}"#));
    assert_eq!(r.outcome, "failure");
    assert_eq!(job(&r, "j").steps[0].attempts, 2);
}

#[test]
fn step_timeout() {
    let t = Instant::now();
    let (r, _) = run_ok(&wf(r#"{"j":{"steps":[{"timeout":"150ms","script":[{"call":"time.sleep(5000)"}]}]}}"#));
    assert_eq!(r.outcome, "failure");
    assert!(job(&r, "j").steps[0].error.as_ref().unwrap().contains("timed out"), "{}", r.summary());
    assert!(t.elapsed() < Duration::from_secs(3));
}

#[test]
fn job_timeout_applies_across_steps() {
    let (r, _) = run_ok(&wf(r#"{"j":{"timeout":"200ms","steps":[{"script":[{"call":"time.sleep(120)"}]},{"script":[{"call":"time.sleep(5000)"}]}]}}"#));
    assert_eq!(job(&r, "j").result, "failure");
}

#[test]
fn matrix_expansion_include_exclude() {
    let (r, _) = run_ok(&wf(r#"{
      "m":{"strategy":{"matrix":{"os":["a","b"],"v":[1,2],"include":[{"os":"c","v":3},{"os":"a","extra":"x"}],"exclude":[{"os":"b","v":2}]}},
           "steps":[{"script":[{"print":"${matrix.os}${matrix.v}${matrix.extra ?? ''}"}]}]},
      "after":{"needs":["m"],"steps":[{"script":[{"assert":"jobs.m.result == 'success'"}]}]}
    }"#));
    assert!(r.ok(), "{}", r.summary());
    let mut ids: Vec<&str> = r.jobs.iter().map(|j| j.id.as_str()).filter(|i| i.starts_with("m[")).collect();
    ids.sort();
    assert_eq!(ids, vec!["m[os=a,v=1,extra=x]", "m[os=a,v=2,extra=x]", "m[os=b,v=1]", "m[os=c,v=3]"]);
}

#[test]
fn run_steps_capture_output() {
    #[cfg(unix)]
    let step = r#"{"id":"e","run":"echo","args":["hi there"],"outputs":{"line":"${stdout.trim()}","code":"${code}"}}"#;
    #[cfg(windows)]
    let step = r#"{"id":"e","run":"cmd","args":["/c","echo","hi there"],"outputs":{"line":"${stdout.trim()}","code":"${code}"}}"#;
    let (r, out) = run_ok(&wf(&format!(r#"{{"j":{{"steps":[{},{{"script":[{{"assert":"steps.e.outputs.line.starts_with('hi there') && steps.e.code == 0"}}]}}]}}}}"#, step)));
    assert!(r.ok(), "{}\n{}", r.summary(), out);
    assert!(out.contains("[j/e] hi there"), "{}", out);
}

#[test]
fn run_step_failure_and_on_error() {
    #[cfg(unix)]
    let step = r#"{"id":"f","run":"sh","args":["-c","exit 3"],"on_error":[{"print":"handler saw: ${error.message}"}]}"#;
    #[cfg(windows)]
    let step = r#"{"id":"f","run":"cmd","args":["/c","exit","3"],"on_error":[{"print":"handler saw: ${error.message}"}]}"#;
    let (r, out) = run_ok(&wf(&format!(r#"{{"j":{{"steps":[{}]}}}}"#, step)));
    assert_eq!(r.outcome, "failure");
    assert_eq!(job(&r, "j").steps[0].code, Some(3));
    assert!(out.contains("handler saw:") && out.contains("code 3"), "{}", out);
}

#[test]
fn finally_always_runs() {
    let (r, out) = run_ok(&wf(r#"{"j":{"steps":[{"script":[{"throw":"${new Error('x')}"}]},{"script":[{"print":"never"}]}],"finally":[{"script":[{"print":"cleanup ran"}]}]}}"#));
    assert_eq!(job(&r, "j").result, "failure");
    assert!(out.contains("cleanup ran") && !out.contains("never"), "{}", out);
    assert_eq!(job(&r, "j").steps[1].result, "skipped");
}

#[test]
fn uses_function_with_named_args() {
    let (r, _) = run_ok(&wf(r#"{"j":{"steps":[{"id":"u","uses":"add","with":{"a":5}},{"id":"u2","uses":"add","with":{"a":1,"b":2}},{"script":[{"assert":"steps.u.outputs.result == 15 && steps.u2.outputs.result == 3"}]}]}}"#));
    assert!(r.ok(), "{}", r.summary());
    let (r, _) = run_ok(&wf(r#"{"j":{"steps":[{"uses":"add","with":{"zz":1}}]}}"#));
    assert!(job(&r, "j").steps[0].error.as_ref().unwrap().contains("no parameter `zz`"));
}

#[test]
fn inputs_are_validated_and_typed() {
    let src = r#"{"nin":"1","workflow":{"inputs":{"n":{"type":"int","default":2},"name":{"type":"string","required":true}},"jobs":{"j":{"steps":[{"script":[{"assert":"inputs.n * 2 == 10 && inputs.name == 'x'"}]}]}}}}"#;
    let (r, _) = exec(src, Options { inputs: vec![("n".into(), "5".into()), ("name".into(), "x".into())], ..Default::default() });
    assert!(r.unwrap().ok());
    let (r, _) = exec(src, Options::default());
    assert!(r.unwrap_err().contains("missing required input `name`"));
    let (r, _) = exec(src, Options { inputs: vec![("nmae".into(), "x".into())], ..Default::default() });
    assert!(r.unwrap_err().contains("did you mean `name`"));
    let (r, _) = exec(src, Options { inputs: vec![("n".into(), "abc".into()), ("name".into(), "x".into())], ..Default::default() });
    assert!(r.unwrap_err().contains("not an int"));
}

#[test]
fn graph_errors() {
    let (r, _) = exec(&wf(r#"{"a":{"needs":["b"],"steps":[{"script":[]}]},"b":{"needs":["a"],"steps":[{"script":[]}]}}"#), Options::default());
    assert!(r.unwrap_err().contains("dependency cycle: a -> b -> a"));
    let (r, _) = exec(&wf(r#"{"a":{"needs":["zz"],"steps":[{"script":[]}]}}"#), Options::default());
    assert!(r.unwrap_err().contains("unknown job `zz`"));
}

#[test]
fn job_selection_runs_dependencies_only() {
    let (r, _) = exec(&wf(r#"{"a":{"steps":[{"script":[]}]},"b":{"needs":["a"],"steps":[{"script":[]}]},"c":{"steps":[{"script":[]}]}}"#), Options { jobs: vec!["b".into()], ..Default::default() });
    let r = r.unwrap();
    let ids: Vec<&str> = r.jobs.iter().map(|j| j.id.as_str()).collect();
    assert_eq!(ids, vec!["a", "b"]);
}

#[test]
fn dry_run_executes_nothing() {
    let (r, out) = exec(&wf(r#"{"a":{"steps":[{"script":[{"throw":"${new Error('x')}"}]}]}}"#), Options { dry_run: true, ..Default::default() });
    assert!(r.unwrap().ok());
    assert!(out.contains("[dry-run] job a"));
}

#[test]
fn user_cancel_stops_workflow() {
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let c2 = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        c2.store(true, Ordering::SeqCst);
    });
    let t = Instant::now();
    let (r, _) = exec(&wf(r#"{"a":{"steps":[{"script":[{"call":"time.sleep(20000)"}]}]},"b":{"needs":["a"],"steps":[{"script":[]}]}}"#), Options { cancel, ..Default::default() });
    let r = r.unwrap();
    assert!(t.elapsed() < Duration::from_secs(5));
    assert_eq!(r.outcome, "cancelled");
    assert_ne!(job(&r, "b").result, "success");
}

#[test]
fn plan_levels_and_graph_text() {
    let interp = nin_std::builder().build();
    let mut ex = Exec::new(interp);
    let prog = ex.parse_source("g.nin", &wf(r#"{"a":{"steps":[{"script":[]}]},"b":{"needs":["a"],"steps":[{"script":[]}]},"c":{"needs":["a"],"steps":[{"script":[]}]},"d":{"needs":["b","c"],"steps":[{"script":[]}]}}"#)).unwrap();
    let w = prog.workflow.unwrap();
    assert_eq!(plan(&w).unwrap(), vec![vec!["a"], vec!["b", "c"], vec!["d"]]);
    assert!(graph_text(&w).unwrap().contains("stage 2  (parallel)"));
}

#[test]
fn report_json_and_summary() {
    let (r, _) = run_ok(&wf(r#"{"a":{"steps":[{"id":"s","script":[{"output":"k","value":1}]}]}}"#));
    let j = r.to_json();
    assert_eq!(j["jobs"][0]["steps"][0]["outputs"]["k"], 1);
    assert!(r.summary().contains("success"));
}

#[test]
fn job_limits_are_accepted() {
    let (r, _) = run_ok(&wf(r#"{"a":{"limits":{"memory":"512MB","processes":16,"cpu_time":"1m"},"steps":[{"script":[]}]}}"#));
    assert!(r.ok(), "{}", r.summary());
}
