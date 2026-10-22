//! M3 over real processes: the schedule endpoints of the station, the weekly grid and the validator of the
//! library service, the station reading a station block and a scheduled start from the library, and the
//! block log reached through the library's proxy.

mod support_api;
use library::{import_file, ImportOptions, ImportOutcome};
use serde_json::{json, Value};
use support_api::*;

fn iso(s: &str) -> i64 {
    serve::util::parse_time(s).unwrap()
}

fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[test]
fn the_schedule_endpoints_the_week_grid_and_the_validator() {
    let mut e = Env::new(
        "m3api",
        &Opts {
            tones: 3,
            secs: 60.0,
            ..Opts::default()
        },
    );
    // A tone for the ID category.
    let p = make_tone(&e.dir.join("src"), "id0", 2000.0, 4.0, -20.0);
    match import_file(
        &e.lib,
        &p,
        &ImportOptions {
            category: Some("id".into()),
            source: "test".into(),
        },
    ) {
        ImportOutcome::Done { .. } => {}
        x => panic!("{x:?}"),
    }
    let id_cat = e.lib.category_id("id").unwrap().unwrap();
    let rot = e.lib.playlist_id("Rot").unwrap().unwrap();
    e.spawn_station(&[]);
    e.spawn_library();
    let (st, lb) = (e.st(), e.lb());

    // Nothing is scheduled yet.
    let r = get(&st, "/api/v1/schedule/upcoming?hours=24");
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(r.json()["events"], json!([]));
    let r = get(&st, "/api/v1/program/state");
    assert_eq!(r.status, 200);
    let ps = r.json();
    assert_eq!(ps["running"], true);
    assert_eq!(ps["program"]["playlist"], "Rot");
    assert!(ps["next_block"].is_null() && ps["expected"].is_null());
    assert!(get(&st, "/api/v1/blocks/log").json()["rows"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        get(&lb, "/api/v1/stations/office/blocks/log").status,
        200,
        "through the proxy, not the station's block definitions"
    );

    // A station block with a window of 90 s around every hour; a nonsense recurrence is refused on its row.
    let bad = post(
        &lb,
        "/api/v1/stations/office/blocks",
        &json!({"kind": "insert", "name": "bad", "trigger": "timed", "params": {"source": {"category_id": id_cat}, "when": {"kind": "recurrence", "cron": "61 * * * *"}}}),
    );
    assert_eq!(bad.status, 422, "{}", String::from_utf8_lossy(&bad.body));
    assert!(bad.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("minute"));
    let ok = post(
        &lb,
        "/api/v1/stations/office/blocks",
        &json!({"kind": "insert", "name": "Top of the hour", "trigger": "timed",
        "params": {"source": {"category_id": id_cat}, "when": {"kind": "recurrence", "cron": "0 * * * *"}, "mode": "soft_then_hard", "window_early_s": 60, "window_late_s": 30}}),
    );
    assert_eq!(ok.status, 201, "{}", String::from_utf8_lossy(&ok.body));
    let ev = wait_for("the station to list the ID", 15.0, || {
        let j = get(&st, "/api/v1/schedule/upcoming?hours=3").json();
        j["events"]
            .as_array()
            .and_then(|a| a.iter().find(|x| x["origin"] == "station").cloned())
    });
    let target = iso(ev["target"].as_str().unwrap());
    assert_eq!(target % 3_600_000, 0, "on the hour");
    assert_eq!(iso(ev["window"]["from"].as_str().unwrap()), target - 60_000);
    assert_eq!(iso(ev["window"]["to"].as_str().unwrap()), target + 30_000);
    assert_eq!(ev["mode"], "soft_then_hard");
    let predicted = iso(ev["predicted_at"].as_str().unwrap());
    assert!(
        (target - 60_000..=target + 30_000).contains(&predicted),
        "predicted {predicted} outside the window of {target}"
    );
    println!(
        "m3 api: the station lists the ID for {} predicted at {}",
        ev["target"], ev["predicted_at"]
    );
    // The next block for the Deck's countdown.
    let ps = get(&st, "/api/v1/program/state").json();
    assert_eq!(ps["next_block"]["type"], "insert");
    assert!(ps["next_block"]["in_ms"].as_i64().unwrap() > 0);

    // A one-shot change: validation, listing, cancelling.
    let at = chrono_free_iso(wall_ms() + 2 * 3_600_000);
    assert_eq!(
        post(
            &st,
            "/api/v1/schedule/changes",
            &json!({"playlist_id": rot})
        )
        .status,
        400
    );
    assert_eq!(
        post(
            &st,
            "/api/v1/schedule/changes",
            &json!({"playlist_id": rot, "at": chrono_free_iso(wall_ms() - 5000)})
        )
        .status,
        422
    );
    assert_eq!(
        post(
            &st,
            "/api/v1/schedule/changes",
            &json!({"playlist_id": 999, "at": at})
        )
        .status,
        404
    );
    assert_eq!(
        post(
            &st,
            "/api/v1/schedule/changes",
            &json!({"playlist_id": rot, "at": at, "how": "sideways"})
        )
        .status,
        400
    );
    let made = post(
        &st,
        "/api/v1/schedule/changes",
        &json!({"playlist_id": rot, "at": at, "how": "segue"}),
    );
    assert_eq!(made.status, 202, "{}", String::from_utf8_lossy(&made.body));
    let change_id = made.json()["result"]["id"].as_i64().unwrap();
    let up = get(&st, "/api/v1/schedule/upcoming?hours=3").json();
    let one: Vec<&Value> = up["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["origin"] == "oneshot")
        .collect();
    assert_eq!(one.len(), 1);
    assert_eq!(iso(one[0]["target"].as_str().unwrap()), iso(&at));
    assert_eq!(get(&st, "/api/v1/schedule/changes").status, 405);
    let d = send(
        &st,
        "DELETE",
        &format!("/api/v1/schedule/changes/{change_id}"),
        &json!({}),
    );
    assert_eq!(d.status, 200);
    let up = get(&st, "/api/v1/schedule/upcoming?hours=3").json();
    assert!(up["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|x| x["origin"] != "oneshot"));
    assert_eq!(
        send(
            &st,
            "DELETE",
            &format!("/api/v1/schedule/changes/{change_id}"),
            &json!({})
        )
        .status,
        404
    );
    assert_eq!(
        post(&st, "/api/v1/program/continue", &json!({})).status,
        409,
        "no wait block waits for the operator"
    );

    // The weekly grid: a manual start Mondays 18:45 shows up, the station reports it as expected.
    let tier = post(
        &lb,
        &format!("/api/v1/playlists/{rot}/entries"),
        &json!({"block": {"kind": "start", "name": "Eve", "trigger": "first",
        "params": {"schedule": [{"cron": "45 18 * * 1"}], "anchor": "manual", "how": "segue"}}}),
    );
    assert_eq!(tier.status, 201, "{}", String::from_utf8_lossy(&tier.body));
    let week = get(&lb, "/api/v1/schedule/week?station=office").json();
    assert_eq!(week["time_zone"], "America/New_York");
    let st0 = &week["starts"][0];
    assert_eq!(st0["anchor"], "manual");
    assert_eq!(
        st0["slots"],
        json!([{"day": 1, "time": "18:45", "minute_of_day": 1125}])
    );
    let exp = wait_for("the station to expect the start", 15.0, || {
        let ps = get(&st, "/api/v1/program/state").json();
        (!ps["expected"].is_null()).then(|| ps["expected"].clone())
    });
    assert_eq!(exp["anchor"], "manual");
    assert_eq!(exp["playlist"], "Rot");
    assert!(exp["in_ms"].as_i64().unwrap() > 0);
    // A manual start is expected, not fired: nothing of it in the upcoming firings.
    let up = get(&st, "/api/v1/schedule/upcoming?hours=200").json();
    assert!(up["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|x| x["origin"] != "start"));

    // The validator: two timed flow blocks at one time, and a playlist with an error cannot be enabled.
    let pl = post(
        &lb,
        "/api/v1/playlists",
        &json!({"name": "Show", "mode": "sequential"}),
    )
    .json();
    let pid = pl["id"].as_i64().unwrap();
    for (name, kind) in [("stop", "stop"), ("fade", "fade_stop")] {
        let r = post(
            &lb,
            &format!("/api/v1/playlists/{pid}/entries"),
            &json!({"block": {"kind": kind, "name": name, "trigger": "timed", "params": {"when": {"kind": "relative", "offset_ms": 900000}}}}),
        );
        assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
    }
    let v = post(
        &lb,
        &format!("/api/v1/playlists/{pid}/validate"),
        &json!({}),
    )
    .json();
    assert_eq!(v["ok"], false);
    assert_eq!(v["problems"][0]["code"], "same_time");
    let r = send(
        &lb,
        "PATCH",
        &format!("/api/v1/playlists/{pid}"),
        &json!({"enabled": true}),
    );
    assert_eq!(r.status, 422);
    println!("m3 api: {}", r.json()["error"]["message"]);
}

/// RFC 3339 UTC for unix milliseconds (the tests have no date library).
fn chrono_free_iso(ms: i64) -> String {
    station::state::iso_ms(ms)
}
