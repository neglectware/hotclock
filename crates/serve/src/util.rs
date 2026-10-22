//! Small helpers the two services share: time parsing, CSV, the artwork answer.

use crate::http::Response;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::Path;

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2026-09-30T12:00:00.250Z`, `...+02:00`, a date alone, or unix milliseconds, as unix milliseconds.
pub fn parse_time(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.chars().all(|c| c.is_ascii_digit()) && s.len() >= 10 {
        return s.parse().ok();
    }
    let (date, rest) = match s.split_once(['T', ' ']) {
        Some((d, r)) => (d, r),
        None => (s, "00:00:00Z"),
    };
    let mut dp = date.split('-');
    let (y, mo, d): (i64, i64, i64) = (
        dp.next()?.parse().ok()?,
        dp.next()?.parse().ok()?,
        dp.next()?.parse().ok()?,
    );
    let (clock, offset_min) = if let Some(c) = rest.strip_suffix('Z') {
        (c, 0)
    } else if let Some(i) = rest.rfind(['+', '-']) {
        let (c, off) = rest.split_at(i);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let (oh, om) = off[1..].split_once(':').unwrap_or((&off[1..], "0"));
        (
            c,
            sign * (oh.parse::<i64>().ok()? * 60 + om.parse::<i64>().ok()?),
        )
    } else {
        (rest, 0)
    };
    let mut cp = clock.split(':');
    let h: i64 = cp.next()?.parse().ok()?;
    let mi: i64 = cp.next().unwrap_or("0").parse().ok()?;
    let sec_text = cp.next().unwrap_or("0");
    let (sec, ms) = match sec_text.split_once('.') {
        Some((a, b)) => {
            let frac: String = b.chars().take(3).collect();
            (
                a.parse::<i64>().ok()?,
                format!("{frac:0<3}").parse::<i64>().ok()?,
            )
        }
        None => (sec_text.parse::<i64>().ok()?, 0),
    };
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 24 || mi > 59 || sec > 60 {
        return None;
    }
    Some(
        ((days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec - offset_min * 60) * 1000)
            + ms,
    )
}

/// One CSV field, quoted when it needs to be.
pub fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// The artwork answer (7.2, 7.3): the 600 or 144 px JPEG of the item, the hash as `ETag`, `404` when the
/// item has none.
pub fn art_response(
    library_db: &Path,
    media_dir: &Path,
    item_id: i64,
    size: &str,
    if_none_match: Option<&str>,
) -> Response {
    let size: u32 = match size {
        "600" => 600,
        "144" => 144,
        _ => {
            return Response::error(400, "bad_request", "size must be 600 or 144");
        }
    };
    let conn = match Connection::open_with_flags(library_db, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(e) => return Response::error(503, "unavailable", &format!("library.db: {e}")),
    };
    let sha: Option<Option<String>> = conn
        .query_row(
            "SELECT art_sha FROM items WHERE id = ?1 AND deleted_at IS NULL",
            [item_id],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None);
    let Some(sha) = sha else {
        return Response::error(404, "not_found", &format!("no item {item_id}"));
    };
    let Some(sha) = sha else {
        return Response::error(404, "not_found", "the item has no artwork");
    };
    let etag = format!("\"{sha}\"");
    let cache = "public, max-age=31536000, immutable";
    if if_none_match.is_some_and(|m| m.split(',').any(|t| t.trim() == etag)) {
        let mut r = Response::new(304).with_header("etag", &etag);
        r.headers.push(("cache-control".into(), cache.into()));
        return r;
    }
    match std::fs::read(media_dir.join(library::store::art_rel(&sha, size))) {
        Ok(bytes) => Response::text(200, "image/jpeg", bytes)
            .with_header("etag", &etag)
            .with_header("cache-control", cache),
        Err(_) => Response::error(404, "not_found", "the artwork file is missing"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("1970-01-01T00:00:01.250Z"), Some(1250));
        assert_eq!(parse_time("2026-09-30T12:00:00Z"), Some(1_790_769_600_000));
        assert_eq!(
            parse_time("2026-09-30T14:00:00+02:00"),
            Some(1_790_769_600_000)
        );
        assert_eq!(parse_time("2026-09-30"), Some(1_790_726_400_000));
        assert_eq!(parse_time("1790769600000"), Some(1_790_769_600_000));
        assert_eq!(parse_time("nonsense"), None);
    }

    #[test]
    fn csv() {
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("plain"), "plain");
    }
}
