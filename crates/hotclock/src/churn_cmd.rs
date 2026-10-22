//! `hotclock soak-churn` (M1.8): the clients of a long soak. A station and the library service run for hours
//! with people looking at them: a status page polling, browsers opening and closing the meter WebSocket, a
//! client that stops reading, an operator pressing `next` now and then. This command plays all of them against
//! the real processes and reports what it saw: requests, errors, the slowest `/health` answer (playout must
//! never be held up by a reader), WebSocket sessions and frames.
//!
//! It writes JSON lines to `--report` (one per minute and a summary at the end) and exits 0 when no request
//! failed and no `/health` took more than a second.

use clap::Args;
use serde_json::json;
use serve::http::client;
use serve::ws::client::Conn;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Args)]
pub struct ChurnArgs {
    /// The station's address, `127.0.0.1:8301`.
    #[arg(long)]
    station: String,
    /// The library service's address (none: not exercised).
    #[arg(long)]
    library: Option<String>,
    /// Stop after this many seconds (default: until SIGTERM).
    #[arg(long)]
    seconds: Option<f64>,
    /// Press `next` on the station about this often, seconds (0: never).
    #[arg(long, default_value_t = 900)]
    next_every: u64,
    #[arg(long)]
    report: Option<PathBuf>,
    #[arg(long, default_value_t = 1)]
    seed: u64,
}

#[derive(Default)]
struct Stats {
    requests: AtomicU64,
    failures: AtomicU64,
    slow_health: AtomicU64,
    max_health_ms: AtomicU64,
    ws_sessions: AtomicU64,
    ws_frames: AtomicU64,
    ws_stalled: AtomicU64,
    ws_errors: AtomicU64,
    commands: AtomicU64,
    last_error: Mutex<String>,
}

fn rng(state: &mut u64) -> u64 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    state.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

fn get(addr: &str, path: &str, st: &Stats) -> Option<client::Reply> {
    st.requests.fetch_add(1, Ordering::Relaxed);
    match client::request(addr, "GET", path, &[], &[], Duration::from_secs(10)) {
        Ok(r) => Some(r),
        Err(e) => {
            st.failures.fetch_add(1, Ordering::Relaxed);
            if let Ok(mut l) = st.last_error.lock() {
                *l = format!("GET {addr}{path}: {e}");
            }
            None
        }
    }
}

pub fn run(a: ChurnArgs) -> ExitCode {
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let _ = ctrlc::set_handler(move || s2.store(true, Ordering::SeqCst));
    let st = Arc::new(Stats::default());
    let t0 = Instant::now();
    let mut threads = vec![];

    // The status page: /health every second, the state and metrics now and then.
    {
        let (st, stop, addr, lib) = (
            st.clone(),
            stop.clone(),
            a.station.clone(),
            a.library.clone(),
        );
        threads.push(std::thread::spawn(move || {
            let mut n = 0u64;
            while !stop.load(Ordering::SeqCst) {
                let t = Instant::now();
                let h = get(&addr, "/health", &st);
                let ms = t.elapsed().as_millis() as u64;
                st.max_health_ms.fetch_max(ms, Ordering::Relaxed);
                if ms > 1000 {
                    st.slow_health.fetch_add(1, Ordering::Relaxed);
                }
                if h.as_ref()
                    .is_some_and(|r| r.status >= 500 && r.status != 503)
                {
                    st.failures.fetch_add(1, Ordering::Relaxed);
                }
                if n.is_multiple_of(3) {
                    get(&addr, "/api/v1/state", &st);
                }
                if n.is_multiple_of(5) {
                    get(&addr, "/metrics", &st);
                    get(&addr, "/api/v1/queue", &st);
                    get(&addr, "/api/v1/history?limit=20", &st);
                }
                if let Some(l) = &lib {
                    if n.is_multiple_of(10) {
                        get(l, "/health", &st);
                        get(l, "/api/v1/items?limit=20", &st);
                    }
                }
                n += 1;
                std::thread::sleep(Duration::from_secs(1));
            }
        }));
    }

    // Browsers opening and closing the meter feed, a client that stops reading, one that just vanishes.
    for k in 0..3u64 {
        let (st, stop, addr) = (st.clone(), stop.clone(), a.station.clone());
        let mut seed = a.seed.wrapping_mul(1_000_003).wrapping_add(k + 1) | 1;
        threads.push(std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let kind = rng(&mut seed) % 10;
                let topics = [
                    "meters",
                    "meters,now",
                    "now,queue,alerts,asrun",
                    "meters,now,queue,alerts",
                ][(rng(&mut seed) % 4) as usize];
                let path = format!("/api/v1/ws?topics={topics}");
                st.requests.fetch_add(1, Ordering::Relaxed);
                let conn = if kind == 0 {
                    Conn::connect_with(&addr, &path, &[], Some(4096))
                } else {
                    Conn::connect(&addr, &path, &[])
                };
                let mut c = match conn {
                    Ok(c) => c,
                    Err(e) => {
                        st.ws_errors.fetch_add(1, Ordering::Relaxed);
                        if let Ok(mut l) = st.last_error.lock() {
                            *l = format!("ws connect: {e}");
                        }
                        std::thread::sleep(Duration::from_secs(5));
                        continue;
                    }
                };
                st.ws_sessions.fetch_add(1, Ordering::Relaxed);
                let secs = 5 + rng(&mut seed) % 115;
                match kind {
                    // Stops reading and holds the connection: the server must cut it off, not grow.
                    0 => {
                        st.ws_stalled.fetch_add(1, Ordering::Relaxed);
                        sleep_unless_stopped(&stop, Duration::from_secs(secs));
                    }
                    // Vanishes without a close.
                    1 => {
                        sleep_unless_stopped(&stop, Duration::from_secs(secs.min(20)));
                        let _ = c.socket().shutdown(std::net::Shutdown::Both);
                    }
                    // Reads for a while and leaves.
                    _ => {
                        let until = Instant::now() + Duration::from_secs(secs);
                        c.set_timeout(Some(Duration::from_secs(5)));
                        while Instant::now() < until && !stop.load(Ordering::SeqCst) {
                            match c.recv() {
                                Ok(_) => {
                                    st.ws_frames.fetch_add(1, Ordering::Relaxed);
                                }
                                Err(e)
                                    if matches!(
                                        e.kind(),
                                        std::io::ErrorKind::WouldBlock
                                            | std::io::ErrorKind::TimedOut
                                    ) => {}
                                Err(_) => break,
                            }
                        }
                    }
                }
                drop(c);
                sleep_unless_stopped(&stop, Duration::from_secs(1 + rng(&mut seed) % 5));
            }
        }));
    }

    // The operator.
    if a.next_every > 0 {
        let (st, stop, addr) = (st.clone(), stop.clone(), a.station.clone());
        let every = a.next_every;
        threads.push(std::thread::spawn(move || loop {
            sleep_unless_stopped(&stop, Duration::from_secs(every));
            if stop.load(Ordering::SeqCst) {
                break;
            }
            st.requests.fetch_add(1, Ordering::Relaxed);
            st.commands.fetch_add(1, Ordering::Relaxed);
            let r = client::request(
                &addr,
                "POST",
                "/api/v1/transport/next",
                &[
                    ("X-Requested-With", "soak-churn"),
                    ("Content-Type", "application/json"),
                ],
                b"{}",
                Duration::from_secs(10),
            );
            match r {
                Ok(r) if r.status < 500 || r.status == 503 => {}
                Ok(r) => {
                    st.failures.fetch_add(1, Ordering::Relaxed);
                    if let Ok(mut l) = st.last_error.lock() {
                        *l = format!("POST next: {}", r.status);
                    }
                }
                Err(e) => {
                    st.failures.fetch_add(1, Ordering::Relaxed);
                    if let Ok(mut l) = st.last_error.lock() {
                        *l = format!("POST next: {e}");
                    }
                }
            }
        }));
    }

    let mut rep = a.report.as_ref().and_then(|p| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .ok()
    });
    let line = |kind: &str, st: &Stats, t0: &Instant| {
        json!({
            "type": kind, "run_s": t0.elapsed().as_secs(),
            "requests": st.requests.load(Ordering::Relaxed), "failures": st.failures.load(Ordering::Relaxed),
            "max_health_ms": st.max_health_ms.load(Ordering::Relaxed), "slow_health": st.slow_health.load(Ordering::Relaxed),
            "ws_sessions": st.ws_sessions.load(Ordering::Relaxed), "ws_frames": st.ws_frames.load(Ordering::Relaxed),
            "ws_stalled": st.ws_stalled.load(Ordering::Relaxed), "ws_errors": st.ws_errors.load(Ordering::Relaxed),
            "commands": st.commands.load(Ordering::Relaxed),
            "last_error": st.last_error.lock().map(|l| l.clone()).unwrap_or_default(),
        })
    };
    let mut last = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        if a.seconds.is_some_and(|s| t0.elapsed().as_secs_f64() >= s) {
            break;
        }
        if last.elapsed() >= Duration::from_secs(60) {
            last = Instant::now();
            if let Some(f) = rep.as_mut() {
                let _ = writeln!(f, "{}", line("status", &st, &t0));
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    stop.store(true, Ordering::SeqCst);
    for t in threads {
        let _ = t.join();
    }
    let summary = line("summary", &st, &t0);
    println!("{summary}");
    if let Some(f) = rep.as_mut() {
        let _ = writeln!(f, "{summary}");
    }
    if st.failures.load(Ordering::Relaxed) == 0 && st.slow_health.load(Ordering::Relaxed) == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn sleep_unless_stopped(stop: &AtomicBool, d: Duration) {
    let until = Instant::now() + d;
    while Instant::now() < until && !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
    }
}
