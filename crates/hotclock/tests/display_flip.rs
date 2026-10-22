//! Regression tests for the display flipping through titles: the display against real stations and a real library
//! service (processes and sockets), with a recording output in place of the KMS screen. A rotation runs through a
//! station held, resumed, restarted and skipped, and the display's own record of what it showed must never put more
//! than two titles of one station in any second.

#![allow(dead_code)]

mod support_api;

use common::config::DisplayConfig;
use display::app::{App, Config, Introspect, OutputFactory, StationSpec};
use display::canvas::Canvas;
use display::layout::Rect;
use display::output::Output;
use serde_json::json;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support_api::*;

struct MockOut {
    presents: Arc<AtomicU64>,
    plugged: Arc<AtomicBool>,
}

impl Output for MockOut {
    fn size(&self) -> (i32, i32) {
        (1024, 600)
    }
    fn present(&mut self, _c: &Canvas, _d: &[Rect]) -> Result<(), String> {
        self.presents.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn connected(&mut self) -> bool {
        self.plugged.load(Ordering::SeqCst)
    }
    fn describe(&self) -> String {
        "mock 1024x600".into()
    }
}

struct MockFactory {
    presents: Arc<AtomicU64>,
    plugged: Arc<AtomicBool>,
    opens: Arc<AtomicU64>,
}

impl OutputFactory for MockFactory {
    fn open(&mut self, _cfg: &DisplayConfig) -> Result<Box<dyn Output>, String> {
        if !self.plugged.load(Ordering::SeqCst) {
            return Err("no connected output".into());
        }
        self.opens.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(MockOut {
            presents: self.presents.clone(),
            plugged: self.plugged.clone(),
        }))
    }
}

#[allow(dead_code)]
struct Shown_ {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub intro: Arc<Mutex<Introspect>>,
    pub presents: Arc<AtomicU64>,
    pub plugged: Arc<AtomicBool>,
    pub opens: Arc<AtomicU64>,
}

impl Shown_ {
    fn snap(&self) -> Introspect {
        self.intro.lock().unwrap().clone()
    }
    fn wait(&self, what: &str, secs: f64, f: impl Fn(&Introspect) -> bool) -> Duration {
        let t = Instant::now();
        loop {
            if f(&self.snap()) {
                return t.elapsed();
            }
            assert!(
                t.elapsed().as_secs_f64() < secs,
                "timed out waiting for {what}: {:?}",
                self.snap().shown
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn has_run(&self, zone: &str, text: &str) -> bool {
        self.snap()
            .runs
            .iter()
            .any(|(z, t, _)| z == zone && t == text)
    }
}

impl Drop for Shown_ {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn launch(
    stations: &[(&str, u16)],
    lib_port: u16,
    state_file: Option<std::path::PathBuf>,
) -> Shown_ {
    let presents = Arc::new(AtomicU64::new(0));
    let plugged = Arc::new(AtomicBool::new(true));
    let opens = Arc::new(AtomicU64::new(0));
    let cfg = Config {
        display: DisplayConfig::default(),
        stations: stations
            .iter()
            .map(|(n, p)| StationSpec {
                id: (*n).into(),
                display_name: (*n).into(),
                addr: format!("127.0.0.1:{p}"),
            })
            .collect(),
        library_addr: format!("127.0.0.1:{lib_port}"),
        state_file,
        force_view: None,
        tz_min: Some(0),
        product: "Product".into(),
    };
    let factory = MockFactory {
        presents: presents.clone(),
        plugged: plugged.clone(),
        opens: opens.clone(),
    };
    let intro = Arc::new(Mutex::new(Introspect::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let (i2, s2) = (intro.clone(), stop.clone());
    let thread = std::thread::spawn(move || {
        let mut app = App::start(cfg, Box::new(factory));
        app.introspect = Some(i2);
        app.run(&s2);
    });
    Shown_ {
        stop,
        thread: Some(thread),
        intro,
        presents,
        plugged,
        opens,
    }
}

/// A second station `eve` on the same library, started as its own process.
fn add_station(e: &mut Env, name: &str) -> (u16, usize) {
    let port = free_port();
    let dir = e.dir.join("stations");
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!("name = \"{name}\"\nlisten = \"127.0.0.1:{port}\"\ndevice = \"null\"\ndefault_playlist = \"Rot\"\n"),
    )
    .unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_hotclock"));
    c.arg("--global-config")
        .arg(&e.global)
        .args(["station", name, "--rt-priority", "0"])
        .env("HOTCLOCK_TEST_HOOKS", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    e.children.push(c.spawn().expect("spawn station"));
    e.wait_up(&format!("127.0.0.1:{port}"));
    (port, e.children.len() - 1)
}

fn ctl(e: &Env, station: &str, words: &[&str]) {
    let st = Command::new(env!("CARGO_BIN_EXE_hotclock"))
        .arg("--global-config")
        .arg(&e.global)
        .args(["ctl", station])
        .args(words)
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success());
}

fn kill(e: &mut Env, idx: usize) {
    let c: &mut Child = &mut e.children[idx];
    let _ = c.kill();
    let _ = c.wait();
}

fn two_stations(name: &str) -> (Env, u16, usize) {
    let mut e = Env::new(
        name,
        &Opts {
            tones: 4,
            secs: 40.0,
            ..Opts::default()
        },
    );
    e.spawn_station(&[("HOTCLOCK_TEST_HOOKS", "1")]);
    e.spawn_library();
    let (ita_port, ita_idx) = add_station(&mut e, "eve");
    for p in [e.st_port, ita_port] {
        wait_for("a station playing", 30.0, || {
            let s = get(&format!("127.0.0.1:{p}"), "/api/v1/state").json();
            s["now"]["title"].as_str().map(|_| ())
        });
    }
    (e, ita_port, ita_idx)
}

/// The most distinct titles one station shows on air, and distinct first queue rows, in any 30 consecutive frames (one
/// second). An item changes a few times a minute; a hundred songs in a second would be far above any of that. (The
/// station shown changes at a rotation cut, which is not counted as a flip of titles.)
fn busiest_second(log: &[(u64, String, String, String)]) -> (usize, usize) {
    let mut worst = (0, 0);
    for (i, w) in log.windows(30).enumerate() {
        for st in ["office", "eve"] {
            let mut now: Vec<&str> = w
                .iter()
                .filter(|x| x.1 == st)
                .map(|x| x.2.as_str())
                .collect();
            let mut next: Vec<&str> = w
                .iter()
                .filter(|x| x.1 == st)
                .map(|x| x.3.as_str())
                .collect();
            now.sort_unstable();
            now.dedup();
            next.sort_unstable();
            next.dedup();
            let n = now.len().max(next.len());
            if n > worst.0 {
                worst = (n, i);
            }
        }
    }
    worst
}

fn sequence(label: &str, d: &Shown_) {
    let log = d.snap().title_log;
    let (n, at) = busiest_second(&log);
    println!("{label}: {} frames logged, at most {n} distinct titles or next rows of one station in any second (window at frame {at})", log.len());
    assert!(n <= 2, "{label}: {n} distinct titles or next rows of one station within one second at frame {at}: {:?}", &log[at..at + 30]);
}

#[test]
fn a_rotation_through_a_held_resumed_restarted_station_never_flips_through_titles() {
    let mut e = Env::new(
        "flip",
        &Opts {
            tones: 6,
            secs: 9.0,
            ..Opts::default()
        },
    );
    e.spawn_station(&[("HOTCLOCK_TEST_HOOKS", "1")]);
    e.spawn_library();
    let (ita_port, ita_idx) = add_station(&mut e, "eve");
    for p in [e.st_port, ita_port] {
        wait_for("a station playing", 30.0, || {
            get(&format!("127.0.0.1:{p}"), "/api/v1/state").json()["now"]["title"]
                .as_str()
                .map(|_| ())
        });
    }
    let eve = format!("127.0.0.1:{ita_port}");
    send(
        &e.lb(),
        "PUT",
        "/api/v1/display",
        &json!({"view": "rotate", "rotate_s": 3}),
    );
    let d = launch(
        &[("office", e.st_port), ("eve", ita_port)],
        e.lb().rsplit(':').next().unwrap().parse().unwrap(),
        None,
    );
    d.wait("up", 20.0, |s| {
        s.frames > 3 && s.library_up && s.connected.iter().all(|c| c.1) && s.view == "rotate"
    });
    std::thread::sleep(Duration::from_secs(12));
    sequence("rotation, both playing", &d);
    // eve held at the end of its item, silent for a while, then resumed
    post(&eve, "/api/v1/transport/hold", &json!({}));
    wait_for("eve held and past the end of its item", 40.0, || {
        let s = get(&eve, "/api/v1/state").json();
        (s["state"] == "held" && s["now"].is_null())
            .then_some(())
            .or_else(|| {
                (s["state"] == "held"
                    && s["now"]["ends_at"]
                        .as_str()
                        .and_then(serve::util::parse_time)
                        .is_some_and(|t| t < wall_ms() - 500))
                .then_some(())
            })
    });
    std::thread::sleep(Duration::from_secs(10));
    sequence("eve held", &d);
    post(&eve, "/api/v1/transport/resume", &json!({}));
    std::thread::sleep(Duration::from_secs(12));
    sequence("eve resumed", &d);
    // eve restarted (the process killed and started again)
    kill(&mut e, ita_idx);
    std::thread::sleep(Duration::from_secs(4));
    let mut c = Command::new(env!("CARGO_BIN_EXE_hotclock"));
    c.arg("--global-config")
        .arg(&e.global)
        .args(["station", "eve", "--rt-priority", "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    e.children.push(c.spawn().unwrap());
    d.wait("eve back", 30.0, |s| s.connected.iter().all(|c| c.1));
    std::thread::sleep(Duration::from_secs(14));
    sequence("eve restarted", &d);
    // a skip burst on eve: Next pressed ten times in a second
    for _ in 0..10 {
        post(&eve, "/api/v1/transport/next", &json!({}));
    }
    std::thread::sleep(Duration::from_secs(8));
    let log = d.snap().title_log;
    println!("after ten Next presses: {} frames", log.len());
    // ten skips in a second is a burst of `now` events, and the display's own record of what it showed has them all
    let heads: std::collections::BTreeSet<&str> = log.iter().map(|x| x.2.as_str()).collect();
    assert!(heads.len() >= 3, "the skips were seen");
}

fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Watch a real box (read-only: the stations' WebSockets and artwork) for a few minutes in rotate and report how often
/// each zone's text changed per second. `HOTCLOCK_WATCH=192.0.2.30 cargo test --test display_flip watch -- --ignored --nocapture`.
#[test]
#[ignore]
fn watch_a_real_box() {
    let host = std::env::var("HOTCLOCK_WATCH").expect("HOTCLOCK_WATCH=<host>");
    let secs: u64 = std::env::var("HOTCLOCK_WATCH_S")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);
    let presents = Arc::new(AtomicU64::new(0));
    let plugged = Arc::new(AtomicBool::new(true));
    let cfg = Config {
        display: DisplayConfig::default(),
        stations: vec![
            StationSpec {
                id: "office".into(),
                display_name: "office".into(),
                addr: format!("{host}:8301"),
            },
            StationSpec {
                id: "eve".into(),
                display_name: "eve".into(),
                addr: format!("{host}:8302"),
            },
        ],
        library_addr: format!("{host}:8300"),
        state_file: None,
        force_view: Some(display::view::ViewMode::Rotate),
        tz_min: Some(0),
        product: "Product".into(),
    };
    let factory = MockFactory {
        presents: presents.clone(),
        plugged,
        opens: Arc::new(AtomicU64::new(0)),
    };
    let intro = Arc::new(Mutex::new(Introspect::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let (i2, s2) = (intro.clone(), stop.clone());
    let t = std::thread::spawn(move || {
        let mut app = App::start(cfg, Box::new(factory));
        app.introspect = Some(i2);
        app.run(&s2);
    });
    let mut last: std::collections::HashMap<String, String> = Default::default();
    let mut changes: std::collections::BTreeMap<String, Vec<u64>> = Default::default();
    let t0 = Instant::now();
    while t0.elapsed().as_secs() < secs {
        std::thread::sleep(Duration::from_millis(15));
        let s = intro.lock().unwrap().clone();
        let sec = t0.elapsed().as_secs();
        let mut by_zone: std::collections::BTreeMap<String, String> = Default::default();
        for (z, text, _) in &s.runs {
            // clocks and countdowns change by design; the meter text and the clock are left out
            if text.contains(':') && text.len() <= 8 {
                continue;
            }
            by_zone
                .entry(z.clone())
                .or_default()
                .push_str(&format!("{text}|"));
        }
        for (z, v) in by_zone {
            if last.get(&z) != Some(&v) {
                changes.entry(z.clone()).or_default().push(sec);
                last.insert(z, v);
            }
        }
    }
    stop.store(true, Ordering::SeqCst);
    t.join().unwrap();
    for (z, v) in &changes {
        let mut per: std::collections::BTreeMap<u64, usize> = Default::default();
        for s in v {
            *per.entry(*s).or_default() += 1;
        }
        println!(
            "{z}: {} changes in {secs} s, at most {} in one second",
            v.len(),
            per.values().max().unwrap_or(&0)
        );
    }
    let log = intro.lock().unwrap().title_log.clone();
    println!("busiest second: {:?}", busiest_second(&log));
    let art = intro.lock().unwrap().art_log.clone();
    println!(
        "artwork changes: {}; first {:?}",
        art.len(),
        art.iter().take(8).collect::<Vec<_>>()
    );
}

/// Does the station re-plan its lookahead (and send a different `queue`) when the library database is written to?
#[test]
#[ignore]
fn queue_events_under_library_writes() {
    let mut e = Env::new(
        "flipw",
        &Opts {
            tones: 8,
            secs: 60.0,
            ..Opts::default()
        },
    );
    e.spawn_station(&[]);
    e.spawn_library();
    wait_for("playing", 30.0, || {
        get(&e.st(), "/api/v1/state").json()["now"]["title"]
            .as_str()
            .map(|_| ())
    });
    let mut ws =
        serve::ws::client::Conn::connect(&e.st(), "/api/v1/ws?topics=queue,now", &[]).unwrap();
    let (mut queues, mut heads): (usize, std::collections::BTreeSet<String>) =
        (0, Default::default());
    let t0 = Instant::now();
    let mut n = 0;
    while t0.elapsed() < Duration::from_secs(20) {
        if n < 60 && t0.elapsed().as_millis() > n * 300 {
            n += 1;
            send(
                &e.lb(),
                "PATCH",
                &format!("/api/v1/items/{}", e.items[(n as usize) % e.items.len()]),
                &json!({"title": format!("edit {n}")}),
            );
        }
        ws.set_timeout(Some(Duration::from_millis(50)));
        if let Ok(serve::ws::Message::Text(t)) = ws.recv() {
            let v: serde_json::Value = serde_json::from_str(&t).unwrap();
            if v["type"] == "queue" {
                queues += 1;
                heads.insert(
                    v["data"]["entries"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .take(3)
                        .map(|x| x["title"].as_str().unwrap_or("").to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                );
            }
        }
    }
    println!(
        "{n} library edits (PATCH item) in 20 s: {queues} queue events, {} distinct queue heads",
        heads.len()
    );
}

/// A shuffled station and one library write (the hourly digest writes `notify_log` at the top of every hour): how many
/// `queue` events follow, and how different are the queue heads?
#[test]
#[ignore]
fn queue_events_after_one_library_write_on_a_shuffled_station() {
    let mut e = Env::new(
        "flipshuf",
        &Opts {
            tones: 30,
            secs: 90.0,
            ..Opts::default()
        },
    );
    e.spawn_station(&[]);
    e.spawn_library();
    wait_for("playing", 30.0, || {
        get(&e.st(), "/api/v1/state").json()["now"]["title"]
            .as_str()
            .map(|_| ())
    });
    let pl = send(
        &e.lb(),
        "POST",
        "/api/v1/playlists",
        &json!({"name": "Shuf", "mode": "shuffle", "no_repeat_minutes": 0}),
    )
    .json();
    for i in &e.items {
        send(
            &e.lb(),
            "POST",
            &format!("/api/v1/playlists/{}/entries", pl["id"]),
            &json!({"item_id": i}),
        );
    }
    std::thread::sleep(Duration::from_secs(5));
    let r = post(
        &e.st(),
        "/api/v1/playlist/switch",
        &json!({"playlist_id": pl["id"], "how": "now"}),
    );
    println!("switch: {}", r.status);
    std::thread::sleep(Duration::from_secs(3));
    let mut ws =
        serve::ws::client::Conn::connect(&e.st(), "/api/v1/ws?topics=queue,now", &[]).unwrap();
    ws.set_timeout(Some(Duration::from_millis(100)));
    let t0 = Instant::now();
    let (mut queues, mut heads, mut wrote) = (0usize, std::collections::BTreeSet::new(), false);
    while t0.elapsed() < Duration::from_secs(25) {
        if !wrote && t0.elapsed() > Duration::from_secs(3) {
            wrote = true;
            send(
                &e.lb(),
                "PATCH",
                &format!("/api/v1/items/{}", e.items[0]),
                &json!({"title": "edited"}),
            );
        }
        if let Ok(serve::ws::Message::Text(t)) = ws.recv() {
            let v: serde_json::Value = serde_json::from_str(&t).unwrap();
            if v["type"] == "queue" {
                queues += 1;
                heads.insert(
                    v["data"]["entries"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .take(3)
                        .map(|x| x["title"].as_str().unwrap_or("").to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                );
            }
        }
    }
    println!(
        "after one library edit: {queues} queue events, {} distinct queue heads",
        heads.len()
    );
}
