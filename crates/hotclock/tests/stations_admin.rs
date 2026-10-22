//! Station management over real HTTP against a real library service, with `dev/fake-systemctl.sh` standing in for
//! systemd (it runs and stops real station processes): validation, create, the demo station, rename with its
//! dry run, rollback of a failing rename and of a failing create, delete with and without the data.

mod support_api;
use serde_json::{json, Value};
use std::path::PathBuf;
use support_api::*;

fn setup(name: &str) -> (Env, PathBuf) {
    let mut e = Env::new(name, &Opts::default());
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../dev/fake-systemctl.sh");
    let sc = e.dir.join("sc");
    std::fs::create_dir_all(&sc).unwrap();
    std::env::set_var("HOTCLOCK_SYSTEMCTL", fake);
    std::env::set_var("FAKE_SC_DIR", &sc);
    std::env::set_var("FAKE_SC_BIN", env!("CARGO_BIN_EXE_hotclock"));
    std::env::set_var("FAKE_SC_GLOBAL", &e.global);
    std::env::set_var("HOTCLOCK_STATION_START_TIMEOUT_S", "8");
    e.spawn_library();
    (e, sc)
}

fn station_json(e: &Env, id: &str) -> Value {
    let r = get(&e.lb(), "/api/v1/station-admin");
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()["stations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .cloned()
        .unwrap_or(Value::Null)
}

fn health_ok(e: &Env, id: &str) -> bool {
    let s = station_json(e, id);
    s["port"].as_u64().is_some_and(|p| {
        serve::http::client::request(
            &format!("127.0.0.1:{p}"),
            "GET",
            "/health",
            &[],
            b"",
            std::time::Duration::from_secs(2),
        )
        .is_ok_and(|r| r.status == 200)
    })
}

#[test]
fn create_validates_starts_and_the_demo_station_plays() {
    let (e, _sc) = setup("stadm_create");
    let lb = e.lb();
    let r = get(&lb, "/api/v1/station-admin");
    let j = r.json();
    assert_eq!(j["manageable"], true);
    assert!(j["next_port"].as_u64().unwrap() >= 8301);
    assert!(j["playlists"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p == "Rot"));
    assert_eq!(j["stations"][0]["id"], "office");
    // Validation: every error names its field and nothing is written.
    let office_port = j["stations"][0]["port"].as_u64().unwrap();
    for (body, key) in [
        (json!({"name": "Bad Name"}), "name"),
        (json!({"name": "test"}), "name"),
        (json!({"name": "office"}), "name"),
        (json!({"name": "ok", "port": office_port}), "port"),
        (json!({"name": "ok", "port": 80}), "port"),
        (json!({"name": "ok", "output": "card"}), "device"),
        (
            json!({"name": "ok", "default_playlist": "nope"}),
            "default_playlist",
        ),
        (
            json!({"name": "ok", "transition_preset": "nope"}),
            "transition_preset",
        ),
    ] {
        let r = post(&lb, "/api/v1/station-admin", &body);
        assert_eq!(r.status, 400, "{body}: {}", r.text());
        assert!(
            r.json()["errors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|x| x["key"] == key),
            "{body}: {}",
            r.text()
        );
    }
    assert!(!e.dir.join("stations/ok.toml").exists());
    // Create a stream-only station.
    let r = post(
        &lb,
        "/api/v1/station-admin",
        &json!({"name": "late", "display_name": "Late Night", "output": "stream", "default_playlist": "Rot", "loudness": true}),
    );
    assert_eq!(r.status, 201, "{}", r.text());
    let j = r.json();
    assert_eq!(j["station"]["output"], "stream");
    assert_eq!(j["station"]["display_name"], "Late Night");
    assert!(j["stream_url"]
        .as_str()
        .unwrap()
        .ends_with("/stream/program.ogg"));
    assert!(health_ok(&e, "late"));
    let text = std::fs::read_to_string(e.dir.join("stations/late.toml")).unwrap();
    assert!(
        text.contains("name = \"late\"")
            && text.contains("[loudness]")
            && text.contains("expect_listener = false"),
        "{text}"
    );
    // The library proxy and the settings page see it without any registration.
    assert_eq!(get(&lb, "/api/v1/stations/late/state").status, 200);
    assert!(get(&lb, "/api/v1/settings").json()["stations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["id"] == "late"));
    assert_eq!(
        post(&lb, "/api/v1/station-admin", &json!({"name": "late"})).status,
        400,
        "a second one with the name"
    );
    // The demo station: content once, a stream-only station on it, playing.
    let r = post(&lb, "/api/v1/station-admin/demo", &json!({}));
    assert_eq!(r.status, 201, "{}", r.text());
    let j = r.json();
    assert_eq!(j["created"], "demo");
    assert!(health_ok(&e, "demo"));
    let pls = get(&lb, "/api/v1/playlists").json();
    let demo = pls["playlists"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Demo tones")
        .unwrap();
    assert_eq!(demo["entries"], 6, "{demo}");
    let st = get(
        &format!("127.0.0.1:{}", station_json(&e, "demo")["port"]),
        "/api/v1/state",
    )
    .json();
    assert!(st["state"].is_string(), "{st}");
    // A second demo makes demo-2 with its own port; the content is not imported twice.
    let r = post(&lb, "/api/v1/station-admin/demo", &json!({}));
    assert_eq!(r.status, 201, "{}", r.text());
    assert_eq!(r.json()["created"], "demo-2");
    assert_ne!(
        station_json(&e, "demo")["port"],
        station_json(&e, "demo-2")["port"]
    );
}

#[test]
fn a_failing_create_leaves_nothing_behind() {
    let (e, sc) = setup("stadm_create_fail");
    let lb = e.lb();
    // systemctl start fails
    std::fs::write(sc.join("fail"), "start:boom").unwrap();
    let r = post(&lb, "/api/v1/station-admin", &json!({"name": "boom"}));
    assert_eq!(r.status, 502, "{}", r.text());
    assert!(r.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("journalctl"));
    assert!(
        !e.dir.join("stations/boom.toml").exists(),
        "the file is removed"
    );
    // the unit starts but the station never answers
    std::fs::write(sc.join("fail"), "").unwrap();
    std::fs::write(sc.join("hang"), "hung").unwrap();
    let r = post(&lb, "/api/v1/station-admin", &json!({"name": "hung"}));
    assert_eq!(r.status, 502, "{}", r.text());
    assert!(!e.dir.join("stations/hung.toml").exists());
    let log = std::fs::read_to_string(sc.join("systemctl.log")).unwrap();
    assert!(
        log.contains("stop hotclock-station@hung.service"),
        "the unit was stopped again: {log}"
    );
    // and the box still works: the same name can be made once the fault is gone
    std::fs::write(sc.join("hang"), "").unwrap();
    assert_eq!(
        post(&lb, "/api/v1/station-admin", &json!({"name": "hung"})).status,
        201
    );
}

#[test]
fn rename_shows_a_plan_moves_everything_and_rolls_back_when_the_new_unit_will_not_start() {
    let (e, sc) = setup("stadm_rename");
    let lb = e.lb();
    assert_eq!(
        post(
            &lb,
            "/api/v1/station-admin",
            &json!({"name": "late", "default_playlist": "Rot"})
        )
        .status,
        201
    );
    // things that mention the station: a station block, a meter override, a snapshot file, a display view
    e.lib
        .add_station_block(
            "late",
            &library::playlists::NewBlock::insert_every("liner", 4, json!({"category": "liner"})),
        )
        .unwrap();
    let mut m = get(&lb, "/api/v1/display/meters?station=late").json();
    for k in ["station", "inherits", "presets", "settings"] {
        m.as_object_mut().unwrap().remove(k);
    }
    m["station"] = json!("late");
    assert_eq!(send(&lb, "PUT", "/api/v1/display/meters", &m).status, 200);
    assert_eq!(
        get(&lb, "/api/v1/display/meters?station=late").json()["inherits"],
        false
    );
    assert_eq!(
        send(
            &lb,
            "PUT",
            "/api/v1/display",
            &json!({"view": "single", "station": "late"})
        )
        .status,
        200
    );
    std::fs::create_dir_all(e.data.join("backup")).unwrap();
    std::fs::write(e.data.join("backup/station-late-20261001.db"), "x").unwrap();
    wait_for("state.db", 10.0, || {
        e.data.join("stations/late/state.db").exists().then_some(())
    });
    // validation
    for (to, why) in [
        ("Bad", "case"),
        ("office", "taken"),
        ("late", "same"),
        ("test", "reserved"),
    ] {
        let r = post(
            &lb,
            "/api/v1/station-admin/late/rename",
            &json!({"new_name": to}),
        );
        assert_eq!(r.status, 400, "{why}: {}", r.text());
    }
    assert_eq!(
        post(
            &lb,
            "/api/v1/station-admin/ghost/rename",
            &json!({"new_name": "x"})
        )
        .status,
        404
    );
    // the dry run changes nothing
    let r = post(
        &lb,
        "/api/v1/station-admin/late/rename",
        &json!({"new_name": "night", "dry_run": true}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    let plan = r.json()["plan"].clone();
    assert_eq!(plan["running"], true);
    assert!(plan["changes"]["snapshots"].as_u64().unwrap() >= 1);
    assert_eq!(plan["changes"]["meter_override"], true);
    assert!(
        plan["changes"]["library_rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["table"] == "blocks"),
        "{plan}"
    );
    assert!(
        e.dir.join("stations/late.toml").exists() && !e.dir.join("stations/night.toml").exists()
    );
    // a failing start of the new name: everything is put back and the old station runs again
    std::fs::write(sc.join("fail"), "start:night").unwrap();
    let r = post(
        &lb,
        "/api/v1/station-admin/late/rename",
        &json!({"new_name": "night"}),
    );
    assert_eq!(r.status, 500, "{}", r.text());
    let j = r.json();
    assert_eq!(j["old_station_running_again"], true, "{j}");
    assert!(j["not_undone"].is_null(), "{j}");
    assert!(
        e.dir.join("stations/late.toml").exists() && !e.dir.join("stations/night.toml").exists()
    );
    assert!(std::fs::read_to_string(e.dir.join("stations/late.toml"))
        .unwrap()
        .contains("name = \"late\""));
    assert!(
        e.data.join("stations/late/state.db").exists() && !e.data.join("stations/night").exists()
    );
    assert!(e.data.join("backup/station-late-20261001.db").exists());
    let blocks: i64 = e
        .lib
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM blocks WHERE station_id = 'late'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        blocks,
        1,
        "{:?}",
        e.lib
            .conn()
            .prepare("SELECT id, name, station_id FROM blocks")
            .unwrap()
            .query_map([], |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?
            )))
            .unwrap()
            .collect::<Vec<_>>()
    );
    assert_eq!(
        get(&lb, "/api/v1/display/meters?station=late").json()["inherits"],
        false
    );
    wait_for("the old station back", 10.0, || {
        health_ok(&e, "late").then_some(())
    });
    // the real rename
    std::fs::write(sc.join("fail"), "").unwrap();
    let r = post(
        &lb,
        "/api/v1/station-admin/late/rename",
        &json!({"new_name": "night"}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(!e.dir.join("stations/late.toml").exists());
    assert!(std::fs::read_to_string(e.dir.join("stations/night.toml"))
        .unwrap()
        .contains("name = \"night\""));
    assert!(
        e.data.join("stations/night/state.db").exists() && !e.data.join("stations/late").exists()
    );
    assert!(
        e.data.join("backup/station-night-20261001.db").exists()
            && !e.data.join("backup/station-late-20261001.db").exists()
    );
    let n = |sql: &str| -> i64 { e.lib.conn().query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(
        n("SELECT COUNT(*) FROM blocks WHERE station_id = 'night'"),
        1
    );
    assert_eq!(n("SELECT COUNT(*) FROM stations WHERE id = 'late'"), 0);
    assert_eq!(
        n("SELECT COUNT(*) FROM display_view WHERE station = 'night'"),
        1
    );
    assert_eq!(
        get(&lb, "/api/v1/display/meters?station=night").json()["inherits"],
        false
    );
    assert!(health_ok(&e, "night") && station_json(&e, "late").is_null());
    assert_eq!(get(&lb, "/api/v1/stations/night/state").status, 200);
    assert_eq!(get(&lb, "/api/v1/stations/late/state").status, 404);
    // the port did not change
    assert_eq!(
        r.json()["plan"]["unchanged"]["port"],
        r.json()["station"]["port"].to_string().trim_matches('"')
    );
}

#[test]
fn delete_needs_the_name_and_keeps_or_removes_the_data() {
    let (e, _sc) = setup("stadm_delete");
    let lb = e.lb();
    for n in ["keepme", "dropme"] {
        assert_eq!(
            post(
                &lb,
                "/api/v1/station-admin",
                &json!({"name": n, "default_playlist": "Rot"})
            )
            .status,
            201
        );
        e.lib
            .add_station_block(
                n,
                &library::playlists::NewBlock::insert_every(
                    "liner",
                    4,
                    json!({"category": "liner"}),
                ),
            )
            .unwrap();
    }
    wait_for("state", 10.0, || {
        e.data
            .join("stations/keepme/state.db")
            .exists()
            .then_some(())
    });
    let r = send(&lb, "DELETE", "/api/v1/station-admin/keepme", &json!({}));
    assert_eq!(r.status, 400, "no confirmation: {}", r.text());
    let r = send(
        &lb,
        "DELETE",
        "/api/v1/station-admin/keepme?confirm=other",
        &json!({}),
    );
    assert_eq!(r.status, 400);
    assert!(e.dir.join("stations/keepme.toml").exists());
    assert_eq!(
        send(
            &lb,
            "DELETE",
            "/api/v1/station-admin/keepme?confirm=keepme&data=maybe",
            &json!({})
        )
        .status,
        400
    );
    // keep (the default)
    let r = send(
        &lb,
        "DELETE",
        "/api/v1/station-admin/keepme?confirm=keepme",
        &json!({}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    let arch = PathBuf::from(r.json()["archive"].as_str().unwrap());
    assert!(
        arch.join("keepme.toml").exists() && arch.join("state/state.db").exists(),
        "{arch:?}"
    );
    assert!(
        !e.dir.join("stations/keepme.toml").exists() && !e.data.join("stations/keepme").exists()
    );
    assert!(station_json(&e, "keepme").is_null());
    let blocks: i64 = e
        .lib
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM blocks WHERE station_id = 'keepme'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(blocks, 1, "the library keeps the schedule records");
    assert_eq!(
        get(&lb, "/api/v1/station-admin").json()["archived"][0]["name"],
        "keepme"
    );
    // delete the data too
    std::fs::write(e.data.join("backup/station-dropme-20261001.db"), "x").unwrap_or_else(|_| {
        std::fs::create_dir_all(e.data.join("backup")).unwrap();
        std::fs::write(e.data.join("backup/station-dropme-20261001.db"), "x").unwrap();
    });
    let r = send(
        &lb,
        "DELETE",
        "/api/v1/station-admin/dropme?confirm=dropme&data=delete",
        &json!({}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(
        !e.dir.join("stations/dropme.toml").exists() && !e.data.join("stations/dropme").exists()
    );
    assert!(!e.data.join("backup/station-dropme-20261001.db").exists());
    let blocks: i64 = e
        .lib
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM blocks WHERE station_id = 'dropme'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(blocks, 0);
    assert_eq!(
        get(&lb, "/api/v1/station-admin").json()["archived"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // the name can be used again
    assert_eq!(
        post(&lb, "/api/v1/station-admin", &json!({"name": "dropme"})).status,
        201
    );
    assert_eq!(
        send(
            &lb,
            "DELETE",
            "/api/v1/station-admin/ghost?confirm=ghost",
            &json!({})
        )
        .status,
        404
    );
}
