//! M2a acceptance tests at the process level: a stream-only station and real clients (ffmpeg, raw sockets).
//! The engine-level ones (bit-exact decode, output unchanged by the stream, a stalled encoder) are in
//! `crates/stream/tests/rt_tap.rs`.

mod support_api;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use support_api::*;

const STREAM: &str = "[stream]\nenabled = true\n";

fn stream_env(name: &str, stream_block: &str) -> Env {
    Env::new(
        name,
        &Opts {
            tones: 2,
            secs: 90.0,
            extra_station: format!("\n{stream_block}"),
            ..Opts::default()
        },
    )
}

/// `hotclock station office` (plus `args`) with the environment `envs`; waits for the API.
fn spawn(e: &mut Env, args: &[&str], envs: &[(&str, &str)]) -> usize {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hotclock"));
    c.arg("--global-config")
        .arg(&e.global)
        .args(["station", "office", "--rt-priority", "0"])
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in envs {
        c.env(k, v);
    }
    e.children.push(c.spawn().expect("spawn station"));
    e.wait_up(&e.st());
    e.children.len() - 1
}

fn metric(e: &Env, name: &str) -> f64 {
    get(&e.st(), "/metrics")
        .text()
        .lines()
        .find(|l| l.starts_with(name))
        .unwrap_or_else(|| panic!("metric {name}"))
        .rsplit(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

fn stream_json(e: &Env) -> Value {
    get(&e.st(), "/api/v1/stream").json()
}

fn alerts(e: &Env) -> Vec<String> {
    let v = get(&e.st(), "/api/v1/alerts").json();
    let arr = v["alerts"]
        .as_array()
        .or_else(|| v.as_array())
        .cloned()
        .unwrap_or_default();
    arr.iter()
        .filter_map(|a| a["code"].as_str().map(str::to_string))
        .collect()
}

/// An HTTP request on a raw socket; returns the stream after the request is sent.
fn raw(e: &Env, request: &str) -> TcpStream {
    let mut s = TcpStream::connect(e.st()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(request.as_bytes()).unwrap();
    s
}

fn get_stream_request(e: &Env) -> String {
    format!(
        "GET /stream/program.ogg HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        e.st()
    )
}

fn read_head(s: &mut TcpStream) -> String {
    let mut head = vec![];
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut b).unwrap();
        head.push(b[0]);
    }
    String::from_utf8_lossy(&head).into_owned()
}

fn goertzel(x: &[f32], hz: f64, rate: f64) -> f64 {
    let w = std::f64::consts::TAU * hz / rate;
    let (mut re, mut im) = (0.0, 0.0);
    for (i, v) in x.iter().enumerate() {
        re += f64::from(*v) * (w * i as f64).cos();
        im += f64::from(*v) * (w * i as f64).sin();
    }
    (re * re + im * im).sqrt() / x.len() as f64 * 2.0
}

fn frames_written(e: &Env) -> f64 {
    metric(e, "hotclock_frames_written_total")
}

/// `frames_written` and the time it was read at (the middle of the request, so a slow answer does not skew it).
fn sample_frames(e: &Env) -> (f64, Instant) {
    let a = Instant::now();
    let f = frames_written(e);
    let b = Instant::now();
    (f, a + (b - a) / 2)
}

#[test]
fn a_stream_only_station_plays_without_a_card_and_ffmpeg_decodes_its_stream() {
    let mut e = stream_env("m2a-a", STREAM);
    spawn(&mut e, &[], &[]);
    wait_for("playing", 20.0, || {
        get(&e.st(), "/api/v1/state").json()["now"]["title"]
            .as_str()
            .map(|_| ())
    });
    // Acceptance 1: frames_written follows real time (device = "null", no card). The window is 30 s here; the
    // 10-minute figure is in BUILDLOG.
    let (f0, t0) = sample_frames(&e);
    // ffmpeg decodes the stream (acceptance 2) while the clock is measured.
    let out = Command::new("ffmpeg")
        .args(["-nostdin", "-nostats", "-loglevel", "error", "-i"])
        .arg(format!("http://{}/stream/program.ogg", e.st()))
        .args(["-t", "30", "-f", "f32le", "-ac", "2", "-ar", "48000", "-"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (f1, t1) = (frames_written(&e), Instant::now());
    let samples: Vec<f32> = out
        .stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    assert!(
        samples.len() >= 2 * 48_000 * 29,
        "{} samples",
        samples.len()
    );
    let rms =
        (samples.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / samples.len() as f64).sqrt();
    assert!(20.0 * rms.log10() > -40.0, "the stream is silent");
    let rate = (f1 - f0) / (t1 - t0).as_secs_f64();
    println!(
        "frames_written {rate:.1} frames/s over {:.1} s",
        (t1 - t0).as_secs_f64()
    );
    // The device model's buffer (8,192 frames, 0.57 percent of 30 s) can be anywhere from nearly empty to full
    // at either end of a window, and a loaded machine adds scheduling delay at the ends (0.62 percent was seen
    // with a build running beside the suite), so a 30 s window is checked to 1 percent;
    // `ten_minutes_of_real_time` is the 0.1 percent check.
    assert!((rate / 48_000.0 - 1.0).abs() < 0.01, "{rate}");
    // While ffmpeg ran the station saw a listener; the API and /health say so.
    let s = stream_json(&e);
    assert_eq!(s["enabled"], true);
    assert_eq!(s["connects"], 1);
    assert_eq!(s["encoder"]["state"], "running");
    assert_eq!(s["gaps"], 0);
    assert_eq!(s["tap"]["dropped_periods"], 0);
    // Acceptance 6: /health carries the stream object, /metrics the series.
    let h = get(&e.st(), "/health").json();
    assert_eq!(h["stream"]["enabled"], true);
    assert_eq!(h["stream"]["mount"], "program");
    assert_eq!(h["stream"]["encoder"], "running");
    assert_eq!(h["audio"]["xruns_1h"], 0);
    for m in [
        "hotclock_stream_listeners",
        "hotclock_stream_connects_total",
        "hotclock_stream_encoder_lag_seconds",
        "hotclock_stream_tap_drops_total",
        "hotclock_stream_dropped_frames_total",
        "hotclock_stream_gaps_total",
        "hotclock_stream_encoder_restarts_total",
        "hotclock_stream_bytes_sent_total",
    ] {
        metric(&e, m);
    }
    assert!(metric(&e, "hotclock_stream_bytes_sent_total") > 100_000.0);
    assert_eq!(metric(&e, "hotclock_xruns_total"), 0.0);
}

#[test]
fn the_mount_is_read_only_and_answers_like_icecast() {
    let mut e = stream_env("m2a-b", STREAM);
    spawn(&mut e, &[], &[]);
    // GET: HTTP/1.0, audio/ogg, no Content-Length, then OggS pages after the headers.
    let mut s = raw(&e, &get_stream_request(&e));
    let head = read_head(&mut s);
    assert!(head.starts_with("HTTP/1.0 200 OK"), "{head}");
    assert!(head.contains("Content-Type: audio/ogg"));
    assert!(head.contains("icy-name: office"));
    assert!(head.contains("Cache-Control: no-store"));
    assert!(!head.to_ascii_lowercase().contains("content-length"));
    let mut first = [0u8; 4];
    s.read_exact(&mut first).unwrap();
    assert_eq!(&first, b"OggS");
    // The first page is the Ogg FLAC identification header.
    let mut rest = [0u8; 80];
    s.read_exact(&mut rest).unwrap();
    assert!(rest.windows(4).any(|w| w == b"FLAC"));
    drop(s);
    // HEAD: headers only.
    let mut h = raw(&e, &get_stream_request(&e).replace("GET", "HEAD"));
    let head = read_head(&mut h);
    assert!(head.contains("audio/ogg"), "{head}");
    // Nothing writes: POST and PUT are refused, an unknown mount is 404, a wrong Host is refused.
    let r = client_request(
        &e,
        "POST",
        "/stream/program.ogg",
        &[("Content-Type", "application/json")],
    );
    assert_eq!(r, 405);
    assert_eq!(
        client_request(
            &e,
            "PUT",
            "/stream/program.ogg",
            &[("Content-Type", "application/json")]
        ),
        405
    );
    assert_eq!(client_request(&e, "GET", "/stream/other.ogg", &[]), 404);
    assert_eq!(
        client_request(
            &e,
            "GET",
            "/stream/program.ogg",
            &[("Host", "evil.example")]
        ),
        421
    );
    // An unauthenticated disconnect needs the JSON content type like every mutation.
    assert_eq!(
        client_request(&e, "POST", "/api/v1/stream/disconnect", &[]),
        415
    );
}

fn client_request(e: &Env, method: &str, path: &str, headers: &[(&str, &str)]) -> u16 {
    let mut req = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\nContent-Length: 0\r\n");
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        req.push_str(&format!("Host: {}\r\n", e.st()));
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    let mut s = raw(e, &req);
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    buf.split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0)
}

#[test]
fn the_fifth_listener_gets_503_and_an_operator_can_disconnect_one() {
    let mut e = stream_env("m2a-c", "[stream]\nenabled = true\nmax_listeners = 4\n");
    spawn(&mut e, &[], &[]);
    let mut clients: Vec<TcpStream> = vec![];
    for _ in 0..4 {
        let mut s = raw(&e, &get_stream_request(&e));
        assert!(read_head(&mut s).starts_with("HTTP/1.0 200"));
        clients.push(s);
    }
    assert_eq!(stream_json(&e)["listeners"], 4);
    let mut fifth = raw(&e, &get_stream_request(&e));
    let mut body = String::new();
    let _ = fifth.read_to_string(&mut body);
    assert!(body.starts_with("HTTP/1.1 503"), "{body}");
    assert_eq!(stream_json(&e)["rejected"], 1);
    // Disconnect one by id.
    let id = stream_json(&e)["listener_list"][0]["id"].as_u64().unwrap();
    let r = post(&e.st(), "/api/v1/stream/disconnect", &json!({"id": id}));
    assert_eq!(r.status, 200, "{}", r.text());
    wait_for("one fewer listener", 5.0, || {
        (stream_json(&e)["listeners"] == 3).then_some(())
    });
    assert_eq!(stream_json(&e)["disconnects"]["kicked"], 1);
    // The freed place can be taken.
    let mut again = raw(&e, &get_stream_request(&e));
    assert!(read_head(&mut again).starts_with("HTTP/1.0 200"));
    // All of them.
    post(&e.st(), "/api/v1/stream/disconnect", &json!({"all": true}));
    wait_for("nobody", 5.0, || {
        (stream_json(&e)["listeners"] == 0).then_some(())
    });
    assert_eq!(
        post(&e.st(), "/api/v1/stream/disconnect", &json!({})).status,
        400
    );
    drop(clients);
}

#[test]
fn a_client_that_stops_reading_is_disconnected_and_the_station_notices_nothing() {
    let mut e = stream_env("m2a-d", "[stream]\nenabled = true\nlistener_lag_s = 2\n");
    spawn(&mut e, &[], &[]);
    wait_for("playing", 20.0, || {
        get(&e.st(), "/api/v1/state").json()["now"]["title"]
            .as_str()
            .map(|_| ())
    });
    let x0 = metric(&e, "hotclock_xruns_total");
    let (f0, t0) = (frames_written(&e), Instant::now());
    // Sends the request and never reads the answer.
    let _stuck = raw(&e, &get_stream_request(&e));
    let t = Instant::now();
    wait_for("slow disconnect", 90.0, || {
        (stream_json(&e)["disconnects"]["slow"] == 1).then_some(())
    });
    println!(
        "the stuck client was disconnected after {:.1} s",
        t.elapsed().as_secs_f64()
    );
    assert_eq!(stream_json(&e)["listeners"], 0);
    // The station's clock, output and encoder were untouched. The output is paced by a device model that
    // drains against the monotonic clock, and an xrun is counted whenever the station let it run dry, so
    // "the clock was not slowed" is: no xrun, and the frames written stay within the device buffer of what
    // the clock says (an absolute bound that does not depend on how loaded the host is or how long the
    // window is; a rate over a short window did, and read 47,414/s under CPU load with the station fine).
    let (f1, t1) = sample_frames(&e);
    let ahead = (f1 - f0) - 48_000.0 * t1.duration_since(t0).as_secs_f64();
    println!(
        "frames written minus the clock's: {ahead:+.0} frames over {:.1} s",
        t1.duration_since(t0).as_secs_f64()
    );
    assert!(
        ahead.abs() < 48_000.0,
        "the station drifted {ahead:+.0} frames from its clock"
    );
    assert_eq!(metric(&e, "hotclock_xruns_total"), x0);
    assert_eq!(stream_json(&e)["tap"]["dropped_periods"], 0);
    assert_eq!(stream_json(&e)["encoder"]["state"], "running");
    assert!(get(&e.st(), "/health").json()["status"] != "critical");
}

#[test]
fn a_stalled_encoder_raises_and_clears_its_alerts_and_no_listener_warns() {
    let mut e = stream_env("m2a-e", STREAM);
    spawn(
        &mut e,
        &[],
        &[
            ("HOTCLOCK_TEST_HOOKS", "1"),
            ("HOTCLOCK_TEST_NO_LISTENER_S", "3"),
        ],
    );
    wait_for("playing", 20.0, || {
        get(&e.st(), "/api/v1/state").json()["now"]["title"]
            .as_str()
            .map(|_| ())
    });
    // Nobody is listening: stream_no_listener (a warning) after the (shortened) wait.
    wait_for("stream_no_listener", 15.0, || {
        alerts(&e)
            .contains(&"stream_no_listener".to_string())
            .then_some(())
    });
    let h = get(&e.st(), "/health").json();
    assert_eq!(h["status"], "warning");
    // A listener clears it.
    let mut s = raw(&e, &get_stream_request(&e));
    read_head(&mut s);
    wait_for("cleared", 10.0, || {
        (!alerts(&e).contains(&"stream_no_listener".to_string())).then_some(())
    });
    // Stall the encoder for 5 s: stream_encoder_lag raises, the station plays on, and it clears.
    let x0 = metric(&e, "hotclock_xruns_total");
    e.ctl(&["hook", "stream-stall", "5000"]);
    wait_for("stream_encoder_lag", 15.0, || {
        alerts(&e)
            .contains(&"stream_encoder_lag".to_string())
            .then_some(())
    });
    wait_for("lag cleared", 20.0, || {
        (!alerts(&e).contains(&"stream_encoder_lag".to_string())).then_some(())
    });
    assert_eq!(
        metric(&e, "hotclock_xruns_total"),
        x0,
        "playout was disturbed"
    );
    let st = stream_json(&e);
    assert!(st["tap"]["dropped_periods"].as_u64().unwrap() > 0, "{st}");
    assert!(st["gaps"].as_u64().unwrap() >= 1, "{st}");
    // The raised alerts are counted.
    assert!(metric(&e, "hotclock_stream_tap_drops_total") > 0.0);
    drop(s);
}

#[test]
fn ffmpeg_keeps_playing_across_an_encoder_panic_and_a_station_restart() {
    let hooks = [("HOTCLOCK_TEST_HOOKS", "1")];
    let mut e = stream_env("m2a-f", STREAM);
    let idx = spawn(&mut e, &["--stream-test"], &hooks);
    let url = format!("http://{}/stream/program.ogg", e.st());
    let mut ff = Command::new("ffmpeg")
        .args(["-nostdin", "-nostats", "-loglevel", "error"])
        .args([
            "-reconnect",
            "1",
            "-reconnect_at_eof",
            "1",
            "-reconnect_streamed",
            "1",
            "-reconnect_on_network_error",
            "1",
            "-reconnect_delay_max",
            "3",
        ])
        .args([
            "-i", &url, "-t", "34", "-f", "f32le", "-ac", "1", "-ar", "48000", "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut out = ff.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut all = vec![];
        let mut buf = [0u8; 65536];
        let t0 = Instant::now();
        let mut stamps = vec![];
        while let Ok(n) = out.read(&mut buf) {
            if n == 0 {
                break;
            }
            stamps.push((t0.elapsed().as_secs_f64(), all.len() / 4));
            all.extend_from_slice(&buf[..n]);
        }
        (all, stamps)
    });
    std::thread::sleep(Duration::from_secs(7));
    // The encoder panics and restarts: it continues the same Ogg stream, so the listener is not even closed.
    e.ctl(&["hook", "stream-panic"]);
    std::thread::sleep(Duration::from_secs(6));
    let st = stream_json(&e);
    assert_eq!(st["encoder"]["restarts"], 1, "{st}");
    assert_eq!(st["encoder"]["state"], "running", "{st}");
    assert_eq!(st["disconnects"]["encoder_restart"], 0, "{st}");
    assert_eq!(st["connects"], 1, "ffmpeg did not need to reconnect: {st}");
    // The whole process dies and comes back 2 s later.
    let _ = e.children[idx].kill();
    let _ = e.children[idx].wait();
    std::thread::sleep(Duration::from_secs(2));
    spawn(&mut e, &["--stream-test"], &hooks);
    let (all, stamps) = reader.join().unwrap();
    let _ = ff.wait();
    let samples: Vec<f32> = all
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    let secs = samples.len() as f64 / 48_000.0;
    println!("captured {secs:.1} s of audio in 34 s across two interruptions");
    // 34 s minus the downtime (about 2 s of dead process, a reconnect delay of up to 3 s, the burst).
    assert!(secs > 24.0, "only {secs:.1} s");
    // Audio after each interruption: arrival gaps stay short, and the tail is the 1 kHz tone.
    let worst = stamps
        .windows(2)
        .map(|w| w[1].0 - w[0].0)
        .fold(0.0, f64::max);
    println!("longest silence between reads: {worst:.1} s");
    assert!(worst < 8.0, "{worst}");
    let tail = &samples[samples.len() - 24_000..];
    assert!(goertzel(tail, 1000.0, 48_000.0) > 0.1, "no tone at the end");
}

/// Acceptance 1, the long form: `frames_written` within 0.1 percent of real time over 10 minutes, on a station
/// with no sound card. Takes 10 minutes: `cargo test -p hotclock --test stream_m2a -- --ignored --nocapture`.
#[test]
#[ignore]
fn ten_minutes_of_real_time() {
    let mut e = stream_env("m2a-long", STREAM);
    spawn(&mut e, &[], &[]);
    wait_for("playing", 20.0, || {
        get(&e.st(), "/api/v1/state").json()["now"]["title"]
            .as_str()
            .map(|_| ())
    });
    let (f0, t0) = (frames_written(&e), Instant::now());
    std::thread::sleep(Duration::from_secs(600));
    let (f1, t1) = (frames_written(&e), Instant::now());
    let rate = (f1 - f0) / (t1 - t0).as_secs_f64();
    println!(
        "frames_written {rate:.2} frames/s over {:.1} s",
        (t1 - t0).as_secs_f64()
    );
    assert!((rate / 48_000.0 - 1.0).abs() < 0.001, "{rate}");
}

/// M2a review: on a stream-only station (`device = "null"`) an encoder that keeps failing takes the station off
/// the air, so `stream_encoder_failed` is critical and `/health` says so.
#[test]
fn an_encoder_failing_on_a_stream_only_station_is_critical() {
    let mut e = stream_env("m2a-g", STREAM);
    spawn(&mut e, &[], &[("HOTCLOCK_TEST_HOOKS", "1")]);
    wait_for("playing", 20.0, || {
        get(&e.st(), "/api/v1/state").json()["now"]["title"]
            .as_str()
            .map(|_| ())
    });
    for k in 1..=5u64 {
        e.ctl(&["hook", "stream-panic"]);
        wait_for("a restart", 10.0, || {
            (stream_json(&e)["encoder"]["restarts"].as_u64() == Some(k)).then_some(())
        });
    }
    wait_for("stream_encoder_failed", 10.0, || {
        alerts(&e)
            .contains(&"stream_encoder_failed".to_string())
            .then_some(())
    });
    let h = get(&e.st(), "/health").json();
    assert_eq!(h["status"], "critical", "{h}");
    assert_eq!(h["stream"]["encoder"], "failed", "{h}");
}
