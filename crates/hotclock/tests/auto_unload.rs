//! The automatic unload of a stopped program against real processes: a library service and a station (sockets,
//! `state.db`), the display in-process with a recording output and its `HOTCLOCK_DISPLAY_SNAPSHOT` pictures, and a
//! WebSocket client. A show stops at its own T0 + 20 s; the countdown is aged with the `unload-age` test hook
//! instead of waiting minutes. The same show is also ended by hand with End program and stop the audio, and the two
//! ends are compared: the queue, the program state, the station state and the topic messages.
//!
//! `HOTCLOCK_SCREENSHOTS=1` writes the panel's pictures to `docs/screenshots/auto-unload/` (else the test directory).

mod support_api;

use common::config::DisplayConfig;
use display::app::{App, Config, Introspect, OutputFactory, StationSpec};
use display::canvas::Canvas;
use display::layout::Rect;
use display::output::Output;
use library::playlists::{Mode, NewBlock, NewPlaylist};
use serde_json::{json, Value};
use serve::ws::{client::Conn, Message};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use support_api::*;

struct MockOut;

impl Output for MockOut {
    fn size(&self) -> (i32, i32) {
        (1024, 600)
    }
    fn present(&mut self, _c: &Canvas, _d: &[Rect], _l: &common::night::Lut) -> Result<(), String> {
        Ok(())
    }
    fn connected(&mut self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "mock 1024x600".into()
    }
}

struct MockFactory;

impl OutputFactory for MockFactory {
    fn open(&mut self, _cfg: &DisplayConfig) -> Result<Box<dyn Output>, String> {
        Ok(Box::new(MockOut))
    }
}

struct Panel {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    intro: Arc<Mutex<Introspect>>,
    png: PathBuf,
}

impl Panel {
    fn launch(st_port: u16, lib_port: u16, png: PathBuf) -> Panel {
        let display = DisplayConfig {
            night_mode: "off".into(),
            ..Default::default()
        };
        let cfg = Config {
            display,
            stations: vec![StationSpec {
                id: "office".into(),
                display_name: "office".into(),
                addr: format!("127.0.0.1:{st_port}"),
            }],
            library_addr: format!("127.0.0.1:{lib_port}"),
            state_file: None,
            force_view: Some(display::view::ViewMode::Single),
            tz_min: Some(0),
            product: "Product".into(),
            force_night: None,
            snapshot: Some(png.clone()),
        };
        let intro = Arc::new(Mutex::new(Introspect::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (i2, s2) = (intro.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let mut app = App::start(cfg, Box::new(MockFactory));
            app.introspect = Some(i2);
            app.run(&s2);
        });
        Panel {
            stop,
            thread: Some(thread),
            intro,
            png,
        }
    }

    fn runs(&self) -> Vec<(String, String)> {
        self.intro
            .lock()
            .unwrap()
            .runs
            .iter()
            .map(|(z, t, _)| (z.clone(), t.clone()))
            .collect()
    }

    fn shows(&self, text: &str) -> bool {
        self.runs().iter().any(|(_, t)| t.contains(text))
    }

    /// Wait for a snapshot written after `since` (the display writes one every 10 s) and copy it to `to`.
    fn picture(&self, since: SystemTime, to: &Path) {
        wait_for("a fresh display snapshot", 25.0, || {
            let m = std::fs::metadata(&self.png).ok()?.modified().ok()?;
            (m > since).then_some(())
        });
        std::thread::sleep(Duration::from_millis(300));
        std::fs::copy(&self.png, to).unwrap();
        println!("panel picture: {}", to.display());
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Every message of the station's topics, collected by a thread.
struct Topics {
    msgs: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
}

impl Topics {
    fn open(addr: &str) -> Topics {
        let mut ws = Conn::connect(
            addr,
            "/api/v1/ws?topics=state,now,queue,alerts,asrun,program",
            &[],
        )
        .unwrap();
        ws.set_timeout(Some(Duration::from_millis(200)));
        let msgs = Arc::new(Mutex::new(vec![]));
        let stop = Arc::new(AtomicBool::new(false));
        let (m2, s2) = (msgs.clone(), stop.clone());
        std::thread::spawn(move || {
            while !s2.load(Ordering::SeqCst) {
                if let Ok(Message::Text(t)) = ws.recv() {
                    m2.lock()
                        .unwrap()
                        .push(serde_json::from_str::<Value>(&t).unwrap());
                }
            }
        });
        Topics { msgs, stop }
    }

    fn mark(&self) -> usize {
        self.msgs.lock().unwrap().len()
    }

    fn since(&self, mark: usize) -> Vec<Value> {
        self.msgs.lock().unwrap()[mark..].to_vec()
    }
}

impl Drop for Topics {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn show_playlist(e: &Env, stop_after_ms: i64) -> i64 {
    let show = e
        .lib
        .create_playlist(&NewPlaylist {
            name: "The show".into(),
            mode: Mode::Sequential,
            no_repeat_minutes: 0,
            separation: Value::Object(Default::default()),
            station_id: None,
        })
        .unwrap();
    let rot = e
        .lib
        .playlists()
        .unwrap()
        .into_iter()
        .find(|p| p.name == "Rot")
        .unwrap()
        .id;
    for (n, k, t, p) in [
        (
            "Expected Mondays",
            "start",
            "first",
            json!({"schedule": [{"cron": "45 18 * * 1"}], "anchor": "manual", "arm_window_min": 60, "fallback_start": {"enabled": false}}),
        ),
        (
            "Music",
            "play",
            "sequence",
            json!({"source": {"playlist_id": rot}, "until": "next"}),
        ),
        (
            "Stop",
            "stop",
            "timed",
            json!({"when": {"kind": "relative", "offset_ms": stop_after_ms}, "how": "now", "fade_ms": 1000}),
        ),
    ] {
        e.lib
            .add_block_to_playlist(
                show,
                &NewBlock {
                    name: n.into(),
                    kind: k.into(),
                    trigger: t.into(),
                    priority: 100,
                    params: p,
                },
            )
            .unwrap();
    }
    show
}

fn start_show(e: &Env, show: i64) {
    wait_for("the station to know the show and start it", 20.0, || {
        let r = post(
            &e.st(),
            "/api/v1/program/start",
            &json!({"playlist_id": show, "t0": "now", "how": "now", "source": "api"}),
        );
        (r.status == 200).then_some(())
    });
    wait_for("the show playing", 10.0, || {
        let s = get(&e.st(), "/api/v1/state").json();
        (s["state"] == "playing" && s["now"]["title"].is_string()).then_some(())
    });
}

/// The station's state, queue and program with the times taken out, for comparing two ends.
fn end_state(e: &Env) -> Value {
    let mut s = get(&e.st(), "/api/v1/state").json();
    for k in ["at", "output", "alerts", "health"] {
        s.as_object_mut().unwrap().remove(k);
    }
    let mut p = get(&e.st(), "/api/v1/program/state").json();
    for k in [
        "at",
        "last_block",
        "upcoming",
        "next_block",
        "expected",
        "last_start",
    ] {
        p.as_object_mut().unwrap().remove(k);
    }
    json!({"state": s, "queue": get(&e.st(), "/api/v1/queue").json(), "program": p})
}

/// What a client was told after an end: the message types in order (without the 1 Hz ones) and the data of the
/// snapshot, queue and program messages with their times taken out.
fn told(msgs: &[Value]) -> (Vec<String>, Vec<Value>) {
    let mut types = vec![];
    let mut data = vec![];
    for m in msgs {
        let t = m["type"].as_str().unwrap_or("").to_string();
        if matches!(
            t.as_str(),
            "timing" | "loudness" | "alert" | "alert_cleared"
        ) {
            continue;
        }
        let mut d = m["data"].clone();
        if let Some(o) = d.as_object_mut() {
            for k in [
                "at",
                "output",
                "alerts",
                "health",
                "last_block",
                "upcoming",
                "next_block",
                "expected",
                "last_start",
            ] {
                o.remove(k);
            }
        }
        if t == "asrun" {
            d = json!(d["end_reason"]);
        }
        types.push(t);
        data.push(d);
    }
    (types, data)
}

fn screenshots_dir(e: &Env) -> PathBuf {
    if std::env::var_os("HOTCLOCK_SCREENSHOTS").is_some_and(|v| v == "1") {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/screenshots/auto-unload");
        std::fs::create_dir_all(&d).unwrap();
        d
    } else {
        e.dir.clone()
    }
}

#[test]
fn a_stopped_show_unloads_by_itself_like_end_program_and_stop_the_audio() {
    let mut e = Env::new(
        "auto-unload",
        &Opts {
            tones: 3,
            secs: 40.0,
            ..Opts::default()
        },
    );
    // A part-time station like eve: no default playlist, quiet when stopped. One minute before the unload.
    std::fs::write(
        e.dir.join("stations").join("office.toml"),
        format!("name = \"office\"\nlisten = \"127.0.0.1:{}\"\ndevice = \"null\"\nstopped_is_normal = true\n", e.st_port),
    )
    .unwrap();
    let mut g = std::fs::read_to_string(&e.global).unwrap();
    g.push_str("[programs]\nunload_after_min = 1\n");
    std::fs::write(&e.global, g).unwrap();
    let show = show_playlist(&e, 20_000);
    e.spawn_station(&[("HOTCLOCK_TEST_HOOKS", "1")]);
    e.spawn_library();
    let shots = screenshots_dir(&e);
    let panel = Panel::launch(e.st_port, e.lib_port, e.dir.join("panel-live.png"));
    let topics = Topics::open(&e.st());

    // ---- 1. ended by hand: End program and stop the audio, during the countdown ----
    start_show(&e, show);
    wait_for("the show's stop and a countdown", 40.0, || {
        get(&e.st(), "/api/v1/program/state").json()["unload"]["at"]
            .as_str()
            .map(|_| ())
    });
    let mark = topics.mark();
    let r = post(
        &e.st(),
        "/api/v1/program/abort",
        &json!({"stop_audio": true}),
    );
    assert!(r.status / 100 == 2, "{} {}", r.status, r.text());
    std::thread::sleep(Duration::from_secs(3));
    let manual = end_state(&e);
    let manual_told = told(&topics.since(mark));

    // ---- 2. the same show stops by itself and unloads by itself ----
    start_show(&e, show);
    let title = get(&e.st(), "/api/v1/state").json()["now"]["title"]
        .as_str()
        .unwrap()
        .to_string();
    let ps = wait_for("the show's stop and a countdown", 40.0, || {
        let p = get(&e.st(), "/api/v1/program/state").json();
        p["unload"]["at"].is_string().then_some(p)
    });
    println!("countdown: {}", ps["unload"]);
    assert_eq!(ps["unload"]["after_min"], 1);
    assert!(
        ps["unload"]["in_ms"].as_i64().unwrap() > 50_000,
        "a minute to go"
    );
    assert!(
        matches!(ps["program"]["state"].as_str(), Some("stopped" | "halted")),
        "{}",
        ps["program"]["state"]
    );
    assert_eq!(ps["program"]["playlist"], "The show");
    // before: the panel shows the stopped show's last item with STOPPED
    let t_stop = SystemTime::now();
    wait_for("the panel shows STOPPED and the last item", 10.0, || {
        (panel.shows("STOPPED") && panel.shows(&title)).then_some(())
    });
    panel.picture(t_stop, &shots.join("panel-before-stopped.png"));
    // Keep loaded is refused when nothing counts down, and the API names it
    let mark = topics.mark();
    e.ctl(&["hook", "unload-age", "58"]);
    let unloaded = Instant::now();
    wait_for("the automatic unload", 15.0, || {
        get(&e.st(), "/api/v1/program/state").json()["program"]
            .is_null()
            .then_some(())
    });
    println!(
        "unloaded {:.1} s after the hook aged the countdown to 0:58",
        unloaded.elapsed().as_secs_f64()
    );
    std::thread::sleep(Duration::from_secs(3));
    let auto = end_state(&e);
    let auto_told = told(&topics.since(mark));
    let after = SystemTime::now();
    wait_for("the panel drops the last item", 10.0, || {
        (!panel.shows(&title)).then_some(())
    });
    panel.picture(after, &shots.join("panel-after-unloaded.png"));
    println!(
        "panel after: {:?}",
        panel
            .runs()
            .iter()
            .filter(|r| r.0.starts_with("now") || r.0 == "header")
            .collect::<Vec<_>>()
    );
    assert!(panel.shows("STOPPED"));

    // ---- the two ends are the same ----
    println!("manual end: {manual}\nauto end:   {auto}");
    assert_eq!(manual, auto, "the queue, the program and the state");
    assert_eq!(
        auto["queue"]["entries"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0),
        0,
        "{}",
        auto["queue"]
    );
    assert!(auto["state"]["now"].is_null());
    assert!(auto["state"]["active_playlist"].is_null());
    assert_eq!(auto["state"]["state"], "stopped");
    assert_eq!(auto["program"]["running"], false);
    assert!(auto["program"]["unload"].is_null());
    println!(
        "manual told: {:?}\nauto told:   {:?}",
        manual_told.0, auto_told.0
    );
    assert_eq!(manual_told.0, auto_told.0, "the same topic messages");
    assert_eq!(manual_told.1, auto_told.1, "with the same content");
    assert!(
        auto_told.0.iter().any(|t| t == "snapshot"),
        "a snapshot tells the displays"
    );
    let snap = auto_told.1[auto_told.0.iter().position(|t| t == "snapshot").unwrap()].clone();
    assert!(
        snap["now"].is_null() && snap["queue"] == json!([]) && snap["state"] == "stopped",
        "{snap}"
    );

    // ---- the block log: one row each, by the operator and automatic ----
    let log = get(&e.st(), "/api/v1/blocks/log").json();
    let ends: Vec<(String, String)> = log["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["type"] == "end_program")
        .map(|r| {
            (
                r["name"].as_str().unwrap().into(),
                r["how"].as_str().unwrap().into(),
            )
        })
        .collect();
    println!("block log: {ends:?}");
    assert_eq!(
        ends,
        [
            (
                "End program and stop the audio".to_string(),
                "operator".to_string()
            ),
            (
                "End program and stop the audio".to_string(),
                "automatic".to_string()
            )
        ]
    );
    let r = post(&e.st(), "/api/v1/program/keep-loaded", &json!({}));
    assert_eq!(r.status, 409, "nothing to keep: {}", r.text());
}

#[test]
fn keep_loaded_and_the_live_setting() {
    let mut e = Env::new(
        "keep-loaded",
        &Opts {
            tones: 3,
            secs: 40.0,
            ..Opts::default()
        },
    );
    std::fs::write(
        e.dir.join("stations").join("office.toml"),
        format!("name = \"office\"\nlisten = \"127.0.0.1:{}\"\ndevice = \"null\"\nstopped_is_normal = true\n", e.st_port),
    )
    .unwrap();
    let show = show_playlist(&e, 15_000);
    e.spawn_station(&[("HOTCLOCK_TEST_HOOKS", "1")]);
    e.spawn_library();
    // the default: 5 minutes
    start_show(&e, show);
    let p = wait_for("a countdown", 40.0, || {
        let p = get(&e.st(), "/api/v1/program/state").json();
        p["unload"]["at"].is_string().then_some(p)
    });
    assert_eq!(p["unload"]["after_min"], 5, "five minutes by default");
    // Keep loaded cancels it: an aged countdown does not unload
    let r = post(&e.st(), "/api/v1/program/keep-loaded", &json!({}));
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(get(&e.st(), "/api/v1/program/state").json()["unload"].is_null());
    e.ctl(&["hook", "unload-age", "600"]);
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !get(&e.st(), "/api/v1/program/state").json()["program"].is_null(),
        "kept loaded"
    );
    // A play starts the show's queue again; Stop starts a new countdown, from the setting saved on the Settings page.
    let r = send(
        &e.lb(),
        "PUT",
        "/api/v1/settings/global",
        &json!({"values": {"programs.unload_after_min": 2}}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    println!("save: {}", r.json());
    assert_eq!(r.json()["live"]["applied"], true, "{}", r.json());
    assert!(std::fs::read_to_string(&e.global)
        .unwrap()
        .contains("unload_after_min = 2"));
    post(&e.st(), "/api/v1/transport/play", &json!({}));
    wait_for("playing", 10.0, || {
        (get(&e.st(), "/api/v1/state").json()["state"] == "playing").then_some(())
    });
    assert!(
        get(&e.st(), "/api/v1/program/state").json()["unload"].is_null(),
        "no countdown while it plays"
    );
    post(&e.st(), "/api/v1/transport/stop", &json!({"fade_ms": 500}));
    let p = wait_for("a new countdown", 10.0, || {
        let p = get(&e.st(), "/api/v1/program/state").json();
        p["unload"]["at"].is_string().then_some(p)
    });
    assert_eq!(
        p["unload"]["after_min"], 2,
        "the saved setting, without a restart"
    );
    // Never, saved live: the countdown goes, and an aged stop stays loaded
    let r = send(
        &e.lb(),
        "PUT",
        "/api/v1/settings/global",
        &json!({"values": {"programs.unload_after_min": "never"}}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(get(&e.st(), "/api/v1/program/state").json()["unload"].is_null());
    e.ctl(&["hook", "unload-age", "7200"]);
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !get(&e.st(), "/api/v1/program/state").json()["program"].is_null(),
        "Never keeps it"
    );
    // a bad value is refused with the key's name
    let r = send(
        &e.lb(),
        "PUT",
        "/api/v1/settings/global",
        &json!({"values": {"programs.unload_after_min": 0}}),
    );
    assert_eq!(r.status, 400, "{}", r.text());
    assert!(
        r.text().contains("programs.unload_after_min"),
        "{}",
        r.text()
    );
    // the Settings page's catalog has it on This box with its choices
    let s = get(&e.lb(), "/api/v1/settings").json();
    let d = s["catalog"]["global"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["key"] == "programs.unload_after_min")
        .unwrap()
        .clone();
    assert_eq!(
        d["options"],
        json!(["1", "2", "5", "10", "15", "30", "60", "never"])
    );
    assert_eq!(d["default"], 5);
    assert_eq!(d["apply"], "live");
    assert_eq!(
        s["global"]["effective"]["programs.unload_after_min"],
        "never"
    );
}
