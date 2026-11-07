//! `schedules.toml` persistence, id generation and due-task selection.

use crate::trigger::{next_run, parse_trigger, Trigger};
use crate::{Result, SchedError};
use chrono::NaiveDateTime;
use std::path::{Path, PathBuf};
use toml::{Table, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledTask {
    /// Unique slug (see [`make_id`]).
    pub id: String,
    pub name: String,
    /// argv, e.g. `["nin.exe", "run", "script.nin"]`.
    pub command: Vec<String>,
    pub workdir: Option<String>,
    pub env: Vec<(String, String)>,
    pub trigger: Trigger,
    pub enabled: bool,
    /// True if the task is registered with the OS scheduler (then the in-process runner skips it).
    pub os_registered: bool,
    pub created: NaiveDateTime,
    pub last_run: Option<NaiveDateTime>,
    pub last_exit: Option<i32>,
    pub run_count: u64,
}

impl ScheduledTask {
    /// New enabled task with empty history.
    pub fn new(id: &str, name: &str, command: Vec<String>, trigger: Trigger, created: NaiveDateTime) -> Self {
        ScheduledTask {
            id: id.to_string(),
            name: name.to_string(),
            command,
            workdir: None,
            env: Vec::new(),
            trigger,
            enabled: true,
            os_registered: false,
            created,
            last_run: None,
            last_exit: None,
            run_count: 0,
        }
    }
}

/// Build a unique slug id from a name: lowercase alphanumerics, other runs become `-`; `-2`, `-3`.. on collision.
pub fn make_id(name: &str, existing: &[ScheduledTask]) -> String {
    let mut slug = String::new();
    let mut dash = true;
    for c in name.chars() {
        if c.is_alphanumeric() {
            slug.extend(c.to_lowercase());
            dash = false;
        } else if !dash {
            slug.push('-');
            dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        slug.push_str("task");
    }
    let taken = |s: &str| existing.iter().any(|t| t.id == s);
    if !taken(&slug) {
        return slug;
    }
    let mut n = 2;
    loop {
        let c = format!("{slug}-{n}");
        if !taken(&c) {
            return c;
        }
        n += 1;
    }
}

#[derive(Debug)]
pub struct Store {
    path: PathBuf,
    tasks: Vec<ScheduledTask>,
}

const DT: &str = "%Y-%m-%dT%H:%M:%S";

fn terr(m: impl Into<String>) -> SchedError {
    SchedError::Toml(m.into())
}

impl Store {
    /// New empty store bound to `path` (nothing is read or written).
    pub fn new(path: &Path) -> Store {
        Store { path: path.to_path_buf(), tasks: Vec::new() }
    }

    /// `%APPDATA%\nin\schedules.toml` on Windows; `$XDG_CONFIG_HOME/nin/schedules.toml` or `~/.config/nin/schedules.toml` elsewhere.
    pub fn default_path() -> PathBuf {
        let base = if cfg!(windows) {
            std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("AppData").join("Roaming")))
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        };
        base.unwrap_or_else(|| PathBuf::from(".")).join("nin").join("schedules.toml")
    }

    /// Load the store; a missing file yields an empty store.
    pub fn open(path: &Path) -> Result<Store> {
        let mut store = Store::new(path);
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(store),
            Err(e) => return Err(e.into()),
        };
        let root: Table = text.parse().map_err(|e| terr(format!("{}: {e}", path.display())))?;
        if let Some(v) = root.get("task") {
            let arr = v.as_array().ok_or_else(|| terr("'task' must be an array of tables"))?;
            for (i, item) in arr.iter().enumerate() {
                let tbl = item.as_table().ok_or_else(|| terr(format!("task #{} is not a table", i + 1)))?;
                let t = table_to_task(tbl).map_err(|e| terr(format!("task #{}: {e}", i + 1)))?;
                if store.get(&t.id).is_some() {
                    return Err(SchedError::Duplicate(t.id));
                }
                store.tasks.push(t);
            }
        }
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn list(&self) -> &[ScheduledTask] {
        &self.tasks
    }
    pub fn get(&self, id: &str) -> Option<&ScheduledTask> {
        self.tasks.iter().find(|t| t.id == id)
    }
    pub fn get_mut(&mut self, id: &str) -> Option<&mut ScheduledTask> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    /// Add in memory (call [`Store::save`] to persist).
    pub fn add(&mut self, t: ScheduledTask) -> Result<()> {
        if t.id.trim().is_empty() {
            return Err(SchedError::Parse("task id must not be empty".into()));
        }
        if t.command.is_empty() {
            return Err(SchedError::Parse("task command must not be empty".into()));
        }
        if self.get(&t.id).is_some() {
            return Err(SchedError::Duplicate(t.id));
        }
        self.tasks.push(t);
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Result<ScheduledTask> {
        let i = self.tasks.iter().position(|t| t.id == id).ok_or_else(|| SchedError::NotFound(id.into()))?;
        Ok(self.tasks.remove(i))
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<()> {
        self.get_mut(id).ok_or_else(|| SchedError::NotFound(id.into()))?.enabled = enabled;
        Ok(())
    }

    pub fn set_os_registered(&mut self, id: &str, v: bool) -> Result<()> {
        self.get_mut(id).ok_or_else(|| SchedError::NotFound(id.into()))?.os_registered = v;
        Ok(())
    }

    /// Record a run (start time + exit code, increments `run_count`).
    /// Unknown ids are ignored.
    pub fn record_run(&mut self, id: &str, at: &NaiveDateTime, exit: i32) {
        if let Some(t) = self.get_mut(id) {
            t.last_run = Some(*at);
            t.last_exit = Some(exit);
            t.run_count += 1;
        }
    }

    /// Atomic save: write `<file>.tmp`, fsync, rename over the target.
    pub fn save(&self) -> Result<()> {
        use std::io::Write;
        if let Some(dir) = self.path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let mut root = Table::new();
        root.insert("version".into(), Value::Integer(1));
        root.insert("task".into(), Value::Array(self.tasks.iter().map(|t| Value::Table(task_to_table(t))).collect()));
        let text = format!("# NIN schedules (managed by `nin schedule`)\n{}", root);
        let mut name = self.path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        name.push(".tmp");
        let tmp = self.path.with_file_name(name);
        let res = (|| -> std::io::Result<()> {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            drop(f);
            std::fs::rename(&tmp, &self.path)
        })();
        if let Err(e) = res {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(())
    }
}

fn task_to_table(t: &ScheduledTask) -> Table {
    let mut m = Table::new();
    m.insert("id".into(), t.id.clone().into());
    m.insert("name".into(), t.name.clone().into());
    m.insert("command".into(), Value::Array(t.command.iter().map(|s| s.clone().into()).collect()));
    if let Some(w) = &t.workdir {
        m.insert("workdir".into(), w.clone().into());
    }
    if !t.env.is_empty() {
        m.insert("env".into(), Value::Array(t.env.iter().map(|(k, v)| format!("{k}={v}").into()).collect()));
    }
    m.insert("trigger".into(), t.trigger.to_spec_string().into());
    m.insert("enabled".into(), t.enabled.into());
    m.insert("os_registered".into(), t.os_registered.into());
    m.insert("created".into(), t.created.format(DT).to_string().into());
    if let Some(l) = &t.last_run {
        m.insert("last_run".into(), l.format(DT).to_string().into());
    }
    if let Some(e) = t.last_exit {
        m.insert("last_exit".into(), Value::Integer(e as i64));
    }
    m.insert("run_count".into(), Value::Integer(t.run_count.min(i64::MAX as u64) as i64));
    m
}

fn req_str<'a>(m: &'a Table, k: &str) -> Result<&'a str> {
    m.get(k).and_then(Value::as_str).ok_or_else(|| terr(format!("missing or invalid '{k}'")))
}

fn opt_dt(m: &Table, k: &str) -> Result<Option<NaiveDateTime>> {
    match m.get(k) {
        None => Ok(None),
        Some(v) => {
            let s = v.as_str().ok_or_else(|| terr(format!("'{k}' must be a string")))?;
            NaiveDateTime::parse_from_str(s, DT).map(Some).map_err(|e| terr(format!("'{k}': {e}")))
        }
    }
}

fn table_to_task(m: &Table) -> Result<ScheduledTask> {
    let command: Vec<String> = m
        .get("command")
        .and_then(Value::as_array)
        .ok_or_else(|| terr("missing 'command'"))?
        .iter()
        .map(|v| v.as_str().map(str::to_string).ok_or_else(|| terr("'command' must be strings")))
        .collect::<Result<_>>()?;
    let mut env = Vec::new();
    if let Some(v) = m.get("env") {
        for e in v.as_array().ok_or_else(|| terr("'env' must be an array"))? {
            let s = e.as_str().ok_or_else(|| terr("'env' entries must be strings"))?;
            let (k, val) = s.split_once('=').ok_or_else(|| terr(format!("bad env entry '{s}'")))?;
            env.push((k.to_string(), val.to_string()));
        }
    }
    let flag = |k: &str, d: bool| m.get(k).and_then(Value::as_bool).unwrap_or(d);
    Ok(ScheduledTask {
        id: req_str(m, "id")?.to_string(),
        name: req_str(m, "name")?.to_string(),
        command,
        workdir: m.get("workdir").and_then(Value::as_str).map(str::to_string),
        env,
        trigger: parse_trigger(req_str(m, "trigger")?)?,
        enabled: flag("enabled", true),
        os_registered: flag("os_registered", false),
        created: opt_dt(m, "created")?.ok_or_else(|| terr("missing 'created'"))?,
        last_run: opt_dt(m, "last_run")?,
        last_exit: m.get("last_exit").and_then(Value::as_integer).map(|v| v as i32),
        run_count: m.get("run_count").and_then(Value::as_integer).unwrap_or(0).max(0) as u64,
    })
}

/// Slot a task is waiting for, given the baseline `base` (>= last run / creation).
/// `Once` is pending until it has run (fires late if overdue).
pub(crate) fn pending_slot(t: &ScheduledTask, base: NaiveDateTime) -> Option<NaiveDateTime> {
    match &t.trigger {
        Trigger::Once { at } => t.last_run.is_none().then_some(*at),
        Trigger::Logon | Trigger::Boot => None,
        tr => next_run(tr, base, Some(base)),
    }
}

/// Ids of enabled, non-OS-registered clock tasks whose pending slot (after last run, or creation) is `<= now`.
pub fn due_tasks(store: &Store, now: NaiveDateTime) -> Vec<String> {
    due_tasks_with(store, now, false)
}

/// Like [`due_tasks`]; with `include_os_registered` OS-registered tasks are also considered.
pub fn due_tasks_with(store: &Store, now: NaiveDateTime, include_os_registered: bool) -> Vec<String> {
    store
        .list()
        .iter()
        .filter(|t| t.enabled && (include_os_registered || !t.os_registered))
        .filter(|t| {
            let base = t.last_run.unwrap_or(t.created);
            pending_slot(t, base).map_or(false, |s| s <= now)
        })
        .map(|t| t.id.clone())
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub fn dt(s: &str) -> NaiveDateTime {
        crate::trigger::parse_datetime(s).unwrap()
    }
    pub fn tmp_dir(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!("nin-sched-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    pub fn task(id: &str, trig: &str, created: &str) -> ScheduledTask {
        ScheduledTask::new(id, id, vec!["nin.exe".into(), "run".into(), "a.nin".into()], parse_trigger(trig).unwrap(), dt(created))
    }

    #[test]
    fn make_id_slugs() {
        let mut ex = vec![];
        assert_eq!(make_id("Nightly Backup!", &ex), "nightly-backup");
        assert_eq!(make_id("  --A__b  ", &ex), "a-b");
        assert_eq!(make_id("!!!", &ex), "task");
        assert_eq!(make_id("백업 작업", &ex), "백업-작업");
        ex.push(task("nightly-backup", "logon", "2026-01-01 00:00"));
        assert_eq!(make_id("nightly backup", &ex), "nightly-backup-2");
        ex.push(task("nightly-backup-2", "logon", "2026-01-01 00:00"));
        assert_eq!(make_id("Nightly Backup", &ex), "nightly-backup-3");
    }

    #[test]
    fn store_round_trip() {
        let dir = tmp_dir("rt");
        let path = dir.join("sub").join("schedules.toml");
        assert!(Store::open(&path).unwrap().list().is_empty());
        let mut s = Store::open(&path).unwrap();
        let mut t = task("a", "weekly mon,fri 09:00", "2026-11-04 10:00:00");
        t.workdir = Some("C:\\work dir".into());
        t.env = vec![("K".into(), "v=1 \"q\"".into()), ("E".into(), "".into())];
        t.command = vec!["nin.exe".into(), "run".into(), "a \"b\".nin".into(), "ü\n".into()];
        t.last_run = Some(dt("2026-11-05 09:00:01"));
        t.last_exit = Some(-3);
        t.run_count = 7;
        t.enabled = false;
        t.os_registered = true;
        s.add(t.clone()).unwrap();
        s.add(task("b", "every 5m from 2026-11-05 09:00", "2026-11-04 10:00")).unwrap();
        s.add(task("c", "at 2026-11-05 09:00:30", "2026-11-04 10:00")).unwrap();
        s.save().unwrap();
        let s2 = Store::open(&path).unwrap();
        assert_eq!(s2.list(), s.list());
        assert_eq!(s2.get("a"), Some(&t));
        assert!(!dir.join("sub").join("schedules.toml.tmp").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn atomic_save_overwrites_and_leaves_no_tmp() {
        let dir = tmp_dir("atomic");
        let path = dir.join("schedules.toml");
        let mut s = Store::new(&path);
        s.add(task("a", "logon", "2026-01-01 00:00")).unwrap();
        s.save().unwrap();
        s.add(task("b", "boot", "2026-01-01 00:00")).unwrap();
        s.save().unwrap();
        assert_eq!(Store::open(&path).unwrap().list().len(), 2);
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["schedules.toml".to_string()]);
        // stale tmp from a crashed save does not affect loading
        std::fs::write(dir.join("schedules.toml.tmp"), "garbage {{{").unwrap();
        assert_eq!(Store::open(&path).unwrap().list().len(), 2);
        s.save().unwrap();
        assert!(!dir.join("schedules.toml.tmp").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn crud_and_duplicates() {
        let mut s = Store::new(Path::new("x.toml"));
        s.add(task("a", "logon", "2026-01-01 00:00")).unwrap();
        assert!(matches!(s.add(task("a", "boot", "2026-01-01 00:00")), Err(SchedError::Duplicate(id)) if id == "a"));
        let mut bad = task("", "logon", "2026-01-01 00:00");
        assert!(matches!(s.add(bad.clone()), Err(SchedError::Parse(_))));
        bad.id = "z".into();
        bad.command.clear();
        assert!(matches!(s.add(bad), Err(SchedError::Parse(_))));
        s.set_enabled("a", false).unwrap();
        assert!(!s.get("a").unwrap().enabled);
        assert!(matches!(s.set_enabled("nope", true), Err(SchedError::NotFound(_))));
        s.record_run("a", &dt("2026-02-02 02:02"), 5);
        let t = s.get("a").unwrap();
        assert_eq!((t.last_exit, t.run_count, t.last_run), (Some(5), 1, Some(dt("2026-02-02 02:02"))));
        s.record_run("a", &dt("2026-02-03 02:02"), 0);
        assert_eq!(s.get("a").unwrap().run_count, 2);
        s.record_run("q", &dt("2026-02-03 02:02"), 0);
        assert_eq!(s.list().len(), 1);
        assert_eq!(s.remove("a").unwrap().id, "a");
        assert!(matches!(s.remove("a"), Err(SchedError::NotFound(_))));
    }

    #[test]
    fn open_errors() {
        let dir = tmp_dir("err");
        let p = dir.join("s.toml");
        std::fs::write(&p, "not [valid").unwrap();
        assert!(matches!(Store::open(&p), Err(SchedError::Toml(_))));
        std::fs::write(&p, "[[task]]\nid=\"a\"\n").unwrap();
        assert!(matches!(Store::open(&p), Err(SchedError::Toml(_))));
        let one = "[[task]]\nid=\"a\"\nname=\"a\"\ncommand=[\"x\"]\ntrigger=\"logon\"\ncreated=\"2026-01-01T00:00:00\"\n";
        std::fs::write(&p, format!("{one}{one}")).unwrap();
        assert!(matches!(Store::open(&p), Err(SchedError::Duplicate(_))));
        std::fs::write(&p, one.replace("logon", "bogus")).unwrap();
        { let r = Store::open(&p); assert!(matches!(&r, Err(SchedError::Toml(m)) if m.contains("unknown trigger"))); }
        std::fs::write(&p, one).unwrap();
        let s = Store::open(&p).unwrap();
        assert!(s.get("a").unwrap().enabled);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn default_path_shape() {
        let p = Store::default_path();
        assert!(p.ends_with(Path::new("nin").join("schedules.toml")));
    }

    #[test]
    fn due_semantics() {
        let mut s = Store::new(Path::new("x.toml"));
        s.add(task("iv", "every 5m", "2026-11-05 09:00")).unwrap();
        s.add(task("daily", "daily 03:00", "2026-11-05 09:00")).unwrap();
        s.add(task("once", "at 2026-11-05 12:00", "2026-11-05 09:00")).unwrap();
        s.add(task("logon", "logon", "2026-11-05 09:00")).unwrap();
        let mut off = task("off", "every 1m", "2026-11-05 09:00");
        off.enabled = false;
        s.add(off).unwrap();
        let mut os = task("os", "every 1m", "2026-11-05 09:00");
        os.os_registered = true;
        s.add(os).unwrap();

        assert!(due_tasks(&s, dt("2026-11-05 09:04:59")).is_empty());
        assert_eq!(due_tasks(&s, dt("2026-11-05 09:05")), vec!["iv"]);
        assert_eq!(due_tasks_with(&s, dt("2026-11-05 09:05"), true), vec!["iv", "os"]);
        assert_eq!(due_tasks(&s, dt("2026-11-05 12:00")), vec!["iv", "once"]);
        assert_eq!(due_tasks(&s, dt("2026-11-06 03:00")), vec!["iv", "daily", "once"]);

        // after running, no longer due for that slot
        s.record_run("iv", &dt("2026-11-06 03:00:10"), 0);
        s.record_run("once", &dt("2026-11-06 03:00:10"), 0);
        s.record_run("daily", &dt("2026-11-06 03:00:10"), 0);
        assert!(due_tasks(&s, dt("2026-11-06 03:04")).is_empty());
        assert_eq!(due_tasks(&s, dt("2026-11-06 03:05:10")), vec!["iv"]);
        // overdue Once fires late (not before its time though)
        assert_eq!(due_tasks(&s, dt("2026-11-07 03:00")), vec!["iv", "daily"]);
    }
}
