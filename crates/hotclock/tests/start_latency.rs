//! Starts from silence are as quick as a sampler's (owner, 2026-10-12): from the request to the first sample above
//! about -80 dBFS entering the output buffer, under 50 ms (the target is 30), whatever the station was doing: stopped,
//! held at the end of an item, or waiting in assist. The output buffer comes on top of this, as on any device.
//!
//! The station runs as a real process on the `null` output, which takes audio at the real rate like a sound card. The
//! output thread stamps the write that carried the first sound after silence; the state reports it as
//! `output.last_onset` (a wall time in ms with a fraction).

mod support_api;
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use support_api::*;

const LIMIT_MS: f64 = 50.0;

fn ms_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        * 1000.0
}

fn onset(st: &str) -> (u64, f64) {
    let s = get(st, "/api/v1/state").json();
    let o = &s["output"]["last_onset"];
    (
        o["count"].as_u64().unwrap_or(0),
        o["at_ms"].as_f64().unwrap_or(0.0),
    )
}

fn state(st: &str) -> Value {
    get(st, "/api/v1/state").json()
}

/// Send the request and wait for the next sound after silence: the time from just before the request to the moment
/// its first sample entered the output buffer.
fn measure(st: &str, path: &str, body: &Value) -> f64 {
    let (c0, _) = onset(st);
    let t = ms_now();
    let r = post(st, path, body);
    assert!(r.status < 300, "{path}: {} {}", r.status, r.text());
    let at = wait_for("the sound", 5.0, || {
        let (c, at) = onset(st);
        (c > c0).then_some(at)
    });
    at - t
}

/// Wait until the output has been silent for a while (no new sound for 400 ms), so the next start is from silence.
fn quiet(st: &str) {
    let mut last = onset(st).0;
    let mut since = std::time::Instant::now();
    while since.elapsed() < Duration::from_millis(400) {
        std::thread::sleep(Duration::from_millis(50));
        let c = onset(st).0;
        if c != last {
            last = c;
            since = std::time::Instant::now();
        }
    }
}

#[test]
fn a_start_from_silence_reaches_the_output_buffer_within_50_ms_stopped_held_and_in_assist() {
    let mut e = Env::new(
        "startlat",
        &Opts {
            tones: 6,
            secs: 4.0,
            ..Opts::default()
        },
    );
    e.spawn_station(&[]);
    let st = e.st();
    wait_for("the station playing", 20.0, || {
        (state(&st)["state"] == "playing").then_some(())
    });
    let mut out: Vec<(&str, f64)> = vec![];

    // Stopped, then Play.
    for _ in 0..6 {
        let r = post(&st, "/api/v1/transport/stop", &json!({"fade_ms": 10}));
        assert!(r.status < 300, "{}", r.text());
        wait_for("stopped", 5.0, || {
            (state(&st)["state"] == "stopped").then_some(())
        });
        quiet(&st);
        out.push((
            "stopped -> play",
            measure(&st, "/api/v1/transport/play", &json!({})),
        ));
    }

    // Held at the end of an item, then Resume.
    for _ in 0..3 {
        let r = post(&st, "/api/v1/transport/hold", &json!({}));
        assert!(r.status < 300, "{}", r.text());
        // "held" from the press; the item on air plays to its end, then the station is silent
        wait_for("held and silent", 10.0, || {
            let n = get(&st, "/api/v1/now").json();
            (state(&st)["state"] == "held" && n["playing"] == false).then_some(())
        });
        quiet(&st);
        out.push((
            "held -> resume",
            measure(&st, "/api/v1/transport/resume", &json!({})),
        ));
    }

    // Assist: the item ends and the station waits for Next.
    let r = post(&st, "/api/v1/mode", &json!({"mode": "assist"}));
    assert!(r.status < 300, "{}", r.text());
    for _ in 0..3 {
        wait_for("waiting in assist", 10.0, || {
            let n = get(&st, "/api/v1/now").json();
            (n["playing"] == false).then_some(())
        });
        quiet(&st);
        out.push((
            "assist -> next",
            measure(&st, "/api/v1/transport/next", &json!({})),
        ));
    }

    report(&out);
}

/// A Start / Switch Playlist key and a Start Playlist as Program key from stopped. The switch deals the new playlist's
/// plan (30 minutes deep) before its first item starts, so the items here are two minutes long, as songs are: with
/// 4 s items the plan is 450 entries and dealing it took 700 ms.
#[test]
fn a_playlist_switch_or_a_program_start_from_stopped_reaches_the_output_buffer_within_50_ms() {
    let mut e = Env::new(
        "startlat-switch",
        &Opts {
            tones: 6,
            secs: 120.0,
            ..Opts::default()
        },
    );
    e.spawn_station(&[]);
    let st = e.st();
    wait_for("the station playing", 20.0, || {
        (state(&st)["state"] == "playing").then_some(())
    });
    let mut out: Vec<(&str, f64)> = vec![];
    let rot = e.lib.playlist_id("Rot").unwrap().unwrap();
    for (what, path, body) in [
        (
            "stopped -> switch playlist",
            "/api/v1/playlist/switch",
            json!({"playlist_id": rot, "how": "now"}),
        ),
        (
            "stopped -> start as program",
            "/api/v1/program/start",
            json!({"playlist_id": rot, "t0": "now", "source": "deck", "station": "office"}),
        ),
    ] {
        for _ in 0..3 {
            let r = post(&st, "/api/v1/transport/stop", &json!({"fade_ms": 10}));
            assert!(r.status < 300, "{}", r.text());
            wait_for("stopped", 5.0, || {
                (state(&st)["state"] == "stopped").then_some(())
            });
            quiet(&st);
            out.push((what, measure(&st, path, &body)));
        }
    }

    report(&out);
}

fn report(out: &[(&str, f64)]) {
    for (what, ms) in out {
        println!("start latency: {what}: {ms:.1} ms");
    }
    let worst = out.iter().map(|x| x.1).fold(0.0, f64::max);
    let mut v: Vec<f64> = out.iter().map(|x| x.1).collect();
    v.sort_by(|a, b| a.total_cmp(b));
    println!(
        "start latency: median {:.1} ms, worst {worst:.1} ms over {} starts",
        v[v.len() / 2],
        v.len()
    );
    let bad: Vec<_> = out.iter().filter(|x| x.1 > LIMIT_MS).collect();
    assert!(bad.is_empty(), "over {LIMIT_MS} ms: {bad:?}");
}
