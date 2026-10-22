//! The preset pickers and the Settings API over real HTTP against real processes: the station's default preset
//! (set, read back, refused when wrong, kept across a restart), the live settings, the Settings page's file edits
//! (comments kept, a `.bak`, validation before writing, live versus restart) and the restart endpoint with a
//! fake `systemctl`.

mod support_api;
use serde_json::json;
use support_api::*;

fn kill_station(e: &mut Env) {
    let mut c = e.children.remove(0);
    let _ = c.kill();
    let _ = c.wait();
}

#[test]
fn the_station_preset_is_set_read_back_refused_when_wrong_and_kept_across_a_restart() {
    let mut e = Env::new("preset_api", &Opts::default());
    e.spawn_station(&[]);
    let st = e.st();
    let r = get(&st, "/api/v1/transition-preset");
    assert_eq!(r.status, 200);
    let j = r.json();
    assert_eq!(j["preset"], "plexamp");
    assert_eq!(j["overridden"], false);
    assert_eq!(j["presets"].as_array().unwrap().len(), 5);
    assert_eq!(
        post(&st, "/api/v1/transition-preset", &json!({"name": "sam"})).status,
        202
    );
    let j = get(&st, "/api/v1/transition-preset").json();
    assert_eq!(
        (j["preset"].as_str(), j["overridden"].as_bool()),
        (Some("sam"), Some(true))
    );
    wait_for("the published state to follow", 5.0, || {
        (get(&st, "/api/v1/state").json()["transition_preset"] == "sam").then_some(())
    });
    for bad in [json!({"name": "nope"}), json!({}), json!({"name": 3})] {
        let r = post(&st, "/api/v1/transition-preset", &bad);
        assert_eq!(r.status, 400, "{bad}: {}", r.text());
    }
    assert_eq!(
        get(&st, "/api/v1/transition-preset").json()["preset"],
        "sam"
    );
    // Live settings: wrong values refuse everything, right ones apply.
    let r = post(
        &st,
        "/api/v1/live-settings",
        &json!({"lock_ahead_s": 30, "silence_dbfs": 5}),
    );
    assert_eq!(r.status, 400);
    let r = post(
        &st,
        "/api/v1/live-settings",
        &json!({"lock_ahead_s": 30, "target_lufs": -16.0, "device": "x"}),
    );
    assert_eq!(r.status, 400, "a restart-only key is refused");
    let r = post(
        &st,
        "/api/v1/live-settings",
        &json!({"lock_ahead_s": 30, "target_lufs": -16.0}),
    );
    assert_eq!(r.status, 202, "{}", r.text());
    // A restart (the state.db is the same): the runtime value is back.
    std::thread::sleep(std::time::Duration::from_millis(500));
    kill_station(&mut e);
    e.spawn_station(&[]);
    let j = get(&e.st(), "/api/v1/transition-preset").json();
    assert_eq!(j["preset"], "sam", "{j}");
    assert_eq!(j["config_preset"], "plexamp");
}

#[test]
fn the_settings_page_edits_the_file_with_comments_validates_first_and_restarts_through_systemctl() {
    let mut e = Env::new("settings_api", &Opts::default());
    let station_file = e.dir.join("stations/office.toml");
    let orig = std::fs::read_to_string(&station_file).unwrap();
    std::fs::write(
        &station_file,
        format!("# my office\n{orig}# trailing note\n"),
    )
    .unwrap();
    // A fake systemctl that records what it was asked and "restarts" nothing.
    let fake = e.dir.join("systemctl");
    let log = e.dir.join("systemctl.log");
    std::fs::write(
        &fake,
        format!("#!/bin/sh\necho \"$@\" >> {}\n", log.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("HOTCLOCK_SYSTEMCTL", &fake);
    e.spawn_station(&[]);
    e.spawn_library();
    let lb = e.lb();
    let s = get(&lb, "/api/v1/settings");
    assert_eq!(s.status, 200, "{}", s.text());
    let j = s.json();
    assert_eq!(j["stations"][0]["id"], "office");
    assert_eq!(j["stations"][0]["writable"], true);
    assert_eq!(j["stations"][0]["effective"]["lock_ahead_s"], 20);
    assert!(j["devices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["value"] == "null"));
    assert!(j["playlists"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p == "Rot"));
    assert!(j["catalog"]["station"].as_array().unwrap().len() >= 30);
    // Rejected before anything is written.
    let before = std::fs::read_to_string(&station_file).unwrap();
    let r = send(
        &lb,
        "PUT",
        "/api/v1/settings/station/office",
        &json!({"values": {"lock_ahead_s": 500, "period_frames": 1000}}),
    );
    assert_eq!(r.status, 400, "{}", r.text());
    let errs = r.json()["errors"].as_array().unwrap().clone();
    assert!(
        errs.iter().any(|x| x["key"] == "lock_ahead_s")
            && errs.iter().any(|x| x["key"] == "period_frames"),
        "{errs:?}"
    );
    assert_eq!(std::fs::read_to_string(&station_file).unwrap(), before);
    assert_eq!(
        send(
            &lb,
            "PUT",
            "/api/v1/settings/station/office",
            &json!({"values": {"nosuch": 1}})
        )
        .status,
        400
    );
    assert_eq!(
        send(
            &lb,
            "PUT",
            "/api/v1/settings/station/ghost",
            &json!({"values": {}})
        )
        .status,
        404
    );
    // A mix of a live key, a boundary key and a restart key.
    let r = send(
        &lb,
        "PUT",
        "/api/v1/settings/station/office",
        &json!({"values": {"lock_ahead_s": 30, "transition_preset": "fm", "periods": 6}}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    let j = r.json();
    assert_eq!(j["saved"], true);
    assert_eq!(j["station_restart"], json!(["periods"]));
    let ch = j["changed"].as_array().unwrap();
    let applied = |k: &str| {
        ch.iter().find(|c| c["key"] == k).unwrap()["applied"]
            .as_bool()
            .unwrap()
    };
    assert!(applied("lock_ahead_s") && applied("transition_preset") && !applied("periods"));
    let text = std::fs::read_to_string(&station_file).unwrap();
    assert!(
        text.contains("# my office") && text.contains("# trailing note"),
        "{text}"
    );
    assert!(
        text.contains("lock_ahead_s = 30")
            && text.contains("periods = 6")
            && text.contains("transition_preset = \"fm\"")
    );
    assert_eq!(
        std::fs::read_to_string(e.dir.join("stations/office.toml.bak")).unwrap(),
        before
    );
    // The running station took the live ones.
    assert_eq!(
        get(&e.st(), "/api/v1/transition-preset").json()["preset"],
        "fm"
    );
    // A null removes the key.
    let r = send(
        &lb,
        "PUT",
        "/api/v1/settings/station/office",
        &json!({"values": {"periods": null}}),
    );
    assert_eq!(r.status, 200);
    assert!(!std::fs::read_to_string(&station_file)
        .unwrap()
        .contains("periods"));
    // Global file: library.listen needs the library restarted.
    let r = send(
        &lb,
        "PUT",
        "/api/v1/settings/global",
        &json!({"values": {"time_zone": "Europe/Berlin"}}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["library_restart"], json!(["time_zone"]));
    // The restart endpoint: only configured stations, through the (fake) systemctl, health-checked.
    assert_eq!(
        post(
            &lb,
            "/api/v1/system/restart",
            &json!({"unit": "station", "name": "ghost"})
        )
        .status,
        404
    );
    assert_eq!(
        post(
            &lb,
            "/api/v1/system/restart",
            &json!({"unit": "station", "name": "../x"})
        )
        .status,
        404
    );
    assert_eq!(
        post(&lb, "/api/v1/system/restart", &json!({"unit": "sshd"})).status,
        400
    );
    let r = post(
        &lb,
        "/api/v1/system/restart",
        &json!({"unit": "station", "name": "office"}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["healthy"], true);
    assert_eq!(
        std::fs::read_to_string(&log).unwrap().trim(),
        "restart hotclock-station@office.service"
    );
}
