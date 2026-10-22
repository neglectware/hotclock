//! M1.3 acceptance 5 through the real binary: `kill -9` of the importer during a copy leaves no
//! partial file under `media/` (only in `upload-tmp/`), and the next start cleans `upload-tmp/`.
//! Also: the CLI import end to end.

use library::{Library, Paths};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn hotclock() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hotclock"))
}

fn tmpdir(name: &str) -> PathBuf {
    let d = fixtures::testdir::process_dir().join(format!("cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut v = vec![];
    library::store::walk_files(dir, &mut v);
    v
}

#[test]
fn kill_during_a_copy_leaves_nothing_in_the_store() {
    let (fx, report) = fixtures::shared_dir(&["slowly_growing_deaf.flac"]);
    if !report.skipped.is_empty() {
        eprintln!("SKIPPED (no ffmpeg?)");
        return;
    }
    let src = fx.join("slowly_growing_deaf.flac");
    let data = tmpdir("kill").join("data");
    let paths = Paths::from_data_dir(&data);
    let mut child = fixtures::testdir::KillOnDrop(
        hotclock()
            .args(["import", "--data-dir"])
            .arg(&data)
            .arg(&src)
            .env("HOTCLOCK_TEST_COPY_DELAY_MS", "40")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    // Wait until a partial copy exists in upload-tmp with some bytes in it.
    let t0 = Instant::now();
    let partial = loop {
        let found = files_under(&paths.upload_tmp)
            .into_iter()
            .find(|p| std::fs::metadata(p).map_or(0, |m| m.len()) > 100_000);
        if let Some(p) = found {
            break p;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "no partial copy appeared"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let size_at_kill = std::fs::metadata(&partial).unwrap().len();
    let media_files_before = files_under(&paths.media).len();
    child.kill().unwrap(); // SIGKILL on Unix
    let status = child.wait().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(
        status.signal(),
        Some(9),
        "the importer must have died of SIGKILL"
    );
    println!(
        "killed with SIGKILL while copying: partial file in upload-tmp/ ({} of {} bytes), files under media/: {}",
        size_at_kill,
        std::fs::metadata(&src).unwrap().len(),
        files_under(&paths.media).len()
    );
    assert_eq!(media_files_before, 0);
    assert_eq!(
        files_under(&paths.media).len(),
        0,
        "no partial file may appear under media/"
    );
    assert!(
        !files_under(&paths.upload_tmp).is_empty(),
        "the remains stay in upload-tmp/"
    );
    // The next start cleans upload-tmp/ (a service start opens with cleaning), and there is no row.
    let lib = Library::open(paths.clone()).unwrap();
    assert_eq!(
        files_under(&paths.upload_tmp).len(),
        0,
        "upload-tmp/ is cleaned on start"
    );
    let n: i64 = lib
        .conn()
        .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
    drop(lib);
    // And the same file imports normally afterwards.
    let out = hotclock()
        .args(["import", "--data-dir"])
        .arg(&data)
        .arg(&src)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let lib = Library::open_worker(paths.clone()).unwrap();
    let n: i64 = lib
        .conn()
        .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(files_under(&paths.media).len(), 1);
}

#[test]
fn cli_import_and_analyze() {
    let (fx, report) =
        fixtures::shared_dir(&["tone_fade.flac", "cold_start.flac", "truncated.flac"]);
    if !report.skipped.is_empty() {
        eprintln!("SKIPPED (no ffmpeg?)");
        return;
    }
    let data = tmpdir("cli").join("data");
    let out = hotclock()
        .args(["import", "--data-dir"])
        .arg(&data)
        .arg(fx.join("tone_fade.flac"))
        .arg(fx.join("cold_start.flac"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    println!("{text}");
    assert!(out.status.success());
    assert!(text.contains("2 files: 2 imported, 0 duplicate, 0 failed"));
    // A second run finds duplicates; a truncated file fails with exit status 1.
    let out = hotclock()
        .args(["import", "--data-dir"])
        .arg(&data)
        .arg(fx.join("tone_fade.flac"))
        .arg(fx.join("truncated.flac"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(!out.status.success());
    assert!(
        text.contains("duplicate") && text.contains("FAILED") && text.contains("truncated"),
        "{text}"
    );
    // analyze prints JSON with the markers.
    let out = hotclock()
        .arg("analyze")
        .arg(fx.join("tone_fade.flac"))
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["cue_in"], 480);
    assert!(v["flags"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f == "cold_start"));
}
