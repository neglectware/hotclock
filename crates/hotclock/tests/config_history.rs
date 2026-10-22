//! The configuration history with real processes and real HTTP (`support_api`): every save of the station file is a
//! version, a drag is one version, a restore applies live and is a version of its own, and a start keeps a hand edit.

mod support_api;

use serde_json::{json, Value};
use support_api::*;

fn env(name: &str) -> Env {
    Env::new(
        name,
        &Opts {
            tones: 2,
            secs: 300.0,
            level: -18.0,
            extra_station: "processing_preset = \"radio\"\n".into(),
            ..Opts::default()
        },
    )
}

fn wait_playing(e: &Env) {
    wait_for("playing", 30.0, || {
        get(&e.st(), "/api/v1/state").json()["now"]["title"]
            .as_str()
            .map(|_| ())
    });
}

fn versions(e: &Env) -> Vec<Value> {
    let r = get(&e.st(), "/api/v1/config-history");
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()["versions"].as_array().unwrap().clone()
}

fn eq_db(e: &Env) -> f64 {
    get(&e.st(), "/api/v1/chain").json()["blocks"]["eq"]["b2_db"]
        .as_f64()
        .unwrap()
}

fn cfg_path(e: &Env) -> std::path::PathBuf {
    e.dir.join("stations").join("office.toml")
}

#[test]
fn history_coalesces_a_drag_restores_live_and_a_restore_can_be_undone() {
    let mut e = env("hist-main");
    e.spawn_station(&[]);
    wait_playing(&e);
    let st = e.st();
    let v = versions(&e);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0]["source"], "loaded at start");
    // A drag: three changes within a second are one version.
    for db in [-6.0, -7.0, -8.5] {
        let r = send(
            &st,
            "PUT",
            "/api/v1/chain/eq",
            &json!({"enabled": true, "b2_db": db}),
        );
        assert_eq!(r.status, 200, "{}", r.text());
    }
    let v = versions(&e);
    assert_eq!(v.len(), 2, "{v:?}");
    assert_eq!(v[0]["source"], "processing: eq");
    let changes: Vec<String> = v[0]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect();
    println!("summary of the drag: {changes:?}");
    assert!(
        changes
            .iter()
            .any(|c| c.starts_with("Equalizer \u{b7} band 2 gain: ")
                && c.ends_with("→ \u{2212}8.5 dB")),
        "{changes:?}"
    );
    assert_eq!(eq_db(&e), -8.5);
    // One version's text and its differences.
    let id0 = v[0]["id"].as_i64().unwrap();
    let one = get(&st, &format!("/api/v1/config-history/{id0}")).json();
    assert!(one["toml"].as_str().unwrap().contains("b2_db = -8.5"));
    assert_eq!(one["same_as_current"], true);
    let older = v[1]["id"].as_i64().unwrap();
    let vs = get(&st, &format!("/api/v1/config-history/{older}")).json();
    assert!(vs["vs_current"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["key"] == "chain.eq.b2_db"));
    // Restore the version before the drag: live, without a restart, and a version of its own.
    let before_db = {
        let t = get(&st, &format!("/api/v1/config-history/{older}")).json()["toml"]
            .as_str()
            .unwrap()
            .to_string();
        t.contains("b2_db")
    };
    assert!(!before_db, "the loaded file had no equalizer band");
    let r = post(
        &st,
        &format!("/api/v1/config-history/{older}/restore"),
        &json!({}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    let j = r.json();
    println!("restore answer: {j}");
    assert_eq!(j["applied"]["chain"], true);
    assert_eq!(j["needs_restart"], json!([]));
    let eq_default = eq_db(&e);
    assert_ne!(eq_default, -8.5, "the running chain went back");
    assert!(!std::fs::read_to_string(cfg_path(&e))
        .unwrap()
        .contains("b2_db"));
    let v = versions(&e);
    assert_eq!(v.len(), 3);
    assert_eq!(v[0]["source"], "restore");
    assert!(v[0]["of_time"].is_string());
    // The restore is undone by restoring the version before it.
    let r = post(
        &st,
        &format!("/api/v1/config-history/{}/restore", v[1]["id"]),
        &json!({}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(eq_db(&e), -8.5);
    assert_eq!(versions(&e).len(), 4);
    // The loudness card is a source of its own, and a restore starts it too.
    let r = send(
        &st,
        "PUT",
        "/api/v1/loudness",
        &json!({"enabled": true, "target_lufs": -16.0}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    let v = versions(&e);
    assert_eq!(v[0]["source"], "loudness");
    assert_eq!(
        get(&st, "/api/v1/loudness").json()["settings"]["target_lufs"],
        -16.0
    );
    let r = post(
        &st,
        &format!("/api/v1/config-history/{}/restore", v[1]["id"]),
        &json!({}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["applied"]["loudness"], true);
    assert_ne!(
        get(&st, "/api/v1/loudness").json()["settings"]["target_lufs"],
        -16.0
    );
    // Errors.
    assert_eq!(get(&st, "/api/v1/config-history/99999").status, 404);
    assert_eq!(
        post(&st, "/api/v1/config-history/99999/restore", &json!({})).status,
        404
    );
    assert_eq!(
        send(&st, "DELETE", "/api/v1/config-history/1", &json!({})).status,
        405
    );
}

#[test]
fn a_restore_names_what_needs_a_restart_and_a_start_keeps_a_hand_edit() {
    let mut e = env("hist-restart");
    e.spawn_station(&[]);
    wait_playing(&e);
    let st = e.st();
    // A hand edit while the station runs, then a change through the page: the edit becomes a version.
    let p = cfg_path(&e);
    let text = std::fs::read_to_string(&p).unwrap();
    std::fs::write(&p, format!("display_name = \"Hand\"\n{text}")).unwrap();
    let r = send(
        &st,
        "PUT",
        "/api/v1/chain/eq",
        &json!({"enabled": true, "b2_db": -3.0}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    let v = versions(&e);
    assert_eq!(v[1]["source"], "edited outside the page", "{v:?}");
    // Restoring the loaded file takes the hand edit's name key out: that part waits for a restart.
    let loaded = v.last().unwrap()["id"].as_i64().unwrap();
    let r = post(
        &st,
        &format!("/api/v1/config-history/{loaded}/restore"),
        &json!({}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    let j = r.json();
    println!("restore answer: {j}");
    assert_eq!(j["applied"]["chain"], true);
    assert!(
        j["needs_restart"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k == "display_name"),
        "{j}"
    );
    // A hand edit while the station is stopped is a version at the next start.
    e.children[0].kill().unwrap();
    let _ = e.children[0].wait();
    e.children.clear();
    let text = std::fs::read_to_string(&p).unwrap();
    std::fs::write(&p, format!("{text}\n# edited by hand\nsilence_s = 45\n")).unwrap();
    e.spawn_station(&[]);
    wait_playing(&e);
    let v = versions(&e);
    assert_eq!(v[0]["source"], "loaded at start", "{v:?}");
    assert!(
        v[0]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c.as_str().unwrap().starts_with("silence_s:")),
        "{v:?}"
    );
    // Restoring the version before the edit puts the single setting back while the station runs.
    let r = post(
        &st_of(&e),
        &format!("/api/v1/config-history/{}/restore", v[1]["id"]),
        &json!({}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    let j = r.json();
    println!("restore answer: {j}");
    assert_eq!(j["applied"]["settings"], json!(["silence_s"]), "{j}");
    assert_eq!(j["needs_restart"], json!([]), "{j}");
}

fn st_of(e: &Env) -> String {
    e.st()
}
