//! M5 over real processes: the program endpoints of the station, the dry run, the example and the templates
//! of the library service, and a real UDP receiver for the OSC message of a block fired by hand.

mod support_api;
use serde_json::{json, Value};
use station::side::{osc_parse, OscArg};
use std::net::UdpSocket;
use std::time::Duration;
use support_api::*;

#[test]
fn the_program_endpoints_the_dry_run_and_the_example() {
    let mut e = Env::new(
        "m5api",
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

    // The templates and the examples are served.
    let t = get(&lb, "/api/v1/block-templates").json();
    let wing = t
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == "wing_channel_mute")
        .expect("template");
    assert_eq!(wing["params"]["port"], 2223);
    assert_eq!(wing["params"]["address"], "/ch/2/mute");
    assert_eq!(get(&lb, "/api/v1/examples").json()[0]["id"], "ita_monday");

    // A receiver for the OSC message.
    let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
    rx.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let port = rx.local_addr().unwrap().port();

    // The example, over the one playlist the box has (pre-show and bed may be the same playlist).
    let r = post(
        &lb,
        "/api/v1/examples/ita_monday",
        &json!({"pre_show_playlist_id": rot, "elevator_playlist_id": rot, "osc_host": "127.0.0.1", "osc_port": port, "channel": 2}),
    );
    assert_eq!(r.status, 201, "{}", r.text());
    let monday = r.json();
    let mid = monday["id"].as_i64().unwrap();
    assert_eq!(monday["mode"], "sequential");
    let kinds: Vec<String> = monday["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["block"]["kind"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        kinds,
        ["start", "play", "fade_stop", "osc_out", "play", "stop"]
    );
    assert_eq!(
        post(&lb, "/api/v1/examples/nothing", &json!({})).status,
        404
    );
    assert_eq!(
        post(&lb, "/api/v1/examples/ita_monday", &json!({})).status,
        400
    );

    // The validator: one warning (the silent gap), no errors; the playlist can be enabled.
    let v = post(
        &lb,
        &format!("/api/v1/playlists/{mid}/validate"),
        &json!({}),
    )
    .json();
    assert_eq!(v["ok"], true);
    assert_eq!(v["problems"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["problems"][0]["code"], "silent_gap");

    // The dry run for T0 18:45:00 on a Monday; nothing fires.
    let pv = get(
        &lb,
        &format!("/api/v1/playlists/{mid}/preview?t0=2027-01-18T18:45:00-05:00&now=2027-01-18T19:00:30-05:00"),
    );
    assert_eq!(pv.status, 200, "{}", pv.text());
    let pv = pv.json();
    let at = |name: &str| {
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
    assert_eq!(at("Fade out"), "2027-01-18T18:59:40.000-05:00");
    assert_eq!(at("Mute the Wing"), "2027-01-18T19:00:00.000-05:00");
    assert_eq!(at("Elevator bed"), "2027-01-18T19:00:01.000-05:00");
    assert_eq!(at("Stop after midnight"), "2027-01-19T00:30:00.000-05:00");
    assert_eq!(pv["silent_gaps"][0]["seconds"], 14);
    let past: Vec<(String, String)> = pv["past"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["name"].as_str().unwrap().into(),
                p["action"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert!(
        past.contains(&("Fade out".into(), "skip".into()))
            && past.contains(&("Mute the Wing".into(), "run".into())),
        "{past:?}"
    );
    assert_eq!(
        get(&lb, &format!("/api/v1/playlists/{mid}/preview?t0=garbage")).status,
        400
    );
    assert_eq!(get(&lb, "/api/v1/playlists/9999/preview").status, 404);

    // A copy as a Thursday one-off with its own start date.
    let c = post(
        &lb,
        &format!("/api/v1/playlists/{mid}/copy"),
        &json!({"name": "Eve Thursday", "start": "2027-01-21T18:45:00"}),
    );
    assert_eq!(c.status, 201, "{}", c.text());
    let cj = c.json();
    assert_eq!(
        cj["entries"][0]["block"]["params"]["schedule"],
        json!([{"date": "2027-01-21T18:45:00"}])
    );
    assert_eq!(cj["entries"].as_array().unwrap().len(), 6);
    assert_eq!(
        post(
            &lb,
            &format!("/api/v1/playlists/{mid}/copy"),
            &json!({"name": "x", "start": "soon"})
        )
        .status,
        400
    );

    // Program steering with nothing waiting.
    let r = wait_for("the station to know the program", 15.0, || {
        let r = post(&st, "/api/v1/program/continue", &json!({}));
        (r.status == 409).then_some(r)
    });
    assert!(r.text().contains("no wait block"));
    assert_eq!(
        post(&st, "/api/v1/program/arm", &json!({"playlist_id": mid})).status,
        409,
        "the example's start block has no fallback"
    );
    assert_eq!(post(&st, "/api/v1/program/start", &json!({})).status, 400);
    assert_eq!(
        post(&st, "/api/v1/program/start", &json!({"playlist_id": 9999})).status,
        409
    );
    assert_eq!(get(&st, "/api/v1/program/start").status, 405);

    // Start it as the program (T0 = now).
    let s = wait_for("the station to start the Monday program", 20.0, || {
        let r = post(
            &st,
            "/api/v1/program/start",
            &json!({"playlist_id": mid, "t0": "now"}),
        );
        (r.status == 200).then(|| r.json())
    });
    assert_eq!(s["started"], true);
    assert_eq!(s["program"]["playlist"], "Eve Monday");
    assert_eq!(s["program"]["t0_source"], "api");
    // A press of the Stream Deck key, and the web UI's Change program, say who started it: never "auto" (that
    // is the fallback start alone). Each press is a fresh start from a new T0.
    for src in ["deck", "ui"] {
        let r = wait_for("a start by hand", 20.0, || {
            let r = post(
                &st,
                "/api/v1/program/start",
                &json!({"playlist_id": mid, "t0": "now", "source": src}),
            );
            (r.status == 200).then(|| r.json())
        });
        assert_eq!(r["program"]["t0_source"], src);
        assert!(r["program"]["t0"].as_str().unwrap() > s["program"]["t0"].as_str().unwrap());
    }
    // A press meant for another station, or from a key with no station, never starts anything here.
    let r = post(
        &st,
        "/api/v1/program/start",
        &json!({"playlist_id": mid, "t0": "now", "source": "deck", "station": "eve"}),
    );
    assert_eq!(r.status, 409, "{}", r.text());
    assert!(r.text().contains("this is station office"), "{}", r.text());
    let r = post(
        &st,
        "/api/v1/program/start",
        &json!({"playlist_id": mid, "t0": "now", "source": "deck", "station": ""}),
    );
    assert_eq!(r.status, 409, "{}", r.text());
    // A program made for station eve is refused on office, with what to do.
    e.lib
        .conn()
        .execute(
            "INSERT OR IGNORE INTO stations(id, display_name) VALUES('eve', 'Eve')",
            [],
        )
        .unwrap();
    let r = post(
        &lb,
        "/api/v1/examples/ita_monday",
        &json!({"name": "Eve Monday (eve)", "station_id": "eve", "pre_show_playlist_id": rot, "elevator_playlist_id": rot, "osc_host": "127.0.0.1", "osc_port": port, "channel": 2}),
    );
    assert_eq!(r.status, 201, "{}", r.text());
    let ita_id = r.json()["id"].as_i64().unwrap();
    let refused = wait_for("the station to know the eve program", 20.0, || {
        let r = post(
            &st,
            "/api/v1/program/start",
            &json!({"playlist_id": ita_id, "t0": "now", "source": "deck"}),
        );
        // (until the station has read the new playlist, it answers "no playlist")
        (r.status == 409 && r.text().contains("is a program of station")).then(|| r.text())
    });
    assert!(
        refused.contains("is a program of station eve; this is station office"),
        "{refused}"
    );
    // The playlist editor sets and clears a program's station; an unknown station is refused.
    let path = format!("/api/v1/playlists/{ita_id}");
    let r = send(&lb, "PATCH", &path, &json!({"station_id": "nowhere"}));
    assert_eq!(r.status, 422, "{}", r.text());
    assert_eq!(get(&lb, &path).json()["station_id"], "eve");
    let r = send(&lb, "PATCH", &path, &json!({"station_id": null}));
    assert!(r.status < 300, "{}", r.text());
    assert!(get(&lb, &path).json()["station_id"].is_null());
    let r = send(&lb, "PATCH", &path, &json!({"station_id": "eve"}));
    assert!(r.status < 300, "{}", r.text());
    assert_eq!(get(&lb, &path).json()["station_id"], "eve");
    let ps = get(&st, "/api/v1/program/state").json();
    assert_eq!(
        ps["program"]["playlist_id"], mid,
        "the running program is untouched"
    );
    assert_eq!(ps["program"]["state"], "running");
    let names: Vec<&str> = ps["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "Expected Mondays",
            "Pre-show music",
            "Fade out",
            "Mute the Wing",
            "Elevator bed",
            "Stop after midnight"
        ]
    );
    assert_eq!(ps["next_block"]["name"], "Fade out");
    let in_ms = ps["next_block"]["in_ms"].as_i64().unwrap();
    assert!((870_000..=881_000).contains(&in_ms), "{in_ms}");
    // The queue carries the upcoming blocks between the tracks.
    let q = get(&st, "/api/v1/queue").json()["entries"].clone();
    let blocks: Vec<&Value> = q
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["kind"] == "block")
        .collect();
    println!("queue: {q}");
    println!(
        "queue block rows: {:?}",
        blocks
            .iter()
            .map(|b| (
                b["badge"].as_str().unwrap(),
                b["name"].as_str().unwrap(),
                b["in_ms"].as_i64().unwrap()
            ))
            .collect::<Vec<_>>()
    );
    assert!(
        blocks.iter().any(|b| b["badge"] == "FADE") && blocks.iter().any(|b| b["badge"] == "OSC")
    );
    assert!(q
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["kind"] != "block")
        .all(|x| x["entry_id"].is_i64()));

    // Fire the OSC block by hand: the message reaches a real receiver, once, and not again at its time.
    let osc_id = monday_block(&lb, mid, "osc_out");
    let f = post(
        &st,
        &format!("/api/v1/program/blocks/{osc_id}/fire"),
        &json!({}),
    );
    assert_eq!(f.status, 200, "{}", f.text());
    let mut buf = [0u8; 256];
    let (n, _) = rx.recv_from(&mut buf).expect("the OSC message arrives");
    let (addr, args) = osc_parse(&buf[..n]).unwrap();
    assert_eq!((addr.as_str(), args), ("/ch/2/mute", vec![OscArg::Int(1)]));
    let row = wait_for("the block log row", 15.0, || {
        get(&st, "/api/v1/blocks/log").json()["rows"]
            .as_array()
            .and_then(|a| a.iter().find(|r| r["name"] == "Mute the Wing").cloned())
    });
    assert_eq!(row["how"], "fired");
    assert_eq!(
        post(
            &st,
            &format!("/api/v1/program/blocks/{}/fire", 987654),
            &json!({})
        )
        .status,
        409
    );

    // Skip the next block (the fade).
    let k = post(&st, "/api/v1/program/blocks/next/skip", &json!({}));
    assert_eq!(k.status, 200, "{}", k.text());
    assert_eq!(k.json()["skipped"], "Fade out");

    // The external event named continue answers even when no wait is waiting.
    let x = post(&st, "/api/v1/external/continue", &json!({"value": 1}));
    assert_eq!(x.status, 202, "{}", x.text());

    // end-at: a back-timed one-off program that ends at a time.
    let when = station::state::iso_ms(wall_ms() + 6 * 60_000);
    assert_eq!(
        post(&st, "/api/v1/end-at", &json!({"playlist_id": rot})).status,
        400
    );
    assert_eq!(
        post(
            &st,
            "/api/v1/end-at",
            &json!({"playlist_id": rot, "at": "2000-01-01T00:00:00Z"})
        )
        .status,
        409
    );
    let r = post(
        &st,
        "/api/v1/end-at",
        &json!({"playlist_id": rot, "at": when, "then": {"switch_playlist": rot}}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    println!("end-at answered {}", r.text());
    let bt = get(&st, "/api/v1/program/state").json();
    println!("state after end-at: {}", bt["program"]);
    assert_eq!(bt["back_time"]["mode"], "exact");
    assert_eq!(
        bt["back_time"]["state"], "on_target",
        "the planner reaches the time exactly: {}",
        bt["back_time"]
    );
    assert_eq!(bt["back_time"]["delta_s"], 0.0);

    // Abort, also stopping the audio.
    let a = post(&st, "/api/v1/program/abort", &json!({"stop_audio": true}));
    assert_eq!(a.status, 202, "{}", a.text());
    wait_for("the station to stop", 10.0, || {
        (get(&st, "/api/v1/state").json()["state"] == "stopped").then_some(())
    });
    // A program can be started again after an abort, with the audio stopped, and songs are queued.
    let again = post(
        &st,
        "/api/v1/program/start",
        &json!({"playlist_id": mid, "t0": "now"}),
    );
    assert_eq!(again.status, 200, "{}", again.text());
    let q = get(&st, "/api/v1/queue").json()["entries"].clone();
    assert!(
        q.as_array().unwrap().iter().any(|x| x["kind"] == "track"),
        "songs are queued after the restart"
    );
    // And again while it is running.
    let again = post(
        &st,
        "/api/v1/program/start",
        &json!({"playlist_id": mid, "t0": "now"}),
    );
    assert_eq!(again.status, 200, "{}", again.text());
    let q = get(&st, "/api/v1/queue").json()["entries"].clone();
    assert!(q.as_array().unwrap().iter().any(|x| x["kind"] == "track"));
}

fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn monday_block(lb: &str, playlist: i64, kind: &str) -> i64 {
    get(lb, &format!("/api/v1/playlists/{playlist}")).json()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["block"]["kind"] == kind)
        .unwrap()["block"]["id"]
        .as_i64()
        .unwrap()
}
