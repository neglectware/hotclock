//! Local wall-clock time without a time-zone database in the tree: the UTC offset asked from the system's
//! `date`, cached for an hour. Used for the things that happen "at 02:30" (the nightly snapshots, 5.5).
//! On any failure the offset is taken as zero and the caller logs nothing: a snapshot at 02:30 UTC instead of
//! local time is harmless.

use std::sync::Mutex;

/// Minutes east of UTC at the unix time `wall_ms`.
pub fn utc_offset_min(wall_ms: i64) -> i64 {
    static CACHE: Mutex<Option<(i64, i64)>> = Mutex::new(None);
    let bucket = wall_ms.div_euclid(3_600_000);
    if let Ok(g) = CACHE.lock() {
        if let Some((b, off)) = *g {
            if b == bucket {
                return off;
            }
        }
    }
    let off = query_offset(wall_ms.div_euclid(1000)).unwrap_or(0);
    if let Ok(mut g) = CACHE.lock() {
        *g = Some((bucket, off));
    }
    off
}

fn query_offset(unix: i64) -> Option<i64> {
    let gnu = std::process::Command::new("date")
        .args(["-d", &format!("@{unix}"), "+%z"])
        .output()
        .ok()
        .filter(|o| o.status.success());
    let out = match gnu {
        Some(o) => o,
        None => std::process::Command::new("date")
            .args(["-r", &unix.to_string(), "+%z"])
            .output()
            .ok()
            .filter(|o| o.status.success())?,
    };
    parse_zone(String::from_utf8(out.stdout).ok()?.trim())
}

/// `+0530` or `-0400` to minutes.
pub fn parse_zone(z: &str) -> Option<i64> {
    if z.len() != 5 {
        return None;
    }
    let sign = match z.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let h: i64 = z[1..3].parse().ok()?;
    let m: i64 = z[3..5].parse().ok()?;
    Some(sign * (h * 60 + m))
}

/// The local calendar day number (days since 1970-01-01 local) and the minute of that day.
pub fn local_day_minute(wall_ms: i64) -> (i64, i64) {
    let local_min = wall_ms.div_euclid(60_000) + utc_offset_min(wall_ms);
    (local_min.div_euclid(1440), local_min.rem_euclid(1440))
}

/// `YYYYMMDD` of a local day number.
pub fn ymd(day: i64) -> String {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones_parse() {
        assert_eq!(parse_zone("-0400"), Some(-240));
        assert_eq!(parse_zone("+0530"), Some(330));
        assert_eq!(parse_zone("0530"), None);
        assert_eq!(parse_zone("+05:30"), None);
    }

    #[test]
    fn day_numbers_print_as_dates() {
        assert_eq!(ymd(0), "19700101");
        assert_eq!(ymd(11_016), "20000229");
        assert_eq!(ymd(20_301), "20250801");
    }
}
