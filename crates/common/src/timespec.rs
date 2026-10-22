//! When a timed block acts (architecture 4.4, 1.6): a time relative to the program's T0, a local time
//! of day with a `day_offset` from T0's date, a recurrence (cron-like, local time), or a date and time
//! (a one-off), with skip dates. Everything is evaluated in the station's time zone with `jiff`, so the
//! DST changes fall out of the zone rules: at the fall-back hour a recurrence that runs every hour fires
//! once per real hour (both 01:00 hours), a recurrence that names an hour fires once (the first), and a
//! nonexistent local time moves to the first valid instant (an hourly recurrence just has no event in the
//! hour that does not exist).
//!
//! Instants are unix milliseconds throughout, the unit of the scheduler.

pub use jiff::civil::DateTime as CivilDateTime;
use jiff::civil::{Date, DateTime, Time};
use jiff::tz::{AmbiguousOffset, TimeZone};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// A time zone, by name.
#[derive(Debug, Clone)]
pub struct Zone {
    tz: TimeZone,
    name: String,
}

impl Zone {
    pub fn named(name: &str) -> Result<Zone, String> {
        let tz = TimeZone::get(name).map_err(|e| format!("time zone `{name}`: {e}"))?;
        Ok(Zone {
            tz,
            name: name.to_string(),
        })
    }

    pub fn utc() -> Zone {
        Zone {
            tz: TimeZone::UTC,
            name: "UTC".into(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn ts(ms: i64) -> Timestamp {
        Timestamp::from_millisecond(ms).unwrap_or(Timestamp::UNIX_EPOCH)
    }

    /// The local civil date and time at `ms`.
    pub fn civil(&self, ms: i64) -> DateTime {
        self.tz.to_datetime(Self::ts(ms))
    }

    /// The local date at `ms`, `YYYY-MM-DD`.
    pub fn date_string(&self, ms: i64) -> String {
        self.civil(ms).date().to_string()
    }

    /// `YYYY-MM-DDTHH:MM:SS.mmm` local time at `ms`, with the UTC offset (`-04:00`).
    pub fn local_string(&self, ms: i64) -> String {
        let dt = self.civil(ms);
        let off = self.tz.to_offset(Self::ts(ms)).seconds();
        let (sign, a) = if off < 0 { ('-', -off) } else { ('+', off) };
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}{}{:02}:{:02}",
            dt.year(),
            dt.month(),
            dt.day(),
            dt.hour(),
            dt.minute(),
            dt.second(),
            dt.subsec_nanosecond() / 1_000_000,
            sign,
            a / 3600,
            (a % 3600) / 60
        )
    }

    /// Weekday of the local date at `ms`, Sunday 0 .. Saturday 6.
    pub fn weekday(&self, ms: i64) -> u8 {
        self.civil(ms).date().weekday().to_sunday_zero_offset() as u8
    }

    /// The instants a civil time can mean: none (a gap), one, or two (a fold, earlier first).
    pub fn instants(&self, dt: DateTime) -> Vec<i64> {
        let amb = self.tz.to_ambiguous_timestamp(dt);
        match amb.offset() {
            AmbiguousOffset::Unambiguous { .. } => amb
                .unambiguous()
                .map(|t| vec![t.as_millisecond()])
                .unwrap_or_default(),
            AmbiguousOffset::Gap { .. } => vec![],
            AmbiguousOffset::Fold { .. } => {
                let a = amb.earlier().map(|t| t.as_millisecond());
                let b = amb.later().map(|t| t.as_millisecond());
                a.into_iter().chain(b).collect()
            }
        }
    }

    /// The first valid instant at or after the civil time (a gap moves forward), the earlier instant of
    /// a fold.
    pub fn compatible(&self, dt: DateTime) -> Option<i64> {
        self.tz
            .to_ambiguous_timestamp(dt)
            .compatible()
            .ok()
            .map(|t| t.as_millisecond())
    }

    /// True when the civil time does not exist (inside a spring-forward gap).
    pub fn in_gap(&self, dt: DateTime) -> bool {
        matches!(
            self.tz.to_ambiguous_timestamp(dt).offset(),
            AmbiguousOffset::Gap { .. }
        )
    }
}

fn bit(v: u32) -> u64 {
    1u64 << v
}

/// A five-field cron expression: `minute hour day-of-month month day-of-week`, each `*`, a number, a
/// range `a-b`, a list, or a step (`*/15`, `a-b/n`). Day of week is 0 to 7 (0 and 7 are Sunday). As in
/// cron, a restricted day of month and a restricted day of week both match (either one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Cron {
    pub src: String,
    minutes: u64,
    hours: u64,
    dom: u64,
    months: u64,
    dow: u64,
    /// The hour field is `*` or a step over the whole day: "every real hour".
    hour_any: bool,
    dom_any: bool,
    dow_any: bool,
}

fn field(s: &str, lo: u32, hi: u32, name: &str) -> Result<(u64, bool), String> {
    let mut set = 0u64;
    let mut any = false;
    for part in s.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, st)) => (
                r,
                st.parse::<u32>()
                    .map_err(|_| format!("{name}: `{st}` is not a step"))?,
            ),
            None => (part, 1),
        };
        if step == 0 {
            return Err(format!("{name}: a step of 0"));
        }
        let (a, b) = if range == "*" {
            if step == 1 {
                any = true;
            }
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (
                a.parse::<u32>()
                    .map_err(|_| format!("{name}: `{a}` is not a number"))?,
                b.parse::<u32>()
                    .map_err(|_| format!("{name}: `{b}` is not a number"))?,
            )
        } else {
            let v = range
                .parse::<u32>()
                .map_err(|_| format!("{name}: `{range}` is not a number"))?;
            // `5/10` means from 5 to the end in steps of 10.
            (v, if part.contains('/') { hi } else { v })
        };
        if a < lo || b > hi || a > b {
            return Err(format!("{name}: {a}-{b} is outside {lo}-{hi}"));
        }
        let mut v = a;
        while v <= b {
            set |= bit(v);
            v += step;
        }
    }
    if set == 0 {
        return Err(format!("{name}: no value"));
    }
    Ok((set, any))
}

impl TryFrom<String> for Cron {
    type Error = String;
    fn try_from(s: String) -> Result<Cron, String> {
        Cron::parse(&s)
    }
}

impl From<Cron> for String {
    fn from(c: Cron) -> String {
        c.src
    }
}

impl Cron {
    pub fn parse(src: &str) -> Result<Cron, String> {
        let f: Vec<&str> = src.split_whitespace().collect();
        if f.len() != 5 {
            return Err(format!(
                "a recurrence has five fields (minute hour day month weekday), got {}",
                f.len()
            ));
        }
        let (minutes, _) = field(f[0], 0, 59, "minute")?;
        let (hours, hour_star) = field(f[1], 0, 23, "hour")?;
        // "Every real hour": `*`, or a step that visits every hour of the day (`*/1`).
        let hour_any = hour_star || hours.count_ones() == 24;
        let (dom, dom_any) = field(f[2], 1, 31, "day of month")?;
        let (months, _) = field(f[3], 1, 12, "month")?;
        let (mut dow, dow_any) = field(f[4], 0, 7, "weekday")?;
        if dow & bit(7) != 0 {
            dow |= bit(0);
            dow &= !bit(7);
        }
        Ok(Cron {
            src: f.join(" "),
            minutes,
            hours,
            dom,
            months,
            dow,
            hour_any,
            dom_any,
            dow_any,
        })
    }

    fn day_matches(&self, d: Date) -> bool {
        if self.months & bit(d.month() as u32) == 0 {
            return false;
        }
        let dom = self.dom & bit(d.day() as u32) != 0;
        let dow = self.dow & bit(d.weekday().to_sunday_zero_offset() as u32) != 0;
        match (self.dom_any, self.dow_any) {
            (true, true) => true,
            (true, false) => dow,
            (false, true) => dom,
            (false, false) => dom || dow,
        }
    }

    /// The weekdays (Sunday 0) the recurrence names, when it is restricted to weekdays only.
    pub fn weekdays(&self) -> Option<Vec<u8>> {
        if self.dow_any || !self.dom_any {
            return None;
        }
        Some(
            (0..7u8)
                .filter(|d| self.dow & bit(u32::from(*d)) != 0)
                .collect(),
        )
    }

    /// The times of day the recurrence names, `(hour, minute)`.
    pub fn times_of_day(&self) -> Vec<(u8, u8)> {
        let mut out = Vec::new();
        for h in 0..24u8 {
            if self.hours & bit(u32::from(h)) == 0 {
                continue;
            }
            for m in 0..60u8 {
                if self.minutes & bit(u32::from(m)) != 0 {
                    out.push((h, m));
                }
            }
        }
        out
    }

    pub fn hour_any(&self) -> bool {
        self.hour_any
    }
}

/// How a block's time is given (4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum When {
    /// T0 plus an offset (never negative).
    Relative { offset_ms: i64 },
    /// A local time of day on T0's date plus `day_offset` days (`HH:MM[:SS[.mmm]]`).
    Local { at: String, day_offset: i64 },
    /// Cron-like, in local time.
    Recurrence { cron: Cron },
    /// A local date and time, once (`2026-10-31T18:00:00`).
    Date { local: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSpec {
    #[serde(flatten)]
    pub when: When,
    /// Local dates (`YYYY-MM-DD`) on which a recurrence does not fire.
    #[serde(default)]
    pub skip_dates: Vec<String>,
}

/// An RFC 3339 time (`2027-01-18T18:45:00-05:00`) as unix milliseconds.
pub fn parse_rfc3339_ms(text: &str) -> Option<i64> {
    text.parse::<jiff::Timestamp>()
        .ok()
        .map(|t| t.as_millisecond())
}

/// `HH:MM`, `HH:MM:SS` or `HH:MM:SS.mmm` to milliseconds of the day.
pub fn parse_time_of_day(s: &str) -> Result<i64, String> {
    let bad = || format!("`{s}` is not a time of day (HH:MM:SS.mmm)");
    let (hms, frac) = match s.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    let parts: Vec<&str> = hms.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return Err(bad());
    }
    let h: i64 = parts[0].parse().map_err(|_| bad())?;
    let m: i64 = parts[1].parse().map_err(|_| bad())?;
    let sec: i64 = parts
        .get(2)
        .map_or(Ok(0), |p| p.parse())
        .map_err(|_| bad())?;
    let ms: i64 = match frac {
        None => 0,
        Some(f) if !f.is_empty() && f.len() <= 3 && f.chars().all(|c| c.is_ascii_digit()) => {
            format!("{f:0<3}").parse().map_err(|_| bad())?
        }
        Some(_) => return Err(bad()),
    };
    if !(0..24).contains(&h) || !(0..60).contains(&m) || !(0..60).contains(&sec) {
        return Err(bad());
    }
    Ok(((h * 60 + m) * 60 + sec) * 1000 + ms)
}

fn civil_of(date: Date, ms_of_day: i64) -> Option<DateTime> {
    let h = (ms_of_day / 3_600_000) as i8;
    let m = ((ms_of_day / 60_000) % 60) as i8;
    let s = ((ms_of_day / 1000) % 60) as i8;
    let ns = ((ms_of_day % 1000) * 1_000_000) as i32;
    Time::new(h, m, s, ns)
        .ok()
        .map(|t| DateTime::from_parts(date, t))
}

impl TimeSpec {
    pub fn relative(offset_ms: i64) -> TimeSpec {
        TimeSpec {
            when: When::Relative { offset_ms },
            skip_dates: vec![],
        }
    }

    pub fn recurrence(cron: &str) -> Result<TimeSpec, String> {
        Ok(TimeSpec {
            when: When::Recurrence {
                cron: Cron::parse(cron)?,
            },
            skip_dates: vec![],
        })
    }

    pub fn local(at: &str, day_offset: i64) -> Result<TimeSpec, String> {
        parse_time_of_day(at)?;
        Ok(TimeSpec {
            when: When::Local {
                at: at.into(),
                day_offset,
            },
            skip_dates: vec![],
        })
    }

    pub fn date(local: &str) -> Result<TimeSpec, String> {
        local
            .parse::<DateTime>()
            .map_err(|e| format!("`{local}` is not a local date and time: {e}"))?;
        Ok(TimeSpec {
            when: When::Date {
                local: local.into(),
            },
            skip_dates: vec![],
        })
    }

    pub fn with_skip(mut self, dates: &[&str]) -> TimeSpec {
        self.skip_dates = dates.iter().map(|d| d.to_string()).collect();
        self
    }

    /// Relative and `Local` times need the program's T0.
    pub fn needs_t0(&self) -> bool {
        matches!(self.when, When::Relative { .. } | When::Local { .. })
    }

    pub fn is_recurring(&self) -> bool {
        matches!(self.when, When::Recurrence { .. })
    }

    /// Every instant of the spec in `(after_ms, upto_ms]`, ascending. `t0_ms` is the program's T0 (needed
    /// for `Relative` and `Local`; without it those give nothing).
    pub fn occurrences(
        &self,
        zone: &Zone,
        t0_ms: Option<i64>,
        after_ms: i64,
        upto_ms: i64,
    ) -> Vec<i64> {
        let mut out: Vec<i64> = match &self.when {
            When::Relative { offset_ms } => t0_ms.map(|t| t + offset_ms).into_iter().collect(),
            When::Local { at, day_offset } => {
                let (Some(t0), Ok(tod)) = (t0_ms, parse_time_of_day(at)) else {
                    return vec![];
                };
                let d0 = zone.civil(t0).date();
                let d = d0
                    .checked_add(jiff::Span::new().days(*day_offset))
                    .unwrap_or(d0);
                civil_of(d, tod)
                    .and_then(|dt| zone.compatible(dt))
                    .into_iter()
                    .collect()
            }
            When::Date { local } => local
                .parse::<DateTime>()
                .ok()
                .and_then(|dt| zone.compatible(dt))
                .into_iter()
                .collect(),
            When::Recurrence { cron } => self.cron_occurrences(cron, zone, after_ms, upto_ms),
        };
        out.retain(|t| *t > after_ms && *t <= upto_ms);
        out.retain(|t| !self.skip_dates.contains(&zone.date_string(*t)));
        out.sort_unstable();
        out
    }

    fn cron_occurrences(&self, cron: &Cron, zone: &Zone, after_ms: i64, upto_ms: i64) -> Vec<i64> {
        let mut out = Vec::new();
        if upto_ms <= after_ms {
            return out;
        }
        // One day either side covers every offset, so no local date that can hold an instant of the
        // range is missed.
        let mut day = zone.civil(after_ms - 86_400_000).date();
        let last = zone.civil(upto_ms + 86_400_000).date();
        let times = cron.times_of_day();
        let mut guard = 0;
        while day <= last && guard < 4000 {
            guard += 1;
            if cron.day_matches(day) {
                for (h, m) in &times {
                    let Some(dt) = civil_of(day, (i64::from(*h) * 60 + i64::from(*m)) * 60_000)
                    else {
                        continue;
                    };
                    if cron.hour_any {
                        // Every real hour: both instants of a fold, none for a gap.
                        out.extend(zone.instants(dt));
                    } else if let Some(t) = zone.compatible(dt) {
                        out.push(t);
                    }
                }
            }
            match day.tomorrow() {
                Ok(d) => day = d,
                Err(_) => break,
            }
        }
        out
    }

    /// The first instant after `after_ms` within `limit_ms` of it, if any.
    pub fn first_after(
        &self,
        zone: &Zone,
        t0_ms: Option<i64>,
        after_ms: i64,
        limit_ms: i64,
    ) -> Option<i64> {
        // Recurrences are searched a window at a time so a daily one does not enumerate a year.
        let mut from = after_ms;
        let end = after_ms.saturating_add(limit_ms);
        while from < end {
            let to = (from + 8 * 86_400_000).min(end);
            if let Some(t) = self.occurrences(zone, t0_ms, from, to).first() {
                return Some(*t);
            }
            from = to;
        }
        None
    }

    pub fn check(&self) -> Result<(), String> {
        match &self.when {
            When::Relative { offset_ms } if *offset_ms < 0 => {
                Err("a negative relative offset".into())
            }
            When::Local { at, .. } => parse_time_of_day(at).map(|_| ()),
            When::Date { local } => local
                .parse::<DateTime>()
                .map(|_| ())
                .map_err(|e| format!("`{local}` is not a local date and time: {e}")),
            When::Recurrence { cron } => Cron::parse(&cron.src).map(|_| ()),
            _ => Ok(()),
        }?;
        for d in &self.skip_dates {
            d.parse::<Date>()
                .map_err(|_| format!("skip date `{d}` is not YYYY-MM-DD"))?;
        }
        Ok(())
    }

    /// A short sentence for the editor and the preview.
    pub fn describe(&self) -> String {
        match &self.when {
            When::Relative { offset_ms } => {
                let s = offset_ms / 1000;
                format!("T0 + {:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
            }
            When::Local { at, day_offset } => match day_offset {
                0 => at.clone(),
                1 => format!("{at} next day"),
                n => format!("{at} {n} days on"),
            },
            When::Recurrence { cron } => cron.src.clone(),
            When::Date { local } => local.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ny() -> Zone {
        Zone::named("America/New_York").expect("tz database")
    }

    fn at(z: &Zone, s: &str) -> i64 {
        let dt: DateTime = s.parse().unwrap();
        z.compatible(dt).unwrap()
    }

    #[test]
    fn an_hourly_recurrence_fires_at_both_one_oclock_of_the_fall_back_night() {
        let z = ny();
        let spec = TimeSpec::recurrence("0 * * * *").unwrap();
        // 2026-11-01: clocks go back at 02:00 EDT to 01:00 EST. The day has 25 real hours.
        let from = at(&z, "2026-11-01T00:00:00") - 1;
        let to = at(&z, "2026-11-02T00:00:00") - 1;
        let occ = spec.occurrences(&z, None, from, to);
        assert_eq!(occ.len(), 25);
        let one_am: Vec<i64> = occ
            .iter()
            .copied()
            .filter(|t| z.civil(*t).hour() == 1)
            .collect();
        assert_eq!(one_am.len(), 2);
        assert_eq!(one_am[1] - one_am[0], 3_600_000);
        // Strictly one per real hour.
        assert!(occ.windows(2).all(|w| w[1] - w[0] == 3_600_000));
    }

    #[test]
    fn an_hourly_recurrence_skips_only_the_missing_hour_on_the_spring_forward_night() {
        let z = ny();
        let spec = TimeSpec::recurrence("0 * * * *").unwrap();
        // 2026-03-08: 02:00 EST jumps to 03:00 EDT. The day has 23 real hours; 02:00 never happens.
        let from = at(&z, "2026-03-08T00:00:00") - 1;
        let to = at(&z, "2026-03-09T00:00:00") - 1;
        let occ = spec.occurrences(&z, None, from, to);
        assert_eq!(occ.len(), 23);
        assert!(occ.windows(2).all(|w| w[1] - w[0] == 3_600_000));
        let hours: Vec<i8> = occ.iter().map(|t| z.civil(*t).hour()).collect();
        assert!(!hours.contains(&2));
        assert!(hours.contains(&3) && hours.contains(&1));
    }

    #[test]
    fn a_named_hour_fires_once_on_the_fold_and_at_the_first_valid_instant_in_the_gap() {
        let z = ny();
        let daily_130 = TimeSpec::recurrence("30 1 * * *").unwrap();
        let from = at(&z, "2026-11-01T00:00:00") - 1;
        let to = at(&z, "2026-11-02T00:00:00") - 1;
        assert_eq!(daily_130.occurrences(&z, None, from, to).len(), 1);
        let daily_230 = TimeSpec::recurrence("30 2 * * *").unwrap();
        let from = at(&z, "2026-03-08T00:00:00") - 1;
        let to = at(&z, "2026-03-09T00:00:00") - 1;
        let occ = daily_230.occurrences(&z, None, from, to);
        assert_eq!(occ.len(), 1);
        let dt = z.civil(occ[0]);
        assert_eq!((dt.hour(), dt.minute()), (3, 30));
    }

    #[test]
    fn weekday_recurrence_and_skip_dates() {
        let z = ny();
        // 18:00 Monday to Friday; Wednesday 2026-10-07 skipped.
        let spec = TimeSpec::recurrence("0 18 * * 1-5")
            .unwrap()
            .with_skip(&["2026-10-07"]);
        let from = at(&z, "2026-10-05T00:00:00") - 1;
        let to = at(&z, "2026-10-12T00:00:00") - 1;
        let occ = spec.occurrences(&z, None, from, to);
        let days: Vec<String> = occ.iter().map(|t| z.date_string(*t)).collect();
        assert_eq!(
            days,
            ["2026-10-05", "2026-10-06", "2026-10-08", "2026-10-09"]
        );
        assert!(occ.iter().all(|t| z.civil(*t).hour() == 18));
    }

    #[test]
    fn relative_local_and_date_times() {
        let z = ny();
        let t0 = at(&z, "2026-10-05T18:45:15");
        let rel = TimeSpec::relative(14 * 60_000 + 40_000);
        assert_eq!(
            rel.occurrences(&z, Some(t0), 0, i64::MAX / 2),
            vec![t0 + 880_000]
        );
        assert!(rel.occurrences(&z, None, 0, i64::MAX / 2).is_empty());
        // 00:30 on the next day, from T0's date.
        let next_day = TimeSpec::local("00:30", 1).unwrap();
        let want = at(&z, "2026-10-06T00:30:00");
        assert_eq!(
            next_day.occurrences(&z, Some(t0), 0, i64::MAX / 2),
            vec![want]
        );
        let one_off = TimeSpec::date("2026-10-31T18:00:00").unwrap();
        assert_eq!(
            one_off.occurrences(&z, None, 0, i64::MAX / 2),
            vec![at(&z, "2026-10-31T18:00:00")]
        );
    }

    #[test]
    fn cron_errors_name_the_field() {
        assert!(Cron::parse("0 * * *").unwrap_err().contains("five fields"));
        assert!(Cron::parse("61 * * * *").unwrap_err().contains("minute"));
        assert!(Cron::parse("0 25 * * *").unwrap_err().contains("hour"));
        assert!(TimeSpec::local("25:00", 0).is_err());
        assert_eq!(parse_time_of_day("18:59:40.250").unwrap(), 68_380_250);
    }
}
