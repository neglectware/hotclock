//! A program tied to a station on a running box: the station's own process refuses it once the playlist
//! editor ties it to another station, also when the station had already read (and even started) it.
//! On the box (2026-10-12) a PATCH of `station_id` to `eve` left office starting Eve Monday from a deck key.

mod support_api;
use serde_json::json;
use support_api::*;

#[test]
fn a_program_tied_to_another_station_after_the_station_read_it_is_refused() {
    let mut e = Env::new(
        "progstation",
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
    // Station eve exists in the library (on the box it is a configured station with its own blocks).
    e.lib
        .conn()
        .execute(
            "INSERT OR IGNORE INTO stations(id, display_name) VALUES('eve', 'Eve')",
            [],
        )
        .unwrap();

    // The weekly example, made for no station first: office reads it and starts it from a deck key.
    let r = post(
        &lb,
        "/api/v1/examples/ita_monday",
        &json!({"name": "Eve Monday", "pre_show_playlist_id": rot, "elevator_playlist_id": rot, "osc_host": "127.0.0.1", "osc_port": 9, "channel": 2}),
    );
    assert_eq!(r.status, 201, "{}", r.text());
    let id = r.json()["id"].as_i64().unwrap();
    let start = json!({"playlist_id": id, "t0": "now", "source": "deck", "station": "office"});
    wait_for("office to start the program of no station", 20.0, || {
        let r = post(&st, "/api/v1/program/start", &start);
        (r.status == 200).then_some(())
    });
    let ps = get(&st, "/api/v1/program/state").json();
    assert_eq!(ps["program"]["playlist_id"], id);
    // Long enough for the station's own copy to have been refreshed at least once, as on the box.
    std::thread::sleep(std::time::Duration::from_secs(3));

    // The playlist editor ties it to eve while it runs on office.
    let path = format!("/api/v1/playlists/{id}");
    let r = send(&lb, "PATCH", &path, &json!({"station_id": "eve"}));
    assert!(r.status < 300, "{}", r.text());
    assert_eq!(r.json()["station_id"], "eve");

    // The very next press on office is refused: the station reads the tie from the library at the press,
    // without waiting for its own copy of the playlists (refreshed every 2 s).
    let r = post(&st, "/api/v1/program/start", &start);
    assert_eq!(r.status, 409, "{}", r.text());
    let refused = r.text();
    assert!(
        refused.contains("is a program of station eve; this is station office"),
        "{refused}"
    );

    // And it stays refused once the station's copy has caught up.
    std::thread::sleep(std::time::Duration::from_secs(3));
    let r = post(&st, "/api/v1/program/start", &start);
    assert_eq!(r.status, 409, "{}", r.text());

    // After the program ends, the same: the tie holds whatever the station is doing.
    let r = post(&st, "/api/v1/program/abort", &json!({}));
    assert!(r.status < 300, "{}", r.text());
    let r = post(&st, "/api/v1/program/start", &start);
    assert_eq!(r.status, 409, "{}", r.text());

    // Untied again, office may run it.
    let r = send(&lb, "PATCH", &path, &json!({"station_id": null}));
    assert!(r.status < 300, "{}", r.text());
    wait_for("office to start the untied program", 10.0, || {
        let r = post(&st, "/api/v1/program/start", &start);
        (r.status == 200).then_some(())
    });
}
