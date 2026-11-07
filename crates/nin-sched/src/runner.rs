//! In-process runner used by `nin schedule daemon`.
//!
//! Semantics: when a task is first seen by the runner (start-up, newly added, or re-enabled) its next slot is
//! armed. Slots that were missed before that point are skipped unless `catch_up(true)`, which fires the task
//! once for the latest missed slot. Slots missed while the loop is running (e.g. a long task) also fire once only.
//! `Logon`/`Boot` tasks fire once on the first tick when `run_startup_tasks(true)`.
//! Tasks with `os_registered` are left to Windows Task Scheduler.

use crate::store::{pending_slot, ScheduledTask, Store};
use crate::trigger::{next_run, Trigger};
use chrono::NaiveDateTime;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub struct Runner {
    store: Store,
    clock: Box<dyn Fn() -> NaiveDateTime + Send>,
    catch_up: bool,
    startup: bool,
    startup_done: bool,
    /// id -> next slot (None = nothing more to run).
    armed: HashMap<String, Option<NaiveDateTime>>,
    save_error: Option<String>,
}

impl Runner {
    pub fn new(store: Store) -> Self {
        Self::with_clock(store, Box::new(|| chrono::Local::now().naive_local()))
    }

    pub fn with_clock(store: Store, clock: Box<dyn Fn() -> NaiveDateTime + Send>) -> Self {
        Runner { store, clock, catch_up: false, startup: false, startup_done: false, armed: HashMap::new(), save_error: None }
    }

    pub fn catch_up(&mut self, on: bool) -> &mut Self {
        self.catch_up = on;
        self
    }

    pub fn run_startup_tasks(&mut self, flag: bool) -> &mut Self {
        self.startup = flag;
        self
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    /// Error text of the most recent failed state save, if any.
    pub fn last_save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    pub fn into_store(self) -> Store {
        self.store
    }

    fn arm(&self, t: &ScheduledTask, now: NaiveDateTime) -> Option<NaiveDateTime> {
        if t.trigger.is_event() {
            return None;
        }
        let base = t.last_run.unwrap_or(t.created);
        let slot = pending_slot(t, base)?;
        if slot > now {
            return Some(slot);
        }
        // overdue at arm time
        if self.catch_up {
            return Some(slot);
        }
        match t.trigger {
            Trigger::Once { .. } => None,
            _ => next_run(&t.trigger, now, Some(now)),
        }
    }

    /// Fire everything that is due; returns ids fired (startup tasks first, then due tasks in store order).
    pub fn tick(&mut self, exec: &mut dyn FnMut(&ScheduledTask) -> i32) -> Vec<String> {
        let now = (self.clock)();
        let mut fired = Vec::new();

        // forget removed / disabled tasks so re-enabling re-arms from "now"
        let live: Vec<String> =
            self.store.list().iter().filter(|t| t.enabled && !t.os_registered).map(|t| t.id.clone()).collect();
        self.armed.retain(|id, _| live.contains(id));

        if self.startup && !self.startup_done {
            self.startup_done = true;
            let ids: Vec<String> = self
                .store
                .list()
                .iter()
                .filter(|t| t.enabled && !t.os_registered && t.trigger.is_event())
                .map(|t| t.id.clone())
                .collect();
            for id in ids {
                self.fire(&id, now, exec, &mut fired);
            }
        }

        for id in live {
            if !self.armed.contains_key(&id) {
                let slot = self.store.get(&id).and_then(|t| self.arm(t, now));
                self.armed.insert(id.clone(), slot);
            }
            if matches!(self.armed.get(&id), Some(Some(s)) if *s <= now) {
                self.fire(&id, now, exec, &mut fired);
                let next = self.store.get(&id).and_then(|t| match t.trigger {
                    Trigger::Once { .. } => None,
                    _ => next_run(&t.trigger, now, Some(now)),
                });
                self.armed.insert(id, next);
            }
        }

        if !fired.is_empty() {
            self.save_error = self.store.save().err().map(|e| e.to_string());
        }
        fired
    }

    fn fire(&mut self, id: &str, now: NaiveDateTime, exec: &mut dyn FnMut(&ScheduledTask) -> i32, fired: &mut Vec<String>) {
        let Some(task) = self.store.get(id).cloned() else { return };
        let code = exec(&task);
        self.store.record_run(id, &now, code);
        fired.push(id.to_string());
    }

    /// Tick every `tick` until `stop` is set. Sleeps in short slices so `stop` is honoured promptly.
    pub fn run_loop(&mut self, stop: &AtomicBool, tick: Duration, exec: &mut dyn FnMut(&ScheduledTask) -> i32) {
        while !stop.load(Ordering::SeqCst) {
            self.tick(exec);
            let start = Instant::now();
            while !stop.load(Ordering::SeqCst) {
                let el = start.elapsed();
                if el >= tick {
                    break;
                }
                std::thread::sleep((tick - el).min(Duration::from_millis(50)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{dt, task, tmp_dir};
    use std::sync::{Arc, Mutex};

    fn clock() -> (Arc<Mutex<NaiveDateTime>>, Box<dyn Fn() -> NaiveDateTime + Send>) {
        let c = Arc::new(Mutex::new(dt("2026-11-05 09:00")));
        let c2 = c.clone();
        (c, Box::new(move || *c2.lock().unwrap()))
    }
    fn set(c: &Arc<Mutex<NaiveDateTime>>, s: &str) {
        *c.lock().unwrap() = dt(s);
    }
    fn store_with(dir: &std::path::Path, tasks: Vec<ScheduledTask>) -> Store {
        let mut s = Store::new(&dir.join("s.toml"));
        for t in tasks {
            s.add(t).unwrap();
        }
        s
    }
    fn tick(r: &mut Runner) -> Vec<String> {
        r.tick(&mut |_| 0)
    }

    #[test]
    fn interval_fires_and_records() {
        let dir = tmp_dir("r1");
        let (c, ck) = clock();
        let mut r = Runner::with_clock(store_with(&dir, vec![task("iv", "every 5m", "2026-11-05 09:00")]), ck);
        assert!(tick(&mut r).is_empty());
        set(&c, "2026-11-05 09:04:59");
        assert!(tick(&mut r).is_empty());
        set(&c, "2026-11-05 09:05");
        let mut seen = vec![];
        let f = r.tick(&mut |t| {
            seen.push(t.id.clone());
            7
        });
        assert_eq!(f, vec!["iv"]);
        assert_eq!(seen, vec!["iv"]);
        let t = r.store().get("iv").unwrap();
        assert_eq!((t.last_exit, t.run_count, t.last_run), (Some(7), 1, Some(dt("2026-11-05 09:05"))));
        assert!(tick(&mut r).is_empty());
        set(&c, "2026-11-05 09:09");
        assert!(tick(&mut r).is_empty());
        set(&c, "2026-11-05 09:10");
        assert_eq!(tick(&mut r), vec!["iv"]);
        // state persisted
        let s2 = Store::open(&dir.join("s.toml")).unwrap();
        assert_eq!(s2.get("iv").unwrap().run_count, 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missed_slots_skipped_by_default() {
        let dir = tmp_dir("r2");
        let (c, ck) = clock();
        set(&c, "2026-11-07 12:00");
        let mut t = task("d", "daily 03:00", "2026-11-01 00:00");
        t.last_run = Some(dt("2026-11-02 03:00"));
        let mut o = task("o", "at 2026-11-03 10:00", "2026-11-01 00:00");
        o.last_run = None;
        let mut r = Runner::with_clock(store_with(&dir, vec![t, o]), ck);
        assert!(tick(&mut r).is_empty(), "no catch-up storm");
        assert_eq!(r.store().get("d").unwrap().run_count, 0);
        set(&c, "2026-11-08 03:00");
        assert_eq!(tick(&mut r), vec!["d"]);
        assert!(tick(&mut r).is_empty());
        assert_eq!(r.store().get("o").unwrap().run_count, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn catch_up_fires_once() {
        let dir = tmp_dir("r3");
        let (c, ck) = clock();
        set(&c, "2026-11-07 12:00");
        let mut t = task("d", "daily 03:00", "2026-11-01 00:00");
        t.last_run = Some(dt("2026-11-02 03:00"));
        let o = task("o", "at 2026-11-03 10:00", "2026-11-01 00:00");
        let mut r = Runner::with_clock(store_with(&dir, vec![t, o]), ck);
        r.catch_up(true);
        assert_eq!(tick(&mut r), vec!["d", "o"]);
        assert!(tick(&mut r).is_empty());
        assert_eq!(r.store().get("d").unwrap().run_count, 1, "once, not once per missed day");
        set(&c, "2026-11-08 03:00");
        assert_eq!(tick(&mut r), vec!["d"]);
        set(&c, "2026-11-20 00:00");
        assert_eq!(tick(&mut r), vec!["d"]);
        assert_eq!(r.store().get("o").unwrap().run_count, 1, "Once never repeats");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn once_fires_exactly_once() {
        let dir = tmp_dir("r4");
        let (c, ck) = clock();
        let mut r = Runner::with_clock(store_with(&dir, vec![task("o", "at 2026-11-05 09:30", "2026-11-05 09:00")]), ck);
        assert!(tick(&mut r).is_empty());
        set(&c, "2026-11-05 09:30");
        assert_eq!(tick(&mut r), vec!["o"]);
        set(&c, "2026-12-05 09:30");
        assert!(tick(&mut r).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_tasks() {
        let dir = tmp_dir("r5");
        let (_c, ck) = clock();
        let mut off = task("off", "logon", "2026-11-05 09:00");
        off.enabled = false;
        let mut os = task("os", "boot", "2026-11-05 09:00");
        os.os_registered = true;
        let ts = vec![task("lg", "logon", "2026-11-05 09:00"), task("bt", "boot", "2026-11-05 09:00"), off, os];
        let mut r = Runner::with_clock(store_with(&dir, ts.clone()), ck);
        assert!(tick(&mut r).is_empty(), "disabled by default");
        let (_c2, ck2) = clock();
        let mut r = Runner::with_clock(store_with(&dir, ts), ck2);
        r.run_startup_tasks(true);
        assert_eq!(tick(&mut r), vec!["lg", "bt"]);
        assert!(tick(&mut r).is_empty(), "only once");
        assert_eq!(r.store().get("lg").unwrap().run_count, 1);
        assert_eq!(r.store().get("off").unwrap().run_count, 0);
        assert_eq!(r.store().get("os").unwrap().run_count, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn disabled_and_os_registered_skipped_and_reenable() {
        let dir = tmp_dir("r6");
        let (c, ck) = clock();
        let mut off = task("off", "every 1m", "2026-11-05 09:00");
        off.enabled = false;
        let mut os = task("os", "every 1m", "2026-11-05 09:00");
        os.os_registered = true;
        let mut r = Runner::with_clock(store_with(&dir, vec![off, os]), ck);
        set(&c, "2026-11-05 10:00");
        assert!(tick(&mut r).is_empty());
        r.store_mut().set_enabled("off", true).unwrap();
        assert!(tick(&mut r).is_empty(), "armed from now, no backlog");
        set(&c, "2026-11-05 10:01");
        assert_eq!(tick(&mut r), vec!["off"]);
        r.store_mut().set_enabled("off", false).unwrap();
        set(&c, "2026-11-05 10:05");
        assert!(tick(&mut r).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn long_gap_in_loop_fires_once() {
        let dir = tmp_dir("r7");
        let (c, ck) = clock();
        let mut r = Runner::with_clock(store_with(&dir, vec![task("g", "every 1m from 2026-11-05 09:00", "2026-11-05 09:00")]), ck);
        set(&c, "2026-11-05 09:00:30");
        assert!(tick(&mut r).is_empty());
        set(&c, "2026-11-05 09:10:30");
        assert_eq!(tick(&mut r), vec!["g"]);
        set(&c, "2026-11-05 09:10:59");
        assert!(tick(&mut r).is_empty());
        set(&c, "2026-11-05 09:11");
        assert_eq!(tick(&mut r), vec!["g"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn multiple_tasks_and_added_later() {
        let dir = tmp_dir("r8");
        let (c, ck) = clock();
        let mut r = Runner::with_clock(store_with(&dir, vec![task("a", "every 1m", "2026-11-05 09:00")]), ck);
        assert!(tick(&mut r).is_empty());
        r.store_mut().add(task("b", "every 2m", "2026-11-05 09:00")).unwrap();
        set(&c, "2026-11-05 09:01");
        assert!(tick(&mut r).iter().eq(["a"].iter()), "b armed at 09:01 -> 09:03");
        set(&c, "2026-11-05 09:03");
        assert_eq!(tick(&mut r), vec!["a", "b"]);
        r.store_mut().remove("a").unwrap();
        set(&c, "2026-11-05 09:10");
        assert_eq!(tick(&mut r), vec!["b"]);
        let store = r.into_store();
        assert_eq!(store.list().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn run_loop_stops() {
        let dir = tmp_dir("r9");
        let mut r = Runner::new(store_with(&dir, vec![task("a", "logon", "2026-11-05 09:00")]));
        r.run_startup_tasks(true);
        let stop = AtomicBool::new(false);
        let mut n = 0;
        let stop_ref = &stop;
        r.run_loop(stop_ref, Duration::from_millis(10), &mut |_| {
            n += 1;
            stop_ref.store(true, Ordering::SeqCst);
            0
        });
        assert_eq!(n, 1);
        // already-stopped loop does nothing
        let mut r2 = Runner::new(Store::new(&dir.join("x.toml")));
        r2.run_loop(&stop, Duration::from_millis(10), &mut |_| 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
