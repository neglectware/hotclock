//! M7 acceptance tests that run the display against real stations and a real library service (processes and
//! sockets, as in M1.7), with a recording output in place of the KMS screen: the screen's content is read from
//! the display's own log of what it drew, and the panel's presence is a switch the test flips.

mod support_api;

use common::config::DisplayConfig;
use display::app::{App, Config, Introspect, OutputFactory, StationSpec};
use display::canvas::{theme, Canvas};
use display::layout::Rect;
use display::output::Output;
use display::scene::Shown;
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

#[test]
fn live_stations_show_meters_and_names_and_a_stopped_station_says_no_connection_within_5_s() {
    let (mut e, ita_port, ita_idx) = two_stations("m7live");
    let d = launch(
        &[("office", e.st_port), ("eve", ita_port)],
        e.lib_port,
        None,
    );
    // single view: one station, the other one's indicator in the header (acceptance 1)
    d.wait("both stations connected and drawn", 20.0, |s| {
        s.frames > 5 && s.connected.iter().all(|c| c.1) && s.output_up
    });
    assert_eq!(d.snap().view, "single");
    assert!(
        d.has_run("header", "office") && d.has_run("header", "eve"),
        "{:?}",
        d.snap().runs
    );
    assert!(d.has_run("header", "PLAYING"));
    // meters: the tones are at -20 dBFS peak; the bars of the station shown light near -20 dB
    d.wait("a meter frame on the bars", 10.0, |s| {
        s.bars
            .iter()
            .any(|b| b.1.is_some_and(|v| v > -25.0 && v < -15.0))
    });
    // a second view of the same box: split, both halves drawn and updating at 30 Hz
    send(&e.lb(), "PUT", "/api/v1/display", &json!({"view": "split"}));
    d.wait("split", 2.0, |s| {
        matches!(s.shown, Some(Shown::Split { .. }))
    });
    d.wait("both halves", 5.0, |s| {
        s.runs.iter().any(|r| r.0 == "L.header" && r.1 == "PLAYING")
            && s.runs.iter().any(|r| r.0 == "R.header" && r.1 == "PLAYING")
    });
    let f0 = d.snap().frames;
    std::thread::sleep(Duration::from_secs(2));
    let rate = (d.snap().frames - f0) as f64 / 2.0;
    assert!((24.0..=36.0).contains(&rate), "frame rate {rate}/s");
    println!(
        "display tick rate: {rate:.1} frames/s; presents so far {}",
        d.presents.load(Ordering::SeqCst)
    );
    // stop the eve station: NO CONNECTION for it within 5 s; the office half keeps updating (acceptance 2)
    kill(&mut e, ita_idx);
    let t = d.wait("NO CONNECTION on the right half", 5.0, |s| {
        s.runs
            .iter()
            .any(|r| r.0 == "R.header" && r.1 == "NO CONNECTION")
    });
    println!("NO CONNECTION shown {:?} after the station stopped", t);
    assert!(t < Duration::from_secs(5));
    assert!(d
        .snap()
        .runs
        .iter()
        .any(|r| r.0 == "R.alarms" && r.1 == "NO CONNECTION"));
    assert!(d
        .snap()
        .runs
        .iter()
        .any(|r| r.0 == "L.header" && r.1 == "PLAYING"));
    let b0 = d.snap().bars.clone();
    let remaining = |d: &Shown_| {
        d.snap()
            .runs
            .iter()
            .find(|r| r.0 == "L.now" && r.1.contains(':') && !r.1.contains("SEGUE"))
            .map(|r| r.1.clone())
    };
    let r0 = remaining(&d);
    std::thread::sleep(Duration::from_millis(2300));
    assert_ne!(
        remaining(&d),
        r0,
        "the surviving station's countdown still runs"
    );
    let _ = b0;
    // the station comes back: the half recovers
    let mut c = Command::new(env!("CARGO_BIN_EXE_hotclock"));
    c.arg("--global-config")
        .arg(&e.global)
        .args(["station", "eve", "--rt-priority", "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    e.children.push(c.spawn().unwrap());
    d.wait("eve back", 25.0, |s| {
        s.runs.iter().any(|r| r.0 == "R.header" && r.1 == "PLAYING")
    });
}

#[test]
fn views_switch_within_a_second_and_survive_restarts_of_the_display_and_the_library() {
    let (mut e, ita_port, _) = two_stations("m7views");
    let state = e.dir.join("view.json");
    let d = launch(
        &[("office", e.st_port), ("eve", ita_port)],
        e.lib_port,
        Some(state.clone()),
    );
    d.wait("up", 20.0, |s| {
        s.frames > 3 && s.library_up && s.connected.iter().all(|c| c.1)
    });
    // every switch is followed within 1 s of the PUT answering
    for (body, check) in [
        (
            json!({"view": "split"}),
            (|s: &Introspect| matches!(s.shown, Some(Shown::Split { .. })))
                as fn(&Introspect) -> bool,
        ),
        (json!({"view": "single", "station": "eve"}), |s| {
            matches!(s.shown, Some(Shown::Single { idx: 1, .. }))
                && s.runs.iter().any(|r| r.0 == "header" && r.1 == "eve")
        }),
        (json!({"view": "single", "station": "office"}), |s| {
            matches!(s.shown, Some(Shown::Single { idx: 0, .. }))
        }),
        (json!({"view": "rotate"}), |s| s.view == "rotate"),
        (json!({"view": "split"}), |s| {
            matches!(s.shown, Some(Shown::Split { .. }))
        }),
    ] {
        let t = Instant::now();
        let r = send(&e.lb(), "PUT", "/api/v1/display", &body);
        assert_eq!(r.status, 200, "{body}");
        let after_put = t.elapsed();
        let waited = d.wait(&format!("{body}"), 1.5, check);
        println!("view {body}: put took {after_put:?}, the screen followed {waited:?} later");
        assert!(
            after_put + waited < Duration::from_secs(1),
            "{body}: {:?}",
            after_put + waited
        );
    }
    // the view is saved; a restart of the display comes back with it, even with the library down (index 1: the library)
    kill(&mut e, 1);
    d.wait("library down seen", 5.0, |s| !s.library_up);
    assert!(
        matches!(d.snap().shown, Some(Shown::Split { .. })),
        "the display keeps its view when the library is down"
    );
    // a switch attempt fails visibly: the PUT cannot connect
    assert!(serve::http::client::request(
        &e.lb(),
        "PUT",
        "/api/v1/display",
        &[JSON],
        b"{\"view\":\"single\"}",
        Duration::from_secs(2)
    )
    .is_err());
    drop(d);
    let d2 = launch(
        &[("office", e.st_port), ("eve", ita_port)],
        e.lib_port,
        Some(state.clone()),
    );
    d2.wait(
        "restarted display on the saved view, library still down",
        15.0,
        |s| s.frames > 3 && matches!(s.shown, Some(Shown::Split { .. })),
    );
    // the library comes back: the view is still there, from its own database
    let mut c = Command::new(env!("CARGO_BIN_EXE_hotclock"));
    c.arg("--global-config")
        .arg(&e.global)
        .arg("library")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    e.children.push(c.spawn().unwrap());
    e.wait_up(&e.lb());
    d2.wait("library up again", 20.0, |s| s.library_up);
    assert_eq!(get(&e.lb(), "/api/v1/display").json()["view"], "split");
    assert!(matches!(d2.snap().shown, Some(Shown::Split { .. })));
}

#[test]
fn rotate_alternates_pins_on_a_critical_alert_within_a_second_and_releases() {
    let (e, ita_port, _) = two_stations("m7rotate");
    let d = launch(
        &[("office", e.st_port), ("eve", ita_port)],
        e.lib_port,
        None,
    );
    d.wait("up", 20.0, |s| {
        s.frames > 3 && s.library_up && s.connected.iter().all(|c| c.1)
    });
    send(
        &e.lb(),
        "PUT",
        "/api/v1/display",
        &json!({"view": "rotate", "rotate_s": 3}),
    );
    d.wait("rotate", 2.0, |s| s.view == "rotate");
    // the stations alternate
    let idx = |s: &Introspect| match s.shown {
        Some(Shown::Single { idx, .. }) => Some(idx),
        _ => None,
    };
    let first = idx(&d.snap()).unwrap();
    d.wait("the other station", 6.0, |s| {
        idx(s).is_some_and(|i| i != first)
    });
    d.wait("and back", 6.0, |s| idx(s) == Some(first));
    // a critical alert on the hidden station pins it within 1 s
    let hidden = 1 - first;
    let hidden_name = ["office", "eve"][hidden];
    ctl(&e, hidden_name, &["hook", "alert", "silence", "critical"]);
    let t = d.wait(
        "pinned to the alerting station",
        3.0,
        |s| matches!(s.shown, Some(Shown::Single { idx, pinned: true, .. }) if idx == hidden),
    );
    println!(
        "pinned {:?} after the alert was raised (including the station's own 0.5 s control poll)",
        t
    );
    // and stays pinned past the rotation interval
    std::thread::sleep(Duration::from_millis(3600));
    assert!(
        matches!(d.snap().shown, Some(Shown::Single { idx, pinned: true, .. }) if idx == hidden)
    );
    assert!(
        d.snap().header_bg.iter().any(|h| h.0 == "header" && h.1),
        "the header is red"
    );
    ctl(&e, hidden_name, &["hook", "clear-alert", "silence"]);
    d.wait("released", 3.0, |s| {
        matches!(s.shown, Some(Shown::Single { pinned: false, .. }))
    });
    // in single, a critical alert on the other station turns its header indicator red
    send(
        &e.lb(),
        "PUT",
        "/api/v1/display",
        &json!({"view": "single", "station": "office"}),
    );
    d.wait("single office", 2.0, |s| {
        matches!(
            s.shown,
            Some(Shown::Single {
                idx: 0,
                pinned: false,
                rotate: None,
                ..
            })
        )
    });
    assert!(
        d.snap()
            .indicators
            .iter()
            .any(|i| i.0 == "eve" && i.1 != theme::RED),
        "calm before the alert"
    );
    let t0 = Instant::now();
    ctl(&e, "eve", &["hook", "alert", "silence", "critical"]);
    let t = d.wait("the indicator turns red", 3.0, |s| {
        s.indicators
            .iter()
            .any(|i| i.0 == "eve" && i.1 == theme::RED)
    });
    println!(
        "eve's header indicator red {:?} after the ctl command returned ({:?} in all)",
        t,
        t0.elapsed()
    );
    assert!(t0.elapsed() < Duration::from_secs(1), "within 1 s");
}

#[test]
fn an_unplugged_monitor_is_noticed_and_the_display_comes_back_within_10_s_with_the_stations_untouched(
) {
    let (e, ita_port, _) = two_stations("m7plug");
    let d = launch(
        &[("office", e.st_port), ("eve", ita_port)],
        e.lib_port,
        None,
    );
    d.wait("up", 20.0, |s| s.frames > 3 && s.output_up);
    assert_eq!(d.opens.load(Ordering::SeqCst), 1);
    let health = |p: u16| get(&format!("127.0.0.1:{p}"), "/health").status;
    let t = Instant::now();
    d.plugged.store(false, Ordering::SeqCst);
    d.wait("noticed", 5.0, |s| !s.output_up);
    println!("unplug noticed after {:?}", t.elapsed());
    // the stations play on while there is no screen
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!((health(e.st_port), health(ita_port)), (200, 200));
    let t = Instant::now();
    d.plugged.store(true, Ordering::SeqCst);
    d.wait("recovered", 10.0, |s| s.output_up && s.frames > 0);
    let frames_before = d.snap().frames;
    d.wait("drawing again", 3.0, |s| s.frames > frames_before + 3);
    println!("replug recovered after {:?}", t.elapsed());
    assert!(t.elapsed() < Duration::from_secs(10));
    assert_eq!(d.opens.load(Ordering::SeqCst), 2);
}

#[test]
fn a_box_with_no_screen_logs_and_retries_without_touching_the_stations() {
    let (e, ita_port, _) = two_stations("m7noscreen");
    let d = launch(
        &[("office", e.st_port), ("eve", ita_port)],
        e.lib_port,
        None,
    );
    d.plugged.store(false, Ordering::SeqCst);
    std::thread::sleep(Duration::from_secs(5));
    assert!(!d.snap().output_up);
    assert_eq!(
        get(&format!("127.0.0.1:{}", e.st_port), "/health").status,
        200
    );
    assert!(d.presents.load(Ordering::SeqCst) < 1000);
    d.plugged.store(true, Ordering::SeqCst);
    d.wait("a screen at last", 10.0, |s| s.output_up);
}

#[test]
fn the_headless_renderer_writes_a_picture_and_a_log() {
    let dir = std::env::temp_dir().join(format!("m7png-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let png = dir.join("a.png");
    let log = dir.join("a.json");
    let st = Command::new(env!("CARGO_BIN_EXE_hotclock"))
        .args(["display", "--render-png"])
        .arg(&png)
        .args([
            "--size",
            "1024x600",
            "--view",
            "split",
            "--feed",
            "demo:both",
            "--log",
        ])
        .arg(&log)
        .status()
        .unwrap();
    assert!(st.success());
    let img = image::open(&png).unwrap().to_rgba8();
    assert_eq!(img.dimensions(), (1024, 600));
    let j: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&log).unwrap()).unwrap();
    assert_eq!(j["plan"]["min_px"], 31);
    assert!(j["glyphs"]
        .as_array()
        .unwrap()
        .iter()
        .all(|g| g["size"].as_i64().unwrap() >= 31));
    // an unknown demo and a bad size are refused with a message
    assert!(!Command::new(env!("CARGO_BIN_EXE_hotclock"))
        .args([
            "display",
            "--render-png",
            "/tmp/x.png",
            "--feed",
            "demo:nope"
        ])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success());
    assert!(!Command::new(env!("CARGO_BIN_EXE_hotclock"))
        .args(["display", "--render-png", "/tmp/x.png", "--size", "big"])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn meter_ballistics_are_settable_through_the_api_and_reach_the_display_within_a_second() {
    let (e, ita_port, _) = two_stations("m7ballistics");
    let d = launch(
        &[("office", e.st_port), ("eve", ita_port)],
        e.lib_port,
        None,
    );
    d.wait("up", 20.0, |s| {
        s.frames > 3 && s.library_up && s.connected.iter().all(|c| c.1)
    });
    assert_eq!(d.snap().ballistics, ["ppm_iec1", "ppm_iec1"]);
    let g = get(&e.lb(), "/api/v1/display/meters").json();
    assert_eq!(g["preset"], "ppm_iec1");
    assert!(
        g["inherits"].is_null(),
        "no station asked: the box default, as before"
    );
    assert_eq!(g["presets"].as_array().unwrap().len(), 5);
    for preset in ["digital_fast", "vu", "ppm_bbc", "ppm_iec1"] {
        let t = Instant::now();
        let r = send(
            &e.lb(),
            "PUT",
            "/api/v1/display/meters",
            &json!({"preset": preset}),
        );
        assert_eq!(r.status, 200, "{preset}");
        let w = d.wait(preset, 1.0, |s| s.ballistics.iter().all(|b| b == preset));
        println!(
            "meters {preset}: the display followed {:?} after the PUT began",
            t.elapsed().max(w)
        );
        assert!(t.elapsed() < Duration::from_secs(1));
    }
    // a custom setting, and the refusals
    let r = send(
        &e.lb(),
        "PUT",
        "/api/v1/display/meters",
        &json!({"release_db_s": 45.0, "attack_ms": 5.0, "hold": false}),
    );
    assert_eq!(
        (r.status, r.json()["preset"].as_str().map(str::to_string)),
        (200, Some("custom".into()))
    );
    d.wait("custom", 1.0, |s| {
        s.ballistics.iter().all(|b| b == "custom")
    });
    for bad in [
        json!({"preset": "nope"}),
        json!({"release_db_s": 0}),
        json!({"source": "rms"}),
        json!({"hold_ms": -1}),
    ] {
        assert_eq!(
            send(&e.lb(), "PUT", "/api/v1/display/meters", &bad).status,
            422,
            "{bad}"
        );
    }
    assert_eq!(
        get(&e.lb(), "/api/v1/display/meters").json()["release_db_s"],
        45.0
    );
    // saved next to the database: it survives a restart of the library
    assert!(e.data.join("display-meters.json").exists());
}

#[test]
fn meter_ballistics_are_per_station_with_a_box_default_each_station_inherits() {
    let (e, ita_port, _) = two_stations("m7perstation");
    let d = launch(
        &[("office", e.st_port), ("eve", ita_port)],
        e.lib_port,
        None,
    );
    d.wait("up", 20.0, |s| {
        s.frames > 3 && s.library_up && s.connected.iter().all(|c| c.1)
    });
    let put = |q: &str, b: serde_json::Value| {
        send(&e.lb(), "PUT", &format!("/api/v1/display/meters{q}"), &b)
    };
    // a box default, inherited by both
    assert_eq!(put("", json!({"preset": "vu"})).status, 200);
    d.wait("both vu", 1.0, |s| s.ballistics == ["vu", "vu"]);
    let g = get(&e.lb(), "/api/v1/display/meters?station=eve").json();
    assert_eq!(
        (
            g["preset"].as_str(),
            g["inherits"].as_bool(),
            g["station"].as_str()
        ),
        (Some("vu"), Some(true), Some("eve"))
    );
    // office gets its own (the query form), eve stays on the default
    let t = Instant::now();
    assert_eq!(
        put("?station=office", json!({"preset": "digital_fast"})).status,
        200
    );
    d.wait("office alone", 1.0, |s| {
        s.ballistics == ["digital_fast", "vu"]
    });
    println!(
        "per-station meters reached the display in {:?}",
        t.elapsed()
    );
    let g = get(&e.lb(), "/api/v1/display/meters?station=office").json();
    assert_eq!(
        (g["preset"].as_str(), g["inherits"].as_bool()),
        (Some("digital_fast"), Some(false))
    );
    // the body form, a field edit that starts from what eve uses now, and the box default unchanged
    let r = put("", json!({"station": "eve", "release_db_s": 40.0})).json();
    assert_eq!(
        (r["preset"].as_str(), r["inherits"].as_bool()),
        (Some("custom"), Some(false))
    );
    assert_eq!(
        get(&e.lb(), "/api/v1/display/meters").json()["preset"],
        "vu"
    );
    d.wait("eve custom", 1.0, |s| {
        s.ballistics == ["digital_fast", "custom"]
    });
    // a change of the default moves only the stations that inherit
    put("", json!({"preset": "ppm_bbc"}));
    d.wait("default moved", 1.0, |s| {
        s.ballistics == ["digital_fast", "custom"]
    });
    put("?station=eve", json!({"inherit": true}));
    d.wait("eve inherits again", 1.0, |s| {
        s.ballistics == ["digital_fast", "ppm_bbc"]
    });
    // refusals
    assert_eq!(put("?station=nobody", json!({"preset": "vu"})).status, 404);
    assert_eq!(put("?station=eve", json!({"preset": "nope"})).status, 422);
    put("?station=office", json!({"inherit": true}));
    put("", json!({"preset": "ppm_iec1"}));
}

#[test]
fn the_screen_top_bar_endpoint_has_the_version_the_cpu_use_and_the_items_from_the_settings() {
    let mut e = Env::new(
        "m7bar",
        &Opts {
            tones: 1,
            secs: 5.0,
            ..Opts::default()
        },
    );
    e.spawn_library();
    let health = get(&e.lb(), "/health").json();
    let b = get(&e.lb(), "/api/v1/display/bar").json();
    assert_eq!(b["product"], health["product"]);
    // the crate version, with the package revision when the build has one (a test build has none)
    let full = health["version"].as_str().unwrap();
    assert!(full.starts_with(b["version"].as_str().unwrap()), "{b}");
    assert_eq!(b["version"], env!("CARGO_PKG_VERSION"));
    if std::path::Path::new("/proc/stat").exists() {
        let p = b["cpu_percent"]
            .as_u64()
            .expect("a percent with /proc/stat");
        assert!(p <= 100, "{b}");
    } else {
        assert!(b["cpu_percent"].is_null(), "{b}");
    }
    assert!(
        b["network"].is_null() || b["network"] == "up" || b["network"] == "down",
        "{b}"
    );
    assert_eq!(
        b["items"],
        json!({"product": true, "clock": true, "date": true, "cpu": true, "network": false})
    );
    // a change on the Settings page reaches the bar at its next request, and says it is applied
    let r = send(
        &e.lb(),
        "PUT",
        "/api/v1/settings/global",
        &json!({"values": {"display.bar_cpu": false, "display.bar_network": true}}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.json()["changed"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["apply"] == "live" && c["applied"] == true));
    let b = get(&e.lb(), "/api/v1/display/bar").json();
    assert_eq!(b["items"]["cpu"], false);
    assert_eq!(b["items"]["network"], true);
    assert_eq!(b["items"]["date"], true);
    assert_eq!(
        send(&e.lb(), "PUT", "/api/v1/display/bar", &json!({})).status,
        405
    );
}

#[test]
fn the_native_split_bar_reads_the_library_and_follows_a_settings_change_and_the_lc_bar_shows() {
    let (e, ita_port, _) = two_stations("m7bar");
    let d = launch(
        &[("office", e.st_port), ("eve", ita_port)],
        e.lib_port,
        None,
    );
    d.wait("up", 20.0, |s| {
        s.frames > 3 && s.library_up && s.connected.iter().all(|c| c.1)
    });
    assert_eq!(
        send(&e.lb(), "PUT", "/api/v1/display", &json!({"view": "split"})).status,
        200
    );
    let bar = |s: &Introspect| -> Vec<String> {
        s.runs
            .iter()
            .filter(|r| r.0 == "bar")
            .map(|r| r.1.clone())
            .collect()
    };
    let health = get(&e.lb(), "/health").json();
    let want = format!(
        "{} {}",
        health["product"].as_str().unwrap().to_uppercase(),
        env!("CARGO_PKG_VERSION")
    );
    // the version and the CPU use from GET /api/v1/display/bar within the 2 s poll
    d.wait("the bar's items", 6.0, |s| {
        let b = bar(s);
        b.contains(&want)
            && b.iter().any(|t| t.starts_with("CPU ") && t.ends_with('%'))
            && b.iter()
                .any(|t| t.len() >= 9 && t.chars().nth(3) == Some(' '))
    });
    // hide the date on the Settings page: gone within a poll, without a restart; the rest stays
    let r = send(
        &e.lb(),
        "PUT",
        "/api/v1/settings/global",
        &json!({"values": {"display.bar_date": false, "display.bar_network": true}}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    d.wait("the date to go", 6.0, |s| {
        let b = bar(s);
        b.contains(&want) && b.len() == 3
    });
    drop(d);
    drop(e);
    // a station whose controller runs: its gain range from GET /api/v1/loudness, the bar and `LC +x.x` in single
    let mut e = Env::new(
        "m7lc",
        &Opts {
            tones: 2,
            secs: 30.0,
            extra_station: "\n[loudness]\nenabled = true\ntarget_lufs = -14.0\n".into(),
            ..Opts::default()
        },
    );
    e.spawn_station(&[]);
    e.spawn_library();
    let d = launch(&[("office", e.st_port)], e.lib_port, None);
    d.wait("the LC label and number", 20.0, |s| {
        let m: Vec<&String> = s
            .runs
            .iter()
            .filter(|r| r.0 == "meters")
            .map(|r| &r.1)
            .collect();
        m.iter().any(|t| *t == "LC")
            && m.iter().any(|t| {
                (t.starts_with('+') || t.starts_with('-')) && t.contains('.') && t.len() <= 5
            })
    });
}
