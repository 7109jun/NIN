//! Windows Task Scheduler integration through `schtasks.exe` and exact Task Scheduler XML.
//!
//! Notes:
//! * Tasks live in the `\NIN\` folder (`NIN\<id>`), imported with `schtasks /Create /XML <file> /TN "NIN\<id>" /F`.
//! * `Boot` tasks run as SYSTEM and therefore need an elevated (administrator) shell to register.
//!   `schtasks` stderr is surfaced verbatim in [`SchedError::Os`].
//! * `Logon` tasks run as the current user (`%USERDOMAIN%\%USERNAME%`) when it is known.
//! * Task Scheduler repetition has a one-minute granularity: shorter intervals are rounded up to 1 minute.
//! * `monthly 31` maps to the "last day of month" selector (exact clamp); days 29/30 are literal and
//!   Task Scheduler skips months that lack that day.
//! * Task Scheduler cannot set environment variables; tasks with `env` are wrapped in `cmd.exe /d /s /c "set ... && cmd"`.

use crate::store::ScheduledTask;
use crate::trigger::{next_run, Trigger};
use crate::{Result, SchedError};
use chrono::{NaiveDateTime, Weekday};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Task Scheduler folder that holds all NIN tasks.
pub const DEFAULT_TASK_PREFIX: &str = "NIN";

/// Full task name for an id: `NIN\<id>`.
pub fn task_name_for(id: &str) -> String {
    format!("{DEFAULT_TASK_PREFIX}\\{id}")
}

/// Quote one argument following the Windows `CommandLineToArgvW` rules.
pub fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '\n', '\x0b', '"']) {
        return a.to_string();
    }
    let mut out = String::from("\"");
    let mut bs = 0usize;
    for c in a.chars() {
        match c {
            '\\' => bs += 1,
            '"' => {
                out.extend(std::iter::repeat('\\').take(bs * 2 + 1));
                out.push('"');
                bs = 0;
            }
            _ => {
                out.extend(std::iter::repeat('\\').take(bs));
                bs = 0;
                out.push(c);
            }
        }
    }
    out.extend(std::iter::repeat('\\').take(bs * 2));
    out.push('"');
    out
}

/// Join argv items into a Windows command line (without the program).
pub fn join_args(args: &[String]) -> String {
    args.iter().map(|a| quote_arg(a)).collect::<Vec<_>>().join(" ")
}

/// Escape text for XML content: `& < > " '`; XML-invalid control characters become spaces.
pub fn xml_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            '\t' | '\n' | '\r' => o.push(c),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => o.push(' '),
            c => o.push(c),
        }
    }
    o
}

/// ISO-8601 duration as used by Task Scheduler, e.g. `PT5M`, `P1DT2H`. Rounds up to whole seconds, minimum 1 minute.
pub fn iso_duration(d: Duration) -> String {
    let mut secs = d.as_secs() + u64::from(d.subsec_nanos() > 0);
    secs = secs.max(60);
    let (days, h, m, s) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60, secs % 60);
    let mut o = String::from("P");
    if days > 0 {
        o.push_str(&format!("{days}D"));
    }
    if h > 0 || m > 0 || s > 0 {
        o.push('T');
        if h > 0 {
            o.push_str(&format!("{h}H"));
        }
        if m > 0 {
            o.push_str(&format!("{m}M"));
        }
        if s > 0 {
            o.push_str(&format!("{s}S"));
        }
    }
    o
}

fn boundary(d: &NaiveDateTime) -> String {
    d.format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn weekday_xml(d: Weekday) -> &'static str {
    match d {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

fn current_user() -> Option<String> {
    let user = std::env::var("USERNAME").ok().filter(|s| !s.is_empty())?;
    Some(match std::env::var("USERDOMAIN").ok().filter(|s| !s.is_empty()) {
        Some(d) => format!("{d}\\{user}"),
        None => user,
    })
}

fn trigger_xml(task: &ScheduledTask, user: Option<&str>) -> String {
    let created_date = task.created.date();
    let mut x = String::new();
    match &task.trigger {
        Trigger::Interval { every, start } => {
            let sb = start.unwrap_or_else(|| next_run(&task.trigger, task.created, None).unwrap_or(task.created));
            x.push_str("    <TimeTrigger>\n");
            x.push_str("      <Repetition>\n");
            x.push_str(&format!("        <Interval>{}</Interval>\n", iso_duration(*every)));
            x.push_str("        <StopAtDurationEnd>false</StopAtDurationEnd>\n");
            x.push_str("      </Repetition>\n");
            x.push_str(&format!("      <StartBoundary>{}</StartBoundary>\n", boundary(&sb)));
            x.push_str("      <Enabled>true</Enabled>\n");
            x.push_str("    </TimeTrigger>\n");
        }
        Trigger::Once { at } => {
            x.push_str("    <TimeTrigger>\n");
            x.push_str(&format!("      <StartBoundary>{}</StartBoundary>\n", boundary(at)));
            x.push_str("      <Enabled>true</Enabled>\n");
            x.push_str("    </TimeTrigger>\n");
        }
        Trigger::Daily { time } => {
            x.push_str("    <CalendarTrigger>\n");
            x.push_str(&format!("      <StartBoundary>{}</StartBoundary>\n", boundary(&created_date.and_time(*time))));
            x.push_str("      <Enabled>true</Enabled>\n");
            x.push_str("      <ScheduleByDay>\n        <DaysInterval>1</DaysInterval>\n      </ScheduleByDay>\n");
            x.push_str("    </CalendarTrigger>\n");
        }
        Trigger::Weekly { days, time } => {
            x.push_str("    <CalendarTrigger>\n");
            x.push_str(&format!("      <StartBoundary>{}</StartBoundary>\n", boundary(&created_date.and_time(*time))));
            x.push_str("      <Enabled>true</Enabled>\n");
            x.push_str("      <ScheduleByWeek>\n        <DaysOfWeek>\n");
            for d in days {
                x.push_str(&format!("          <{}/>\n", weekday_xml(*d)));
            }
            x.push_str("        </DaysOfWeek>\n        <WeeksInterval>1</WeeksInterval>\n      </ScheduleByWeek>\n");
            x.push_str("    </CalendarTrigger>\n");
        }
        Trigger::Monthly { day, time } => {
            x.push_str("    <CalendarTrigger>\n");
            x.push_str(&format!("      <StartBoundary>{}</StartBoundary>\n", boundary(&created_date.and_time(*time))));
            x.push_str("      <Enabled>true</Enabled>\n");
            x.push_str("      <ScheduleByMonth>\n        <DaysOfMonth>\n");
            if *day >= 31 {
                x.push_str("          <Day>Last</Day>\n");
            } else {
                x.push_str(&format!("          <Day>{day}</Day>\n"));
            }
            x.push_str("        </DaysOfMonth>\n        <Months>\n");
            for m in ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"] {
                x.push_str(&format!("          <{m}/>\n"));
            }
            x.push_str("        </Months>\n      </ScheduleByMonth>\n");
            x.push_str("    </CalendarTrigger>\n");
        }
        Trigger::Logon => {
            x.push_str("    <LogonTrigger>\n");
            x.push_str("      <Enabled>true</Enabled>\n");
            if let Some(u) = user {
                x.push_str(&format!("      <UserId>{}</UserId>\n", xml_escape(u)));
            }
            x.push_str("    </LogonTrigger>\n");
        }
        Trigger::Boot => {
            x.push_str("    <BootTrigger>\n      <Enabled>true</Enabled>\n    </BootTrigger>\n");
        }
    }
    x
}

/// Characters that cannot be carried through the `cmd.exe` wrapper used for environment variables.
fn env_unsafe(s: &str) -> bool {
    s.chars().any(|c| matches!(c, '"' | '%' | '\r' | '\n' | '\0'))
}

fn cmd_quote(a: &str) -> String {
    if !a.is_empty() && !a.chars().any(|c| matches!(c, ' ' | '\t' | '&' | '|' | '<' | '>' | '^' | '(' | ')' | '"' | ',' | ';' | '=')) {
        a.to_string()
    } else {
        quote_arg(a)
    }
}

/// (Command, Arguments) of the Exec action.
fn exec_parts(task: &ScheduledTask) -> (String, String) {
    if task.env.is_empty() {
        let prog = task.command.first().cloned().unwrap_or_default();
        return (prog, join_args(task.command.get(1..).unwrap_or(&[])));
    }
    let mut line = String::new();
    for (k, v) in &task.env {
        line.push_str(&format!("set \"{k}={v}\" && "));
    }
    let cmdline: Vec<String> = task.command.iter().map(|a| cmd_quote(a)).collect();
    line.push_str(&cmdline.join(" "));
    ("cmd.exe".to_string(), format!("/d /s /c \"{line}\""))
}

/// Task Scheduler (schema 1.4) XML for `task`.
pub fn task_xml(task: &ScheduledTask) -> String {
    let user = current_user();
    let (cmd, args) = exec_parts(task);
    let mut x = String::new();
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n");
    x.push_str("<Task version=\"1.4\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n");
    x.push_str("  <RegistrationInfo>\n");
    x.push_str(&format!("    <Description>{}</Description>\n", xml_escape(&format!("NIN scheduled task: {}", task.name))));
    x.push_str(&format!("    <URI>\\{}</URI>\n", xml_escape(&task_name_for(&task.id))));
    x.push_str("  </RegistrationInfo>\n");
    x.push_str("  <Triggers>\n");
    x.push_str(&trigger_xml(task, user.as_deref()));
    x.push_str("  </Triggers>\n");
    x.push_str("  <Principals>\n");
    if matches!(task.trigger, Trigger::Boot) {
        x.push_str("    <Principal id=\"Author\">\n      <UserId>S-1-5-18</UserId>\n      <RunLevel>HighestAvailable</RunLevel>\n    </Principal>\n");
    } else {
        x.push_str("    <Principal id=\"Author\">\n      <LogonType>InteractiveToken</LogonType>\n      <RunLevel>LeastPrivilege</RunLevel>\n    </Principal>\n");
    }
    x.push_str("  </Principals>\n");
    x.push_str("  <Settings>\n");
    x.push_str("    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n");
    x.push_str("    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n");
    x.push_str("    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n");
    x.push_str("    <AllowHardTerminate>true</AllowHardTerminate>\n");
    x.push_str("    <StartWhenAvailable>true</StartWhenAvailable>\n");
    x.push_str("    <AllowStartOnDemand>true</AllowStartOnDemand>\n");
    x.push_str(&format!("    <Enabled>{}</Enabled>\n", task.enabled));
    x.push_str("    <Hidden>false</Hidden>\n");
    x.push_str("    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n");
    x.push_str("  </Settings>\n");
    x.push_str("  <Actions Context=\"Author\">\n    <Exec>\n");
    x.push_str(&format!("      <Command>{}</Command>\n", xml_escape(&cmd)));
    if !args.is_empty() {
        x.push_str(&format!("      <Arguments>{}</Arguments>\n", xml_escape(&args)));
    }
    if let Some(w) = task.workdir.as_deref().filter(|w| !w.is_empty()) {
        x.push_str(&format!("      <WorkingDirectory>{}</WorkingDirectory>\n", xml_escape(w)));
    }
    x.push_str("    </Exec>\n  </Actions>\n");
    x.push_str("</Task>\n");
    x
}

/// The XML as UTF-16LE bytes with a BOM (what `schtasks /XML` expects).
pub fn task_xml_utf16(task: &ScheduledTask) -> Vec<u8> {
    let xml = task_xml(task);
    let mut out = vec![0xFF, 0xFE];
    for u in xml.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

/// `schtasks` arguments to import an XML definition. `task_name` is the full name, e.g. `NIN\backup`.
pub fn schtasks_create_args(task_name: &str, xml_path: &Path) -> Vec<String> {
    vec!["/Create".into(), "/XML".into(), xml_path.to_string_lossy().into_owned(), "/TN".into(), task_name.into(), "/F".into()]
}

pub fn schtasks_delete_args(task_name: &str) -> Vec<String> {
    vec!["/Delete".into(), "/TN".into(), task_name.into(), "/F".into()]
}

pub fn schtasks_query_args(task_name: &str) -> Vec<String> {
    vec!["/Query".into(), "/TN".into(), task_name.into(), "/FO".into(), "LIST".into(), "/V".into()]
}

pub fn schtasks_run_args(task_name: &str) -> Vec<String> {
    vec!["/Run".into(), "/TN".into(), task_name.into()]
}

fn list_args() -> Vec<String> {
    vec!["/Query".into(), "/FO".into(), "CSV".into(), "/NH".into()]
}

/// True if the CSV output of `schtasks /Query /FO CSV /NH` contains a task named `\<task_name>`.
pub fn find_task_in_csv(csv: &str, task_name: &str) -> bool {
    let want = format!("\\{}", task_name.trim_start_matches('\\')).to_lowercase();
    csv.lines().any(|l| {
        let l = l.trim().trim_start_matches('\u{feff}');
        l.strip_prefix('"')
            .and_then(|r| r.split("\",").next())
            .map_or(false, |first| first.trim_end_matches('"').to_lowercase() == want)
    })
}

fn check_id(id: &str) -> Result<()> {
    if id.is_empty() || id.chars().any(|c| matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()) {
        return Err(SchedError::Parse(format!("invalid task id for the OS scheduler: '{id}'")));
    }
    Ok(())
}

/// Validate that a task can be expressed as a Task Scheduler action.
pub fn check_task_for_os(task: &ScheduledTask) -> Result<()> {
    check_id(&task.id)?;
    if task.command.is_empty() || task.command[0].is_empty() {
        return Err(SchedError::Parse("task command must not be empty".into()));
    }
    for (k, v) in &task.env {
        if k.is_empty() || k.contains(['=', '"', '%']) || env_unsafe(k) || env_unsafe(v) {
            return Err(SchedError::Parse(format!(
                "environment variable '{k}' cannot be registered with the OS scheduler (no '\"' or '%' allowed)"
            )));
        }
    }
    if !task.env.is_empty() && task.command.iter().any(|a| a.contains(['"', '%'])) {
        return Err(SchedError::Parse("with environment variables, arguments must not contain '\"' or '%'".into()));
    }
    Ok(())
}

fn run_schtasks(args: &[String]) -> Result<(bool, String, String)> {
    if !cfg!(windows) {
        return Err(SchedError::Unsupported("Windows Task Scheduler is only available on Windows".into()));
    }
    let out = Command::new("schtasks.exe").args(args).output()?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    ))
}

fn os_error(what: &str, stdout: &str, stderr: &str, boot: bool) -> SchedError {
    let mut m = format!("schtasks {what} failed");
    let detail = if stderr.is_empty() { stdout } else { stderr };
    if !detail.is_empty() {
        m.push_str(": ");
        m.push_str(detail);
    }
    if boot {
        m.push_str(" (boot tasks require an elevated administrator shell)");
    }
    SchedError::Os(m)
}

fn unsupported() -> SchedError {
    SchedError::Unsupported("Windows Task Scheduler is only available on Windows".into())
}

/// Register (or replace) the task with Windows Task Scheduler. `Boot` tasks need administrator rights.
pub fn os_register(task: &ScheduledTask) -> Result<()> {
    if !cfg!(windows) {
        return Err(unsupported());
    }
    check_task_for_os(task)?;
    let safe: String = task.id.chars().map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' }).collect();
    let path = std::env::temp_dir().join(format!("nin-sched-{}-{}.xml", safe, std::process::id()));
    std::fs::write(&path, task_xml_utf16(task))?;
    let res = run_schtasks(&schtasks_create_args(&task_name_for(&task.id), &path));
    let _ = std::fs::remove_file(&path);
    let (ok, so, se) = res?;
    if ok {
        Ok(())
    } else {
        Err(os_error("/Create", &so, &se, matches!(task.trigger, Trigger::Boot)))
    }
}

/// Remove the task from Task Scheduler; succeeds if it is already gone.
pub fn os_unregister(id: &str) -> Result<()> {
    if !cfg!(windows) {
        return Err(unsupported());
    }
    check_id(id)?;
    let name = task_name_for(id);
    let (ok, so, se) = run_schtasks(&schtasks_delete_args(&name))?;
    if ok {
        return Ok(());
    }
    let (lok, lout, _) = run_schtasks(&list_args())?;
    if lok && !find_task_in_csv(&lout, &name) {
        return Ok(());
    }
    Err(os_error("/Delete", &so, &se, false))
}

/// Verbose `schtasks /Query` text for the task, or `None` if it is not registered.
pub fn os_query(id: &str) -> Result<Option<String>> {
    if !cfg!(windows) {
        return Err(unsupported());
    }
    check_id(id)?;
    let name = task_name_for(id);
    let (lok, lout, lerr) = run_schtasks(&list_args())?;
    if !lok {
        return Err(os_error("/Query", &lout, &lerr, false));
    }
    if !find_task_in_csv(&lout, &name) {
        return Ok(None);
    }
    let (ok, so, se) = run_schtasks(&schtasks_query_args(&name))?;
    if ok {
        Ok(Some(so))
    } else {
        Err(os_error("/Query", &so, &se, false))
    }
}

/// Start the registered task immediately.
pub fn os_run_now(id: &str) -> Result<()> {
    if !cfg!(windows) {
        return Err(unsupported());
    }
    check_id(id)?;
    let (ok, so, se) = run_schtasks(&schtasks_run_args(&task_name_for(id)))?;
    if ok {
        Ok(())
    } else {
        Err(os_error("/Run", &so, &se, false))
    }
}

#[cfg(test)]
pub(crate) mod xmlcheck {
    //! Tiny XML well-formedness checker + element text extractor for tests.
    pub fn check(xml: &str) -> Result<(), String> {
        let b = xml.as_bytes();
        let mut i = 0;
        let mut stack: Vec<String> = Vec::new();
        let mut roots = 0;
        while i < b.len() {
            if b[i] == b'<' {
                let end = xml[i..].find('>').ok_or("unterminated tag")? + i;
                let inner = &xml[i + 1..end];
                if inner.starts_with('?') {
                    if !inner.ends_with('?') {
                        return Err("bad PI".into());
                    }
                } else if let Some(name) = inner.strip_prefix('/') {
                    let top = stack.pop().ok_or("close without open")?;
                    if top != name.trim() {
                        return Err(format!("mismatch {top} vs {name}"));
                    }
                } else {
                    let selfclose = inner.ends_with('/');
                    let body = inner.trim_end_matches('/');
                    let name = body.split_whitespace().next().ok_or("empty tag")?;
                    if !name.chars().all(|c| c.is_alphanumeric() || c == ':' || c == '_') {
                        return Err(format!("bad tag name {name}"));
                    }
                    // attributes: key="value"
                    let attrs = body[name.len()..].trim();
                    let mut rest = attrs;
                    while !rest.is_empty() {
                        let eq = rest.find('=').ok_or("attr without =")?;
                        let after = rest[eq + 1..].trim_start();
                        if !after.starts_with('"') {
                            return Err("attr not quoted".into());
                        }
                        let close = after[1..].find('"').ok_or("attr unterminated")? + 1;
                        if after[1..close].contains('<') {
                            return Err("< in attr".into());
                        }
                        rest = after[close + 1..].trim_start();
                    }
                    if stack.is_empty() {
                        roots += 1;
                    }
                    if !selfclose {
                        stack.push(name.to_string());
                    }
                }
                i = end + 1;
            } else {
                let next = xml[i..].find('<').map_or(b.len(), |p| p + i);
                let text = &xml[i..next];
                if text.contains('>') && !text.trim().is_empty() {
                    return Err("raw > in text".into());
                }
                let mut j = 0;
                let tb = text.as_bytes();
                while j < tb.len() {
                    if tb[j] == b'&' {
                        let semi = text[j..].find(';').ok_or("bare &")? + j;
                        let ent = &text[j + 1..semi];
                        if !matches!(ent, "amp" | "lt" | "gt" | "quot" | "apos") && !ent.starts_with('#') {
                            return Err(format!("bad entity {ent}"));
                        }
                        j = semi;
                    }
                    j += 1;
                }
                i = next;
            }
        }
        if !stack.is_empty() {
            return Err(format!("unclosed {stack:?}"));
        }
        if roots != 1 {
            return Err(format!("{roots} roots"));
        }
        Ok(())
    }

    pub fn unescape(s: &str) -> String {
        s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
    }

    /// Text of the first `<name>...</name>` (unescaped).
    pub fn text_of(xml: &str, name: &str) -> Option<String> {
        let s = xml.find(&format!("<{name}>"))? + name.len() + 2;
        let e = xml[s..].find(&format!("</{name}>"))? + s;
        Some(unescape(&xml[s..e]))
    }
}

#[cfg(test)]
mod tests {
    use super::xmlcheck::*;
    use super::*;
    use crate::store::tests::{dt, task};

    #[test]
    fn quoting() {
        assert_eq!(quote_arg("abc"), "abc");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote_arg("C:\\dir\\"), "C:\\dir\\");
        assert_eq!(quote_arg("C:\\my dir\\"), "\"C:\\my dir\\\\\"");
        assert_eq!(quote_arg("a\\\"b"), "\"a\\\\\\\"b\"");
        assert_eq!(join_args(&["x".into(), "y z".into(), "".into()]), "x \"y z\" \"\"");
    }

    #[test]
    fn escaping() {
        assert_eq!(xml_escape("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d&quot;e&apos;f");
        assert_eq!(xml_escape("a\u{1}b"), "a b");
        assert_eq!(xml_escape("한글"), "한글");
    }

    #[test]
    fn iso() {
        assert_eq!(iso_duration(Duration::from_secs(300)), "PT5M");
        assert_eq!(iso_duration(Duration::from_secs(7200)), "PT2H");
        assert_eq!(iso_duration(Duration::from_secs(86400)), "P1D");
        assert_eq!(iso_duration(Duration::from_secs(90000)), "P1DT1H");
        assert_eq!(iso_duration(Duration::from_secs(90)), "PT1M30S");
        assert_eq!(iso_duration(Duration::from_secs(5)), "PT1M");
        assert_eq!(iso_duration(Duration::from_millis(60_001)), "PT1M1S");
    }

    fn xml_for(trig: &str) -> String {
        let t = task("job", trig, "2026-11-04 10:00");
        let x = task_xml(&t);
        check(&x).unwrap_or_else(|e| panic!("{e}\n{x}"));
        x
    }

    #[test]
    fn xml_interval_and_once() {
        let x = xml_for("every 5m");
        assert!(x.contains("<TimeTrigger>") && x.contains("<Interval>PT5M</Interval>"));
        assert!(x.contains("<StartBoundary>2026-11-04T10:05:00</StartBoundary>"));
        let x = xml_for("every 2h from 2026-11-05 09:00");
        assert!(x.contains("<Interval>PT2H</Interval>") && x.contains("<StartBoundary>2026-11-05T09:00:00</StartBoundary>"));
        let x = xml_for("at 2026-11-05T09:00:30");
        assert!(x.contains("<TimeTrigger>") && !x.contains("<Repetition>"));
        assert!(x.contains("<StartBoundary>2026-11-05T09:00:30</StartBoundary>"));
    }

    #[test]
    fn xml_calendar_triggers() {
        let x = xml_for("daily 03:00");
        assert!(x.contains("<CalendarTrigger>") && x.contains("<ScheduleByDay>") && x.contains("<DaysInterval>1</DaysInterval>"));
        assert!(x.contains("2026-11-04T03:00:00"));
        let x = xml_for("weekly mon,fri 09:00");
        assert!(x.contains("<ScheduleByWeek>") && x.contains("<Monday/>") && x.contains("<Friday/>") && !x.contains("<Tuesday/>"));
        assert!(x.contains("<WeeksInterval>1</WeeksInterval>"));
        let x = xml_for("monthly 1 02:30");
        assert!(x.contains("<ScheduleByMonth>") && x.contains("<Day>1</Day>") && x.contains("<January/>") && x.contains("<December/>"));
        assert!(x.contains("2026-11-04T02:30:00"));
        let x = xml_for("monthly 31 02:30");
        assert!(x.contains("<Day>Last</Day>") && !x.contains("<Day>31</Day>"));
    }

    #[test]
    fn xml_event_triggers() {
        let x = xml_for("logon");
        assert!(x.contains("<LogonTrigger>") && x.contains("InteractiveToken"));
        let x = xml_for("boot");
        assert!(x.contains("<BootTrigger>") && x.contains("S-1-5-18") && x.contains("HighestAvailable"));
    }

    #[test]
    fn xml_action_escaping_and_workdir() {
        let mut t = task("job", "daily 03:00", "2026-11-04 10:00");
        t.name = "A & <B>".into();
        t.command = vec!["C:\\Program Files\\nin.exe".into(), "run".into(), "a \"b\" & <c> 'd'.nin".into(), "plain".into(), "".into()];
        t.workdir = Some("C:\\w & d".into());
        let x = task_xml(&t);
        check(&x).unwrap_or_else(|e| panic!("{e}\n{x}"));
        assert_eq!(text_of(&x, "Command").unwrap(), "C:\\Program Files\\nin.exe");
        assert_eq!(text_of(&x, "Arguments").unwrap(), "run \"a \\\"b\\\" & <c> 'd'.nin\" plain \"\"");
        assert_eq!(text_of(&x, "WorkingDirectory").unwrap(), "C:\\w & d");
        assert_eq!(text_of(&x, "Description").unwrap(), "NIN scheduled task: A & <B>");
        assert_eq!(text_of(&x, "URI").unwrap(), "\\NIN\\job");
        assert!(!x.contains("A & <B>"));
        t.enabled = false;
        t.workdir = None;
        t.command = vec!["x.exe".into()];
        let x = task_xml(&t);
        assert!(x.contains("<Enabled>false</Enabled>") && !x.contains("<Arguments>") && !x.contains("<WorkingDirectory>"));
    }

    #[test]
    fn xml_env_wrapper() {
        let mut t = task("job", "daily 03:00", "2026-11-04 10:00");
        t.env = vec![("A".into(), "1 2".into()), ("B".into(), "x".into())];
        t.command = vec!["nin.exe".into(), "run".into(), "my file.nin".into()];
        let x = task_xml(&t);
        check(&x).unwrap();
        assert_eq!(text_of(&x, "Command").unwrap(), "cmd.exe");
        assert_eq!(text_of(&x, "Arguments").unwrap(), "/d /s /c \"set \"A=1 2\" && set \"B=x\" && nin.exe run \"my file.nin\"\"");
        assert!(check_task_for_os(&t).is_ok());
        t.env.push(("C".into(), "50%".into()));
        assert!(matches!(check_task_for_os(&t), Err(SchedError::Parse(_))));
    }

    #[test]
    fn xml_utf16() {
        let t = task("job", "logon", "2026-11-04 10:00");
        let b = task_xml_utf16(&t);
        assert_eq!(&b[..2], &[0xFF, 0xFE]);
        assert_eq!((b.len() - 2) % 2, 0);
        let units: Vec<u16> = b[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(String::from_utf16(&units).unwrap(), task_xml(&t));
        let mut k = task("kor", "logon", "2026-11-04 10:00");
        k.name = "백업 😀".into();
        let b = task_xml_utf16(&k);
        let units: Vec<u16> = b[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert!(String::from_utf16(&units).unwrap().contains("백업 😀"));
    }

    #[test]
    fn checker_rejects_bad_xml() {
        assert!(check("<a><b></a></b>").is_err());
        assert!(check("<a>x & y</a>").is_err());
        assert!(check("<a>").is_err());
        assert!(check("<a/><b/>").is_err());
        assert!(check("<a x=\"1\">&amp;</a>").is_ok());
    }

    #[test]
    fn schtasks_arguments() {
        let a = schtasks_create_args("NIN\\job", Path::new("C:\\t\\x.xml"));
        assert_eq!(a, ["/Create", "/XML", "C:\\t\\x.xml", "/TN", "NIN\\job", "/F"]);
        assert_eq!(schtasks_delete_args("NIN\\job"), ["/Delete", "/TN", "NIN\\job", "/F"]);
        assert_eq!(schtasks_run_args("NIN\\job"), ["/Run", "/TN", "NIN\\job"]);
        assert_eq!(schtasks_query_args("NIN\\job"), ["/Query", "/TN", "NIN\\job", "/FO", "LIST", "/V"]);
        assert_eq!(task_name_for("job"), "NIN\\job");
    }

    #[test]
    fn csv_lookup() {
        let csv = "\"\\NIN\\job\",\"11/5/2026 9:00:00 AM\",\"Ready\"\r\n\"\\Other\",\"N/A\",\"Ready\"\r\n";
        assert!(find_task_in_csv(csv, "NIN\\job"));
        assert!(find_task_in_csv(csv, "nin\\JOB"));
        assert!(!find_task_in_csv(csv, "NIN\\jo"));
        assert!(!find_task_in_csv("INFO: There are no scheduled tasks", "NIN\\job"));
    }

    #[test]
    fn id_validation() {
        let mut t = task("a\\b", "logon", "2026-11-04 10:00");
        assert!(check_task_for_os(&t).is_err());
        t.id = "ok-id".into();
        assert!(check_task_for_os(&t).is_ok());
        t.command.clear();
        assert!(check_task_for_os(&t).is_err());
        let _ = dt("2026-01-01 00:00");
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_off_windows() {
        let t = task("job", "logon", "2026-11-04 10:00");
        assert!(matches!(os_register(&t), Err(SchedError::Unsupported(_))));
        assert!(matches!(os_unregister("job"), Err(SchedError::Unsupported(_))));
        assert!(matches!(os_query("job"), Err(SchedError::Unsupported(_))));
        assert!(matches!(os_run_now("job"), Err(SchedError::Unsupported(_))));
    }
}
