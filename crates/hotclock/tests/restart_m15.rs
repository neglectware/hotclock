//! M1.5 acceptance 6: kill the station process at a random point (20 trials, the real binary, a paced
//! output) and start it again. If the kill came more than 20 s before the item's segue point, the same
//! item resumes within 1 s of where it would have been; otherwise the next item plays; the queue order
//! after it is unchanged either way.

use engine::render::{render_tone, ToneRender};
use engine::wav::WavFormat;
use fixtures::testdir::KillOnDrop;
use library::playlists::{Mode, NewPlaylist};
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use station::state;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn tmpdir(name: &str) -> PathBuf {
    let d = fixtures::testdir::process_dir().join(format!("restart-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

const ITEM_S: f64 = 60.0;

fn spawn(global: &Path, name: &str) -> KillOnDrop {
    KillOnDrop(
        Command::new("nice")
            .args(["-n", "10"])
            .arg(env!("CARGO_BIN_EXE_hotclock"))
            .arg("--global-config")
            .arg(global)
            .args(["station", name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn hotclock"),
    )
}

fn wait_for<T>(what: &str, secs: f64, mut f: impl FnMut() -> Option<T>) -> T {
    let t = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            t.elapsed().as_secs_f64() < secs,
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn m15_6_twenty_kills_of_the_real_process() {
    let dir = tmpdir("kill");
    let data = dir.join("data");
    let lib = Library::open(Paths::from_data_dir(&data)).unwrap();
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut ids = vec![];
    for i in 0..6 {
        let p = src.join(format!("m{i}.wav"));
        render_tone(&ToneRender {
            out: p.clone(),
            rate: 48_000,
            seconds: ITEM_S,
            hz: 300.0 + 100.0 * f64::from(i),
            level_dbfs: -20.0,
            format: WavFormat::F32,
        })
        .unwrap();
        match import_file(
            &lib,
            &p,
            &ImportOptions {
                category: Some("music".into()),
                source: "test".into(),
            },
        ) {
            ImportOutcome::Done { item_id, .. } => ids.push(item_id),
            o => panic!("{o:?}"),
        }
    }
    let mut np = NewPlaylist::shuffle("Rot");
    np.mode = Mode::Shuffle;
    np.no_repeat_minutes = 0;
    let pl = lib.create_playlist(&np).unwrap();
    for i in &ids {
        lib.add_item_to_playlist(pl, *i, 1.0).unwrap();
    }
    let segue_ms = lib.item_effective(ids[0]).unwrap().segue;
    let cue_in_ms = lib.item_effective(ids[0]).unwrap().cue_in;
    println!("item length {ITEM_S} s, segue marker {segue_ms} ms, cue-in {cue_in_ms} ms");
    let stations = dir.join("stations");
    std::fs::create_dir_all(&stations).unwrap();
    let global = dir.join("hotclock.toml");
    std::fs::write(
        &global,
        format!(
            "data_dir = \"{}\"\nstations_dir = \"{}\"\nrun_dir = \"{}\"\n",
            data.display(),
            stations.display(),
            dir.join("run").display()
        ),
    )
    .unwrap();
    const TRIALS: usize = 20;
    const CONCURRENT: usize = 10;
    for t in 0..TRIALS {
        std::fs::write(
            stations.join(format!("t{t:02}.toml")),
            format!(
                "name = \"t{t:02}\"\nlisten = \"127.0.0.1:{}\"\ndevice = \"null\"\ndefault_playlist = \"Rot\"\n",
                8400 + t
            ),
        )
        .unwrap();
    }
    let global = &global;
    let data = &data;
    let mut results: Vec<String> = Vec::new();
    for batch in 0..TRIALS / CONCURRENT {
        let batch_results: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = (batch * CONCURRENT..(batch + 1) * CONCURRENT)
            .map(|t| {
                s.spawn(move || {
                    let name = format!("t{t:02}");
                    let state_db = data.join("stations").join(&name).join("state.db");
                    // Deterministic spread: kill 4 to 56 s after the item started, down 1 to 3 s.
                    let kill_after = 4.0 + (t as f64 * 2.63) % 52.0;
                    let down = 1.0 + (t as f64 * 0.37) % 2.0;
                    let mut child = spawn(global, &name);
                    let first = wait_for("the first as-run row", 30.0, || {
                        state::read_as_run(&state_db).ok().and_then(|r| r.first().cloned())
                    });
                    let orig_start = first.started_ms;
                    // Kill at kill_after seconds after the item's audible start.
                    let kill_at = orig_start + (kill_after * 1000.0) as i64;
                    while wall_ms() < kill_at {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    child.kill().unwrap();
                    let _ = child.wait();
                    let killed_wall = wall_ms();
                    std::thread::sleep(Duration::from_millis((down * 1000.0) as u64));
                    let saved = state::read_sched_readonly(&state_db).expect("saved scheduler state");
                    let on_air_id = saved.on_air.as_ref().map(|e| e.item_id);
                    let queue_before: Vec<i64> = saved.queue.iter().map(|e| e.item_id).collect();
                    assert_eq!(on_air_id, first.item_id, "trial {t}: saved on-air differs from the as-run row");
                    let restart_wall = wall_ms();
                    let mut child = spawn(global, &name);
                    let second = wait_for("the restarted as-run row", 30.0, || {
                        state::read_as_run(&state_db).ok().and_then(|r| r.get(1).cloned())
                    });
                    // After the first row: let the queue settle for a moment, then read it.
                    std::thread::sleep(Duration::from_millis(3500));
                    let after = state::read_sched_readonly(&state_db).expect("state after restart");
                    let queue_after: Vec<i64> = after.queue.iter().map(|e| e.item_id).collect();
                    let rows = state::read_as_run(&state_db).unwrap();
                    child.kill().unwrap();
                    let _ = child.wait();

                    // The expectation from what the log says: where the item would be when the
                    // restarted voice becomes audible.
                    let expected_pos = second.started_ms - orig_start + cue_in_ms;
                    let remaining = segue_ms - expected_pos;
                    let resumed = second.flags.iter().any(|f| f == "resumed");
                    let must_resume = remaining >= 20_000;
                    assert_eq!(rows[0].end_reason.as_deref(), Some("crash"), "trial {t}");
                    let line = if resumed {
                        let from: i64 = second
                            .flags
                            .iter()
                            .find_map(|f| f.strip_prefix("resumed_from_ms="))
                            .and_then(|v| v.parse().ok())
                            .expect("resumed_from_ms");
                        let err = (from - expected_pos).abs();
                        assert!(must_resume || remaining > 18_000, "trial {t}: resumed with only {remaining} ms left");
                        assert!(err < 1000, "trial {t}: resumed at {from} ms, expected {expected_pos} ms");
                        assert_eq!(second.item_id, first.item_id);
                        // The queue after the resumed item is the queue that was saved.
                        assert_eq!(&queue_after[..queue_before.len().min(queue_after.len()).min(3)], &queue_before[..queue_before.len().min(queue_after.len()).min(3)], "trial {t}");
                        format!("trial {t:2}: killed {kill_after:5.1} s in, down {:4.1} s (restart {} ms after the kill): resumed at {from} ms, expected {expected_pos} ms, error {err} ms", down, restart_wall - killed_wall)
                    } else {
                        assert!(!must_resume || remaining < 22_000, "trial {t}: did not resume with {remaining} ms left");
                        assert_eq!(second.item_id, queue_before.first().copied(), "trial {t}: the next item of the saved queue should play");
                        assert_eq!(&queue_after[..queue_before.len().min(queue_after.len()).min(3)], &queue_before[1..1 + queue_before.len().min(queue_after.len()).min(3)], "trial {t}: queue order after the next item");
                        format!("trial {t:2}: killed {kill_after:5.1} s in, down {:4.1} s: the next item played ({remaining} ms were left)", down)
                    };
                    line
                })
            })
            .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        results.extend(batch_results);
    }
    for r in &results {
        println!("{r}");
    }
    let resumed = results.iter().filter(|r| r.contains("resumed at")).count();
    println!("6: {TRIALS} kills of the real process: {resumed} resumed in place, {} started the next item", TRIALS - resumed);
    assert!(
        resumed >= 8 && TRIALS - resumed >= 4,
        "the trials should cover both cases"
    );
}

/// `hotclock playlist`: `insert-every --n 0` is refused (a block due at every boundary), and `list`
/// shows the window each rotation keeps (review of M1.5: the window is cut to fit the playlist).
#[test]
fn playlist_commands_refuse_n_below_one_and_list_the_window_kept() {
    let dir = tmpdir("plcmd");
    let data = dir.join("data");
    let lib = Library::open(Paths::from_data_dir(&data)).unwrap();
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut ids = vec![];
    for i in 0..4 {
        let p = src.join(format!("m{i}.wav"));
        render_tone(&ToneRender {
            out: p.clone(),
            rate: 48_000,
            seconds: 60.0,
            hz: 300.0 + 100.0 * f64::from(i),
            level_dbfs: -20.0,
            format: WavFormat::F32,
        })
        .unwrap();
        let opts = ImportOptions {
            category: Some("music".into()),
            source: "test".into(),
        };
        match import_file(&lib, &p, &opts) {
            ImportOutcome::Done { item_id, .. } => ids.push(item_id.to_string()),
            o => panic!("{o:?}"),
        }
    }
    let global = dir.join("hotclock.toml");
    std::fs::write(
        &global,
        format!(
            "data_dir = \"{}\"\nstations_dir = \"{}\"\nrun_dir = \"{}\"\n",
            data.display(),
            dir.join("stations").display(),
            dir.join("run").display()
        ),
    )
    .unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_hotclock"))
            .arg("--global-config")
            .arg(&global)
            .args(args)
            .output()
            .unwrap()
    };
    let o = run(&["playlist", "create", "Office", "--no-repeat-minutes", "120"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let mut add = vec!["playlist", "add", "Office"];
    add.extend(ids.iter().map(String::as_str));
    assert!(run(&add).status.success());
    let o = run(&[
        "playlist",
        "insert-every",
        "Office",
        "--n",
        "0",
        "--category",
        "music",
    ]);
    let err = String::from_utf8_lossy(&o.stderr);
    println!("insert-every --n 0: {err}");
    assert!(!o.status.success() && err.contains("at least 1"));
    let o = run(&["playlist", "list"]);
    let out = String::from_utf8_lossy(&o.stdout);
    println!("{out}");
    assert!(
        out.contains(
            "no_repeat_configured 120 min, no_repeat_effective 2 min (capped: 4 tracks, 4 min total)"
        ),
        "{out}"
    );
}
