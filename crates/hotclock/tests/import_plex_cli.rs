//! M2 acceptance 1 through the real binary: `hotclock import-plex --token-file` against the mock Plex server of
//! the library tests. The token is read from the file, never from the command line, and never printed.

use std::process::Command;

#[path = "../../library/tests/support/plex_mock.rs"]
mod plex_mock;
use plex_mock::{tmpdir, track, Mock, TOKEN};

fn hotclock() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hotclock"))
}

#[test]
fn import_plex_lists_dry_runs_imports_and_reruns_without_adding() {
    let mock = Mock::start((0..12).map(track).collect());
    let d = tmpdir("cli");
    let data = d.join("data");
    let tok = d.join("plex-token");
    std::fs::write(&tok, format!("{TOKEN}\n")).unwrap();
    let run = |args: &[&str]| {
        let o = hotclock()
            .arg("import-plex")
            .args(["--server", &mock.url(), "--token-file"])
            .arg(&tok)
            .arg("--data-dir")
            .arg(&data)
            .args(args)
            .output()
            .unwrap();
        (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).to_string(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        )
    };
    let (ok, out, _) = run(&["--list"]);
    assert!(
        ok && out.contains("Office Mix")
            && out.contains("Smart Thing")
            && out.contains("API 1.1.1"),
        "{out}"
    );
    let (ok, out, _) = run(&["--playlist", "Office Mix", "--dry-run"]);
    assert!(
        ok && out.contains("(dry run)") && out.matches("would get").count() == 12,
        "{out}"
    );
    assert_eq!(mock.downloads(), 0);
    let (ok, out, err) = run(&["--playlist", "Office Mix"]);
    println!("{out}");
    assert!(ok, "{out}{err}");
    assert!(out.contains("12 tracks in Plex; 12 downloaded and imported, 0 already in the library, 0 failed; 12 added"), "{out}");
    let (ok, out, _) = run(&["--playlist", "Office Mix"]);
    println!("{out}");
    assert!(ok);
    assert!(out.contains("0 downloaded and imported, 12 already in the library, 0 failed; 0 added to the playlist, 12 already in it"), "{out}");
    assert_eq!(mock.downloads(), 12);
    // The token is in neither stream, and a missing token file is a usage error.
    assert!(!out.contains(TOKEN));
    let o = hotclock()
        .args(["import-plex", "--server", &mock.url(), "--list"])
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--token-file"));
    let log = mock.log.lock().unwrap();
    assert!(log.iter().all(|(m, t, _)| m == "GET" && !t.contains(TOKEN)));
}

#[test]
fn fill_art_backfill_from_the_command_line() {
    let mock = Mock::start((0..10).map(track).collect());
    let d = tmpdir("cli-art");
    let data = d.join("data");
    let tok = d.join("plex-token");
    std::fs::write(&tok, format!("{TOKEN}\n")).unwrap();
    let run = |args: &[&str]| {
        let o = hotclock()
            .arg("import-plex")
            .args(["--server", &mock.url(), "--token-file"])
            .arg(&tok)
            .arg("--data-dir")
            .arg(&data)
            .args(args)
            .output()
            .unwrap();
        (
            o.status.code(),
            String::from_utf8_lossy(&o.stdout).to_string(),
        )
    };
    let (c, out) = run(&["--playlist", "Office Mix", "--no-art"]);
    assert_eq!(c, Some(0), "{out}");
    assert!(!out.contains("art "), "{out}");
    assert_eq!(mock.thumb_requests(), 0);
    let (_, out) = run(&["--fill-art", "--all-plex-items", "--dry-run"]);
    println!("{out}");
    // Ten tracks, two per album: albums 0 and 1 have a picture, 2 is junk (a failure), 3 and 4 have none.
    assert!(
        out.contains("(dry run) checked 10, had art 0, Plex can fill 4, Plex has none 4, failed 2"),
        "{out}"
    );
    let (c, out) = run(&["--fill-art", "--all-plex-items"]);
    assert_eq!(c, Some(1), "a junk picture is a failure: {out}");
    assert!(
        out.contains("checked 10, had art 0, filled from Plex 4, Plex has none 4, failed 2"),
        "{out}"
    );
    let (_, out) = run(&["--fill-art", "--all-plex-items"]);
    println!("{out}");
    assert!(
        out.contains("checked 10, had art 4, filled from Plex 0"),
        "{out}"
    );
    let (c, out) = run(&["--fill-art"]);
    assert_eq!(c, Some(1));
    assert!(out.is_empty() || !out.contains("checked"));
}
