//! M2 acceptance 1: `import-plex` against a mock Plex Media Server (no real token exists in the build
//! environment). The mock speaks the shapes of the real API: `/identity`, paged `/playlists` and
//! `/playlists/{key}/items` in JSON with `Media.Part`, and the part download, plain and chunked. It checks
//! the token on every route but `/identity`, and records every request so the tests can assert that the
//! importer only ever sends `GET`, never puts the token in a URL, and downloads nothing twice.

use library::plex::{
    fill_art, import_playlist, read_token_file, FillScope, PlexClient, PlexImportOptions,
};
use library::{Library, Paths};
use std::path::PathBuf;

#[path = "support/plex_mock.rs"]
mod plex_mock;
use plex_mock::{tmpdir, track, Mock, MockTrack, TOKEN};

fn open(name: &str) -> (Library, PathBuf) {
    let d = tmpdir(name);
    (
        Library::open_worker(Paths::from_data_dir(&d.join("data"))).unwrap(),
        d,
    )
}

fn count(lib: &Library, sql: &str) -> i64 {
    lib.conn().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn playlist_rows(lib: &Library, name: &str) -> Vec<(String, String)> {
    lib.conn()
        .prepare(
            "SELECT i.title, i.artist FROM playlist_entries e JOIN playlists p ON p.id = e.playlist_id
             JOIN items i ON i.id = e.item_id WHERE p.name = ?1 ORDER BY e.position",
        )
        .unwrap()
        .query_map([name], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

/// Acceptance 1: a 50-track playlist arrives as a static playlist with the same tracks, titles and artists,
/// in Plex's order; a second run adds nothing and downloads nothing.
#[test]
fn a_50_track_playlist_is_reproduced_and_a_rerun_adds_nothing() {
    let tracks: Vec<MockTrack> = (0..50).map(track).collect();
    let mock = Mock::start(tracks.clone());
    let (lib, _d) = open("fifty");
    let plex = PlexClient::new(&mock.url(), TOKEN.into()).unwrap();
    let rep = import_playlist(&lib, &plex, "Office Mix", &PlexImportOptions::default()).unwrap();
    println!(
        "first run: {} tracks, {} downloaded, {} imported, {} failed, {} entries added, {} titles filled",
        rep.tracks, rep.downloaded, rep.imported, rep.failed.len(), rep.entries_added, rep.metadata_filled
    );
    assert_eq!(
        (
            rep.tracks,
            rep.imported,
            rep.failed.len(),
            rep.entries_added
        ),
        (50, 50, 0, 50),
        "{:?}",
        rep.failed
    );
    assert!(rep.playlist_created);
    assert_eq!(mock.downloads(), 50);

    let rows = playlist_rows(&lib, "Office Mix");
    assert_eq!(rows.len(), 50);
    for (i, (title, artist)) in rows.iter().enumerate() {
        assert_eq!(
            (title.as_str(), artist.as_str()),
            (tracks[i].title.as_str(), tracks[i].artist.as_str()),
            "row {i}"
        );
    }
    let kind: String = lib
        .conn()
        .query_row(
            "SELECT kind FROM playlists WHERE name = 'Office Mix'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kind, "static");
    assert_eq!(
        count(
            &lib,
            "SELECT COUNT(*) FROM items WHERE source LIKE 'plex:%'"
        ),
        50
    );
    let (items, entries, jobs) = (
        count(&lib, "SELECT COUNT(*) FROM items"),
        count(&lib, "SELECT COUNT(*) FROM playlist_entries"),
        count(&lib, "SELECT COUNT(*) FROM import_jobs"),
    );
    // Pictures: albums 0 and 1 serve one, album 2 serves junk, album 3 answers 404, album 4 has none.
    println!(
        "art: checked {}, had {}, filled {}, plex none {}, failed {}",
        rep.art.checked, rep.art.had_art, rep.art.filled, rep.art.plex_none, rep.art.failed
    );
    assert_eq!(
        (rep.art.checked, rep.art.had_art, rep.art.filled),
        (50, 0, 20)
    );
    assert_eq!((rep.art.plex_none, rep.art.failed), (20, 10));
    assert_eq!(
        count(&lib, "SELECT COUNT(*) FROM items WHERE art_sha IS NOT NULL"),
        20
    );
    assert_eq!(
        count(&lib, "SELECT COUNT(*) FROM artwork WHERE origin = 'plex'"),
        2,
        "one picture per album, shared"
    );
    assert_eq!(
        mock.thumb_requests(),
        4,
        "one request per distinct picture path"
    );
    // The year and the album's text came from Plex (the WAVs carry neither).
    assert_eq!(
        count(
            &lib,
            "SELECT COUNT(*) FROM items WHERE year = 1999 AND album LIKE 'Album %'"
        ),
        50
    );
    // No download leftovers.
    let dl = lib.paths.db.parent().unwrap().join("plex-download");
    assert!(!dl.exists() || std::fs::read_dir(&dl).unwrap().next().is_none());

    let rep2 = import_playlist(&lib, &plex, "office mix", &PlexImportOptions::default()).unwrap();
    println!(
        "second run: {} downloaded, {} imported, {} known, {} entries added, {} already present",
        rep2.downloaded, rep2.imported, rep2.known, rep2.entries_added, rep2.entries_present
    );
    assert_eq!(
        (rep2.downloaded, rep2.imported, rep2.entries_added),
        (0, 0, 0)
    );
    assert_eq!((rep2.known, rep2.entries_present), (50, 50));
    assert_eq!(mock.downloads(), 50, "a re-run must not download again");
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM items"), items);
    assert_eq!(
        count(&lib, "SELECT COUNT(*) FROM playlist_entries"),
        entries
    );
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM import_jobs"), jobs);

    // One track added in Plex: exactly one more download, one more entry, at the end.
    mock.tracks.lock().unwrap().push(track(50));
    let rep3 = import_playlist(&lib, &plex, "Office Mix", &PlexImportOptions::default()).unwrap();
    assert_eq!(
        (rep3.downloaded, rep3.imported, rep3.entries_added),
        (1, 1, 1)
    );
    assert_eq!(
        playlist_rows(&lib, "Office Mix").last().unwrap().0,
        "Plex Title 50"
    );
    assert_eq!(mock.downloads(), 51);

    // Read-only against the server, and the token only in the header.
    let log = mock.log.lock().unwrap();
    assert!(log.iter().all(|(m, _, _)| m == "GET"), "only GET");
    assert!(
        log.iter()
            .all(|(_, t, _)| !t.contains(TOKEN) && !t.contains("token")),
        "token in a URL"
    );
    assert!(log
        .iter()
        .filter(|(_, t, _)| t != "/identity")
        .all(|(_, _, tok)| tok.as_deref() == Some(TOKEN)));
    println!("{} requests, all GET, token only in the header", log.len());
}

#[test]
fn a_failed_download_is_reported_and_the_next_run_picks_it_up() {
    let tracks: Vec<MockTrack> = (0..6).map(track).collect();
    let mock = Mock::start(tracks);
    mock.broken.lock().unwrap().push("9003".into());
    let (lib, _d) = open("fail");
    let plex = PlexClient::new(&mock.url(), TOKEN.into()).unwrap();
    let rep = import_playlist(&lib, &plex, "Office Mix", &PlexImportOptions::default()).unwrap();
    assert_eq!(
        (rep.imported, rep.failed.len(), rep.entries_added),
        (5, 1, 5)
    );
    assert!(
        rep.failed[0].contains("404") && rep.failed[0].contains("Plex Title 03"),
        "{:?}",
        rep.failed
    );
    assert_eq!(playlist_rows(&lib, "Office Mix").len(), 5);
    mock.broken.lock().unwrap().clear();
    let rep = import_playlist(&lib, &plex, "Office Mix", &PlexImportOptions::default()).unwrap();
    assert_eq!(
        (
            rep.downloaded,
            rep.imported,
            rep.entries_added,
            rep.failed.len()
        ),
        (1, 1, 1, 0)
    );
    // The recovered track is appended (Plex's order is kept for what arrived together).
    assert_eq!(playlist_rows(&lib, "Office Mix").len(), 6);
}

#[test]
fn a_wrong_token_is_refused_with_a_message_and_writes_nothing() {
    let mock = Mock::start((0..3).map(track).collect());
    let (lib, _d) = open("badtoken");
    let plex = PlexClient::new(&mock.url(), "wrong".into()).unwrap();
    let e = import_playlist(&lib, &plex, "Office Mix", &PlexImportOptions::default())
        .unwrap_err()
        .to_string();
    println!("{e}");
    assert!(e.contains("401") && e.contains("token"), "{e}");
    assert!(!e.contains("wrong"), "the token must not be echoed");
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM items"), 0);
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM playlists"), 0);
}

#[test]
fn an_unknown_playlist_lists_the_ones_there_are_and_a_dry_run_changes_nothing() {
    let mock = Mock::start((0..4).map(track).collect());
    let (lib, _d) = open("names");
    let plex = PlexClient::new(&mock.url(), TOKEN.into()).unwrap();
    let e = import_playlist(&lib, &plex, "Nope", &PlexImportOptions::default())
        .unwrap_err()
        .to_string();
    assert!(e.contains("Office Mix") && e.contains("Smart Thing"), "{e}");
    let rep = import_playlist(
        &lib,
        &plex,
        "Office Mix",
        &PlexImportOptions {
            dry_run: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rep.tracks, 4);
    assert_eq!(
        rep.lines
            .iter()
            .filter(|l| l.starts_with("would get"))
            .count(),
        4
    );
    assert_eq!(mock.downloads(), 0);
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM items"), 0);
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM playlists"), 0);
    let listed = plex.playlists().unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed[1].smart);
    assert_eq!(plex.identity().unwrap()["apiVersion"], "1.1.1");
}

#[test]
fn the_same_file_already_in_the_library_is_linked_not_duplicated() {
    let tracks: Vec<MockTrack> = (0..3).map(track).collect();
    let mock = Mock::start(tracks.clone());
    let (lib, d) = open("linked");
    // Track 1 is already in the library, imported by hand from a file with the same bytes.
    let f = d.join("by-hand.wav");
    std::fs::write(&f, &tracks[1].bytes).unwrap();
    let out = library::import_file(
        &lib,
        &f,
        &library::ImportOptions {
            category: None,
            source: "cli".into(),
        },
    );
    let hand = match out {
        library::ImportOutcome::Done { item_id, .. } => item_id,
        o => panic!("{o:?}"),
    };
    let plex = PlexClient::new(&mock.url(), TOKEN.into()).unwrap();
    let rep = import_playlist(&lib, &plex, "Office Mix", &PlexImportOptions::default()).unwrap();
    assert_eq!((rep.imported, rep.known, rep.entries_added), (2, 1, 3));
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM items"), 3);
    let in_playlist: i64 = lib
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM playlist_entries WHERE item_id = ?1",
            [hand],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(in_playlist, 1);
}

#[test]
fn token_files_hold_the_token_alone() {
    let d = tmpdir("tokfile");
    let p = d.join("token");
    std::fs::write(&p, "\n  abc123  \n").unwrap();
    assert_eq!(read_token_file(&p).unwrap(), "abc123");
    std::fs::write(&p, "X-Plex-Token=abc123\n").unwrap();
    assert_eq!(read_token_file(&p).unwrap(), "abc123");
    std::fs::write(&p, "two words\n").unwrap();
    assert!(read_token_file(&p).is_err());
    std::fs::write(&p, "\n\n").unwrap();
    assert!(read_token_file(&p).is_err());
    assert!(read_token_file(&d.join("missing")).is_err());
    assert!(PlexClient::new("https://x:32400", "t".into()).is_err());
}

fn plex_items(lib: &Library) -> Vec<i64> {
    lib.conn()
        .prepare("SELECT id FROM items WHERE source LIKE 'plex:%' ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

/// The backfill: a dry run counts and changes nothing, the real run fills what Plex has, a second run is a no-op.
#[test]
fn the_backfill_dry_runs_fills_and_is_idempotent() {
    let mock = Mock::start((0..25).map(track).collect());
    let (lib, _d) = open("backfill");
    let plex = PlexClient::new(&mock.url(), TOKEN.into()).unwrap();
    let opts = PlexImportOptions {
        fill_art: false,
        ..Default::default()
    };
    let rep = import_playlist(&lib, &plex, "Office Mix", &opts).unwrap();
    assert_eq!((rep.imported, rep.art.checked), (25, 0));
    assert_eq!(
        count(&lib, "SELECT COUNT(*) FROM items WHERE art_sha IS NULL"),
        25
    );
    let none = library::plex::Sink::default();

    let d = fill_art(&lib, &plex, &FillScope::AllPlexItems, true, &none).unwrap();
    println!("dry run: {d:?}");
    // 25 tracks, albums i % 5: five each. Albums 0, 1 serve a picture, 2 junk, 3 answers 404, 4 has none.
    assert_eq!(
        (
            d.checked,
            d.had_art,
            d.would_fill,
            d.plex_none,
            d.failed,
            d.filled
        ),
        (25, 0, 10, 10, 5, 0)
    );
    assert_eq!(
        count(&lib, "SELECT COUNT(*) FROM items WHERE art_sha IS NOT NULL"),
        0,
        "a dry run writes nothing"
    );
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM artwork"), 0);

    let r = fill_art(&lib, &plex, &FillScope::AllPlexItems, false, &none).unwrap();
    println!("real run: {r:?}");
    assert_eq!(
        (r.checked, r.had_art, r.filled, r.plex_none, r.failed),
        (25, 0, 10, 10, 5)
    );
    assert_eq!(
        count(&lib, "SELECT COUNT(*) FROM items WHERE art_sha IS NOT NULL"),
        10
    );
    assert_eq!(
        count(&lib, "SELECT COUNT(*) FROM artwork WHERE origin = 'plex'"),
        2
    );
    // The stored picture is served: both sizes exist on disk.
    let sha: String = lib
        .conn()
        .query_row(
            "SELECT art_sha FROM items WHERE art_sha IS NOT NULL LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(lib
        .paths
        .media
        .join(library::store::art_rel(&sha, 600))
        .exists());
    assert!(lib
        .paths
        .media
        .join(library::store::art_rel(&sha, 144))
        .exists());

    let again = fill_art(&lib, &plex, &FillScope::AllPlexItems, false, &none).unwrap();
    println!("second run: {again:?}");
    assert_eq!(
        (
            again.checked,
            again.had_art,
            again.filled,
            again.plex_none,
            again.failed
        ),
        (25, 10, 0, 10, 5)
    );
    let ids = plex_items(&lib);
    assert_eq!(ids.len(), 25);

    // Scoped to a playlist.
    let sc = fill_art(
        &lib,
        &plex,
        &FillScope::Playlist("Office Mix".into()),
        true,
        &none,
    )
    .unwrap();
    assert_eq!((sc.checked, sc.had_art), (25, 10));
    assert!(fill_art(
        &lib,
        &plex,
        &FillScope::Playlist("Nope".into()),
        true,
        &none
    )
    .is_err());
    // Plex-side metadata the file lacks: a year that went missing comes back.
    lib.conn()
        .execute("UPDATE items SET year = NULL WHERE id = ?1", [ids[0]])
        .unwrap();
    let m = fill_art(&lib, &plex, &FillScope::AllPlexItems, false, &none).unwrap();
    assert_eq!(m.meta_filled, 1);
    assert_eq!(
        count(&lib, "SELECT COUNT(*) FROM items WHERE year IS NULL"),
        0
    );
    let log = mock.log.lock().unwrap();
    assert!(log
        .iter()
        .all(|(m, t, _)| m == "GET" && !t.contains("token")));
}

/// A picture the file already has is never replaced, from the import and from the backfill alike.
#[test]
fn file_art_is_never_overwritten() {
    let (fx, report) = fixtures::shared_dir(&["art_embedded.flac"]);
    if !report.skipped.is_empty() {
        eprintln!("SKIPPED (no ffmpeg?)");
        return;
    }
    let mut t = track(0); // album 0 has a picture on the Plex side
    t.bytes = std::fs::read(fx.join("art_embedded.flac")).unwrap();
    t.file_name = "embedded.flac".into();
    let mock = Mock::start(vec![t, track(1)]);
    let (lib, _d) = open("neverover");
    let plex = PlexClient::new(&mock.url(), TOKEN.into()).unwrap();
    let rep = import_playlist(&lib, &plex, "Office Mix", &PlexImportOptions::default()).unwrap();
    println!("art: had {}, filled {}", rep.art.had_art, rep.art.filled);
    assert_eq!(
        (rep.art.checked, rep.art.had_art, rep.art.filled),
        (2, 1, 1)
    );
    let (sha0, origin): (String, String) = lib
        .conn()
        .query_row("SELECT i.art_sha, a.origin FROM items i JOIN artwork a ON a.sha256 = i.art_sha WHERE i.orig_filename = 'embedded.flac'", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(origin, "embedded");
    // The backfill leaves it, and a manual picture too.
    let none = library::plex::Sink::default();
    let r = fill_art(&lib, &plex, &FillScope::AllPlexItems, false, &none).unwrap();
    assert_eq!((r.had_art, r.filled), (2, 0));
    let after: String = lib
        .conn()
        .query_row(
            "SELECT art_sha FROM items WHERE orig_filename = 'embedded.flac'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(after, sha0);
    // No picture request was made for the item that had art: only the second track's album asked.
    let thumbs: Vec<String> = mock
        .log
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, t, _)| t.contains("/thumb/"))
        .map(|(_, t, _)| t.clone())
        .collect();
    assert_eq!(thumbs.len(), 1, "{thumbs:?}");
    assert!(thumbs[0].contains("/8001/"));
}
