//! Trigger model, spec-string parser and next-run computation.
//! All times are naive local times; no DST adjustment is ever applied.

use crate::{Result, SchedError};
use chrono::{Datelike, Duration as CDuration, NaiveDate, NaiveDateTime, NaiveTime, Weekday};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// Repeat every `every`. With `start`, slots lie on the grid `start + k*every`;
    /// without, the first run is one interval after creation and later runs one interval after the previous run.
    Interval { every: Duration, start: Option<NaiveDateTime> },
    Once { at: NaiveDateTime },
    Daily { time: NaiveTime },
    Weekly { days: Vec<Weekday>, time: NaiveTime },
    /// Day of month 1..=31; clamped to the last day of shorter months.
    Monthly { day: u32, time: NaiveTime },
    Logon,
    Boot,
}

fn perr<T>(m: impl Into<String>) -> Result<T> {
    Err(SchedError::Parse(m.into()))
}

/// Parse "500ms", "30s", "5m", "2h", "1d", "1w", "1h30m".
pub fn parse_duration(s: &str) -> Result<Duration> {
    let src = s.trim().to_ascii_lowercase();
    if src.is_empty() {
        return perr("empty duration");
    }
    let b = src.as_bytes();
    let mut i = 0;
    let mut total_ms: u64 = 0;
    while i < b.len() {
        let ds = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if ds == i {
            return perr(format!("invalid duration '{s}': expected a number at '{}'", &src[i..]));
        }
        let n: u64 = src[ds..i].parse().map_err(|_| SchedError::Parse(format!("number too large in '{s}'")))?;
        let us = i;
        while i < b.len() && b[i].is_ascii_alphabetic() {
            i += 1;
        }
        let mult: u64 = match &src[us..i] {
            "ms" => 1,
            "s" => 1000,
            "m" => 60_000,
            "h" => 3_600_000,
            "d" => 86_400_000,
            "w" => 604_800_000,
            "" => return perr(format!("duration '{s}' is missing a unit (ms, s, m, h, d, w)")),
            u => return perr(format!("unknown duration unit '{u}' in '{s}'")),
        };
        total_ms = n
            .checked_mul(mult)
            .and_then(|v| total_ms.checked_add(v))
            .ok_or_else(|| SchedError::Parse(format!("duration '{s}' is too large")))?;
    }
    if total_ms == 0 {
        return perr("duration must be greater than zero");
    }
    Ok(Duration::from_millis(total_ms))
}

pub(crate) fn format_duration(d: Duration) -> String {
    let mut ms = d.as_millis() as u64;
    let mut out = String::new();
    for (unit, size) in [("d", 86_400_000u64), ("h", 3_600_000), ("m", 60_000), ("s", 1000), ("ms", 1)] {
        let n = ms / size;
        ms %= size;
        if n > 0 {
            out.push_str(&format!("{n}{unit}"));
        }
    }
    if out.is_empty() {
        out.push_str("0s");
    }
    out
}

pub(crate) fn parse_datetime(s: &str) -> Result<NaiveDateTime> {
    let s = s.trim();
    for f in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"] {
        if let Ok(d) = NaiveDateTime::parse_from_str(s, f) {
            return Ok(d);
        }
    }
    perr(format!("invalid date-time '{s}' (expected YYYY-MM-DD HH:MM[:SS])"))
}

pub(crate) fn format_datetime(d: &NaiveDateTime) -> String {
    use chrono::Timelike;
    if d.second() == 0 && d.nanosecond() == 0 {
        d.format("%Y-%m-%d %H:%M").to_string()
    } else {
        d.format("%Y-%m-%d %H:%M:%S").to_string()
    }
}

pub(crate) fn parse_time(s: &str) -> Result<NaiveTime> {
    let s = s.trim();
    for f in ["%H:%M:%S", "%H:%M"] {
        if let Ok(t) = NaiveTime::parse_from_str(s, f) {
            return Ok(t);
        }
    }
    perr(format!("invalid time '{s}' (expected HH:MM[:SS])"))
}

pub(crate) fn format_time(t: &NaiveTime) -> String {
    use chrono::Timelike;
    if t.second() == 0 {
        t.format("%H:%M").to_string()
    } else {
        t.format("%H:%M:%S").to_string()
    }
}

fn weekday_str(d: Weekday) -> &'static str {
    match d {
        Weekday::Mon => "mon",
        Weekday::Tue => "tue",
        Weekday::Wed => "wed",
        Weekday::Thu => "thu",
        Weekday::Fri => "fri",
        Weekday::Sat => "sat",
        Weekday::Sun => "sun",
    }
}

/// Parse a trigger spec: `every 5m [from 2026-11-05 09:00]`, `at 2026-11-05 09:00`, `daily 03:00`,
/// `weekly mon,fri 09:00`, `monthly 1 02:30`, `logon`/`login`, `boot`/`startup`.
pub fn parse_trigger(s: &str) -> Result<Trigger> {
    let toks: Vec<&str> = s.split_whitespace().collect();
    let Some(head) = toks.first() else { return perr("empty trigger") };
    let rest = &toks[1..];
    match head.to_ascii_lowercase().as_str() {
        "every" => {
            let Some(d) = rest.first() else { return perr("'every' needs a duration, e.g. 'every 5m'") };
            let every = parse_duration(d)?;
            let start = match rest.get(1) {
                None => None,
                Some(k) if k.eq_ignore_ascii_case("from") => {
                    if rest.len() < 3 {
                        return perr("'from' needs a date-time");
                    }
                    Some(parse_datetime(&rest[2..].join(" "))?)
                }
                Some(k) => return perr(format!("unexpected '{k}' after duration")),
            };
            Ok(Trigger::Interval { every, start })
        }
        "at" | "once" => {
            if rest.is_empty() {
                return perr("'at' needs a date-time, e.g. 'at 2026-11-05 09:00'");
            }
            Ok(Trigger::Once { at: parse_datetime(&rest.join(" "))? })
        }
        "daily" => {
            let rest = if rest.first().map_or(false, |t| t.eq_ignore_ascii_case("at")) { &rest[1..] } else { rest };
            if rest.len() != 1 {
                return perr("usage: daily HH:MM");
            }
            Ok(Trigger::Daily { time: parse_time(rest[0])? })
        }
        "weekly" => {
            if rest.len() != 2 {
                return perr("usage: weekly mon,fri HH:MM");
            }
            let mut days: Vec<Weekday> = Vec::new();
            for p in rest[0].split(',') {
                let w: Weekday = p.trim().parse().map_err(|_| SchedError::Parse(format!("invalid weekday '{p}'")))?;
                if !days.contains(&w) {
                    days.push(w);
                }
            }
            days.sort_by_key(|d| d.number_from_monday());
            Ok(Trigger::Weekly { days, time: parse_time(rest[1])? })
        }
        "monthly" => {
            if rest.len() != 2 {
                return perr("usage: monthly DAY HH:MM");
            }
            let day: u32 = rest[0].parse().map_err(|_| SchedError::Parse(format!("invalid day of month '{}'", rest[0])))?;
            if !(1..=31).contains(&day) {
                return perr(format!("day of month must be 1..=31, got {day}"));
            }
            Ok(Trigger::Monthly { day, time: parse_time(rest[1])? })
        }
        "logon" | "login" | "boot" | "startup" => {
            if !rest.is_empty() {
                return perr(format!("'{head}' takes no arguments"));
            }
            Ok(if matches!(head.to_ascii_lowercase().as_str(), "logon" | "login") { Trigger::Logon } else { Trigger::Boot })
        }
        other => perr(format!("unknown trigger '{other}' (every, at, daily, weekly, monthly, logon, boot)")),
    }
}

impl Trigger {
    /// Canonical spec string; `parse_trigger(t.to_spec_string())` yields an equal trigger
    /// (for weekly triggers whose days are sorted and unique, as the parser produces).
    pub fn to_spec_string(&self) -> String {
        match self {
            Trigger::Interval { every, start: None } => format!("every {}", format_duration(*every)),
            Trigger::Interval { every, start: Some(s) } => {
                format!("every {} from {}", format_duration(*every), format_datetime(s))
            }
            Trigger::Once { at } => format!("at {}", format_datetime(at)),
            Trigger::Daily { time } => format!("daily {}", format_time(time)),
            Trigger::Weekly { days, time } => {
                let d: Vec<&str> = days.iter().map(|d| weekday_str(*d)).collect();
                format!("weekly {} {}", d.join(","), format_time(time))
            }
            Trigger::Monthly { day, time } => format!("monthly {} {}", day, format_time(time)),
            Trigger::Logon => "logon".into(),
            Trigger::Boot => "boot".into(),
        }
    }

    /// True for triggers that fire from OS events rather than the clock.
    pub fn is_event(&self) -> bool {
        matches!(self, Trigger::Logon | Trigger::Boot)
    }
}

pub(crate) fn days_in_month(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    NaiveDate::from_ymd_opt(ny, nm, 1)
        .and_then(|d| d.pred_opt())
        .map(|d| d.day())
        .unwrap_or(31)
}

/// Next run time of `trigger` strictly after `after`.
/// * `Interval` without `start` anchors on `last_run` if given (`last_run + every`), else `after + every`.
/// * `Once` yields its time if `last_run` is `None` and it lies after `after`; otherwise `None`.
/// * `Logon`/`Boot` are event driven: always `None`.
pub fn next_run(trigger: &Trigger, after: NaiveDateTime, last_run: Option<NaiveDateTime>) -> Option<NaiveDateTime> {
    match trigger {
        Trigger::Interval { every, start } => {
            let ev = CDuration::from_std(*every).ok()?;
            let evms = ev.num_milliseconds();
            if evms <= 0 {
                return None;
            }
            match start {
                Some(s) if after < *s => Some(*s),
                Some(s) => {
                    let k = (after - *s).num_milliseconds() / evms + 1;
                    s.checked_add_signed(CDuration::milliseconds(k.checked_mul(evms)?))
                }
                None => last_run.unwrap_or(after).checked_add_signed(ev),
            }
        }
        Trigger::Once { at } => (last_run.is_none() && *at > after).then_some(*at),
        Trigger::Daily { time } => {
            let c = after.date().and_time(*time);
            if c > after { Some(c) } else { Some(after.date().succ_opt()?.and_time(*time)) }
        }
        Trigger::Weekly { days, time } => {
            if days.is_empty() {
                return None;
            }
            let mut date = after.date();
            for _ in 0..8 {
                if days.contains(&date.weekday()) {
                    let c = date.and_time(*time);
                    if c > after {
                        return Some(c);
                    }
                }
                date = date.succ_opt()?;
            }
            None
        }
        Trigger::Monthly { day, time } => {
            if !(1..=31).contains(day) {
                return None;
            }
            let (mut y, mut m) = (after.year(), after.month());
            for _ in 0..3 {
                let d = (*day).min(days_in_month(y, m));
                let c = NaiveDate::from_ymd_opt(y, m, d)?.and_time(*time);
                if c > after {
                    return Some(c);
                }
                if m == 12 {
                    y += 1;
                    m = 1;
                } else {
                    m += 1;
                }
            }
            None
        }
        Trigger::Logon | Trigger::Boot => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> NaiveDateTime {
        parse_datetime(s).unwrap()
    }
    fn tm(s: &str) -> NaiveTime {
        parse_time(s).unwrap()
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86400));
        assert_eq!(parse_duration("1w").unwrap(), Duration::from_secs(604800));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration(" 90S ").unwrap(), Duration::from_secs(90));
        for bad in ["", "5", "m", "0s", "5x", "1h 30m", "-5m", "1.5h", "99999999999999999999s", "18446744073709551615d"] {
            assert!(matches!(parse_duration(bad), Err(SchedError::Parse(_))), "{bad}");
        }
    }

    #[test]
    fn duration_format() {
        assert_eq!(format_duration(Duration::from_secs(90)), "1m30s");
        assert_eq!(format_duration(Duration::from_millis(500)), "500ms");
        assert_eq!(format_duration(Duration::from_secs(90000)), "1d1h");
    }

    #[test]
    fn parse_all_forms() {
        assert_eq!(parse_trigger("every 5m").unwrap(), Trigger::Interval { every: Duration::from_secs(300), start: None });
        assert_eq!(
            parse_trigger("every 2h from 2026-11-05T09:00").unwrap(),
            Trigger::Interval { every: Duration::from_secs(7200), start: Some(dt("2026-11-05 09:00")) }
        );
        assert_eq!(parse_trigger("at 2026-11-05 09:00").unwrap(), Trigger::Once { at: dt("2026-11-05 09:00:00") });
        assert_eq!(parse_trigger("at 2026-11-05T09:00:30").unwrap(), Trigger::Once { at: dt("2026-11-05 09:00:30") });
        assert_eq!(parse_trigger("daily 03:00").unwrap(), Trigger::Daily { time: tm("03:00") });
        assert_eq!(parse_trigger("DAILY at 03:00:15").unwrap(), Trigger::Daily { time: tm("03:00:15") });
        assert_eq!(
            parse_trigger("weekly fri,Monday,mon 09:00").unwrap(),
            Trigger::Weekly { days: vec![Weekday::Mon, Weekday::Fri], time: tm("09:00") }
        );
        assert_eq!(parse_trigger("monthly 1 02:30").unwrap(), Trigger::Monthly { day: 1, time: tm("02:30") });
        assert_eq!(parse_trigger("logon").unwrap(), Trigger::Logon);
        assert_eq!(parse_trigger("login").unwrap(), Trigger::Logon);
        assert_eq!(parse_trigger("boot").unwrap(), Trigger::Boot);
        assert_eq!(parse_trigger("startup").unwrap(), Trigger::Boot);
    }

    #[test]
    fn round_trip() {
        for s in [
            "every 5m",
            "every 90s",
            "every 1d",
            "every 1h30m from 2026-11-05 09:00",
            "every 500ms",
            "at 2026-11-05 09:00",
            "at 2026-11-05T09:00:30",
            "daily 03:00",
            "daily 03:00:07",
            "weekly mon,fri 09:00",
            "weekly sun,wed,sat 23:59",
            "monthly 1 02:30",
            "monthly 31 00:00",
            "logon",
            "boot",
        ] {
            let t = parse_trigger(s).unwrap();
            let s2 = t.to_spec_string();
            assert_eq!(parse_trigger(&s2).unwrap(), t, "{s} -> {s2}");
            assert_eq!(parse_trigger(&s2).unwrap().to_spec_string(), s2);
        }
        assert_eq!(parse_trigger("every 90s").unwrap().to_spec_string(), "every 1m30s");
        assert_eq!(parse_trigger("startup").unwrap().to_spec_string(), "boot");
    }

    #[test]
    fn parse_errors() {
        for bad in [
            "", "   ", "every", "every 5", "every 5m from", "every 5m foo", "every 5m from nope", "at", "at tomorrow",
            "at 2026-13-01 00:00", "daily", "daily 25:00", "daily 3", "daily 03:00 extra", "weekly", "weekly mon",
            "weekly xyz 09:00", "weekly mon, 09:00", "monthly 0 01:00", "monthly 32 01:00", "monthly x 01:00",
            "monthly 1", "logon now", "boot 1", "yearly 1", "every 0s",
        ] {
            assert!(matches!(parse_trigger(bad), Err(SchedError::Parse(_))), "should fail: {bad:?}");
        }
    }

    #[test]
    fn interval_next() {
        let t = parse_trigger("every 5m").unwrap();
        assert_eq!(next_run(&t, dt("2026-11-05 10:00"), None), Some(dt("2026-11-05 10:05")));
        assert_eq!(next_run(&t, dt("2026-11-05 10:30"), Some(dt("2026-11-05 10:00"))), Some(dt("2026-11-05 10:05")));
        let t = parse_trigger("every 1d").unwrap();
        assert_eq!(next_run(&t, dt("2026-12-31 12:00"), None), Some(dt("2027-01-01 12:00")));
        // grid with start
        let t = parse_trigger("every 2h from 2026-11-05 09:00").unwrap();
        assert_eq!(next_run(&t, dt("2026-11-05 08:00"), None), Some(dt("2026-11-05 09:00")));
        assert_eq!(next_run(&t, dt("2026-11-05 09:00"), None), Some(dt("2026-11-05 11:00")));
        assert_eq!(next_run(&t, dt("2026-11-05 10:59:59"), None), Some(dt("2026-11-05 11:00")));
        assert_eq!(next_run(&t, dt("2026-11-06 00:30"), None), Some(dt("2026-11-06 01:00")));
    }

    #[test]
    fn once_next() {
        let t = parse_trigger("at 2026-11-05 09:00").unwrap();
        assert_eq!(next_run(&t, dt("2026-11-05 08:59:59"), None), Some(dt("2026-11-05 09:00")));
        assert_eq!(next_run(&t, dt("2026-11-05 09:00"), None), None);
        assert_eq!(next_run(&t, dt("2026-11-01 00:00"), Some(dt("2026-11-05 09:00:01"))), None);
    }

    #[test]
    fn daily_next_boundaries() {
        let t = parse_trigger("daily 03:00").unwrap();
        assert_eq!(next_run(&t, dt("2026-11-05 02:59:59"), None), Some(dt("2026-11-05 03:00")));
        assert_eq!(next_run(&t, dt("2026-11-05 03:00"), None), Some(dt("2026-11-06 03:00")));
        assert_eq!(next_run(&t, dt("2026-12-31 23:00"), None), Some(dt("2027-01-01 03:00")));
        assert_eq!(next_run(&t, dt("2028-02-28 04:00"), None), Some(dt("2028-02-29 03:00")));
        assert_eq!(next_run(&t, dt("2027-02-28 04:00"), None), Some(dt("2027-03-01 03:00")));
        // DST-insensitive: naive wall clock, e.g. 02:30 on a US spring-forward day still exists
        let t = parse_trigger("daily 02:30").unwrap();
        assert_eq!(next_run(&t, dt("2026-03-08 00:00"), None), Some(dt("2026-03-08 02:30")));
    }

    #[test]
    fn weekly_next() {
        let t = parse_trigger("weekly mon,fri 09:00").unwrap();
        // 2026-11-05 is a Thursday
        assert_eq!(dt("2026-11-05 00:00").date().weekday(), Weekday::Thu);
        assert_eq!(next_run(&t, dt("2026-11-05 10:00"), None), Some(dt("2026-11-06 09:00")));
        assert_eq!(next_run(&t, dt("2026-11-06 09:00"), None), Some(dt("2026-11-09 09:00")));
        assert_eq!(next_run(&t, dt("2026-11-06 08:00"), None), Some(dt("2026-11-06 09:00")));
        assert_eq!(next_run(&t, dt("2026-11-09 09:00:01"), None), Some(dt("2026-11-13 09:00")));
        // year boundary: 2026-12-31 is Thursday
        assert_eq!(next_run(&t, dt("2026-12-31 12:00"), None), Some(dt("2027-01-01 09:00")));
        let one = parse_trigger("weekly wed 09:00").unwrap();
        assert_eq!(next_run(&one, dt("2026-11-04 09:00"), None), Some(dt("2026-11-11 09:00")));
        assert_eq!(next_run(&Trigger::Weekly { days: vec![], time: tm("09:00") }, dt("2026-11-04 09:00"), None), None);
    }

    #[test]
    fn monthly_next_and_clamp() {
        let t = parse_trigger("monthly 31 02:30").unwrap();
        assert_eq!(next_run(&t, dt("2026-01-01 00:00"), None), Some(dt("2026-01-31 02:30")));
        assert_eq!(next_run(&t, dt("2026-01-31 02:30"), None), Some(dt("2026-02-28 02:30")));
        assert_eq!(next_run(&t, dt("2028-01-31 02:30"), None), Some(dt("2028-02-29 02:30")));
        assert_eq!(next_run(&t, dt("2026-04-01 00:00"), None), Some(dt("2026-04-30 02:30")));
        assert_eq!(next_run(&t, dt("2026-04-30 02:30"), None), Some(dt("2026-05-31 02:30")));
        assert_eq!(next_run(&t, dt("2026-12-31 02:30"), None), Some(dt("2027-01-31 02:30")));
        let t = parse_trigger("monthly 1 02:30").unwrap();
        assert_eq!(next_run(&t, dt("2026-12-15 00:00"), None), Some(dt("2027-01-01 02:30")));
        assert_eq!(next_run(&t, dt("2026-11-01 02:29"), None), Some(dt("2026-11-01 02:30")));
        let t = parse_trigger("monthly 30 00:00").unwrap();
        assert_eq!(next_run(&t, dt("2026-01-30 00:00"), None), Some(dt("2026-02-28 00:00")));
    }

    #[test]
    fn event_triggers_never_scheduled() {
        assert_eq!(next_run(&Trigger::Logon, dt("2026-01-01 00:00"), None), None);
        assert_eq!(next_run(&Trigger::Boot, dt("2026-01-01 00:00"), None), None);
        assert!(Trigger::Logon.is_event() && !Trigger::Daily { time: tm("01:00") }.is_event());
    }
}
