//! M6a acceptance with real processes and real HTTP (`support_api`): restart (6), the readings on the API
//! (7), a stalled meter thread (8), and the settings API with the config file it writes.
//!
//! The station plays 5-minute tones normalized to a track reference of -17 LUFS while the controller's target
//! is -14, so its static start estimate is +3 dB.

mod support_api;

use serde_json::{json, Value};
use serve::ws::{client::Conn, Message};
use std::time::{Duration, Instant};
use support_api::*;

const LOUD: &str = "\n[loudness]\nenabled = true\ntarget_lufs = -14.0\n";

fn env(name: &str) -> Env {
    Env::new(
        name,
        &Opts {
            tones: 2,
            secs: 300.0,
            target_lufs: Some(-17.0),
            level: -17.0103,
            extra_station: LOUD.into(),
            ..Opts::default()
        },
    )
}

fn loud(e: &Env) -> Value {
    get(&e.st(), "/api/v1/loudness").json()
}

fn wait_playing(e: &Env) {
    wait_for("playing", 30.0, || {
        get(&e.st(), "/api/v1/state").json()["now"]["title"]
            .as_str()
            .map(|_| ())
    });
}

/// Acceptance 6: killing the station mid-run restores the gain within 0.1 dB of the checkpoint, with no
/// re-convergence: the window loudness stays within 1 LU of the target through the restart.
#[test]
fn acceptance_6_a_killed_station_resumes_at_the_checkpointed_gain() {
    let mut e = env("m6a-restart");
    e.spawn_station(&[("HOTCLOCK_TEST_HOOKS", "1")]);
    wait_playing(&e);
    let first = loud(&e);
    let static_db = first["static_start_db"].as_f64().unwrap();
    println!(
        "first start: gain {:+.3} dB (the static estimate is {static_db:+.3}), state {}",
        first["gain_db"].as_f64().unwrap(),
        first["state"]
    );
    assert!((first["gain_db"].as_f64().unwrap() - 3.0).abs() < 0.1);
    // A gain the controller would not have chosen itself (it has not run long enough to move: min_valid 30 s).
    e.ctl(&["hook", "loudness-gain", "3.5"]);
    wait_for("the hook gain", 10.0, || {
        ((loud(&e)["gain_db"].as_f64().unwrap() - 3.5).abs() < 0.02).then_some(())
    });
    // The checkpoint is written every 2 s: let two go by, then kill the process outright.
    std::thread::sleep(Duration::from_secs(6));
    let before_kill = loud(&e);
    println!(
        "before the kill: gain {:+.3} dB, window {} LUFS",
        before_kill["gain_db"].as_f64().unwrap(),
        before_kill["window"]
    );
    e.children[0].kill().unwrap();
    let _ = e.children[0].wait();
    e.children.clear();
    let t0 = Instant::now();
    e.spawn_station(&[("HOTCLOCK_TEST_HOOKS", "1")]);
    wait_playing(&e);
    let after = loud(&e);
    println!(
        "{:.1} s after the kill the new process is up: gain {:+.3} dB (checkpoint {:+.3}, limit 0.1), state {}",
        t0.elapsed().as_secs_f64(),
        after["gain_db"].as_f64().unwrap(),
        before_kill["gain_db"].as_f64().unwrap(),
        after["state"]
    );
    assert!(
        (after["gain_db"].as_f64().unwrap() - before_kill["gain_db"].as_f64().unwrap()).abs() < 0.1,
        "the gain was not restored"
    );
    // No re-convergence: over the next 20 s the gain stays within 0.1 dB, and the window loudness (the
    // output, once it has 0.4 s of audio) is within 1 LU of the target.
    let mut worst_gain: f64 = 0.0;
    let mut worst_w: f64 = 0.0;
    for sec in 0..20 {
        std::thread::sleep(Duration::from_secs(1));
        let l = loud(&e);
        worst_gain = worst_gain.max((l["gain_db"].as_f64().unwrap() - 3.5).abs());
        // The window needs a few seconds of real program after a start to mean anything (on a loaded box the
        // first readings are of a block or two).
        if let Some(w) = l["window"].as_f64().filter(|_| sec >= 4) {
            worst_w = worst_w.max((w + 14.0).abs());
        }
    }
    println!(
        "next 20 s: the gain moved at most {worst_gain:.3} dB from the checkpoint; the window loudness was at most {worst_w:.2} LU from -14 (limit 1)"
    );
    assert!(worst_gain < 0.1);
    assert!(worst_w < 1.0);
}

/// Acceptance 7, the API half: `GET /api/v1/loudness` equals the meter frame of the same `seq`.
#[test]
fn acceptance_7_get_loudness_equals_the_frame_of_the_same_seq() {
    let mut e = env("m6a-parity");
    e.spawn_station(&[]);
    wait_playing(&e);
    let frames: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<u32, Vec<u8>>>> =
        Default::default();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (f2, s2, addr) = (frames.clone(), stop.clone(), e.st());
    let reader = std::thread::spawn(move || {
        let mut ws = Conn::connect(&addr, "/api/v1/ws?topics=meters", &[]).unwrap();
        ws.set_timeout(Some(Duration::from_secs(5)));
        while !s2.load(std::sync::atomic::Ordering::Relaxed) {
            if let Ok(Message::Binary(b)) = ws.recv() {
                let seq = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
                let mut m = f2.lock().unwrap();
                if m.len() > 600 {
                    m.clear();
                }
                m.insert(seq, b);
            }
        }
    });
    std::thread::sleep(Duration::from_secs(8));
    let mut checked = 0;
    for _ in 0..6 {
        let j = loud(&e);
        let seq = j["seq"].as_u64().unwrap() as u32;
        std::thread::sleep(Duration::from_millis(150));
        let frame = frames.lock().unwrap().get(&seq).cloned();
        let Some(frame) = frame else {
            continue; // the frame was built for the GET alone (nobody else wanted it): no WebSocket twin
        };
        let arr: [u8; serve::meters::FRAME_LEN] = frame.try_into().unwrap();
        let fj = serve::meters::frame_json(&arr);
        for (k, fk) in [
            ("target", "target"),
            ("short_term", "short_term"),
            ("window", "window"),
            ("integrated", "integrated"),
            ("gain_db", "gain_db"),
            ("state", "loudness_state"),
        ] {
            assert_eq!(j[k], fj[fk], "seq {seq} field {k}");
        }
        checked += 1;
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = reader.join();
    let j = loud(&e);
    println!(
        "GET /api/v1/loudness against the WebSocket frame of the same seq: {checked} of 6 requests compared, every one of target, short-term, window, integrated, gain and state identical; now S {} W {} I {} gain {} {} (target {})",
        j["short_term"], j["window"], j["integrated"], j["gain_db"], j["state"], j["target"]
    );
    assert!(checked >= 3, "too few GETs had a WebSocket twin: {checked}");
}

/// The settings API: `PUT` changes the controller live and writes the `[loudness]` block of the config file;
/// bad values are refused; reset puts the gain back at the static estimate.
#[test]
fn the_settings_api_changes_the_controller_and_writes_the_config() {
    let mut e = env("m6a-settings");
    e.spawn_station(&[]);
    wait_playing(&e);
    let st = e.st();
    let cfg_path = e.dir.join("stations").join("office.toml");
    let j = loud(&e);
    assert_eq!(j["enabled"], true);
    assert_eq!(j["settings"]["target_lufs"], -14.0);
    // The chain view says the limiter is forced on for the controller.
    let chain = get(&st, "/api/v1/chain").json();
    let lim = chain["stages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "limiter")
        .unwrap()
        .clone();
    let ctl = chain["stages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "loudness_controller")
        .unwrap()
        .clone();
    assert_eq!(lim["enabled"], true);
    assert_eq!(ctl["enabled"], true);
    let rev0 = chain["chain_cfg_rev"].as_u64().unwrap();
    // Live change.
    let r = send(
        &st,
        "PUT",
        "/api/v1/loudness",
        &json!({"target_lufs": -16.0, "true_peak_dbtp": -2.0}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["settings"]["target_lufs"], -16.0);
    assert_eq!(r.json()["config_written"], true);
    let text = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(text.contains("target_lufs = -16"), "{text}");
    assert!(text.contains("true_peak_dbtp = -2"), "{text}");
    assert_eq!(text.matches("[loudness]").count(), 1);
    let chain = get(&st, "/api/v1/chain").json();
    assert!(chain["chain_cfg_rev"].as_u64().unwrap() > rev0);
    let lim = chain["stages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "limiter")
        .unwrap()
        .clone();
    assert_eq!(lim["ceiling_dbtp"], -2.0);
    // Refused.
    for bad in [
        json!({"target_lufs": -3.0}),
        json!({"window_s": 5.0}),
        json!({"bogus": 1}),
        json!({"enabled": "yes"}),
    ] {
        let r = send(&st, "PUT", "/api/v1/loudness", &bad);
        assert_eq!(r.status, 400, "{bad}: {}", r.text());
    }
    // The setting that was refused changed nothing.
    assert_eq!(loud(&e)["settings"]["target_lufs"], -16.0);
    // Off again: the frame says so.
    let r = send(&st, "PUT", "/api/v1/loudness", &json!({"enabled": false}));
    assert_eq!(r.status, 200);
    std::thread::sleep(Duration::from_millis(600));
    let j = loud(&e);
    assert_eq!(j["state"], "off");
    assert!(j["target"].is_null());
    // Reset.
    let r = post(&st, "/api/v1/loudness/reset", &json!({}));
    assert_eq!(r.status, 202);
    println!(
        "settings API: live change of target and ceiling applied and written to the config; four bad bodies refused with 400; off and reset answered; chain_cfg_rev {rev0} -> {}",
        get(&st, "/api/v1/chain").json()["chain_cfg_rev"]
    );
}

/// "Move to makeup": the controller's steady +3 dB goes into the chain's output makeup. The program level does not
/// move (short-term loudness within 0.3 LU, momentary within 0.6 LU through the move), the gain ends near 0, and the
/// chain shows as modified with the makeup in the config file.
#[test]
fn move_to_makeup_keeps_the_level_and_brings_the_gain_to_zero() {
    let mut e = env("m6a-move");
    e.spawn_station(&[]);
    wait_playing(&e);
    let st = e.st();
    let on_level = |l: &Value| l["short_term"].as_f64().filter(|s| (s + 14.0).abs() < 0.2);
    wait_for("the short-term loudness at the target", 60.0, || {
        on_level(&loud(&e)).map(|_| ())
    });
    let j = loud(&e);
    assert!(
        j["note"].is_null(),
        "the tip is for a gain that has stood still for five minutes"
    );
    let (g0, s0) = (
        j["gain_db"].as_f64().unwrap(),
        j["short_term"].as_f64().unwrap(),
    );
    assert!((g0 - 3.0).abs() < 0.1, "gain {g0}");
    let chain0 = get(&st, "/api/v1/chain").json();
    assert_eq!(chain0["custom"], false);
    assert_eq!(chain0["blocks"]["makeup"]["gain_db"], 0.0);
    // Refused with the controller off? Checked at the end. Now the move.
    let t0 = Instant::now();
    let r = post(&st, "/api/v1/loudness/move-to-makeup", &json!({}));
    assert_eq!(r.status, 200, "{}", r.text());
    let m = r.json();
    println!(
        "moved {:+.2} dB: makeup {} -> {}, gliding {:.2} s",
        m["moved_db"].as_f64().unwrap(),
        m["makeup_db_before"],
        m["makeup_db"],
        m["glide_s"].as_f64().unwrap()
    );
    assert_eq!(m["makeup_db"], 3.0);
    assert_eq!(m["chain"]["custom"], true);
    // A second move while the first glides is refused.
    let r2 = post(&st, "/api/v1/loudness/move-to-makeup", &json!({}));
    assert_eq!(r2.status, 409, "{}", r2.text());
    let (mut worst_s, mut worst_m, mut n): (f64, f64, u32) = (0.0, 0.0, 0);
    while t0.elapsed() < Duration::from_secs(9) {
        let l = loud(&e);
        if let Some(s) = l["short_term"].as_f64() {
            worst_s = worst_s.max((s - s0).abs());
        }
        if let Some(mo) = l["momentary"].as_f64() {
            worst_m = worst_m.max((mo - s0).abs());
        }
        n += 1;
    }
    let after = loud(&e);
    let g1 = after["gain_db"].as_f64().unwrap();
    println!(
        "{n} readings over 9 s: short-term moved at most {worst_s:.3} LU (limit 0.3), momentary at most {worst_m:.3} LU (limit 0.6); gain {g0:+.3} -> {g1:+.3} dB (limit +-0.5), state {}",
        after["state"]
    );
    assert!(worst_s < 0.3, "short-term moved {worst_s} LU");
    assert!(worst_m < 0.6, "momentary moved {worst_m} LU");
    assert!(g1.abs() < 0.5, "gain {g1}");
    let chain = get(&st, "/api/v1/chain").json();
    assert_eq!(chain["custom"], true);
    assert_eq!(chain["blocks"]["makeup"]["gain_db"], 3.0);
    assert_eq!(chain["bypassed"], false);
    let text = std::fs::read_to_string(e.dir.join("stations").join("office.toml")).unwrap();
    assert!(
        text.contains("[chain.makeup]") && text.contains("gain_db = 3"),
        "{text}"
    );
    // Nothing left to move: refused. And with the controller off: refused.
    let r = post(&st, "/api/v1/loudness/move-to-makeup", &json!({}));
    assert_eq!(r.status, 400, "{}", r.text());
    let r = send(&st, "PUT", "/api/v1/loudness", &json!({"enabled": false}));
    assert_eq!(r.status, 200);
    let r = post(&st, "/api/v1/loudness/move-to-makeup", &json!({}));
    assert_eq!(r.status, 409, "{}", r.text());
    assert_eq!(get(&st, "/api/v1/loudness/move-to-makeup").status, 405);
}

/// Acceptance 8, the process half: the meter thread stalled by the test hook does not stall rt-out.
#[test]
fn acceptance_8_a_stalled_meter_thread_does_not_stall_the_audio() {
    let mut e = env("m6a-stall");
    e.spawn_station(&[("HOTCLOCK_TEST_HOOKS", "1")]);
    wait_playing(&e);
    let frames = |e: &Env| -> f64 {
        let m = get(&e.st(), "/metrics").text();
        m.lines()
            .find(|l| l.starts_with("hotclock_frames_written_total"))
            .and_then(|l| l.split_whitespace().last()?.parse().ok())
            .unwrap()
    };
    let drops = |e: &Env| -> f64 {
        let m = get(&e.st(), "/metrics").text();
        m.lines()
            .find(|l| l.starts_with("hotclock_meter_tap_drops_total"))
            .and_then(|l| l.split_whitespace().last()?.parse().ok())
            .unwrap()
    };
    let g0 = loud(&e)["gain_db"].as_f64().unwrap();
    let (f0, d0) = (frames(&e), drops(&e));
    e.ctl(&["hook", "loudness-stall", "6000"]);
    std::thread::sleep(Duration::from_secs(5));
    let (f1, d1) = (frames(&e), drops(&e));
    let wall = 5.0;
    println!(
        "meter thread stalled 6 s: frames written advanced {:.0} in about {wall} s ({:.1} s of audio at 48 kHz); tap blocks dropped {} (counted); health {}",
        f1 - f0,
        (f1 - f0) / 48_000.0,
        d1 - d0,
        get(&e.st(), "/health").status
    );
    assert!((f1 - f0) / 48_000.0 > 4.0, "rt-out slowed down");
    assert!(
        d1 > d0,
        "no tap blocks were dropped while the thread was stalled"
    );
    std::thread::sleep(Duration::from_secs(3));
    let g1 = loud(&e)["gain_db"].as_f64().unwrap();
    println!("gain before the stall {g0:+.4} dB, after it resumed {g1:+.4} dB (the law has not run long enough to move it)");
    assert!((g1 - g0).abs() < 0.05);
}
