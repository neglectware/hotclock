//! The show time over real processes: the start block's `show_time`, blocks timed `show`, the validator, the
//! dry run, `GET /program/state` and `POST /program/show-time`.

mod support_api;
use serde_json::{json, Value};
use support_api::*;

use common::timespec::{parse_rfc3339_ms, Zone};

/// An RFC 3339 time from the API, ms.
fn ms(iso: &str) -> i64 {
    parse_rfc3339_ms(iso).unwrap_or_else(|| panic!("not a time: {iso}"))
}

/// `HH:MM:SS` of an instant in `zone`.
fn hms(zone: &Zone, t: i64) -> String {
    zone.local_string(t)[11..19].to_string()
}

fn block_at(state: &Value, name: &str) -> String {
    state["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == name)
        .unwrap_or_else(|| panic!("no block {name}: {state}"))["at"]
        .as_str()
        .unwrap_or_else(|| panic!("{name} has no time: {state}"))
        .to_string()
}

#[test]
fn the_show_time_over_the_api() {
    let mut e = Env::new(
        "showapi",
        &Opts {
            tones: 4,
            secs: 90.0,
            ..Opts::default()
        },
    );
    let rot = e.lib.playlist_id("Rot").unwrap().unwrap();
    e.spawn_station(&[]);
    e.spawn_library();
    let (st, lb) = (e.st(), e.lb());

    let r = post(
        &lb,
        "/api/v1/examples/ita_monday",
        &json!({"pre_show_playlist_id": rot, "elevator_playlist_id": rot, "osc_host": "127.0.0.1", "osc_port": 9, "channel": 2}),
    );
    assert_eq!(r.status, 201, "{}", r.text());
    let monday = r.json();
    let mid = monday["id"].as_i64().unwrap();
    let entry = |name: &str| -> (i64, Value) {
        let x = monday["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["block"]["name"] == name)
            .unwrap_or_else(|| panic!("no entry {name}"));
        (x["id"].as_i64().unwrap(), x["block"]["params"].clone())
    };

    // The station's own clock and zone: the show time is 10 minutes from now, local.
    let st0 = wait_for("the station", 15.0, || {
        let r = get(&st, "/api/v1/program/state");
        (r.status == 200).then(|| r.json())
    });
    let zone = Zone::named(st0["time_zone"].as_str().unwrap()).unwrap();
    let now = st0["at"].as_str().unwrap().to_string();
    let now_ms = ms(&now) / 1000 * 1000;
    // Whole seconds, 10 minutes from now, on the station's local date (the program's T0 date).
    let show = now_ms + 600_000;
    if zone.date_string(now_ms + 3_600_000) != zone.date_string(now_ms) {
        eprintln!("skipped: too close to midnight on the station's clock ({now})");
        return;
    }
    let sec = 1000;
    let patch = |eid: i64, params: Value| {
        let r = send(
            &lb,
            "PATCH",
            &format!("/api/v1/playlists/{mid}/entries/{eid}"),
            &json!({ "params": params }),
        );
        assert_eq!(r.status, 200, "{}", r.text());
    };
    // A fade timed from the show time without a show time on the start block: the validator says so.
    let (fade_e, mut fade_p) = entry("Fade out");
    fade_p["when"] = json!({"kind": "show", "offset_ms": -20_000});
    patch(fade_e, fade_p);
    let v = post(
        &lb,
        &format!("/api/v1/playlists/{mid}/validate"),
        &json!({}),
    )
    .json();
    assert_eq!(v["ok"], false, "{v}");
    assert!(
        v["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "no_show_time"),
        "{v}"
    );
    // A show time that is not a time of day is refused when saved.
    let (start_e, mut start_p) = entry("Expected Mondays");
    start_p["show_time"] = json!("7pm");
    let r = send(
        &lb,
        "PATCH",
        &format!("/api/v1/playlists/{mid}/entries/{start_e}"),
        &json!({ "params": start_p.clone() }),
    );
    assert_eq!(r.status, 422, "{}", r.text());
    start_p["show_time"] = json!(hms(&zone, show));
    patch(start_e, start_p);
    let (osc_e, mut osc_p) = entry("Mute the Wing");
    osc_p["when"] = json!({"kind": "show", "offset_ms": -9_000});
    patch(osc_e, osc_p);
    let v = post(
        &lb,
        &format!("/api/v1/playlists/{mid}/validate"),
        &json!({}),
    )
    .json();
    assert_eq!(v["ok"], true, "{v}");

    // The dry run puts the blocks on the clock.
    let pv = get(
        &lb,
        &format!(
            "/api/v1/playlists/{mid}/preview?t0={}",
            zone.local_string(now_ms).replace('+', "%2B")
        ),
    )
    .json();
    let pv_at = |name: &str| {
        pv["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["name"] == name)
            .unwrap()["at"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(ms(&pv_at("Fade out")), show - 20 * sec, "{pv}");
    assert_eq!(ms(&pv_at("Mute the Wing")), show - 9 * sec, "{pv}");

    // Before the program starts, no program, so nothing to move.
    let r = post(&st, "/api/v1/program/show-time", &json!({"delay_s": 60}));
    assert_eq!(r.status, 409, "{}", r.text());

    // Start it; the state carries the show time and the blocks timed from it.
    wait_for("the program to start", 20.0, || {
        let r = post(
            &st,
            "/api/v1/program/start",
            &json!({"playlist_id": mid, "t0": "now"}),
        );
        (r.status == 200).then_some(())
    });
    let s = wait_for("the show time in the state", 10.0, || {
        let s = get(&st, "/api/v1/program/state").json();
        (!s["program"]["show_time"].is_null()).then_some(s)
    });
    let sh = &s["program"]["show_time"];
    assert_eq!(ms(sh["at"].as_str().unwrap()), show, "{sh}");
    assert_eq!(sh["moved"], false);
    assert!(sh["in_ms"].as_i64().unwrap() > 0);
    assert_eq!(ms(&block_at(&s, "Fade out")), show - 20 * sec);
    assert_eq!(ms(&block_at(&s, "Mute the Wing")), show - 9 * sec);

    // Bad bodies.
    for body in [
        json!({}),
        json!({"delay_s": 60, "reset": true}),
        json!({"local": "25:00"}),
        json!({"at": "soon"}),
        json!({"delay_s": "five"}),
    ] {
        let r = post(&st, "/api/v1/program/show-time", &body);
        assert_eq!(r.status, 400, "{body}: {}", r.text());
    }
    // Before T0 is refused.
    let r = post(
        &st,
        "/api/v1/program/show-time",
        &json!({"local": hms(&zone, now_ms - 3_600_000)}),
    );
    assert_eq!(r.status, 409, "{}", r.text());

    // Delay the show 5 minutes: the show time and every block timed from it move.
    let r = post(&st, "/api/v1/program/show-time", &json!({"delay_s": 300}));
    assert_eq!(r.status, 202, "{}", r.text());
    assert_eq!(
        ms(r.json()["result"]["show_time"].as_str().unwrap()),
        show + 300 * sec
    );
    let s = get(&st, "/api/v1/program/state").json();
    let sh = &s["program"]["show_time"];
    assert_eq!(sh["moved"], true, "{sh}");
    assert_eq!(ms(sh["at"].as_str().unwrap()), show + 300 * sec);
    assert_eq!(ms(sh["playlist_at"].as_str().unwrap()), show);
    assert_eq!(ms(&block_at(&s, "Fade out")), show + 280 * sec);
    assert_eq!(ms(&block_at(&s, "Mute the Wing")), show + 291 * sec);
    // To a time of day.
    let r = post(
        &st,
        "/api/v1/program/show-time",
        &json!({"local": hms(&zone, show + 120 * sec)}),
    );
    assert_eq!(r.status, 202, "{}", r.text());
    let s = get(&st, "/api/v1/program/state").json();
    assert_eq!(ms(&block_at(&s, "Fade out")), show + 100 * sec);
    // Put back.
    let r = post(&st, "/api/v1/program/show-time", &json!({"reset": true}));
    assert_eq!(r.status, 202, "{}", r.text());
    let s = get(&st, "/api/v1/program/state").json();
    assert_eq!(s["program"]["show_time"]["moved"], false);
    assert_eq!(ms(&block_at(&s, "Fade out")), show - 20 * sec);
    assert_eq!(get(&st, "/api/v1/program/show-time").status, 405);
}
