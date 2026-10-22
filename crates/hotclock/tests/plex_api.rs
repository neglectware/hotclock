//! The Plex tools behind `/api/v1/plex` (the Library page's card), against a real library service and the mock
//! Plex server: settings (the token path is never read back), the playlist chooser, import and fill-art as jobs
//! with a dry-run preview first.

#[path = "../../library/tests/support/plex_mock.rs"]
mod plex_mock;
mod support_api;
use plex_mock::{track, Mock, TOKEN};
use serde_json::{json, Value};
use support_api::*;

fn job(lb: &str, r: &serve::http::client::Reply) -> Value {
    assert_eq!(r.status, 202, "{}", r.text());
    let id = r.json()["job"].as_u64().unwrap();
    wait_for("the job", 60.0, || {
        let j = get(lb, &format!("/api/v1/plex/jobs/{id}")).json();
        (j["state"] != "running").then_some(j)
    })
}

#[test]
fn the_plex_card_api_previews_imports_and_fills() {
    let mock = Mock::start((0..10).map(track).collect());
    let mut e = Env::new(
        "plexapi",
        &Opts {
            tones: 0,
            ..Opts::default()
        },
    );
    e.spawn_library();
    let lb = e.lb();
    let tok = e.dir.join("plex-token");
    std::fs::write(&tok, format!("{TOKEN}\n")).unwrap();
    let mut seen = String::new();

    // Settings: a path in, "set and readable" out; neither the path nor the token comes back.
    let r = get(&lb, "/api/v1/plex");
    assert_eq!(r.json()["token_file_set"], false);
    assert_eq!(
        get(&lb, "/api/v1/plex/playlists").status,
        422,
        "no token file yet"
    );
    let r = send(
        &lb,
        "PUT",
        "/api/v1/plex",
        &json!({"server": mock.url(), "token_file": "relative/path"}),
    );
    assert_eq!(r.status, 422);
    let r = send(
        &lb,
        "PUT",
        "/api/v1/plex",
        &json!({"token_file": e.dir.join("nope").to_string_lossy()}),
    );
    assert_eq!(r.status, 422);
    let r = send(
        &lb,
        "PUT",
        "/api/v1/plex",
        &json!({"server": mock.url(), "token_file": tok.to_string_lossy()}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    seen.push_str(&r.text());
    assert_eq!(
        (
            r.json()["token_file_set"].clone(),
            r.json()["token_ok"].clone()
        ),
        (json!(true), json!(true))
    );
    let r = get(&lb, "/api/v1/plex/playlists");
    seen.push_str(&r.text());
    let names: Vec<String> = r.json()["playlists"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["Office Mix", "Smart Thing"]);

    // Import: the preview changes nothing, the import fills pictures, a second import adds nothing.
    let r = post(
        &lb,
        "/api/v1/plex/import",
        &json!({"playlist": "Office Mix", "dry_run": true}),
    );
    let j = job(&lb, &r);
    println!("preview: {}", j["result"]);
    assert_eq!(j["state"], "done");
    assert_eq!(j["result"]["tracks"], 10);
    assert_eq!(j["result"]["imported"], 0);
    assert_eq!(
        e.lib
            .conn()
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let r = post(
        &lb,
        "/api/v1/plex/import",
        &json!({"playlist": "Office Mix", "fill_art": false}),
    );
    let j = job(&lb, &r);
    assert_eq!(
        (
            j["state"].clone(),
            j["result"]["imported"].clone(),
            j["result"]["entries_added"].clone()
        ),
        (json!("done"), json!(10), json!(10)),
        "{j}"
    );
    assert!(
        j["lines"].as_array().unwrap().len() >= 10,
        "the log has the lines"
    );
    // Fill: preview counts, then fill, then nothing left to fill.
    let r = post(
        &lb,
        "/api/v1/plex/fill-art",
        &json!({"all": true, "dry_run": true}),
    );
    let j = job(&lb, &r);
    println!("fill preview: {}", j["result"]);
    assert_eq!(
        (
            j["result"]["checked"].clone(),
            j["result"]["would_fill"].clone(),
            j["result"]["plex_none"].clone(),
            j["result"]["failed"].clone()
        ),
        (json!(10), json!(4), json!(4), json!(2))
    );
    let r = post(
        &lb,
        "/api/v1/plex/fill-art",
        &json!({"playlist": "Office Mix"}),
    );
    let j = job(&lb, &r);
    assert_eq!(j["result"]["filled"], 4);
    let r = post(&lb, "/api/v1/plex/fill-art", &json!({"all": true}));
    let j = job(&lb, &r);
    assert_eq!(
        (
            j["result"]["had_art"].clone(),
            j["result"]["filled"].clone()
        ),
        (json!(4), json!(0))
    );
    assert_eq!(post(&lb, "/api/v1/plex/fill-art", &json!({})).status, 400);
    assert_eq!(
        post(&lb, "/api/v1/plex/import", &json!({"playlist": "Nope"})).status,
        202
    );
    let j = wait_for("the failed job", 30.0, || {
        let l = get(&lb, "/api/v1/plex/jobs").json();
        (l["jobs"][0]["state"] != "running").then(|| l["jobs"][0].clone())
    });
    assert_eq!(j["state"], "failed");
    assert!(j["error"].as_str().unwrap().contains("Office Mix"));
    seen.push_str(&get(&lb, "/api/v1/plex/jobs").text());
    assert!(
        !seen.contains(TOKEN) && !seen.contains(&*tok.to_string_lossy()),
        "neither the token nor its path is returned"
    );
    // Pictures are served by the ordinary art endpoint.
    let id: i64 = e
        .lib
        .conn()
        .query_row(
            "SELECT id FROM items WHERE art_sha IS NOT NULL LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let r = get(&lb, &format!("/api/v1/items/{id}/art?size=600"));
    assert_eq!(r.status, 200);
    assert_eq!(&r.body[..3], &[0xff, 0xd8, 0xff]);
}

/// Not a test: serves the mock Plex server for `dev/m2-ui-check.sh` (the browser drives the Library page's card
/// against it). Writes the token file named by `HOTCLOCK_MOCK_TOKEN` and the base URL to `HOTCLOCK_MOCK_URL_FILE`.
#[test]
#[ignore = "serves the mock for the UI check"]
fn serve_mock_plex_for_the_ui_check() {
    let mock = Mock::start((0..20).map(track).collect());
    std::fs::write(
        std::env::var("HOTCLOCK_MOCK_TOKEN").unwrap(),
        format!("{TOKEN}\n"),
    )
    .unwrap();
    std::fs::write(std::env::var("HOTCLOCK_MOCK_URL_FILE").unwrap(), mock.url()).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(900));
}
