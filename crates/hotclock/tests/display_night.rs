//! The status display's night color end to end: the `display.night_*` keys in the Settings catalog, the state the
//! library service reports for the Settings page (in the machine's time zone, with the sun of its principal city), a
//! save reaching the live display at once over the `display` topic, the table the frames go out through (to the panel
//! and to `HOTCLOCK_DISPLAY_SNAPSHOT`), the Preview button, and the opt-in internet lookup with its fallback.

mod support_api;
use common::night::Lut;
use display::app::{App, Config, Introspect, OutputFactory, StationSpec};
use display::output::Output;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support_api::*;

/// What the panel received: the white of the table of each present, and whether it was the whole screen.
#[derive(Default)]
struct Seen {
    white: Option<(u8, u8, u8)>,
    full: u64,
}

struct Panel(Arc<Mutex<Seen>>);

impl Output for Panel {
    fn size(&self) -> (i32, i32) {
        (1024, 600)
    }
    fn present(
        &mut self,
        _c: &display::canvas::Canvas,
        d: &[display::layout::Rect],
        l: &Lut,
    ) -> Result<(), String> {
        let mut s = self.0.lock().unwrap();
        let w = l.map(255, 255, 255);
        if s.white != Some(w) {
            // a new table goes out on the whole screen at once
            assert!(
                d.iter().any(|r| r.w == 1024 && r.h == 600),
                "a new night table with partial damage {d:?}"
            );
            s.full += 1;
        }
        s.white = Some(w);
        Ok(())
    }
    fn connected(&mut self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "test panel".into()
    }
}

struct Plug(Arc<Mutex<Seen>>);

impl OutputFactory for Plug {
    fn open(&mut self, _c: &common::config::DisplayConfig) -> Result<Box<dyn Output>, String> {
        Ok(Box::new(Panel(self.0.clone())))
    }
}

fn night(lb: &str) -> Value {
    let r = get(lb, "/api/v1/display/night");
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()
}

fn put_global(lb: &str, values: Value) -> Value {
    let r = send(
        lb,
        "PUT",
        "/api/v1/settings/global",
        &json!({ "values": values }),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()
}

/// `HH:MM` in New York, `h` hours from now.
fn ny_hhmm(h: i64) -> String {
    let z = common::timespec::Zone::named("America/New_York").unwrap();
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        + h * 3_600_000;
    let c = z.civil(ms);
    format!("{:02}:{:02}", c.hour(), c.minute())
}

/// The machine's zone for the library (`TZ`; jiff reads it), so the test does not depend on the box's own.
fn library(e: &mut Env, extra: &[(&str, &std::ffi::OsStr)]) {
    let mut envs: Vec<(&str, &std::ffi::OsStr)> = vec![("TZ", "America/New_York".as_ref())];
    envs.extend_from_slice(extra);
    e.spawn_library_with(&envs);
}

#[test]
fn the_settings_and_the_state_the_page_shows() {
    let mut e = Env::new("display_night_state", &Opts::default());
    library(&mut e, &[]);
    let lb = e.lb();
    let s = get(&lb, "/api/v1/settings").json();
    let cat = s["catalog"]["global"].as_array().unwrap();
    let def = |k: &str| {
        cat.iter()
            .find(|d| d["key"] == k)
            .cloned()
            .unwrap_or_else(|| panic!("{k} not in the catalog"))
    };
    // the defaults on an upgrade: sun, 3400 K, no dim, 60 minutes, the time zone's city, the machine's zone
    for (k, v) in [
        ("display.night_mode", json!("sun")),
        ("display.night_kelvin", json!(3400)),
        ("display.night_dim_percent", json!(0)),
        ("display.night_transition_min", json!(60)),
        ("display.night_location", json!("timezone")),
        ("display.night_time_zone", json!("system")),
    ] {
        assert_eq!(def(k)["default"], v, "{k}");
        assert_eq!(def(k)["apply"], "live", "{k}");
        assert_eq!(s["global"]["effective"][k], v, "{k}");
    }
    assert_eq!(
        (
            &def("display.night_kelvin")["min"],
            &def("display.night_kelvin")["max"]
        ),
        (&json!(1900.0), &json!(6500.0))
    );
    assert_eq!(s["system_zone"], "America/New_York");
    assert!(s["zones"]
        .as_array()
        .unwrap()
        .iter()
        .any(|z| z == "Europe/Berlin"));
    assert!(s["display_night"]["state"]["text"].is_string());

    // the state: New York's own coordinates (40.71 N, 74.01 W from tzdata), today's sun, and what comes next
    let n = night(&lb);
    assert_eq!(n["zone"], "America/New_York");
    assert_eq!(n["place"]["from"], "time zone America/New_York");
    assert!(
        (n["place"]["lat"].as_f64().unwrap() - 40.71).abs() < 0.01,
        "{n}"
    );
    assert!(
        (n["place"]["lon"].as_f64().unwrap() + 74.01).abs() < 0.01,
        "{n}"
    );
    assert!(
        n["today"]["sunrise"].as_str().unwrap().len() == 5 && n["today"]["sunset"].is_string(),
        "{n}"
    );
    let text = n["state"]["text"].as_str().unwrap();
    assert!(
        text == "Day (6500 K)"
            || text == "Night (3400 K)"
            || text.starts_with("Warming: ")
            || text.starts_with("Cooling: "),
        "{text}"
    );
    assert!(n["next"]["text"].is_string(), "{n}");
    assert_eq!(n["error"], Value::Null);
    assert_eq!(n["whites"].as_array().unwrap().len(), 47);

    // a schedule that is in its night now; then off
    put_global(
        &lb,
        json!({"display.night_mode": "schedule", "display.night_on": ny_hhmm(-2), "display.night_off": ny_hhmm(2), "display.night_kelvin": 2700}),
    );
    let n = night(&lb);
    assert_eq!(n["state"]["text"], "Night (2700 K)", "{n}");
    assert_eq!(n["next"]["phase"], "cooling");
    put_global(&lb, json!({"display.night_mode": "off"}));
    assert_eq!(night(&lb)["state"]["text"], "Off");
    // by hand: a place with no time zone city, and a bad value is refused naming the key
    put_global(
        &lb,
        json!({"display.night_mode": "sun", "display.night_location": "manual", "display.night_latitude": 52.52, "display.night_longitude": 13.4}),
    );
    let n = night(&lb);
    assert_eq!(
        (&n["place"]["from"], &n["place"]["lat"]),
        (&json!("set by hand"), &json!(52.52))
    );
    let r = send(
        &lb,
        "PUT",
        "/api/v1/settings/global",
        &json!({"values": {"display.night_kelvin": 1200}}),
    );
    assert_eq!(r.status, 400, "{}", r.text());
    assert!(r.text().contains("display.night_kelvin"), "{}", r.text());
    let r = send(
        &lb,
        "PUT",
        "/api/v1/settings/global",
        &json!({"values": {"display.night_time_zone": "Mars/Olympus"}}),
    );
    assert_eq!(r.status, 400, "{}", r.text());
    assert!(r.text().contains("display.night_time_zone"), "{}", r.text());
}

fn wait_intro(
    i: &Arc<Mutex<Introspect>>,
    what: &str,
    secs: f64,
    f: impl Fn(&Introspect) -> bool,
) -> Introspect {
    let t = Instant::now();
    loop {
        let g = i.lock().unwrap().clone();
        if f(&g) {
            return g;
        }
        assert!(
            t.elapsed().as_secs_f64() < secs,
            "timed out waiting for {what}: night {}",
            g.night
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_save_and_the_preview_reach_the_panel_at_once_and_the_snapshot_is_what_the_panel_shows() {
    let mut e = Env::new("display_night_live", &Opts::default());
    library(&mut e, &[]);
    let lb = e.lb();
    put_global(&lb, json!({"display.night_mode": "off"}));
    let seen = Arc::new(Mutex::new(Seen::default()));
    let snap = e.dir.join("snapshot.png");
    let cfg_display = common::config::DisplayConfig {
        night_mode: "off".into(),
        ..Default::default()
    };
    let cfg = Config {
        display: cfg_display,
        // nobody listens there: the display draws its "not connected" screen, which is all this needs
        stations: vec![StationSpec {
            id: "office".into(),
            display_name: "Office".into(),
            addr: format!("127.0.0.1:{}", free_port()),
        }],
        library_addr: lb.clone(),
        state_file: Some(e.dir.join("view.json")),
        force_view: None,
        tz_min: Some(0),
        product: "Hotclock".into(),
        force_night: None,
        // `HOTCLOCK_DISPLAY_SNAPSHOT`
        snapshot: Some(snap.clone()),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let intro = Arc::new(Mutex::new(Introspect::default()));
    let (s2, i2, seen2) = (stop.clone(), intro.clone(), seen.clone());
    let th = std::thread::spawn(move || {
        let mut app = App::start(cfg, Box::new(Plug(seen2)));
        app.introspect = Some(i2);
        app.run(&s2);
    });
    let g = wait_intro(&intro, "the library and a frame", 20.0, |g| {
        g.library_up && g.frames > 3
    });
    assert_eq!(g.night, "Off");
    assert!(g.night_identity);
    assert_eq!(seen.lock().unwrap().white, Some((255, 255, 255)));

    // a save: night by the schedule, 2700 K, no transition; the panel has it within a second or two
    let t = Instant::now();
    put_global(
        &lb,
        json!({"display.night_mode": "schedule", "display.night_on": ny_hhmm(-2), "display.night_off": ny_hhmm(2), "display.night_kelvin": 2700, "display.night_transition_min": 0, "display.night_time_zone": "America/New_York"}),
    );
    wait_intro(&intro, "the night look", 5.0, |g| {
        g.night == "Night (2700 K)" && !g.night_identity
    });
    let lut = Lut::for_look(2700.0, 0.0);
    let white = lut.map(255, 255, 255);
    assert_eq!(white, (255, 177, 89));
    let t2 = Instant::now();
    while seen.lock().unwrap().white != Some(white) {
        assert!(
            t2.elapsed().as_secs() < 3,
            "the panel did not get the 2700 K table"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    eprintln!(
        "the save reached the panel in {} ms",
        t.elapsed().as_millis()
    );
    // the frame as the panel shows it: the canvas through the table, pixel for pixel
    intro.lock().unwrap().want_pixels = true;
    let g = wait_intro(&intro, "the pixels", 3.0, |g| !g.pixels.is_empty());
    assert!(
        g.pixels.chunks_exact(4).all(|p| p[2] <= 89),
        "a pixel bluer than 2700 K white"
    );
    assert!(g.pixels.chunks_exact(4).any(|p| p[2] > 0));

    // the snapshot (written every 10 s) is that frame too, not the canvas
    let t3 = Instant::now();
    loop {
        if let Ok(img) = image::open(&snap) {
            let img = img.to_rgba8();
            if img.pixels().all(|p| p[2] <= 89) {
                break;
            }
        }
        assert!(t3.elapsed().as_secs() < 15, "the snapshot is not tinted");
        std::thread::sleep(Duration::from_millis(200));
    }

    // off again: the identity, and the pass skipped
    put_global(&lb, json!({"display.night_mode": "off"}));
    wait_intro(&intro, "day again", 5.0, |g| {
        g.night == "Off" && g.night_identity
    });
    // the preview: the page's values (not saved) for 15 s, then back
    let r = post(
        &lb,
        "/api/v1/display/night/preview",
        &json!({"kelvin": 1900, "dim_percent": 30}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["seconds"], 15);
    wait_intro(&intro, "the preview", 5.0, |g| g.night == "Night (1900 K)");
    let w = Lut::for_look(1900.0, 0.3).map(255, 255, 255);
    let t4 = Instant::now();
    while seen.lock().unwrap().white != Some(w) {
        assert!(
            t4.elapsed().as_secs() < 3,
            "the panel did not get the preview"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let p0 = Instant::now();
    wait_intro(&intro, "the end of the preview", 20.0, |g| {
        g.night == "Off" && g.night_identity
    });
    let lasted = p0.elapsed().as_secs_f64();
    assert!(lasted > 11.0, "the preview ended after {lasted} s");
    let r = post(
        &lb,
        "/api/v1/display/night/preview",
        &json!({"kelvin": 1000}),
    );
    assert_eq!(r.status, 422);
    stop.store(true, Ordering::SeqCst);
    th.join().unwrap();
    // what the panel was sent: a whole screen for each new table, never partial damage (asserted in `present`)
    assert!(seen.lock().unwrap().full >= 4);
}

/// Read a request through its blank line (or to the end, an error or a full buffer). Answering before the client has
/// sent it all, then closing, makes Linux reset the connection.
fn read_request(s: &mut std::net::TcpStream) {
    let mut b = [0u8; 4096];
    let mut n = 0;
    while n < b.len() && !b[..n].windows(4).any(|w| w == b"\r\n\r\n") {
        match s.read(&mut b[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
}

/// A geolocation service on a local port: answers `body` and counts the requests.
fn geoip(body: &'static str) -> (String, Arc<AtomicU64>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/json/", l.local_addr().unwrap());
    let n = Arc::new(AtomicU64::new(0));
    let n2 = n.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut s = s;
            read_request(&mut s);
            n2.fetch_add(1, Ordering::SeqCst);
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (url, n)
}

#[test]
fn the_internet_location_is_asked_once_a_day_and_the_time_zone_stands_in_when_it_fails() {
    let (url, count) =
        geoip(r#"{"ip":"192.0.2.1","city":"London","latitude":51.5072,"longitude":-0.1276}"#);
    let mut e = Env::new("display_night_geoip", &Opts::default());
    library(&mut e, &[("HOTCLOCK_GEOIP_URL", url.as_ref())]);
    let lb = e.lb();
    // not asked while the location comes from the time zone (the default)
    for _ in 0..3 {
        night(&lb);
    }
    assert_eq!(count.load(Ordering::SeqCst), 0);
    put_global(&lb, json!({"display.night_location": "internet"}));
    let n = wait_for("the internet place", 10.0, || {
        let n = night(&lb);
        (n["place"]["from"] == "internet (London)").then_some(n)
    });
    assert_eq!(n["place"]["lat"], 51.5072);
    for _ in 0..5 {
        night(&lb);
    }
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "asked more than once a day"
    );
    let cache: Value = serde_json::from_str(
        &std::fs::read_to_string(e.data.join("display-location.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(cache["city"], "London");

    // a lookup that fails (both tries): the time zone's city, and the error on the page; no second try that day
    let mut e2 = Env::new("display_night_geoip_down", &Opts::default());
    let dead = format!("http://127.0.0.1:{}/json/", free_port());
    library(
        &mut e2,
        &[
            ("HOTCLOCK_GEOIP_URL", dead.as_ref()),
            ("HOTCLOCK_GEOIP_RETRY_MS", "100".as_ref()),
        ],
    );
    let lb2 = e2.lb();
    put_global(&lb2, json!({"display.night_location": "internet"}));
    let n = wait_for("the failed lookup", 10.0, || {
        let n = night(&lb2);
        n["internet"]["error"].is_string().then_some(n)
    });
    assert_eq!(n["error"], Value::Null);
    assert!(
        n["place"]["from"]
            .as_str()
            .unwrap()
            .starts_with("time zone America/New_York"),
        "{n}"
    );
    let attempt = n["internet"]["attempt_ms"].clone();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(night(&lb2)["internet"]["attempt_ms"], attempt);
}

#[test]
fn a_dropped_internet_lookup_is_tried_once_more_and_a_bad_answer_is_not() {
    // the first connection is dropped without an answer, the second is answered
    let body = r#"{"city":"Paris","latitude":48.8566,"longitude":2.3522}"#;
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/json/", l.local_addr().unwrap());
    let n = Arc::new(AtomicU64::new(0));
    let n2 = n.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut s = s;
            if n2.fetch_add(1, Ordering::SeqCst) == 0 {
                continue; // dropped: the client sees a reset or an empty reply
            }
            read_request(&mut s);
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    let mut e = Env::new("display_night_geoip_retry", &Opts::default());
    library(
        &mut e,
        &[
            ("HOTCLOCK_GEOIP_URL", url.as_ref()),
            ("HOTCLOCK_GEOIP_RETRY_MS", "100".as_ref()),
        ],
    );
    let lb = e.lb();
    put_global(&lb, json!({"display.night_location": "internet"}));
    let n_ = wait_for("the internet place", 10.0, || {
        let n = night(&lb);
        (n["place"]["from"] == "internet (Paris)").then_some(n)
    });
    assert_eq!(n_["internet"]["error"], Value::Null);
    assert_eq!(n.load(Ordering::SeqCst), 2, "one try and one retry");

    // an answer that is not JSON stays a failure after one request
    let (url, count) = geoip("not json");
    let mut e2 = Env::new("display_night_geoip_bad", &Opts::default());
    library(
        &mut e2,
        &[
            ("HOTCLOCK_GEOIP_URL", url.as_ref()),
            ("HOTCLOCK_GEOIP_RETRY_MS", "100".as_ref()),
        ],
    );
    let lb2 = e2.lb();
    put_global(&lb2, json!({"display.night_location": "internet"}));
    wait_for("the failed lookup", 10.0, || {
        night(&lb2)["internet"]["error"].is_string().then_some(())
    });
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(count.load(Ordering::SeqCst), 1, "a bad answer was retried");
}

#[test]
fn render_png_draws_the_frame_as_the_panel_shows_it() {
    let d = tmpdir("display_night_png");
    let run = |args: &[&str], png: &std::path::Path| {
        let st = std::process::Command::new(env!("CARGO_BIN_EXE_hotclock"))
            .arg("--global-config")
            .arg(d.join("none.toml"))
            .args(["display", "--render-png"])
            .arg(png)
            .args(["--feed", "demo:both"])
            .args(args)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(st.success());
        image::open(png).unwrap().to_rgba8()
    };
    let day = run(&[], &d.join("day.png"));
    let k6500 = run(&["--night-kelvin", "6500"], &d.join("6500.png"));
    assert_eq!(day.as_raw(), k6500.as_raw(), "6500 K changes nothing");
    let warm = run(
        &["--night-kelvin", "3400", "--night-dim", "30"],
        &d.join("3400.png"),
    );
    let lut = Lut::for_look(3400.0, 0.3);
    for (a, b) in day.pixels().zip(warm.pixels()) {
        assert_eq!(lut.map(a[0], a[1], a[2]), (b[0], b[1], b[2]));
    }
    let st = std::process::Command::new(env!("CARGO_BIN_EXE_hotclock"))
        .args([
            "display",
            "--render-png",
            "/dev/null",
            "--night-kelvin",
            "1000",
        ])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert_eq!(st.code(), Some(2));
}
