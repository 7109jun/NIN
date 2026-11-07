use serde_json::{json, Value as J};

#[derive(Clone, Debug)]
pub struct StepReport {
    pub id: String,
    pub name: String,
    /// success | failure | skipped | cancelled
    pub result: String,
    pub code: Option<i32>,
    pub duration_ms: u64,
    pub attempts: u32,
    pub outputs: J,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct JobReport {
    pub id: String,
    pub name: String,
    pub result: String,
    pub duration_ms: u64,
    pub attempts: u32,
    pub outputs: J,
    pub continue_on_error: bool,
    pub steps: Vec<StepReport>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub name: String,
    /// success | failure | cancelled
    pub outcome: String,
    pub duration_ms: u64,
    pub jobs: Vec<JobReport>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.outcome == "success"
    }

    pub fn to_json(&self) -> J {
        json!({
            "name": self.name,
            "outcome": self.outcome,
            "duration_ms": self.duration_ms,
            "jobs": self.jobs.iter().map(|j| json!({
                "id": j.id, "name": j.name, "result": j.result, "duration_ms": j.duration_ms,
                "attempts": j.attempts, "outputs": j.outputs, "continue_on_error": j.continue_on_error, "error": j.error,
                "steps": j.steps.iter().map(|s| json!({
                    "id": s.id, "name": s.name, "result": s.result, "code": s.code,
                    "duration_ms": s.duration_ms, "attempts": s.attempts, "outputs": s.outputs, "error": s.error,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }

    /// Human readable summary table.
    pub fn summary(&self) -> String {
        let mark = |r: &str| match r {
            "success" => "ok  ",
            "failure" => "FAIL",
            "skipped" => "skip",
            _ => "stop",
        };
        let w = self.jobs.iter().map(|j| j.id.chars().count()).max().unwrap_or(3).max(3);
        let mut s = format!("workflow {} — {} in {:.2}s\n", self.name, self.outcome, self.duration_ms as f64 / 1000.0);
        for j in &self.jobs {
            s.push_str(&format!("  {} {:<w$}  {:>8.2}s{}\n", mark(&j.result), j.id, j.duration_ms as f64 / 1000.0, if j.attempts > 1 { format!("  ({} attempts)", j.attempts) } else { String::new() }, w = w));
            if let Some(e) = &j.error {
                s.push_str(&format!("        {}\n", e));
            }
            for st in &j.steps {
                s.push_str(&format!("      {} {}{}\n", mark(&st.result), st.name, st.error.as_ref().map(|e| format!(" — {}", e)).unwrap_or_default()));
            }
        }
        s
    }
}
