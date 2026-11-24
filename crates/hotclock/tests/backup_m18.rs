//! M1.8 acceptance 2, as real processes: run a station, take the snapshots with `hotclock backup`, delete both
//! databases, `hotclock restore`, and the station comes back with its playlist. Also: a restore refuses while a
//! station holds its lock, and does nothing without `--yes`.

mod support_api;
use std::process::{Command, Stdio};
use std::time::Duration;
use support_api::*;

fn hc(e: &Env, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_hotclock"))
        .arg("--global-config")
        .arg(&e.global)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn playing(e: &Env) -> String {
    wait_for("on air", 30.0, || {
        let s = get(&e.st(), "/api/v1/state").json();
        s["now"]["title"].is_string().then_some(())
    });
    get(&e.st(), "/health").json()["program"]["playlist"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[test]
fn delete_both_databases_restore_from_the_snapshots_and_the_station_resumes_its_playlist() {
    let mut e = Env::new("restore", &Opts::default());
    e.spawn_station(&[]);
    let pl = playing(&e);
    assert_eq!(pl, "Rot");
    // Let a few checkpoints and the first as-run row reach the database, then snapshot beside the running station.
    std::thread::sleep(Duration::from_millis(2500));
    let out = hc(&e, &["backup"]);
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("library-") && text.contains("station-office-"),
        "{text}"
    );
    assert!(text.contains("integrity ok"), "{text}");
    // The health page carries the snapshot fields.
    let h = get(&e.st(), "/health").json();
    // (the running station reads its status at start; `backup` from outside wrote the sidecar after that)
    assert!(h["db"].is_object(), "{h}");
    // Restore refuses while the station runs (it holds its lock), and changes nothing.
    let out = hc(&e, &["restore", "--yes", "--no-service-check"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("locked")
            || String::from_utf8_lossy(&out.stderr).contains("another process"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Stop it (a kill, as a crash would), lose both databases.
    let mut child = e.children.remove(0);
    child.kill().unwrap();
    child.wait().unwrap();
    let items_before = e.items.len() as i64;
    drop(std::mem::replace(
        &mut e.lib,
        library::Library::open(library::Paths::from_data_dir(&e.dir.join("scratch"))).unwrap(),
    ));
    for f in ["library.db", "library.db-wal", "library.db-shm"] {
        let _ = std::fs::remove_file(e.data.join(f));
    }
    for f in ["state.db", "state.db-wal", "state.db-shm"] {
        let _ = std::fs::remove_file(e.data.join("stations/office").join(f));
    }
    assert!(!e.data.join("library.db").exists());
    // Without --yes: a list, nothing changed.
    let out = hc(&e, &["restore", "--no-service-check"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("Nothing changed"));
    assert!(!e.data.join("library.db").exists());
    // With it.
    let out = hc(&e, &["restore", "--yes", "--no-service-check"]);
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(e.data.join("library.db").exists());
    assert!(e.data.join("stations/office/state.db").exists());
    let lib = library::Library::open_worker(library::Paths::from_data_dir(&e.data)).unwrap();
    let n: i64 = lib
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM items WHERE deleted_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, items_before, "the restored library has its items");
    drop(lib);
    // The station starts on the restored files and plays the same playlist.
    e.spawn_station(&[]);
    assert_eq!(playing(&e), "Rot");
    let s = get(&e.st(), "/api/v1/state").json();
    assert!(s["now"]["title"].is_string(), "{s}");
}

/// A restore runs as root; every file and directory it writes or creates under the data directory (the
/// databases, their `-wal`/`-shm`, the station's `state.db.lock`) ends up
/// owned like the rest of the state tree, or the station fails with "Permission denied". Needs root to chown.
#[test]
fn a_restore_leaves_what_it_writes_owned_by_the_owner_of_the_state_tree() {
    use std::os::unix::fs::MetadataExt;
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipped: needs root (chown)");
        return;
    }
    let mut e = Env::new("restore-owner", &Opts::default());
    e.spawn_station(&[]);
    playing(&e);
    std::thread::sleep(Duration::from_millis(1500));
    assert!(hc(&e, &["backup"]).status.success());
    let mut child = e.children.remove(0);
    child.kill().unwrap();
    child.wait().unwrap();
    drop(std::mem::replace(
        &mut e.lib,
        library::Library::open(library::Paths::from_data_dir(&e.dir.join("scratch"))).unwrap(),
    ));
    // The state tree belongs to a service user; the station's database and its lock are gone.
    let (uid, gid) = (4242, 4243);
    fn walk(p: &std::path::Path, f: &mut dyn FnMut(&std::path::Path)) {
        f(p);
        if p.is_dir() && !p.is_symlink() {
            for x in std::fs::read_dir(p).unwrap().flatten() {
                walk(&x.path(), f);
            }
        }
    }
    for f in ["state.db", "state.db-wal", "state.db-shm", "state.db.lock"] {
        let _ = std::fs::remove_file(e.data.join("stations/office").join(f));
    }
    walk(&e.data, &mut |p| {
        std::os::unix::fs::lchown(p, Some(uid), Some(gid)).unwrap()
    });
    let out = hc(&e, &["restore", "--yes", "--no-service-check"]);
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(e.data.join("stations/office/state.db").exists());
    assert!(e.data.join("stations/office/state.db.lock").exists());
    let mut wrong = vec![];
    walk(&e.data, &mut |p| {
        let m = std::fs::symlink_metadata(p).unwrap();
        if (m.uid(), m.gid()) != (uid, gid) {
            wrong.push(format!("{} {}:{}", p.display(), m.uid(), m.gid()));
        }
    });
    assert!(wrong.is_empty(), "owned by someone else: {wrong:?}");
}
