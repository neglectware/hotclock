//! The 72-hour hardware soak's two Rust parts (M1.8 acceptance 3): `hotclock soak-watch`, the sampler that
//! runs beside the services for the whole soak, and `hotclock soak-report`, which turns what the soak
//! collected into a verdict. `dev/soak-hardware.sh` starts and stops both, injects the faults and runs
//! `loopcheck`; see `docs/hardware-tests.md` ("M1.8 on the target box").
//!
//! A soak directory holds:
//!
//! * `manifest.json`: `{"start_ms", "hours", "data_dir", "stations": [{"name", "port"}], "library_port"}`
//! * `watch.csv`: one line per target per interval (`soak-watch`)
//! * `loopcheck-<station>.jsonl`: the reports of `hotclock loopcheck`, one per station
//! * `faults.log`: JSON lines from the script, `{"t_ms", "kind", "station", "phase", ...}`, with
//!   `phase` `inject` or `recovered` (and `recovery_s`)
//!
//! The report reads the stations' `state.db` (the as-run log and the alerts) and the library, read-only.

use crate::explain::{Explainer, LoopModel};
use crate::procstats;
use clap::Args;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

// ---- soak-watch -------------------------------------------------------------------------------------------

#[derive(Args)]
pub struct WatchArgs {
    /// The soak directory (`watch.csv` is appended to).
    #[arg(long)]
    out: PathBuf,
    /// A station to watch, `NAME=PORT` (repeat). Its process is the main PID of `hotclock-station@NAME`.
    #[arg(long = "station", value_name = "NAME=PORT")]
    stations: Vec<String>,
    /// The library service's port (0: not watched).
    #[arg(long, default_value_t = 8300)]
    library: u16,
    /// Seconds between samples.
    #[arg(long, default_value_t = 60)]
    interval: u64,
    /// Stop after this many hours (default: until SIGTERM).
    #[arg(long)]
    hours: Option<f64>,
    /// `NAME=PID` for a process that is not a systemd unit (a run by hand); repeat.
    #[arg(long = "pid", value_name = "NAME=PID")]
    pids: Vec<String>,
    /// `NAME=FILE`: the process's PID is read from this file at every sample (a process a script restarts).
    #[arg(long = "pidfile", value_name = "NAME=FILE")]
    pidfiles: Vec<String>,
}

fn unit_pid(unit: &str) -> Option<u32> {
    let out = std::process::Command::new("systemctl")
        .args(["show", "-p", "MainPID", "--value", unit])
        .output()
        .ok()?;
    let n: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    (n > 0).then_some(n)
}

fn http_get(port: u16, path: &str) -> Option<(u16, String)> {
    let r = serve::http::client::request(
        &format!("127.0.0.1:{port}"),
        "GET",
        path,
        &[],
        &[],
        Duration::from_secs(5),
    )
    .ok()?;
    Some((r.status, r.text()))
}

/// `name{labels} value` from a Prometheus text body.
fn metric(text: &str, name: &str) -> Option<f64> {
    text.lines()
        .find(|l| l.starts_with(name) && !l.starts_with('#'))
        .and_then(|l| l.rsplit(' ').next())
        .and_then(|v| v.parse().ok())
}

pub const WATCH_HEADER: &str = "ts_ms,target,pid,rss_kb,threads,fds,cpu_s,health_code,status,xruns_total,frames_written,restarts_total,alerts,snapshot_check,disk_free_gb,anon_kb";

pub fn watch(a: WatchArgs) -> ExitCode {
    let mut targets: Vec<(String, Option<u16>, String)> = vec![];
    for s in &a.stations {
        let Some((n, p)) = s.split_once('=') else {
            eprintln!("soak-watch: --station wants NAME=PORT, got `{s}`");
            return ExitCode::from(2);
        };
        let Ok(port) = p.parse::<u16>() else {
            eprintln!("soak-watch: bad port in `{s}`");
            return ExitCode::from(2);
        };
        targets.push((
            n.to_string(),
            Some(port),
            format!("hotclock-station@{n}.service"),
        ));
    }
    if a.library != 0 {
        targets.push((
            "library".into(),
            Some(a.library),
            "hotclock-library.service".into(),
        ));
    }
    let fixed: BTreeMap<String, u32> = a
        .pids
        .iter()
        .filter_map(|p| {
            p.split_once('=')
                .and_then(|(n, v)| Some((n.to_string(), v.parse().ok()?)))
        })
        .collect();
    let pidfiles: BTreeMap<String, PathBuf> = a
        .pidfiles
        .iter()
        .filter_map(|p| {
            p.split_once('=')
                .map(|(n, f)| (n.to_string(), PathBuf::from(f)))
        })
        .collect();
    if let Err(e) = std::fs::create_dir_all(&a.out) {
        eprintln!("soak-watch: {}: {e}", a.out.display());
        return ExitCode::FAILURE;
    }
    let path = a.out.join("watch.csv");
    let fresh = !path.exists();
    let mut f = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("soak-watch: {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    if fresh {
        let _ = writeln!(f, "{WATCH_HEADER}");
    }
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let _ = ctrlc::set_handler(move || s2.store(true, Ordering::SeqCst));
    let t0 = Instant::now();
    let mut next = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        if a.hours
            .is_some_and(|h| t0.elapsed().as_secs_f64() >= h * 3600.0)
        {
            break;
        }
        if Instant::now() >= next {
            next += Duration::from_secs(a.interval);
            let ts = wall_ms();
            for (name, port, unit) in &targets {
                let pid = fixed
                    .get(name)
                    .copied()
                    .or_else(|| {
                        pidfiles
                            .get(name)
                            .and_then(|f| std::fs::read_to_string(f).ok())
                            .and_then(|t| t.trim().parse().ok())
                    })
                    .or_else(|| unit_pid(unit));
                let ps = pid.and_then(procstats::sample).unwrap_or_default();
                let (code, status, xruns, frames, restarts, alerts, snap, disk) =
                    match port.and_then(|p| http_get(p, "/health").map(|h| (p, h))) {
                        Some((p, (code, body))) => {
                            let j: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                            let m = http_get(p, "/metrics").map(|m| m.1).unwrap_or_default();
                            (
                                code,
                                j["status"].as_str().unwrap_or("").to_string(),
                                metric(&m, "hotclock_xruns_total").unwrap_or(-1.0),
                                metric(&m, "hotclock_frames_written_total").unwrap_or(-1.0),
                                metric(&m, "hotclock_restarts_total").unwrap_or(-1.0),
                                j["alerts"].as_array().map_or(0, Vec::len),
                                j["db"]["snapshot_check"].as_str().unwrap_or("").to_string(),
                                j["disk"]["free_gb"].as_f64().unwrap_or(-1.0),
                            )
                        }
                        None => (0, String::new(), -1.0, -1.0, -1.0, 0, String::new(), -1.0),
                    };
                let _ = writeln!(
                    f,
                    "{ts},{name},{},{},{},{},{:.2},{code},{status},{xruns},{frames},{restarts},{alerts},{},{disk},{}",
                    pid.unwrap_or(0),
                    ps.rss_kb,
                    ps.threads,
                    ps.fds,
                    ps.cpu_s,
                    snap.replace(',', ";"),
                    ps.mem_kb
                );
            }
            let _ = f.flush();
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    ExitCode::SUCCESS
}

// ---- soak-report ------------------------------------------------------------------------------------------

#[derive(Args)]
pub struct ReportArgs {
    /// The soak directory.
    #[arg(long)]
    dir: PathBuf,
    /// Write `report.md` and `report.json` here (default: the soak directory).
    #[arg(long)]
    out: Option<PathBuf>,
    /// The loop a station's loopcheck recorded through, `NAME=GAIN_DB[,HIGHPASS_HZ[,ORDER]]` (repeat), e.g.
    /// `eve=-4.5,790,3`; overrides the manifest's `loop_model`. Default: a flat unity loop.
    #[arg(long = "loop", value_name = "NAME=MODEL", allow_hyphen_values = true)]
    loops: Vec<String>,
    /// The loop model's allowance, dB above loopcheck's threshold.
    #[arg(long, default_value_t = crate::explain::LOOP_MARGIN_DB)]
    loop_margin_db: f64,
    /// Read `library.db`, `media/` and `stations/<name>/state.db` from here instead of the manifest's
    /// `data_dir` (for judging a soak on another machine from consistent copies, e.g. `sqlite3 .backup`).
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct Row {
    ts: i64,
    pid: u32,
    rss_kb: f64,
    anon_kb: f64,
    threads: f64,
    fds: f64,
    cpu_s: f64,
    code: u16,
    xruns: f64,
    restarts: f64,
}

fn read_watch(path: &Path) -> BTreeMap<String, Vec<Row>> {
    let mut m: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    let Ok(t) = std::fs::read_to_string(path) else {
        return m;
    };
    for l in t.lines().skip(1) {
        let c: Vec<&str> = l.split(',').collect();
        if c.len() < 14 {
            continue;
        }
        let p = |i: usize| c[i].parse::<f64>().unwrap_or(0.0);
        m.entry(c[1].to_string()).or_default().push(Row {
            ts: c[0].parse().unwrap_or(0),
            pid: c[2].parse().unwrap_or(0),
            rss_kb: p(3),
            anon_kb: if c.len() > 15 { p(15) } else { p(3) },
            threads: p(4),
            fds: p(5),
            cpu_s: p(6),
            code: c[7].parse().unwrap_or(0),
            xruns: p(9),
            restarts: p(11),
        });
    }
    m
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

/// Contiguous stretches of one process (same pid), as index ranges.
fn segments(rows: &[Row]) -> Vec<(usize, usize)> {
    let mut out = vec![];
    let mut s = 0;
    for i in 1..=rows.len() {
        if i == rows.len() || rows[i].pid != rows[s].pid {
            out.push((s, i));
            s = i;
        }
    }
    out
}

struct Check {
    name: String,
    pass: bool,
    detail: String,
}

#[derive(Debug, Clone)]
struct Fault {
    t_ms: i64,
    kind: String,
    station: String,
    phase: String,
    recovery_s: Option<f64>,
    detail: String,
}

fn read_faults(path: &Path) -> Vec<Fault> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|j| Fault {
            t_ms: j["t_ms"].as_i64().unwrap_or(0),
            kind: j["kind"].as_str().unwrap_or("").into(),
            station: j["station"].as_str().unwrap_or("").into(),
            phase: j["phase"].as_str().unwrap_or("").into(),
            recovery_s: j["recovery_s"].as_f64(),
            detail: j["detail"].as_str().unwrap_or("").into(),
        })
        .collect()
}

/// The size of `soak-hardware.sh`'s clock step.
const CLOCK_STEP_MS: i64 = 600_000;

/// The memory line's name (restated 2026-10-04; see the memory section of `report_inner`).
const MEMORY_CHECK: &str =
    "memory high-water: last 24 h within 5 percent (or 2 MB) of hours 2 to 12";
const HEAP_CHECK: &str = "live heap: slope from hour 2 under 1 MB/day";
const MEM_RISE_PCT: f64 = 5.0;
const MEM_RISE_FLOOR_KB: f64 = 2048.0;
const HEAP_SLOPE_KB_DAY: f64 = 1024.0;

/// A rise from `early` to `late` (kB) passes unless it is over `MEM_RISE_PCT` percent and over `MEM_RISE_FLOOR_KB`.
fn mem_rise_ok(early: f64, late: f64) -> bool {
    let rise = late - early;
    !(early > 0.0 && rise / early * 100.0 > MEM_RISE_PCT && rise > MEM_RISE_FLOOR_KB)
}

/// The high-water marks of one process run: hours 2 to 12 and the last 24 hours. A run under 36 h is cut
/// into the first third after hour 2 and the rest, so the two windows never overlap.
fn high_water(seg: &[Row], v: impl Fn(&Row) -> f64) -> (f64, f64) {
    let t0 = seg[0].ts;
    let t_end = seg.last().map_or(t0, |r| r.ts);
    let after2 = (t_end - t0 - 2 * 3_600_000).max(0);
    let early_to = t0 + 2 * 3_600_000 + (10 * 3_600_000).min(after2 / 3);
    let late_from = (t_end - 24 * 3_600_000).max(early_to);
    let max = |from: i64, to: i64| {
        seg.iter()
            .filter(|r| r.ts >= from && r.ts <= to)
            .map(&v)
            .fold(0.0f64, f64::max)
    };
    (max(t0 + 2 * 3_600_000, early_to - 1), max(late_from, t_end))
}

/// The old line's figures (medians of the half hour after hour 2 and of the last half hour), for information.
fn end_vs_hour2(seg: &[Row], v: impl Fn(&Row) -> f64) -> (f64, f64) {
    let t0 = seg[0].ts;
    let t_last = seg.last().map_or(t0, |r| r.ts);
    let med = |from: i64, to: i64| {
        let mut x: Vec<f64> = seg
            .iter()
            .filter(|r| r.ts >= from && r.ts < to)
            .map(&v)
            .collect();
        median(&mut x)
    };
    (
        med(t0 + 2 * 3_600_000, t0 + 2 * 3_600_000 + 1_800_000),
        med(t_last - 1_800_000, t_last + 1),
    )
}

/// One live-heap sample: `(unix_ms, pid, live_heap_kb, sqlite_kb)`.
type HeapSample = (i64, u32, f64, f64);

/// `unix_ms pid live_heap_kb sqlite_kb` lines (`procstats::log_live_heap_from_env`); `#` lines are comments.
fn read_live_heap(path: &Path) -> Vec<HeapSample> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let c: Vec<&str> = l.split_whitespace().collect();
            if c.len() < 4 {
                return None;
            }
            Some((
                c[0].parse().ok()?,
                c[1].parse().ok()?,
                c[2].parse().ok()?,
                c[3].parse().ok()?,
            ))
        })
        .collect()
}

struct HeapFit {
    hours: f64,
    early_kb: f64,
    early_rust_kb: f64,
    early_sqlite_kb: f64,
    late_kb: f64,
    slope_kb_day: f64,
}

/// The longest run of one pid in a live-heap log: its least-squares slope from hour 2 (Rust heap plus SQLite).
/// Time runs on the stamps, except that a step between two samples outside (0, 120 s] (a clock jump) counts
/// as the median step.
fn live_heap_slope(s: &[HeapSample]) -> Option<HeapFit> {
    let mut runs: Vec<&[HeapSample]> = vec![];
    let mut i = 0;
    for j in 1..=s.len() {
        if j == s.len() || s[j].1 != s[i].1 {
            runs.push(&s[i..j]);
            i = j;
        }
    }
    let run = runs.into_iter().max_by_key(|r| r.len())?;
    let mut steps: Vec<f64> = run
        .windows(2)
        .map(|w| (w[1].0 - w[0].0) as f64)
        .filter(|d| *d > 0.0 && *d <= 120_000.0)
        .collect();
    let typical = median(&mut steps);
    if typical <= 0.0 {
        return None;
    }
    let mut t = vec![0.0f64];
    for w in run.windows(2) {
        let d = (w[1].0 - w[0].0) as f64;
        let d = if d > 0.0 && d <= 120_000.0 {
            d
        } else {
            typical
        };
        t.push(t.last().copied().unwrap_or(0.0) + d);
    }
    let hours = t.last().copied().unwrap_or(0.0) / 3_600_000.0;
    if hours < 3.0 {
        return None;
    }
    let h = |x: f64| x * 3_600_000.0;
    let pts: Vec<(f64, f64)> = run
        .iter()
        .zip(&t)
        .filter(|(_, t)| **t >= h(2.0))
        .map(|(r, t)| (*t, r.2 + r.3))
        .collect();
    let n = pts.len() as f64;
    let (mx, my) = (
        pts.iter().map(|p| p.0).sum::<f64>() / n,
        pts.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    let sxx: f64 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let slope_kb_day = if sxx > 0.0 {
        sxy / sxx * 86_400_000.0
    } else {
        0.0
    };
    let med_of = |from: f64, to: f64, f: &dyn Fn(&HeapSample) -> f64| {
        let mut x: Vec<f64> = run
            .iter()
            .zip(&t)
            .filter(|(_, t)| **t >= from && **t < to)
            .map(|(r, _)| f(r))
            .collect();
        median(&mut x)
    };
    let end = t.last().copied().unwrap_or(0.0);
    let early_to = h(8.0).min(h(2.0) + (end - h(2.0)) / 3.0);
    Some(HeapFit {
        hours,
        early_kb: med_of(h(2.0), early_to, &|r| r.2 + r.3),
        early_rust_kb: med_of(h(2.0), early_to, &|r| r.2),
        early_sqlite_kb: med_of(h(2.0), early_to, &|r| r.3),
        late_kb: med_of((end - h(6.0)).max(early_to), end + 1.0, &|r| r.2 + r.3),
        slope_kb_day,
    })
}

/// As-run rows that straddle an injected clock step keep one stamp taken while the wall clock was stepped (the
/// first Box A soak: rows 2040 and 2041, 10 minutes off, 7 loopcheck gaps judged against them). Their
/// `played_ms` is right, so the stepped stamp is rebuilt from the other one: an end stamped while stepped
/// becomes `started + played`, a start stamped while stepped becomes `ended - played`. Rows are
/// `(id, started_ms, ended_ms, played_ms)`; `steps` the injection times. Returns the shifts as a
/// `RowView` (start shift added, end shift subtracted) and the number of rows repaired.
///
/// A row the station itself flagged `clock_jump_ms=<Δ>` (it was open when the wall clock jumped) needs no
/// guess about which stamp is wrong: the end stamp was taken on the new clock, so it is rebuilt as
/// `started + played`. `flagged` holds those ids; the stepped-window rule above stays for older data.
fn clock_step_repairs(
    rows: &[(i64, i64, Option<i64>, Option<i64>)],
    flagged: &std::collections::HashSet<i64>,
    steps: &[i64],
) -> (crate::explain::RowView, usize) {
    let mut v = crate::explain::RowView::default();
    let stepped = |t: i64| {
        steps
            .iter()
            .any(|s| t >= s + CLOCK_STEP_MS - 2_000 && t <= s + CLOCK_STEP_MS + 145_000)
    };
    for &(id, started, ended, played) in rows {
        let (Some(ended), Some(played)) = (ended, played) else {
            continue;
        };
        let d = (ended - started) - played;
        if d.abs() <= 2_000 {
            continue;
        }
        if flagged.contains(&id) || (stepped(ended) && !stepped(started)) {
            v.end_shift.insert(id, d);
        } else if stepped(started) && !stepped(ended) {
            v.start_shift.insert(id, d);
        } else {
            continue;
        }
        v.widen_ms = v.widen_ms.max(d.abs());
    }
    let n = v.start_shift.len() + v.end_shift.len();
    (v, n)
}

/// The repairs for one station's `state.db` (empty when it cannot be read or there was no clock step).
fn station_repairs(db: &Path, steps: &[i64], since_ms: i64) -> (crate::explain::RowView, usize) {
    type Row = (i64, i64, Option<i64>, Option<i64>, String);
    let rows: Vec<Row> =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .and_then(|c| {
                c.prepare("SELECT id, started_ms, ended_ms, played_ms, flags FROM as_run WHERE started_ms >= ?1 ORDER BY id")
                    .and_then(|mut q| {
                        q.query_map([since_ms], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                            .collect::<Result<_, _>>()
                    })
            })
            .unwrap_or_default();
    let flagged: std::collections::HashSet<i64> = rows
        .iter()
        .filter(|r| r.4.contains("clock_jump_ms="))
        .map(|r| r.0)
        .collect();
    let rows: Vec<(i64, i64, Option<i64>, Option<i64>)> =
        rows.into_iter().map(|r| (r.0, r.1, r.2, r.3)).collect();
    if steps.is_empty() && flagged.is_empty() {
        return (crate::explain::RowView::default(), 0);
    }
    clock_step_repairs(&rows, &flagged, steps)
}

/// Loopcheck stamps further than this from the offset between wall time and capture time are stepped stamps.
const STEPPED_STAMP_MS: i64 = 300_000;

/// A finding split into the text that identifies it and the place in a file that it is at: `` `Junk` (item 33)
/// is audible at 18.1 s into its file `` is the key `` `Junk` (item 33) is audible at # s into its file `` and
/// the place 18.1. Other reasons ("no item on air from 1.23 s to 1.5 s of the run") lose their run-relative
/// numbers and have no place.
fn finding_key(why: &str) -> (String, Option<f64>) {
    if let Some((head, tail)) = why.split_once(" is audible at ") {
        if let Some((num, rest)) = tail.split_once(" s") {
            if let Ok(pos) = num.parse::<f64>() {
                return (format!("{head} is audible at # s{rest}"), Some(pos));
            }
        }
    }
    let mut out = String::new();
    let mut chars = why.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            // A whole number, with its fraction: one `#`.
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
            if chars.peek() == Some(&'.') {
                let mut ahead = chars.clone();
                ahead.next();
                if ahead.peek().is_some_and(char::is_ascii_digit) {
                    chars.next();
                    while chars.peek().is_some_and(char::is_ascii_digit) {
                        chars.next();
                    }
                }
            }
            out.push('#');
        } else {
            out.push(c);
        }
    }
    (out, None)
}

/// Occurrences of one finding are the same finding when the station, the reason and the place in the file
/// (within this many seconds) agree: the same note decays at the same place every play, to within the 10 ms
/// windows loopcheck judges in.
const SAME_PLACE_S: f64 = 0.3;

/// The occurrences of one finding: same station, same reason, same place.
struct FindingGroup {
    key: String,
    place_sum: f64,
    place_n: usize,
    count: usize,
    ms: (i64, i64),
    level: Option<(f64, f64)>,
    first: i64,
    last: i64,
}

impl FindingGroup {
    fn place(&self) -> Option<f64> {
        (self.place_n > 0).then(|| self.place_sum / self.place_n as f64)
    }

    fn add(
        list: &mut Vec<FindingGroup>,
        key: &str,
        place: Option<f64>,
        ms: i64,
        at: i64,
        level: Option<f64>,
    ) {
        let same = |g: &FindingGroup| {
            g.key == key
                && match (g.place(), place) {
                    (Some(a), Some(b)) => (a - b).abs() <= SAME_PLACE_S,
                    (None, None) => true,
                    _ => false,
                }
        };
        if let Some(g) = list.iter_mut().find(|g| same(g)) {
            g.count += 1;
            g.ms = (g.ms.0.min(ms), g.ms.1.max(ms));
            g.level = match (g.level, level) {
                (Some((a, b)), Some(l)) => Some((a.min(l), b.max(l))),
                (None, l) => l.map(|l| (l, l)),
                (g, None) => g,
            };
            g.first = g.first.min(at);
            g.last = g.last.max(at);
            if let Some(p) = place {
                g.place_sum += p;
                g.place_n += 1;
            }
        } else {
            list.push(FindingGroup {
                key: key.to_string(),
                place_sum: place.unwrap_or(0.0),
                place_n: usize::from(place.is_some()),
                count: 1,
                ms: (ms, ms),
                level: level.map(|l| (l, l)),
                first: at,
                last: at,
            });
        }
    }

    /// Most frequent first; at most [`Self::MAX_LINES`] lines, the rest counted in a last line.
    const MAX_LINES: usize = 100;
    fn render(list: &[FindingGroup]) -> Vec<String> {
        let mut v: Vec<&FindingGroup> = list.iter().collect();
        v.sort_by(|a, b| b.count.cmp(&a.count).then(a.first.cmp(&b.first)));
        let mut out: Vec<String> = v
            .iter()
            .take(Self::MAX_LINES)
            .map(|g| {
                let key = match g.place() {
                    Some(p) => g.key.replacen('#', &format!("{p:.1}"), 1),
                    None => g.key.clone(),
                };
                let len = if g.ms.1 == 0 {
                    String::new()
                } else if g.ms.0 == g.ms.1 {
                    format!(", {} ms", g.ms.0)
                } else {
                    format!(", {} to {} ms", g.ms.0, g.ms.1)
                };
                let lvl = g.level.map_or(String::new(), |(a, b)| {
                    if (a - b).abs() < 0.05 {
                        format!(", recorded at {a:.1} dBFS")
                    } else {
                        format!(", recorded at {a:.1} to {b:.1} dBFS")
                    }
                });
                if g.count == 1 {
                    format!("{key} at {}{len}{lvl}", iso(g.first))
                } else {
                    format!(
                        "{key} \u{d7}{} ({} to {}{len}{lvl})",
                        g.count,
                        iso(g.first),
                        iso(g.last)
                    )
                }
            })
            .collect();
        if v.len() > Self::MAX_LINES {
            let rest: usize = v[Self::MAX_LINES..].iter().map(|g| g.count).sum();
            out.push(format!(
                "and {} more groups ({rest} occurrences)",
                v.len() - Self::MAX_LINES
            ));
        }
        out
    }
}

pub fn report(a: ReportArgs) -> ExitCode {
    match report_inner(a) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("soak-report: {e}");
            ExitCode::FAILURE
        }
    }
}

fn report_inner(a: ReportArgs) -> Result<bool, String> {
    let man: Value = serde_json::from_str(
        &std::fs::read_to_string(a.dir.join("manifest.json"))
            .map_err(|e| format!("manifest.json: {e}"))?,
    )
    .map_err(|e| format!("manifest.json: {e}"))?;
    let start_ms = man["start_ms"].as_i64().ok_or("manifest: no start_ms")?;
    let data_dir = a
        .data_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(man["data_dir"].as_str().unwrap_or("/var/lib/hotclock")));
    let stations: Vec<String> = man["stations"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let watch = read_watch(&a.dir.join("watch.csv"));
    let faults = read_faults(&a.dir.join("faults.log"));
    let end_ms = watch
        .values()
        .filter_map(|r| r.last().map(|x| x.ts))
        .max()
        .unwrap_or(start_ms);
    let hours = (end_ms - start_ms) as f64 / 3_600_000.0;
    let mut checks: Vec<Check> = vec![];
    let mut sections: Vec<String> = vec![];

    // Windows in which silence or a restart is explained by an injected fault (from the injection until the
    // recovery, and 10 s more).
    // The fourth field: the window belongs to a clock step (loopcheck gaps with a capture time are mapped out of
    // the stepped stamps instead, see below).
    let mut fault_windows: Vec<(i64, i64, String, bool)> = vec![];
    for f in faults.iter().filter(|f| f.phase == "inject") {
        let end = faults
            .iter()
            .find(|r| {
                r.phase == "recovered"
                    && r.kind == f.kind
                    && r.station == f.station
                    && r.t_ms >= f.t_ms
            })
            .map_or(f.t_ms + 120_000, |r| r.t_ms);
        // The clock stays stepped for two minutes after the injection, and its recovery is stamped in stepped
        // time (600 s ahead), so it cannot end the window: the real two minutes (and the step back, plus slack)
        // are this window, and the stepped stamps of the same time get their own window below.
        let end = if f.kind == "clock_step" {
            f.t_ms + 135_000
        } else {
            end
        };
        fault_windows.push((
            f.t_ms - 2_000,
            end + 10_000,
            format!("{} {}", f.kind, f.station),
            f.kind == "clock_step",
        ));
        // While the clock is stepped, everything is stamped about 600 s ahead (soak-hardware.sh steps +600 s
        // for two minutes), so the same two minutes also appear 600 s later in every log.
        if f.kind == "clock_step" {
            fault_windows.push((
                f.t_ms + CLOCK_STEP_MS - 2_000,
                f.t_ms + CLOCK_STEP_MS + 135_000 + 10_000,
                format!("{} {} (stamped while stepped)", f.kind, f.station),
                true,
            ));
        }
    }
    let in_fault = |from: i64, to: i64| fault_windows.iter().any(|w| from <= w.1 && to >= w.0);
    // The same without the clock steps': for what loopcheck recorded with its own capture time.
    let in_fault_not_step = |from: i64, to: i64| {
        fault_windows
            .iter()
            .any(|w| !w.3 && from <= w.1 && to >= w.0)
    };
    // The wall clock is the box's: every station's as-run rows can straddle a step, whichever station it was
    // injected for.
    let clock_steps: Vec<i64> = faults
        .iter()
        .filter(|f| f.phase == "inject" && f.kind == "clock_step")
        .map(|f| f.t_ms)
        .collect();

    // 1. memory, threads, descriptors, CPU.
    //
    // The memory line (restated 2026-10-04 before the second Box A soak; `docs/hardware-tests.md`): glibc keeps
    // freed pages per thread arena, so RssAnon steps up to a high-water mark and settles, and "the end against
    // hour 2" cannot pass on glibc (the first Box A soak read +11 to 12 % for a process that plateaus). Judged
    // instead: the high-water mark over the run's last 24 hours against the high-water mark over hours 2 to 12
    // (for a run under 36 h, the first third after hour 2 against the rest). It fails only when the rise is
    // over 5 percent AND over 2 MB, so a 2 MB process cannot fail on a few hundred kB. The live heap (below)
    // is the stronger check where a station logs it.
    let mut mem = String::from("| process | longest run | high-water, hours 2 to 12 | high-water, last 24 h | rise | end vs hour 2 (old line, for information) | threads | fds | restarts seen |\n|---|---|---|---|---|---|---|---|---|\n");
    let mut mem_ok = true;
    let mut mem_detail: Vec<String> = vec![];
    let mut insufficient = vec![];
    for (name, rows) in &watch {
        if rows.is_empty() {
            continue;
        }
        let segs = segments(rows);
        let restarts = segs.len().saturating_sub(1);
        let best = segs
            .iter()
            .max_by_key(|s| rows[s.1 - 1].ts - rows[s.0].ts)
            .copied()
            .unwrap_or((0, rows.len()));
        let seg = &rows[best.0..best.1];
        let dur_h = (seg.last().map_or(0, |r| r.ts) - seg[0].ts) as f64 / 3_600_000.0;
        if dur_h >= 3.0 {
            // The figure that judges is the anonymous memory when the sampler had it (`RssAnon` on Linux, where it
            // is RSS less mapped files; the physical footprint on macOS, whose RSS counts freed pages the
            // allocator has not returned), else RSS.
            let use_anon = seg.iter().any(|r| r.anon_kb != r.rss_kb);
            let v = |r: &Row| if use_anon { r.anon_kb } else { r.rss_kb };
            let hw = high_water(seg, v);
            let (e2, l2) = end_vs_hour2(seg, v);
            let old = if e2 > 0.0 {
                (l2 - e2) / e2 * 100.0
            } else {
                0.0
            };
            let (early, late) = hw;
            let rise_kb = late - early;
            let rise_pct = if early > 0.0 {
                rise_kb / early * 100.0
            } else {
                0.0
            };
            let pass = mem_rise_ok(early, late);
            mem_ok &= pass;
            mem_detail.push(format!(
                "{name} {rise_pct:+.2} % ({:+.1} MB)",
                rise_kb / 1024.0
            ));
            mem.push_str(&format!(
                "| {name} | {dur_h:.1} h | {:.1} MB | {:.1} MB | {rise_pct:+.2} % ({:+.2} MB){} | {old:+.2} % | {:.0} to {:.0} | {:.0} to {:.0} | {restarts} |\n",
                early / 1024.0,
                late / 1024.0,
                rise_kb / 1024.0,
                if pass { "" } else { " **over**" },
                seg[0].threads,
                seg.last().map_or(0.0, |r| r.threads),
                seg[0].fds,
                seg.last().map_or(0.0, |r| r.fds),
            ));
        } else {
            insufficient.push(name.clone());
            mem.push_str(&format!(
                "| {name} | {dur_h:.1} h | (under 3 h: no figure) | | | | | | {restarts} |\n"
            ));
        }
    }
    checks.push(Check {
        name: MEMORY_CHECK.into(),
        pass: mem_ok && !mem_detail.is_empty(),
        detail: if mem_detail.is_empty() {
            "no process ran for 3 hours without a restart".into()
        } else {
            format!(
                "{}{}",
                mem_detail.join("; "),
                if insufficient.is_empty() {
                    String::new()
                } else {
                    format!("; no figure for {}", insufficient.join(", "))
                }
            )
        },
    });
    let mem = format!(
        "{mem}\nThe rise is the high-water mark of {} over the longest run's last 24 h against hours 2 to 12 (the first third after hour 2 against the rest, for a run under 36 h). It fails only when it is over {MEM_RISE_PCT:.0} percent and over {:.0} MB.\n",
        "anonymous memory (RSS where the sampler had no separate figure)",
        MEM_RISE_FLOOR_KB / 1024.0
    );

    // The live heap (a station built with `alloc-count`, `HOTCLOCK_LIVE_HEAP_LOG`): what the process holds,
    // whatever the allocator keeps resident. A leak shows here as a slope.
    let mut heap_md = String::new();
    let wanted_heap: Vec<(String, String)> = man["live_heap"]
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect()
        })
        .unwrap_or_default();
    let mut heap_names: Vec<String> = wanted_heap.iter().map(|w| w.0.clone()).collect();
    for name in &stations {
        if !heap_names.contains(name) && a.dir.join(format!("live-heap-{name}.log")).exists() {
            heap_names.push(name.clone());
        }
    }
    if !heap_names.is_empty() {
        let (mut heap_ok, mut heap_detail) = (true, vec![]);
        heap_md.push_str("| process | log | longest run | live heap (Rust + SQLite), hours 2 to 8 | last 6 h | slope from hour 2 | limit |\n|---|---|---|---|---|---|---|\n");
        for name in &heap_names {
            let in_dir = a.dir.join(format!("live-heap-{name}.log"));
            let named = wanted_heap
                .iter()
                .find(|w| &w.0 == name && !w.1.is_empty())
                .map(|w| PathBuf::from(&w.1));
            // The soak directory's copy, else the path the manifest names (a service's own log, the library's
            // included), else a station's default place.
            let path = if in_dir.exists() {
                in_dir
            } else if let Some(p) = named.filter(|p| p.exists()) {
                p
            } else {
                data_dir.join("stations").join(name).join("live-heap.log")
            };
            match live_heap_slope(&read_live_heap(&path)) {
                Some(h) => {
                    let pass = h.slope_kb_day <= HEAP_SLOPE_KB_DAY;
                    heap_ok &= pass;
                    heap_detail.push(format!(
                        "{name} {:+.0} kB/day over {:.1} h",
                        h.slope_kb_day, h.hours
                    ));
                    heap_md.push_str(&format!(
                        "| {name} | {} | {:.1} h | {:.1} MB (Rust {:.1}, SQLite {:.1}) | {:.1} MB | {:+.0} kB/day{} | {:.0} kB/day |\n",
                        path.display(),
                        h.hours,
                        h.early_kb / 1024.0,
                        h.early_rust_kb / 1024.0,
                        h.early_sqlite_kb / 1024.0,
                        h.late_kb / 1024.0,
                        h.slope_kb_day,
                        if pass { "" } else { " **over**" },
                        HEAP_SLOPE_KB_DAY
                    ));
                }
                None => {
                    heap_ok = false;
                    heap_detail.push(format!("{name}: no run of 3 h in {}", path.display()));
                    heap_md.push_str(&format!(
                        "| {name} | {} (missing, or no run of 3 h) | | | | | |\n",
                        path.display()
                    ));
                }
            }
        }
        checks.push(Check {
            name: HEAP_CHECK.into(),
            pass: heap_ok,
            detail: heap_detail.join("; "),
        });
        heap_md = format!(
            "\n### Live heap\n\nBytes allocated and not freed (a counting allocator, `alloc-count`), plus SQLite's own, every 30 s. The slope is a least-squares fit from hour 2 of the longest run to its end (stamps across a clock jump are taken at the logger's interval). Limit {HEAP_SLOPE_KB_DAY:.0} kB/day.\n\n{heap_md}"
        );
    }
    sections.push(format!("## Memory, threads, descriptors\n\n{mem}{heap_md}"));

    // CPU per station: 5-minute windows within one process.
    let mut cpu_md = String::from("| station | mean | worst 5 min | limit |\n|---|---|---|---|\n");
    let mut cpu_ok = true;
    let mut cpu_detail = vec![];
    for name in &stations {
        let Some(rows) = watch.get(name) else {
            continue;
        };
        let mut worst = 0.0f64;
        let (mut sum, mut n) = (0.0, 0.0);
        for (s, e) in segments(rows) {
            let seg = &rows[s..e];
            let mut i = 0;
            for j in 1..seg.len() {
                // A wall-clock step (the clock_step injection) makes the stamps jump; a window across it would
                // divide real minutes of CPU by the wrong number of seconds (the 2026-10-01 Box A soak read
                // 22 % for a steady 6.3 %). Start the window again after any jump back or a hole over 3 minutes.
                let step = seg[j].ts - seg[j - 1].ts;
                if !(0..=180_000).contains(&step) {
                    i = j;
                    continue;
                }
                while seg[j].ts - seg[i].ts > 300_000 && i + 1 < j {
                    i += 1;
                }
                let dt = (seg[j].ts - seg[i].ts) as f64 / 1000.0;
                if dt >= 240.0 {
                    let pct = (seg[j].cpu_s - seg[i].cpu_s) / dt * 100.0;
                    worst = worst.max(pct);
                    sum += pct;
                    n += 1.0;
                }
            }
        }
        let mean = if n > 0.0 { sum / n } else { 0.0 };
        cpu_md.push_str(&format!("| {name} | {mean:.2} % | {worst:.2} % | 10 % |\n"));
        cpu_ok &= worst < 10.0;
        cpu_detail.push(format!("{name} worst {worst:.2} %"));
    }
    checks.push(Check {
        name: "CPU under 10 percent of one core per station".into(),
        pass: cpu_ok && !cpu_detail.is_empty(),
        detail: cpu_detail.join("; "),
    });
    sections.push(format!("## CPU\n\n{cpu_md}"));

    // 2. xruns, health.
    let mut xr_md = String::from("| station | xruns during the soak | health samples not 200 | restarts (counter) |\n|---|---|---|---|\n");
    let mut xr_ok = true;
    let mut xr_detail = vec![];
    for name in &stations {
        let Some(rows) = watch.get(name) else {
            continue;
        };
        let ok_rows: Vec<&Row> = rows.iter().filter(|r| r.xruns >= 0.0).collect();
        // An xrun counter resets with the process: count the rises between samples of one process, and leave
        // out the rises that fall in a window an injected fault covers (a stall injected on purpose is one).
        let mut total = 0.0;
        let mut explained = 0.0;
        for (s, e) in segments(rows) {
            let seg: Vec<&Row> = rows[s..e].iter().filter(|r| r.xruns >= 0.0).collect();
            for w in seg.windows(2) {
                let d = (w[1].xruns - w[0].xruns).max(0.0);
                if d > 0.0 {
                    if in_fault(w[0].ts - 60_000, w[1].ts + 5_000) {
                        explained += d;
                    } else {
                        total += d;
                    }
                }
            }
        }
        let bad_health = rows.iter().filter(|r| r.code != 200).count();
        let restarts = ok_rows.iter().map(|r| r.restarts).fold(0.0, f64::max);
        xr_md.push_str(&format!(
            "| {name} | {total:.0} ({explained:.0} more inside injected-fault windows) | {bad_health} of {} | {restarts:.0} |\n",
            rows.len()
        ));
        xr_ok &= total == 0.0;
        xr_detail.push(format!("{name}: {total:.0}"));
    }
    checks.push(Check {
        name: "zero xruns (or every one explained by a fault)".into(),
        pass: xr_ok,
        detail: xr_detail.join("; "),
    });
    sections.push(format!("## Xruns and health\n\n{xr_md}"));

    // 3. loopcheck (when the soak had a capture card).
    let mut lc_md = String::from("| station | recorded | gaps | unexplained | glitches | overruns | device lost | offset ppm |\n|---|---|---|---|---|---|---|---|\n");
    let mut lc_ok = true;
    let mut lc_detail = vec![];
    let mut findings: Vec<String> = vec![];
    let mut any_loop = false;
    let mut loop_models: Vec<String> = vec![];
    let loop_wanted = man["loopcheck"].as_bool().unwrap_or(true);
    for name in &stations {
        if !loop_wanted {
            break;
        }
        let p = a.dir.join(format!("loopcheck-{name}.jsonl"));
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        any_loop = true;
        // loopcheck's own threshold (its start line says, since 2026-10-01; -60 before).
        let threshold_db = text
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|j| j["type"] == "start")
            .and_then(|j| j["threshold_db"].as_f64())
            .unwrap_or(-60.0);
        let spec = a
            .loops
            .iter()
            .find_map(|s| s.strip_prefix(&format!("{name}=")).map(str::to_string))
            .or_else(|| {
                man["loop_model"][name.as_str()]
                    .as_str()
                    .map(str::to_string)
            });
        let model = match &spec {
            Some(s) => LoopModel::parse(s, threshold_db, a.loop_margin_db)?,
            None => LoopModel {
                margin_db: a.loop_margin_db,
                ..LoopModel::flat(threshold_db)
            },
        };
        loop_models.push(format!(
            "{name}: {}{}",
            model.describe(),
            if spec.is_none() {
                " (no model given: flat)"
            } else {
                ""
            }
        ));
        let state_db = data_dir.join("stations").join(name).join("state.db");
        let (view, _) = station_repairs(&state_db, &clock_steps, start_ms);
        let mut ex = Explainer::new(&state_db, &data_dir).with_loop(model);
        let (mut gaps, mut unexpl, mut glitches, mut overruns, mut lost) = (0, 0, 0, 0, 0);
        let mut secs = 0.0f64;
        let mut ppm = None;
        // A step of the wall clock moves loopcheck's `at_ms` stamps and not its capture time `t_s`, so the
        // difference of the two says whether a stamp was taken while stepped (soak 2: five "Mean Street" gaps,
        // stamped 600 s ahead, were judged against the play that really ran at the stamped time). `step_base`
        // is that difference outside a step; a gap whose difference is more than 5 minutes off it is moved
        // back to the real time, and is judged there (not excused by the fault).
        let start_at = text
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|j| j["type"] == "start")
            .and_then(|j| j["at_ms"].as_i64());
        let mut step_base: Option<i64> = start_at.map(|_| 0);
        let mut stepped_gaps = 0usize;
        let mut group_list: Vec<FindingGroup> = vec![];
        for l in text.lines() {
            let Ok(j) = serde_json::from_str::<Value>(l) else {
                continue;
            };
            match j["type"].as_str() {
                Some("gap") => {
                    gaps += 1;
                    let stamp = j["at_ms"].as_i64().unwrap_or(0);
                    let ms = i64::from(j["ms"].as_u64().unwrap_or(0) as u32);
                    let mut at = stamp;
                    let timed = j["t_s"].as_f64().zip(start_at);
                    if let Some((t_s, st)) = timed {
                        let off = stamp - st - (t_s * 1000.0) as i64;
                        match step_base {
                            Some(b) if (off - b).abs() > STEPPED_STAMP_MS => {
                                at = stamp - (off - b);
                                stepped_gaps += 1;
                            }
                            _ => step_base = Some(off),
                        }
                    }
                    let excused = if timed.is_some() {
                        in_fault_not_step(at - 1000, at + ms + 1000)
                    } else {
                        in_fault(at - 1000, at + ms + 1000)
                    };
                    if excused {
                        continue;
                    }
                    if let Some(why) = ex.unexplained(at, ms, &[], &view) {
                        unexpl += 1;
                        let (key, place) = finding_key(&why);
                        FindingGroup::add(
                            &mut group_list,
                            &format!("{name}: gap, {key}"),
                            place,
                            ms,
                            at,
                            j["level_db"].as_f64(),
                        );
                    }
                }
                Some("tone_glitch") => {
                    let at = j["at_ms"].as_i64().unwrap_or(0);
                    if !in_fault(at - 1000, at + 1000) {
                        glitches += 1;
                        FindingGroup::add(
                            &mut group_list,
                            &format!("{name}: a tone glitch"),
                            None,
                            0,
                            at,
                            None,
                        );
                    }
                }
                Some("capture_overrun") => {
                    let at = j["at_ms"].as_i64().unwrap_or(0);
                    if !in_fault(at - 1000, at + 1000) {
                        overruns += 1;
                    }
                }
                Some("device_lost") => {
                    let at = j["at_ms"].as_i64().unwrap_or(0);
                    if !in_fault(at - 1000, at + 1000) {
                        lost += 1;
                    }
                }
                Some("summary") => {
                    secs = j["seconds"].as_f64().unwrap_or(0.0);
                    ppm = j["clock_offset_ppm"].as_f64();
                }
                Some("status") => {
                    secs = secs.max(j["frames"].as_f64().unwrap_or(0.0) / 48_000.0);
                    ppm = j["clock_offset_ppm"].as_f64().or(ppm);
                }
                _ => {}
            }
        }
        if stepped_gaps > 0 {
            loop_models.push(format!(
                "{name}: {stepped_gaps} gap(s) were stamped while the wall clock was stepped; judged at their real time"
            ));
        }
        findings.extend(FindingGroup::render(&group_list));
        lc_md.push_str(&format!(
            "| {name} | {:.1} h | {gaps} | {unexpl} | {glitches} | {overruns} | {lost} | {} |\n",
            secs / 3600.0,
            ppm.map_or("n/a".into(), |p| format!("{p:.1}"))
        ));
        lc_ok &= unexpl == 0 && glitches == 0 && overruns == 0 && lost == 0;
        lc_detail.push(format!("{name}: {unexpl} unexplained of {gaps}"));
    }
    checks.push(Check {
        name: "loopcheck: zero dropouts".into(),
        pass: !loop_wanted || (any_loop && lc_ok),
        detail: if !loop_wanted {
            "not applicable: this soak had no capture card (the as-run log and the stations' own silence detector are the evidence)".into()
        } else if any_loop {
            lc_detail.join("; ")
        } else {
            "no loopcheck report in the soak directory".into()
        },
    });
    sections.push(format!(
        "## Loopcheck (the output recorded through another card)\n\nA gap is a stretch of 20 ms or more under loopcheck's threshold (-60 dBFS RMS in 10 ms windows). It is explained when an injected fault covers it, or when every item on air in it has a quiet stretch of its own at that place (architecture 15's silence rule; the office set is ambient), judged as the loop records it: the source through the play's gain and the loop's measured gain and high-pass, under the threshold plus a margin.\n\n{}{lc_md}\n{}",
        if loop_models.is_empty() { String::new() } else { format!("Loop models: {}.\n\n", loop_models.join("; ")) },
        if findings.is_empty() { String::new() } else { format!("Unexplained:\n\n{}\n", findings.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")) }
    ));

    // 4. as-run completeness.
    let mut ar_md = String::from("| station | plays | rows missing (id gaps) | unended | gaps over 50 ms | worst gap | stamps repaired across a clock step |\n|---|---|---|---|---|---|---|\n");
    let mut ar_ok = true;
    let mut ar_detail = vec![];
    let mut ar_find: Vec<String> = vec![];
    for name in &stations {
        let db = data_dir.join("stations").join(name).join("state.db");
        let Ok(c) =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        else {
            ar_md.push_str(&format!(
                "| {name} | (cannot read {}) | | | | | |\n",
                db.display()
            ));
            ar_ok = false;
            continue;
        };
        type R = (i64, String, i64, Option<i64>, String);
        let rows: Vec<R> = c
            .prepare("SELECT id, origin, started_ms, ended_ms, title FROM as_run WHERE started_ms >= ?1 ORDER BY id")
            .and_then(|mut q| q.query_map([start_ms], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<_, _>>())
            .unwrap_or_default();
        let (view, repaired) = station_repairs(&db, &clock_steps, start_ms);
        let rows: Vec<R> = rows
            .into_iter()
            .map(|mut r| {
                r.2 += view.start_shift.get(&r.0).copied().unwrap_or(0);
                r.3 =
                    r.3.map(|e| e - view.end_shift.get(&r.0).copied().unwrap_or(0));
                r
            })
            .collect();
        let missing = rows
            .windows(2)
            .map(|w| (w[1].0 - w[0].0 - 1).max(0))
            .sum::<i64>();
        let last = rows.last().map(|r| r.0);
        let unended = rows
            .iter()
            .filter(|r| r.3.is_none() && Some(r.0) != last)
            .count();
        let (mut gaps, mut worst) = (0, 0i64);
        let mut prev: Option<&R> = None;
        for r in &rows {
            if r.1 == "restart" || r.1 == "cart" {
                continue;
            }
            if let Some(p) = prev {
                if let Some(end) = p.3 {
                    let gap = r.2 - end;
                    if gap > 50 && !in_fault(end - 1000, r.2 + 1000) {
                        gaps += 1;
                        worst = worst.max(gap);
                        if ar_find.len() < 30 {
                            ar_find.push(format!(
                                "{name}: {gap} ms between `{}` and `{}` at {}",
                                p.4,
                                r.4,
                                iso(end)
                            ));
                        }
                    }
                }
            }
            prev = Some(r);
        }
        ar_md.push_str(&format!(
            "| {name} | {} | {missing} | {unended} | {gaps} | {worst} ms | {repaired} |\n",
            rows.len()
        ));
        // Rows lost to a kill -9 (the writer's last unflushed messages) are explained by the fault.
        let kills = faults
            .iter()
            .filter(|f| f.phase == "inject" && f.kind == "kill9" && f.station == *name)
            .count() as i64;
        ar_ok &= gaps == 0 && unended <= kills as usize + 1 && missing <= kills * 3;
        ar_detail.push(format!(
            "{name}: {} plays, {missing} ids missing, {gaps} gaps",
            rows.len()
        ));
    }
    checks.push(Check {
        name: "as-run log complete: no gap over 50 ms between plays".into(),
        pass: ar_ok && !ar_detail.is_empty(),
        detail: ar_detail.join("; "),
    });
    sections.push(format!(
        "## As-run log\n\nA gap is the time from one play's end to the next play's start when it is over 50 ms and no injected fault covers it. Missing ids are numbers absent from the row sequence: a kill -9 can lose the last unflushed rows. A row that straddles an injected clock step keeps one stamp taken while the clock was stepped; it is rebuilt from the other stamp and `played_ms` (here and in the loopcheck judgment).\n\n{ar_md}\n{}",
        if ar_find.is_empty() { String::new() } else { format!("Gaps:\n\n{}\n", ar_find.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")) }
    ));

    // 5. faults.
    let injected: Vec<&Fault> = faults.iter().filter(|f| f.phase == "inject").collect();
    let required: Vec<String> = man["required_faults"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_else(|| vec!["unplug".into(), "kill9".into(), "clock_step".into()]);
    let mut fl_md =
        String::from("| when | fault | station | recovery | verdict |\n|---|---|---|---|---|\n");
    let mut kinds = std::collections::BTreeSet::new();
    let mut fl_ok = true;
    for f in &injected {
        let rec = faults.iter().find(|r| {
            r.phase == "recovered" && r.kind == f.kind && r.station == f.station && r.t_ms >= f.t_ms
        });
        let limit = match f.kind.as_str() {
            "unplug" => 20.0,
            "kill9" | "kill9_library" | "sigterm" => 8.0,
            "clock_step" => 5.0,
            _ => 30.0,
        };
        let (rs, verdict) = match rec.and_then(|r| r.recovery_s) {
            Some(s) if s <= limit => (format!("{s:.1} s"), "recovered"),
            Some(s) => (format!("{s:.1} s"), "TOO SLOW"),
            None => ("none recorded".into(), "NOT RECOVERED"),
        };
        if verdict == "recovered" {
            kinds.insert(f.kind.clone());
        } else {
            fl_ok = false;
        }
        fl_md.push_str(&format!(
            "| {} | {} {} | {} | {rs} | {verdict} |\n",
            iso(f.t_ms),
            f.kind,
            f.detail,
            f.station
        ));
    }
    checks.push(Check {
        name: format!("fault injections ({}), each recovered", required.join(", ")),
        pass: fl_ok && required.iter().all(|k| kinds.contains(k)),
        detail: format!(
            "recovered: {}; required: {}",
            kinds.iter().cloned().collect::<Vec<_>>().join(", "),
            required.join(", ")
        ),
    });
    sections.push(format!("## Fault injections\n\n{fl_md}"));

    // 6. the run itself.
    let want_h = man["hours"].as_f64().unwrap_or(72.0);
    checks.push(Check {
        name: format!("the soak ran {want_h:.0} hours"),
        pass: hours >= want_h * 0.99,
        detail: format!("{hours:.1} h sampled"),
    });

    // Parts of the milestone this box cannot run (the manifest says why): listed, neither passed nor failed.
    let deferred: Vec<String> = man["deferred"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if !deferred.is_empty() {
        sections.push(format!(
            "## Deferred (not run on this box, not failed)\n\n{}\n",
            deferred
                .iter()
                .map(|d| format!("- {d}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    let all_ok = checks.iter().all(|c| c.pass);
    let mut md = format!(
        "# {} hardware soak report\n\nStarted {}; {hours:.1} h sampled; stations: {}.\n\n**{}**\n\n| check | result | detail |\n|---|---|---|\n",
        crate::brand::DISPLAY_NAME,
        iso(start_ms),
        stations.join(", "),
        if all_ok { "PASS" } else { "FAIL" }
    );
    for c in &checks {
        md.push_str(&format!(
            "| {} | {} | {} |\n",
            c.name,
            if c.pass { "pass" } else { "FAIL" },
            c.detail
        ));
    }
    md.push('\n');
    for s in &sections {
        md.push_str(s);
        md.push('\n');
    }
    let out = a.out.clone().unwrap_or_else(|| a.dir.clone());
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    std::fs::write(out.join("report.md"), &md).map_err(|e| e.to_string())?;
    std::fs::write(
        out.join("report.json"),
        serde_json::to_string_pretty(&json!({
            "pass": all_ok, "hours": hours,
            "checks": checks.iter().map(|c| json!({"name": c.name, "pass": c.pass, "detail": c.detail})).collect::<Vec<_>>(),
        }))
        .unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    println!("{md}");
    Ok(all_ok)
}

/// `YYYY-MM-DD HH:MM:SS` in UTC (a report is read on other machines).
pub fn iso(ms: i64) -> String {
    let s = library::util::iso_utc(ms.div_euclid(1000));
    s.replace('T', " ").trim_end_matches('Z').to_string() + " UTC"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(d: &Path) -> ReportArgs {
        ReportArgs {
            dir: d.to_path_buf(),
            out: None,
            loops: vec![],
            loop_margin_db: crate::explain::LOOP_MARGIN_DB,
            data_dir: None,
        }
    }

    /// A clock step in watch.csv (stamps 600 s ahead for two minutes, then back) does not inflate the CPU.
    #[test]
    fn cpu_windows_do_not_span_a_clock_step() {
        let d = std::env::temp_dir().join(format!("hotclock-report-cpu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let t0 = 1_800_000_000_000i64;
        std::fs::write(
            d.join("manifest.json"),
            json!({"start_ms": t0, "hours": 4.0, "data_dir": d.join("data"),
                   "stations": [{"name": "eve", "port": 8302}], "required_faults": []})
            .to_string(),
        )
        .unwrap();
        let mut w = String::from(WATCH_HEADER);
        w.push('\n');
        for i in 0..=240i64 {
            let stamp = t0 + i * 60_000 + if (120..122).contains(&i) { 600_000 } else { 0 };
            w.push_str(&format!(
                "{stamp},eve,100,200000,20,30,{:.2},200,ok,0,{},0,0,ok,50\n",
                i as f64 * 60.0 * 0.03,
                i * 2_880_000
            ));
        }
        std::fs::write(d.join("watch.csv"), w).unwrap();
        let _ = report_inner(args(&d)).unwrap();
        let md = std::fs::read_to_string(d.join("report.md")).unwrap();
        assert!(md.contains("| eve | 3.00 % | 3.00 % | 10 % |"), "{md}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A clock step stamps its two minutes about 600 s ahead: a gap stamped there is inside the fault, while a
    /// gap in the real minutes after the step back is judged normally (Box A soak, 2026-10-04: the report's
    /// window had ended on the stepped recovery stamp, covering the wrong ten minutes).
    #[test]
    fn a_clock_step_covers_its_stepped_stamps_only() {
        let d = std::env::temp_dir().join(format!("hotclock-report-cs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let t0 = 1_800_000_000_000i64;
        let inj = t0 + 1_800_000;
        std::fs::write(
            d.join("manifest.json"),
            json!({"start_ms": t0, "hours": 1.0, "data_dir": d.join("data"),
                   "stations": [{"name": "eve", "port": 8302}], "required_faults": ["clock_step"]})
            .to_string(),
        )
        .unwrap();
        std::fs::write(d.join("watch.csv"), format!("{WATCH_HEADER}\n")).unwrap();
        std::fs::write(
            d.join("faults.log"),
            [
                json!({"t_ms": inj, "kind": "clock_step", "station": "eve", "phase": "inject"}),
                json!({"t_ms": inj + 600_100, "kind": "clock_step", "station": "eve", "phase": "recovered", "recovery_s": 0.1}),
            ]
            .iter()
            .map(|j| j.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        )
        .unwrap();
        let gap = |at: i64| json!({"type": "gap", "at_ms": at, "ms": 470}).to_string();
        // Stamped while stepped (real inj + 49 s): inside the fault. Real inj + 300 s (after the step back):
        // not covered, and with no state.db it cannot be explained.
        std::fs::write(
            d.join("loopcheck-eve.jsonl"),
            [gap(inj + 600_000 + 49_000), gap(inj + 300_000)].join("\n"),
        )
        .unwrap();
        let _ = report_inner(args(&d)).unwrap();
        let j: Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("report.json")).unwrap()).unwrap();
        let c = j["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"].as_str().unwrap().starts_with("loopcheck"))
            .unwrap()
            .clone();
        assert_eq!(
            c["detail"].as_str().unwrap(),
            "eve: 1 unexplained of 2",
            "{c}"
        );
        let md = std::fs::read_to_string(d.join("report.md")).unwrap();
        assert!(md.contains(&crate::hwsoak_cmd::iso(inj + 300_000)), "{md}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn findings_are_grouped_by_reason_and_place() {
        let (k, p) = finding_key("`Junk` (item 33) is audible at 18.1 s into its file");
        assert_eq!(k, "`Junk` (item 33) is audible at # s into its file");
        assert_eq!(p, Some(18.1));
        let (k, p) = finding_key("no item on air from 1.23 s to 1.50 s of the run");
        assert_eq!(k, "no item on air from # s to # s of the run");
        assert_eq!(p, None);
        let mut g = vec![];
        let key = "eve: gap, `Junk` (item 33) is audible at # s into its file";
        for (i, pos) in [18.10, 18.04, 18.15, 29.8, 18.12].into_iter().enumerate() {
            FindingGroup::add(
                &mut g,
                key,
                Some(pos),
                70 + i as i64,
                1_000 * i as i64,
                Some(-62.0),
            );
        }
        let lines = FindingGroup::render(&g);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].contains("audible at 18.1 s") && lines[0].contains("\u{d7}4"),
            "{}",
            lines[0]
        );
        assert!(lines[0].contains("70 to 74 ms"), "{}", lines[0]);
        assert!(
            lines[1].contains("audible at 29.8 s") && !lines[1].contains('\u{d7}'),
            "{}",
            lines[1]
        );
        // A hundred and more different findings are not cut off at 30: all are listed up to 100 lines and the
        // rest counted.
        let mut many = vec![];
        for i in 0..150 {
            FindingGroup::add(&mut many, &format!("eve: gap, item {i}"), None, 50, i, None);
        }
        let lines = FindingGroup::render(&many);
        assert_eq!(lines.len(), 101);
        assert!(
            lines[100].contains("50 more groups (50 occurrences)"),
            "{}",
            lines[100]
        );
    }

    /// Soak 2 (2026-10-07): five "Mean Street" gaps, stamped while the wall clock was stepped 600 s ahead, were
    /// judged against the play that really ran at the stamped time (a loud one), and the step's fault window
    /// excused the real minutes after the step back. Loopcheck's capture time `t_s` says which stamps were
    /// stepped; those gaps are judged at their real time. The six stepped gaps are the soak's own lines
    /// (`tests/fixtures/soak2-stepped-gaps.jsonl`); the station, items and media are small stand-ins whose quiet
    /// stretches sit where Mean Street's were (114.0 s into the file and after it), under a loud "Boys Toys"
    /// that plays at the stamped time.
    #[test]
    fn gaps_stamped_while_the_clock_was_stepped_are_judged_at_their_real_time() {
        use engine::wav::{WavFormat, WavWriter};
        use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
        let d = std::env::temp_dir().join(format!("hotclock-report-st-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let fixture = include_str!("../tests/fixtures/soak2-stepped-gaps.jsonl");
        let lines: Vec<Value> = fixture
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let gaps: Vec<&Value> = lines.iter().filter(|j| j["type"] == "gap").collect();
        assert_eq!(gaps.len(), 6);
        let offset_ms = 598_216i64; // the step as the soak measured it
        let start_at = lines[0]["at_ms"].as_i64().unwrap();
        let real = |g: &Value| g["at_ms"].as_i64().unwrap() - offset_ms;
        let t_play = real(gaps[0]) - 114_000; // "Mean Street" begins so that the first gap is at 114.0 s
                                              // Media: 8 kHz mono, 125 s. "Mean Street" is quiet around each gap; "Boys Toys" never.
        let wav = |name: &str, hz: f32, quiet: &[(f64, f64)]| {
            let rate = 8_000usize;
            let mut w = WavWriter::create(&d.join(name), rate as u32, 1, WavFormat::F32).unwrap();
            let mut b = Vec::new();
            for i in 0..125 * rate {
                let t = i as f64 / rate as f64;
                let x = if quiet.iter().any(|q| t >= q.0 && t < q.1) {
                    0.0
                } else {
                    0.1 * (i as f32 * hz).sin()
                };
                b.extend_from_slice(&x.to_le_bytes());
            }
            w.write_bytes(&b).unwrap();
            w.finish().unwrap();
        };
        let quiet: Vec<(f64, f64)> = gaps
            .iter()
            .map(|g| {
                let pos = (real(g) - t_play) as f64 / 1000.0;
                (pos - 0.1, pos + g["ms"].as_f64().unwrap() / 1000.0 + 0.1)
            })
            .collect();
        wav("mean.wav", 0.3, &quiet);
        wav("boys.wav", 0.4, &[]);
        wav("junk.wav", 0.5, &[]);
        let lib = Library::open(Paths::from_data_dir(&d.join("data"))).unwrap();
        let imp = |n: &str| match import_file(
            &lib,
            &d.join(n),
            &ImportOptions {
                category: Some("music".into()),
                source: "t".into(),
            },
        ) {
            ImportOutcome::Done { item_id, .. } => item_id,
            o => panic!("{o:?}"),
        };
        let (mean, boys, junk) = (imp("mean.wav"), imp("boys.wav"), imp("junk.wav"));
        drop(lib);
        let sdir = d.join("data/stations/eve");
        std::fs::create_dir_all(&sdir).unwrap();
        let c = rusqlite::Connection::open(sdir.join("state.db")).unwrap();
        c.execute_batch(
            "CREATE TABLE as_run (id INTEGER PRIMARY KEY, item_id INTEGER, title TEXT, origin TEXT, started_ms INTEGER, ended_ms INTEGER, played_ms INTEGER, flags TEXT);
             CREATE TABLE track_cache (item_id INTEGER PRIMARY KEY, markers TEXT);",
        )
        .unwrap();
        // The quiet "Mean Street" play really ran at t_play; the loud one runs at the STAMPED time of the gaps.
        for (id, item, title, s) in [
            (1, mean, "Mean Street", t_play),
            (2, boys, "Boys Toys 1", real(gaps[0]) + offset_ms - 30_000),
        ] {
            c.execute(
                "INSERT INTO as_run VALUES (?1, ?2, ?3, 'rotation', ?4, ?5, 125000, '[\"bound_in_ms=0\"]')",
                rusqlite::params![id, item, title, s, s + 125_000],
            )
            .unwrap();
        }
        drop(c);
        let inj = real(gaps[0]) - 20_000;
        std::fs::write(
            d.join("manifest.json"),
            json!({"start_ms": start_at, "hours": 72.0, "data_dir": d.join("data"),
                   "stations": [{"name": "eve", "port": 8302}], "required_faults": ["clock_step"]})
            .to_string(),
        )
        .unwrap();
        std::fs::write(d.join("watch.csv"), format!("{WATCH_HEADER}\n")).unwrap();
        std::fs::write(
            d.join("faults.log"),
            [
                json!({"t_ms": inj, "kind": "clock_step", "station": "eve", "phase": "inject"}),
                json!({"t_ms": inj + 600_400, "kind": "clock_step", "station": "eve", "phase": "recovered", "recovery_s": 0.4}),
            ]
            .iter()
            .map(|j| j.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        )
        .unwrap();
        // The soak's lines, then three plays of a loud "Junk" with a gap at the same place each time (a group
        // of three) and one at another place; their stamps are real (capture time and stamp agree).
        let mut lc: Vec<String> = fixture.lines().map(str::to_string).collect();
        let c = rusqlite::Connection::open(sdir.join("state.db")).unwrap();
        let tj = start_at + 3_600_000;
        for (k, (off, pos)) in [
            (0, 18.10),
            (60_000, 18.12),
            (120_000, 18.05),
            (180_000, 29.8),
        ]
        .into_iter()
        .enumerate()
        {
            let s = tj + off;
            c.execute(
                "INSERT INTO as_run VALUES (?1, ?2, 'Junk', 'rotation', ?3, ?4, 40000, '[\"bound_in_ms=0\"]')",
                rusqlite::params![10 + k as i64, junk, s, s + 40_000],
            )
            .unwrap();
            let at = s + (pos * 1000.0) as i64;
            lc.push(json!({"type": "gap", "at_ms": at, "ms": 70, "level_db": -62.5, "t_s": (at - start_at) as f64 / 1000.0}).to_string());
        }
        drop(c);
        std::fs::write(d.join("loopcheck-eve.jsonl"), lc.join("\n")).unwrap();
        let _ = report_inner(args(&d)).unwrap();
        let j: Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("report.json")).unwrap()).unwrap();
        let c = j["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"].as_str().unwrap().starts_with("loopcheck"))
            .unwrap()
            .clone();
        // The six stepped gaps are all explained at their real time; the four of "Junk" are the findings.
        assert_eq!(
            c["detail"].as_str().unwrap(),
            "eve: 4 unexplained of 10",
            "{c}"
        );
        let md = std::fs::read_to_string(d.join("report.md")).unwrap();
        assert!(
            md.contains("6 gap(s) were stamped while the wall clock was stepped"),
            "{md}"
        );
        assert!(
            md.contains("`Junk`") && md.contains("audible at 18.1 s into its file \u{d7}3"),
            "{md}"
        );
        assert!(!md.contains("Boys Toys"), "{md}");
        assert_eq!(md.matches("- eve: gap").count(), 2, "{md}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// `--data-dir` replaces the manifest's `data_dir` for the station databases (a soak judged on another
    /// machine from copies).
    #[test]
    fn data_dir_overrides_the_manifest() {
        let d = std::env::temp_dir().join(format!("hotclock-report-dd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let t0 = 1_800_000_000_000i64;
        std::fs::write(
            d.join("manifest.json"),
            json!({"start_ms": t0, "hours": 1.0, "data_dir": "/nonexistent/hotclock",
                   "stations": [{"name": "office", "port": 8301}]})
            .to_string(),
        )
        .unwrap();
        std::fs::write(d.join("watch.csv"), format!("{WATCH_HEADER}\n")).unwrap();
        let mut a = args(&d);
        a.data_dir = Some(d.join("copies"));
        let _ = report_inner(a).unwrap();
        let md = std::fs::read_to_string(d.join("report.md")).unwrap();
        assert!(
            md.contains(&d.join("copies").join("stations").display().to_string()),
            "{md}"
        );
        assert!(!md.contains("/nonexistent/hotclock"), "{md}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_soak_directory_is_judged() {
        let d = std::env::temp_dir().join(format!("hotclock-report-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let t0 = 1_800_000_000_000i64;
        std::fs::write(
            d.join("manifest.json"),
            json!({"start_ms": t0, "hours": 4.0, "data_dir": d.join("data"), "stations": [{"name": "office", "port": 8301}]}).to_string(),
        )
        .unwrap();
        // Four hours of samples every minute: flat memory, 3 % CPU, no xruns.
        let mut w = String::from(WATCH_HEADER);
        w.push('\n');
        for i in 0..=240i64 {
            w.push_str(&format!(
                "{},office,100,{},20,30,{:.2},200,ok,0,{},0,0,ok,50\n",
                t0 + i * 60_000,
                200_000 + (i % 3) * 100,
                i as f64 * 60.0 * 0.03,
                i * 2_880_000
            ));
        }
        std::fs::write(d.join("watch.csv"), w).unwrap();
        std::fs::write(
            d.join("faults.log"),
            [
                json!({"t_ms": t0 + 3_600_000, "kind": "unplug", "station": "office", "phase": "inject"}),
                json!({"t_ms": t0 + 3_660_000, "kind": "unplug", "station": "office", "phase": "recovered", "recovery_s": 4.5}),
                json!({"t_ms": t0 + 7_200_000, "kind": "kill9", "station": "office", "phase": "inject"}),
                json!({"t_ms": t0 + 7_203_000, "kind": "kill9", "station": "office", "phase": "recovered", "recovery_s": 3.0}),
                json!({"t_ms": t0 + 9_000_000, "kind": "clock_step", "station": "office", "phase": "inject"}),
                json!({"t_ms": t0 + 9_001_000, "kind": "clock_step", "station": "office", "phase": "recovered", "recovery_s": 0.5}),
            ]
            .iter()
            .map(|j| j.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        )
        .unwrap();
        // No state.db and no loopcheck file: those checks fail, the others pass.
        let ok = report_inner(args(&d)).unwrap();
        assert!(!ok);
        let j: Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("report.json")).unwrap()).unwrap();
        let by = |n: &str| {
            j["checks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"].as_str().unwrap().starts_with(n))
                .unwrap()["pass"]
                .as_bool()
                .unwrap()
        };
        assert!(by("memory high-water"), "{j}");
        assert!(by("CPU under 10"), "{j}");
        assert!(by("zero xruns"), "{j}");
        assert!(by("fault injections"), "{j}");
        assert!(by("the soak ran"), "{j}");
        assert!(!by("loopcheck"), "{j}");
        assert!(!by("as-run"), "{j}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The target box with its onboard card only (2026-09-30): the unplug is deferred, the manifest requires kill -9 and
    /// the clock step. Without an unplug the fault line passes, and the deferred parts are listed, not failed.
    #[test]
    fn a_one_card_box_defers_the_unplug_and_passes_without_it() {
        let d = std::env::temp_dir().join(format!("hotclock-report3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let t0 = 1_800_000_000_000i64;
        std::fs::write(
            d.join("manifest.json"),
            json!({"start_ms": t0, "hours": 4.0, "data_dir": d.join("data"),
                   "stations": [{"name": "eve", "port": 8302}],
                   "required_faults": ["kill9", "clock_step"],
                   "deferred": ["USB dongle unplug: until radiobox is replaced"]})
            .to_string(),
        )
        .unwrap();
        let mut w = String::from(WATCH_HEADER);
        w.push('\n');
        for i in 0..=240i64 {
            w.push_str(&format!(
                "{},eve,100,200000,20,30,{:.2},200,ok,0,{},0,0,ok,50\n",
                t0 + i * 60_000,
                i as f64 * 60.0 * 0.03,
                i * 2_880_000
            ));
        }
        std::fs::write(d.join("watch.csv"), w).unwrap();
        std::fs::write(
            d.join("faults.log"),
            [
                json!({"t_ms": t0 + 7_200_000, "kind": "kill9", "station": "eve", "phase": "inject"}),
                json!({"t_ms": t0 + 7_203_000, "kind": "kill9", "station": "eve", "phase": "recovered", "recovery_s": 3.0}),
                json!({"t_ms": t0 + 9_000_000, "kind": "clock_step", "station": "eve", "phase": "inject"}),
                json!({"t_ms": t0 + 9_001_000, "kind": "clock_step", "station": "eve", "phase": "recovered", "recovery_s": 0.5}),
            ]
            .iter()
            .map(|j| j.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        )
        .unwrap();
        let _ = report_inner(args(&d)).unwrap();
        let j: Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("report.json")).unwrap()).unwrap();
        let c = j["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"].as_str().unwrap().starts_with("fault injections"))
            .unwrap()
            .clone();
        assert!(c["pass"].as_bool().unwrap(), "{c}");
        assert!(!c["name"].as_str().unwrap().contains("unplug"), "{c}");
        let md = std::fs::read_to_string(d.join("report.md")).unwrap();
        assert!(
            md.contains("## Deferred") && md.contains("radiobox"),
            "{md}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_leak_and_a_busy_station_fail() {
        let d = std::env::temp_dir().join(format!("hotclock-report2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let t0 = 1_800_000_000_000i64;
        std::fs::write(
            d.join("manifest.json"),
            json!({"start_ms": t0, "hours": 5.0, "data_dir": d.join("data"), "stations": [{"name": "office", "port": 8301}]}).to_string(),
        )
        .unwrap();
        let mut w = String::from(WATCH_HEADER);
        w.push('\n');
        for i in 0..=300i64 {
            // +10 percent of the 2-hour size over the next 3 hours; 15 % CPU.
            let rss = 200_000.0 * (1.0 + 0.10 * ((i - 120).max(0) as f64 / 180.0));
            w.push_str(&format!(
                "{},office,100,{rss:.0},20,30,{:.2},200,ok,0,0,0,0,ok,50\n",
                t0 + i * 60_000,
                i as f64 * 60.0 * 0.15
            ));
        }
        std::fs::write(d.join("watch.csv"), w).unwrap();
        let _ = report_inner(args(&d)).unwrap();
        let j: Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("report.json")).unwrap()).unwrap();
        let by = |n: &str| {
            j["checks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"].as_str().unwrap().starts_with(n))
                .unwrap()["pass"]
                .as_bool()
                .unwrap()
        };
        assert!(!by("memory high-water"), "{j}");
        assert!(!by("CPU under 10"), "{j}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// `hours` of one-minute samples of one process (`target`), anonymous memory from `anon_kb(hour)`.
    fn memory_soak(tag: &str, hours: i64, anon_kb: impl Fn(f64) -> f64) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("hotclock-report-mem-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let t0 = 1_800_000_000_000i64;
        std::fs::write(
            d.join("manifest.json"),
            json!({"start_ms": t0, "hours": hours, "data_dir": d.join("data"),
                   "stations": [{"name": "eve", "port": 8302}], "required_faults": []})
            .to_string(),
        )
        .unwrap();
        let mut w = String::from(WATCH_HEADER);
        w.push('\n');
        for i in 0..=hours * 60 {
            let a = anon_kb(i as f64 / 60.0);
            w.push_str(&format!(
                "{},eve,100,{:.0},11,14,{:.2},200,ok,0,0,0,0,ok,50,{a:.0}\n",
                t0 + i * 60_000,
                a + 20_000.0,
                i as f64 * 60.0 * 0.03
            ));
        }
        std::fs::write(d.join("watch.csv"), w).unwrap();
        d
    }

    fn check(d: &Path, prefix: &str) -> Value {
        let j: Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("report.json")).unwrap()).unwrap();
        j["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"].as_str().unwrap().starts_with(prefix))
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// The first Box A soak's pattern: anonymous memory steps up to a high-water mark early and stays there.
    /// The old line (end against hour 2) read +8 %; the restated line passes it.
    #[test]
    fn a_high_water_mark_reached_early_passes() {
        let d = memory_soak("hw", 40, |h| if h < 6.0 { 100_000.0 } else { 108_000.0 });
        let _ = report_inner(args(&d)).unwrap();
        let c = check(&d, "memory high-water");
        assert!(c["pass"].as_bool().unwrap(), "{c}");
        let md = std::fs::read_to_string(d.join("report.md")).unwrap();
        assert!(
            md.contains("+8.00 %"),
            "the old figure is still shown: {md}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A process whose high-water mark keeps creeping (0.5 MB an hour) fails.
    #[test]
    fn a_creeping_high_water_mark_fails() {
        let d = memory_soak("creep", 40, |h| 100_000.0 + 512.0 * h);
        let _ = report_inner(args(&d)).unwrap();
        let c = check(&d, "memory high-water");
        assert!(!c["pass"].as_bool().unwrap(), "{c}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The library: +0.7 MB on a 2.3 MB base is +30 % but under the 2 MB floor.
    #[test]
    fn a_small_rise_on_a_small_process_passes() {
        let d = memory_soak("small", 72, |h| 2_300.0 + 700.0 * (h / 72.0));
        let _ = report_inner(args(&d)).unwrap();
        assert!(check(&d, "memory high-water")["pass"].as_bool().unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn heap_log(d: &Path, hours: f64, kb: impl Fn(f64) -> f64, step_at_h: Option<f64>) {
        let t0 = 1_800_000_000_000i64;
        let mut s = String::from("# unix_ms pid live_heap_kb sqlite_kb\n");
        let n = (hours * 120.0) as i64;
        for i in 0..=n {
            let h = i as f64 / 120.0;
            // A +600 s clock step for two minutes, as soak-hardware.sh injects it.
            let jump = step_at_h.is_some_and(|s| h >= s && h < s + 2.0 / 60.0);
            let ts = t0 + i * 30_000 + if jump { 600_000 } else { 0 };
            s.push_str(&format!("{ts} 4242 {:.0} 1500\n", kb(h)));
        }
        std::fs::write(d.join("live-heap-eve.log"), s).unwrap();
    }

    /// The live heap: flat (with noise and a clock step) passes; 2 MB a day fails; a log the manifest asks for and
    /// that is missing fails.
    #[test]
    fn the_live_heap_slope_is_judged() {
        let d = memory_soak("heap", 40, |_| 100_000.0);
        heap_log(
            &d,
            40.0,
            |h| {
                50_000.0
                    + if (h * 7.0) as i64 % 2 == 0 {
                        800.0
                    } else {
                        0.0
                    }
            },
            Some(20.0),
        );
        let _ = report_inner(args(&d)).unwrap();
        let c = check(&d, "live heap");
        assert!(c["pass"].as_bool().unwrap(), "{c}");
        heap_log(&d, 40.0, |h| 50_000.0 + 2048.0 * h / 24.0, None);
        let _ = report_inner(args(&d)).unwrap();
        let c = check(&d, "live heap");
        assert!(!c["pass"].as_bool().unwrap(), "{c}");
        assert!(
            c["detail"].as_str().unwrap().contains("+2048 kB/day"),
            "{c}"
        );
        std::fs::remove_file(d.join("live-heap-eve.log")).unwrap();
        let mut m: Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("manifest.json")).unwrap())
                .unwrap();
        m["live_heap"] = json!({"eve": "/var/lib/hotclock/stations/eve/live-heap.log"});
        std::fs::write(d.join("manifest.json"), m.to_string()).unwrap();
        let _ = report_inner(args(&d)).unwrap();
        assert!(!check(&d, "live heap")["pass"].as_bool().unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The first Box A soak's rows 2040 and 2041: one stamp each taken while the clock was 599,352 ms ahead.
    #[test]
    fn rows_across_a_clock_step_are_repaired_from_played_ms() {
        let step = 1_800_000_000_000i64;
        let off = 599_352;
        let rows = [
            // Started before the step, ended while stepped.
            (
                2040,
                step - 100_000,
                Some(step + 95_800 + off),
                Some(195_800),
            ),
            // Started while stepped, ended after the step back.
            (
                2041,
                step + 95_800 + off,
                Some(step + 95_800 + 194_826),
                Some(194_826),
            ),
            // A normal row and an unended one.
            (2042, step + 300_000, Some(step + 480_000), Some(180_000)),
            (2043, step + 480_000, None, None),
        ];
        let (v, n) = clock_step_repairs(&rows, &Default::default(), &[step]);
        assert_eq!(n, 2);
        assert_eq!(v.end_shift.get(&2040), Some(&off));
        assert_eq!(v.start_shift.get(&2041), Some(&-off));
        assert!(!v.start_shift.contains_key(&2042) && !v.end_shift.contains_key(&2042));
        assert_eq!(v.widen_ms, off);
        let (_, none) = clock_step_repairs(&rows, &Default::default(), &[]);
        assert_eq!(none, 0);
    }

    /// A row flagged by the station is repaired with no step times known (a real clock jump, not an injection).
    #[test]
    fn a_row_flagged_clock_jump_is_repaired_without_step_times() {
        let t = 1_800_000_000_000i64;
        let rows = [
            (7, t, Some(t + 180_000 + 600_000), Some(180_000)),
            (8, t, Some(t + 181_000 + 600_000), Some(180_000)),
            (9, t, Some(t + 180_500), Some(180_000)),
        ];
        let flagged: std::collections::HashSet<i64> = [7].into_iter().collect();
        let (v, n) = clock_step_repairs(&rows, &flagged, &[]);
        assert_eq!(n, 1);
        assert_eq!(v.end_shift.get(&7), Some(&600_000));
        assert!(
            !v.end_shift.contains_key(&8),
            "unflagged and outside any step window: left alone"
        );
        assert!(!v.end_shift.contains_key(&9));
    }
}
