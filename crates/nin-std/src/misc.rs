//! task, time, math, str, list, map.

use crate::util::*;
use chrono::{Datelike, TimeZone, Timelike};
use indexmap::IndexMap;
use nin_runtime::builtins::{a_fn, a_int, a_list, a_str, call_builtin, LIST_M, MAP_M, STR_M};
use nin_runtime::*;
use std::sync::atomic::Ordering;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

fn dur(ex: &mut Exec, v: &Value) -> R<u64> {
    ex.duration_ms(v)
}

fn task_of(ex: &mut Exec, v: &Value, f: &str) -> R<std::sync::Arc<Task>> {
    match v {
        Value::Task(t) => Ok(t.clone()),
        o if o.is_callable() => match ex.spawn_callable(o.clone(), vec![], "task") {
            Value::Task(t) => Ok(t),
            _ => unreachable!(),
        },
        o => ex.err("TypeError", format!("{}: expected task or function, got {}", f, o.type_name())),
    }
}

pub fn task() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    m.f("spawn", 1, None, |ex, a| {
        let f = a_fn(ex, a, 0, "spawn")?;
        Ok(ex.spawn_callable(f, a[1..].to_vec(), "task"))
    });
    m.f("sleep", 1, Some(1), |ex, a| {
        let ms = dur(ex, &a[0])?;
        ex.sleep_ms(ms)?;
        Ok(Value::Null)
    });
    m.f("all", 1, Some(1), |ex, a| ex.await_value(a[0].clone()));
    m.f("wait", 1, Some(1), |ex, a| {
        let t = task_of(ex, &a[0], "wait")?;
        ex.await_task(&t)
    });
    m.f("cancel", 1, Some(1), |ex, a| {
        let t = task_of(ex, &a[0], "cancel")?;
        t.cancel.store(true, Ordering::SeqCst);
        Ok(Value::Null)
    });
    m.f("race", 1, Some(1), |ex, a| {
        let items = a_list(ex, a, 0, "race")?;
        let mut tasks = vec![];
        for i in &items {
            tasks.push(task_of(ex, i, "race")?);
        }
        if tasks.is_empty() {
            return ex.err("ValueError", "race of empty list");
        }
        loop {
            ex.check_cancel()?;
            if let Some(t) = tasks.iter().find(|t| lk(&t.state).is_some()).cloned() {
                for o in &tasks {
                    o.cancel.store(true, Ordering::SeqCst);
                }
                return ex.await_task(&t);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    m.f("with_timeout", 2, Some(2), |ex, a| {
        let t = task_of(ex, &a[0], "with_timeout")?;
        let ms = dur(ex, &a[1])?;
        let end = Instant::now() + Duration::from_millis(ms);
        loop {
            if lk(&t.state).is_some() {
                return ex.await_task(&t);
            }
            if Instant::now() >= end {
                t.cancel.store(true, Ordering::SeqCst);
                return ex.err("TimeoutError", format!("timed out after {} ms", ms));
            }
            ex.check_cancel()?;
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    m.done()
}

// ───────── time ─────────

fn start() -> Instant {
    static S: OnceLock<Instant> = OnceLock::new();
    *S.get_or_init(Instant::now)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn utc(ms: i64) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::Utc.timestamp_millis_opt(ms).single()
}

pub fn time() -> IndexMap<String, Value> {
    let _ = start();
    let mut m = Mod::new();
    m.f("now", 0, Some(0), |_, _| Ok(Value::str(chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())));
    m.f("now_ms", 0, Some(0), |_, _| Ok(Value::Int(now_ms())));
    m.f("monotonic", 0, Some(0), |_, _| Ok(Value::Int(start().elapsed().as_millis() as i64)));
    m.f("sleep", 1, Some(1), |ex, a| {
        let ms = dur(ex, &a[0])?;
        ex.sleep_ms(ms)?;
        Ok(Value::Null)
    });
    m.f("duration", 1, Some(1), |ex, a| Ok(Value::Int(dur(ex, &a[0])? as i64)));
    m.f("format", 1, Some(3), |ex, a| {
        let ms = a_int(ex, a, 0, "format")?;
        let fmt = if a.len() > 1 { a_str(ex, a, 1, "format")? } else { "%Y-%m-%d %H:%M:%S".to_string() };
        let local = a.len() > 2 && a_str(ex, a, 2, "format")? == "local";
        let dt = match utc(ms) {
            Some(d) => d,
            None => return ex.err("ValueError", "timestamp out of range"),
        };
        let mut out = String::new();
        use std::fmt::Write;
        let r = if local { write!(out, "{}", dt.with_timezone(&chrono::Local).format(&fmt)) } else { write!(out, "{}", dt.format(&fmt)) };
        if r.is_err() {
            return ex.err("ValueError", format!("invalid time format `{}`", fmt));
        }
        Ok(Value::str(out))
    });
    m.f("parse", 1, Some(2), |ex, a| {
        let s = a_str(ex, a, 0, "parse")?;
        if a.len() == 1 {
            return match chrono::DateTime::parse_from_rfc3339(&s) {
                Ok(d) => Ok(Value::Int(d.timestamp_millis())),
                Err(e) => ex.err("ValueError", format!("cannot parse time {}: {}", quote_str(&s), e)),
            };
        }
        let fmt = a_str(ex, a, 1, "parse")?;
        if let Ok(d) = chrono::NaiveDateTime::parse_from_str(&s, &fmt) {
            return Ok(Value::Int(d.and_utc().timestamp_millis()));
        }
        if let Ok(d) = chrono::NaiveDate::parse_from_str(&s, &fmt) {
            return Ok(Value::Int(d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis()));
        }
        ex.err("ValueError", format!("cannot parse time {} with format {}", quote_str(&s), quote_str(&fmt)))
    });
    m.f("add_days", 2, Some(2), |ex, a| Ok(Value::Int(a_int(ex, a, 0, "add_days")? + a_int(ex, a, 1, "add_days")? * 86_400_000)));
    m.f("parts", 1, Some(1), |ex, a| {
        let ms = a_int(ex, a, 0, "parts")?;
        match utc(ms) {
            Some(d) => Ok(Value::map_from([
                ("year", Value::Int(d.year() as i64)),
                ("month", Value::Int(d.month() as i64)),
                ("day", Value::Int(d.day() as i64)),
                ("hour", Value::Int(d.hour() as i64)),
                ("minute", Value::Int(d.minute() as i64)),
                ("second", Value::Int(d.second() as i64)),
                ("weekday", Value::Int(d.weekday().num_days_from_monday() as i64)),
            ])),
            None => ex.err("ValueError", "timestamp out of range"),
        }
    });
    m.done()
}

// ───────── math ─────────

fn f64_arg(ex: &mut Exec, a: &[Value], i: usize, f: &str) -> R<f64> {
    match a.get(i) {
        Some(Value::Int(n)) => Ok(*n as f64),
        Some(Value::Float(x)) => Ok(*x),
        Some(o) => ex.err("TypeError", format!("{}: argument {} must be a number, got {}", f, i + 1, o.type_name())),
        None => ex.err("TypeError", format!("{}: missing argument", f)),
    }
}

fn to_int_checked(ex: &mut Exec, x: f64, f: &str) -> R<Value> {
    if x.is_finite() && x.abs() < 9.2e18 {
        Ok(Value::Int(x as i64))
    } else {
        ex.err("ArithmeticError", format!("{}: result is not a finite integer", f))
    }
}

pub fn math() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    m.c("PI", Value::Float(std::f64::consts::PI));
    m.c("E", Value::Float(std::f64::consts::E));
    macro_rules! fl {
        ($name:literal, $f:expr) => {
            m.f($name, 1, Some(1), |ex, a| {
                let x = f64_arg(ex, a, 0, $name)?;
                let f: fn(f64) -> f64 = $f;
                Ok(Value::Float(f(x)))
            });
        };
    }
    fl!("sqrt", f64::sqrt);
    fl!("sin", f64::sin);
    fl!("cos", f64::cos);
    fl!("tan", f64::tan);
    fl!("atan", f64::atan);
    fl!("asin", f64::asin);
    fl!("acos", f64::acos);
    fl!("exp", f64::exp);
    fl!("log", f64::ln);
    fl!("log10", f64::log10);
    fl!("log2", f64::log2);
    macro_rules! it {
        ($name:literal, $f:expr) => {
            m.f($name, 1, Some(1), |ex, a| {
                if let Value::Int(i) = &a[0] {
                    return Ok(Value::Int(*i));
                }
                let x = f64_arg(ex, a, 0, $name)?;
                let f: fn(f64) -> f64 = $f;
                to_int_checked(ex, f(x), $name)
            });
        };
    }
    it!("floor", f64::floor);
    it!("ceil", f64::ceil);
    it!("round", f64::round);
    it!("trunc", f64::trunc);
    m.f("atan2", 2, Some(2), |ex, a| Ok(Value::Float(f64_arg(ex, a, 0, "atan2")?.atan2(f64_arg(ex, a, 1, "atan2")?))));
    m.f("pow", 2, Some(2), |ex, a| Ok(Value::Float(f64_arg(ex, a, 0, "pow")?.powf(f64_arg(ex, a, 1, "pow")?))));
    m.f("hypot", 2, Some(2), |ex, a| Ok(Value::Float(f64_arg(ex, a, 0, "hypot")?.hypot(f64_arg(ex, a, 1, "hypot")?))));
    m.f("abs", 1, Some(1), |ex, a| match &a[0] {
        Value::Int(i) => Ok(Value::Int(i.wrapping_abs())),
        _ => Ok(Value::Float(f64_arg(ex, a, 0, "abs")?.abs())),
    });
    m.f("sign", 1, Some(1), |ex, a| {
        let x = f64_arg(ex, a, 0, "sign")?;
        Ok(Value::Int(if x > 0.0 { 1 } else if x < 0.0 { -1 } else { 0 }))
    });
    m.f("is_nan", 1, Some(1), |ex, a| Ok(Value::Bool(f64_arg(ex, a, 0, "is_nan")?.is_nan())));
    for name in ["min", "max"] {
        m.f(name, 1, None, move |ex, a| {
            let items = if a.len() == 1 { if let Value::List(l) = &a[0] { lk(l).clone() } else { a.to_vec() } } else { a.to_vec() };
            if items.is_empty() {
                return ex.err("ValueError", format!("{} of empty list", name));
            }
            let mut best = items[0].clone();
            for i in &items[1..] {
                let o = nin_runtime::builtins::cmp_values(ex, i, &best)?;
                if (name == "min" && o == std::cmp::Ordering::Less) || (name == "max" && o == std::cmp::Ordering::Greater) {
                    best = i.clone();
                }
            }
            Ok(best)
        });
    }
    m.f("clamp", 3, Some(3), |ex, a| {
        let (lo, hi) = (&a[1], &a[2]);
        if nin_runtime::builtins::cmp_values(ex, &a[0], lo)? == std::cmp::Ordering::Less {
            return Ok(lo.clone());
        }
        if nin_runtime::builtins::cmp_values(ex, &a[0], hi)? == std::cmp::Ordering::Greater {
            return Ok(hi.clone());
        }
        Ok(a[0].clone())
    });
    m.done()
}

// ───────── str / list / map (functional forms of the builtin methods) ─────────

fn wrap(names: &'static [&'static str]) -> IndexMap<String, Value> {
    let mut m = Mod::new();
    for &n in names {
        m.f(n, 1, None, move |ex, a| call_builtin(ex, &a[0], n, &a[1..]));
    }
    m.done()
}

pub fn str_mod() -> IndexMap<String, Value> {
    wrap(STR_M)
}
pub fn list_mod() -> IndexMap<String, Value> {
    wrap(LIST_M)
}
pub fn map_mod() -> IndexMap<String, Value> {
    wrap(MAP_M)
}
