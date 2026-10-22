//! M1.6 acceptance tests that need the real process: a corrupt `state.db` at start (7), a crash loop into
//! safe mode (6), the engine watchdog (5, with a supervisor standing in for systemd), and the sound card
//! unplugged for 10 s and for 60 s (10, with the test-hook wrapper around a paced file output).
//!
//! The target box steps (a real systemd unit for 5, a real USB dongle for 10) are in `docs/hardware-tests.md`;
//! what runs here is the same code path with the OS pieces replaced by test doubles, and every number
//! printed says so.

use engine::render::{render_tone, ToneRender};
use engine::wav::WavFormat;
use fixtures::testdir::KillOnDrop;
use library::playlists::{Mode, NewPlaylist};
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use station::state;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn tmpdir(name: &str) -> PathBuf {
    let d = fixtures::testdir::process_dir().join(format!("m16p-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Env {
    dir: PathBuf,
    global: PathBuf,
    state_db: PathBuf,
    starts: PathBuf,
}

/// A library of `n` music tones of `secs` seconds (playlist `Rot`, shuffle) and an emergency playlist of
/// two tones, and a station config `office` writing to `device`.
fn env(name: &str, n: usize, secs: f64, device: &str) -> Env {
    let dir = tmpdir(name);
    let data = dir.join("data");
    let lib = Library::open(Paths::from_data_dir(&data)).unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let add = |i: usize, hz: f64, secs: f64| -> i64 {
        let p = dir.join("src").join(format!("t{i}.wav"));
        render_tone(&ToneRender {
            out: p.clone(),
            rate: 48_000,
            seconds: secs,
            hz,
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
            ImportOutcome::Done { item_id, .. } => item_id,
            o => panic!("{o:?}"),
        }
    };
    let music: Vec<i64> = (0..n)
        .map(|i| add(i, 300.0 + 100.0 * i as f64, secs))
        .collect();
    let emerg: Vec<i64> = (0..2)
        .map(|i| add(100 + i, 1200.0 + 200.0 * i as f64, 40.0))
        .collect();
    let mk = |name: &str, mode: Mode, ids: &[i64]| {
        let mut p = NewPlaylist::shuffle(name);
        p.mode = mode;
        p.no_repeat_minutes = 0;
        let pl = lib.create_playlist(&p).unwrap();
        for i in ids {
            lib.add_item_to_playlist(pl, *i, 1.0).unwrap();
        }
    };
    mk("Rot", Mode::Shuffle, &music);
    mk("Emergency", Mode::Sequential, &emerg);
    let stations = dir.join("stations");
    std::fs::create_dir_all(&stations).unwrap();
    std::fs::write(
        stations.join("office.toml"),
        format!(
            "name = \"office\"\nlisten = \"127.0.0.1:8401\"\ndevice = \"{device}\"\ndefault_playlist = \"Rot\"\nemergency_playlist = \"Emergency\"\nram_loop_seconds = 60\n"
        ),
    )
    .unwrap();
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
    Env {
        state_db: data.join("stations").join("office").join("state.db"),
        starts: dir.join("run").join("office.starts"),
        dir,
        global,
    }
}

fn spawn_with(e: &Env, envs: &[(&str, String)]) -> KillOnDrop {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hotclock"));
    c.arg("--global-config")
        .arg(&e.global)
        .args(["station", "office", "--rt-priority", "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in envs {
        c.env(k, v);
    }
    KillOnDrop(c.spawn().expect("spawn hotclock"))
}

fn spawn(e: &Env) -> KillOnDrop {
    spawn_with(e, &[])
}

fn ctl(e: &Env, words: &[&str]) {
    let st = Command::new(env!("CARGO_BIN_EXE_hotclock"))
        .arg("--global-config")
        .arg(&e.global)
        .args(["ctl", "office"])
        .args(words)
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success());
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
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn rows(e: &Env) -> Vec<state::AsRunRow> {
    state::read_as_run(&e.state_db).unwrap_or_default()
}

fn has_alert(e: &Env, code: &str) -> bool {
    state::read_alerts(&e.state_db)
        .unwrap_or_default()
        .iter()
        .any(|a| a.1 == code)
}

// ---- 7. a garbage state.db at start ------------------------------------------------------------------------------

#[test]
fn m16_7_a_garbage_state_db_starts_the_station_within_five_seconds() {
    let e = env("corrupt", 3, 60.0, "null");
    std::fs::create_dir_all(e.state_db.parent().unwrap()).unwrap();
    std::fs::write(&e.state_db, vec![0x5Au8; 300_000]).unwrap();
    let t = Instant::now();
    let mut child = spawn(&e);
    let first = wait_for("the first as-run row", 20.0, || rows(&e).first().cloned());
    let up = t.elapsed().as_secs_f64();
    wait_for("state_db_corrupt", 5.0, || {
        has_alert(&e, "state_db_corrupt").then_some(())
    });
    child.kill().unwrap();
    let _ = child.wait();
    let aside = std::fs::read_dir(e.state_db.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|f| {
            f.file_name()
                .to_string_lossy()
                .starts_with("state.db.corrupt-")
        })
        .count();
    println!(
        "7: real process: the first as-run row {up:.2} s after the spawn (limit 5 s), origin {:?}, {aside} corrupt file(s) moved aside, state_db_corrupt written",
        first.origin
    );
    assert!(up < 5.0);
    assert_eq!(aside, 1);
}

// ---- 6. six crashes, safe mode, acknowledge -------------------------------------------------------------------------------

#[test]
fn m16_6_a_crash_loop_puts_the_station_in_safe_mode_until_it_is_acknowledged() {
    let e = env("crashloop", 4, 60.0, "null");
    let t0 = Instant::now();
    let mut states = vec![];
    for i in 1..=6 {
        let mut child = spawn(&e);
        wait_for("this start to be counted", 20.0, || {
            std::fs::read_to_string(&e.starts)
                .ok()
                .filter(|s| s.lines().count() >= i)
                .map(|_| ())
        });
        // Let it play for a moment, then kill it hard.
        wait_for("a checkpoint", 20.0, || {
            state::read_checkpoint_readonly(&e.state_db).filter(|c| wall_ms() - c.at_wall_ms < 3000)
        });
        std::thread::sleep(Duration::from_millis(600));
        let st = state::read_checkpoint_readonly(&e.state_db).map(|c| c.state);
        states.push(st.clone());
        child.kill().unwrap();
        let _ = child.wait();
    }
    println!(
        "6: six forced crashes in {:.1} s; the checkpoint's state at each: {states:?}",
        t0.elapsed().as_secs_f64()
    );
    assert!(t0.elapsed().as_secs() < 120);
    // The fifth start is already the crash loop.
    assert_eq!(states[4].as_deref(), Some("safe"), "{states:?}");
    assert_eq!(states[5].as_deref(), Some("safe"), "{states:?}");
    // A start after them: safe mode, playing the emergency playlist.
    let n0 = rows(&e).len();
    let mut child = spawn(&e);
    let row = wait_for("a row of the safe-mode start", 20.0, || {
        rows(&e)
            .get(n0..)
            .and_then(|r| r.iter().find(|r| r.origin != "restart").cloned())
    });
    println!(
        "6: safe-mode start plays {:?} (origin {}), flags {:?}",
        row.title, row.origin, row.flags
    );
    assert_eq!(row.origin, "emergency");
    assert!(has_alert(&e, "safe_mode"));
    // Acknowledging returns it to normal programming, at once.
    let n1 = rows(&e).len();
    let t_ack = Instant::now();
    ctl(&e, &["ack", "safe_mode"]);
    let back = wait_for("normal programming", 20.0, || {
        rows(&e)
            .get(n1..)
            .and_then(|r| r.iter().find(|r| r.origin == "rotation").cloned())
    });
    println!(
        "6: acknowledged safe_mode; the first rotation item began {:.1} s later ({})",
        t_ack.elapsed().as_secs_f64(),
        back.title
    );
    let cp = wait_for("a playing checkpoint", 10.0, || {
        state::read_checkpoint_readonly(&e.state_db).filter(|c| c.state == "playing")
    });
    assert_eq!(cp.state, "playing");
    assert!(!e.starts.exists() || std::fs::read_to_string(&e.starts).unwrap().lines().count() <= 1);
    child.kill().unwrap();
    let _ = child.wait();
}

// ---- 5. the engine watchdog -------------------------------------------------------------------------------------------------

/// A stand-in for systemd's watchdog: reads `sd_notify` datagrams, and if no `WATCHDOG=1` comes for
/// `WatchdogSec` (10 s) while a process is being supervised, kills it (systemd would send SIGABRT and
/// restart it; SIGKILL is the same for the purpose).
struct Supervisor {
    ready: Arc<Mutex<Vec<Instant>>>,
    killed_at: Arc<Mutex<Option<Instant>>>,
    pid: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Supervisor {
    /// `honour_trigger`: act on `WATCHDOG=trigger` at once, as systemd 243 and later do; false for an older
    /// systemd, where only the feed stopping ends the process.
    fn start(sock: &Path, watchdog: Duration, honour_trigger: bool) -> Self {
        let _ = std::fs::remove_file(sock);
        let srv = UnixDatagram::bind(sock).unwrap();
        srv.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let ready = Arc::new(Mutex::new(Vec::new()));
        let killed_at = Arc::new(Mutex::new(None));
        let pid = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (r2, k2, p2, s2) = (ready.clone(), killed_at.clone(), pid.clone(), stop.clone());
        let join = std::thread::spawn(move || {
            let mut last = Instant::now();
            let mut buf = [0u8; 256];
            let mut armed = false;
            let mut triggered = false;
            while !s2.load(Ordering::SeqCst) {
                if let Ok(n) = srv.recv(&mut buf) {
                    let m = String::from_utf8_lossy(&buf[..n]).to_string();
                    if m.starts_with("READY=1") {
                        r2.lock().unwrap().push(Instant::now());
                        armed = true;
                        last = Instant::now();
                    }
                    if m.starts_with("WATCHDOG=1") {
                        last = Instant::now();
                    }
                    if m.starts_with("WATCHDOG=trigger") && honour_trigger {
                        triggered = true;
                    }
                }
                let p = p2.load(Ordering::SeqCst);
                if armed && p != 0 && (triggered || last.elapsed() > watchdog) {
                    triggered = false;
                    let _ = Command::new("kill").args(["-9", &p.to_string()]).status();
                    *k2.lock().unwrap() = Some(Instant::now());
                    armed = false;
                }
            }
        });
        Self {
            ready,
            killed_at,
            pid,
            stop,
            join: Some(join),
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// The unit's `RestartSec` (architecture 10): systemd starts the process again this long after it died.
const RESTART_SEC: f64 = 2.0;

fn stall_run(name: &str, honour_trigger: bool) -> (f64, f64, bool) {
    let e = env(name, 4, 60.0, "null");
    let sock = e.dir.join("notify.sock");
    let envs = |sock: &Path| {
        vec![
            ("NOTIFY_SOCKET", sock.display().to_string()),
            ("WATCHDOG_USEC", "10000000".to_string()),
            ("HOTCLOCK_TEST_HOOKS", "1".to_string()),
        ]
    };
    let sup = Supervisor::start(&sock, Duration::from_secs(10), honour_trigger);
    let mut child = spawn_with(&e, &envs(&sock));
    sup.pid.store(child.id(), Ordering::SeqCst);
    wait_for("READY", 20.0, || sup.ready.lock().unwrap().first().copied());
    std::thread::sleep(Duration::from_secs(4)); // fed a couple of times
    assert!(
        sup.killed_at.lock().unwrap().is_none(),
        "the watchdog is fed while healthy"
    );
    let t_inject = Instant::now();
    ctl(&e, &["hook", "stall", "60000"]);
    let killed = wait_for("the watchdog to expire", 30.0, || {
        *sup.killed_at.lock().unwrap()
    });
    let kill_after = killed.duration_since(t_inject).as_secs_f64();
    let _ = child.wait();
    let stall_alert = has_alert(&e, "engine_stall");
    // What systemd does next: start it again after `RestartSec`.
    std::thread::sleep(Duration::from_secs_f64(RESTART_SEC));
    let _ = std::fs::remove_file(&e.starts);
    let mut child2 = spawn_with(&e, &envs(&sock));
    sup.pid.store(child2.id(), Ordering::SeqCst);
    let ready2 = wait_for("READY after the restart", 20.0, || {
        sup.ready.lock().unwrap().get(1).copied()
    });
    let back_after = ready2.duration_since(t_inject).as_secs_f64();
    wait_for("the restarted alert", 10.0, || {
        has_alert(&e, "restarted").then_some(())
    });
    // The message says the figure is an upper bound and where it starts: it is measured from the last checkpoint.
    let msg = state::read_alerts(&e.state_db)
        .unwrap_or_default()
        .into_iter()
        .find(|a| a.1 == "restarted")
        .map(|a| a.3)
        .unwrap_or_default();
    println!("restarted alert: {msg}");
    assert!(
        msg.contains("no more than") && msg.contains("from the last checkpoint"),
        "{msg}"
    );
    child2.kill().unwrap();
    let _ = child2.wait();
    (kill_after, back_after, stall_alert)
}

#[test]
fn m16_5_a_stalled_engine_stops_the_watchdog_and_the_process_is_restarted() {
    let (kill_after, back_after, stall_alert) = stall_run("stall", true);
    println!(
        "5: real process, a supervisor standing in for systemd (WatchdogSec 10, RestartSec 2, WATCHDOG=trigger honoured): rt-out stalled for 60 s at t=0 (the ctl command is read within 0.5 s); killed {kill_after:.1} s later; the new process sent READY {back_after:.1} s after the injection (limit 13 s); engine_stall recorded: {stall_alert}"
    );
    assert!(back_after <= 13.0, "{back_after}");
    assert!(
        kill_after <= 5.0,
        "the trigger acts at the 3 s stall, not at WatchdogSec: {kill_after}"
    );
    assert!(stall_alert);
}

#[test]
fn m16_5b_without_the_trigger_the_stopped_feed_still_ends_it() {
    // A systemd that ignores `WATCHDOG=trigger` (older than 243): the feed stops 1 s after the stall, so the
    // watchdog runs out at most 11 s after it began (plus the 0.5 s the ctl command takes to be read).
    let (kill_after, back_after, stall_alert) = stall_run("stall-b", false);
    println!(
        "5b: the same without the trigger: killed {kill_after:.1} s after the injection (the feed backstop; at most 11.5 s), the new process sent READY {back_after:.1} s after it"
    );
    assert!(kill_after <= 12.0, "{kill_after}");
    assert!(stall_alert);
}

// ---- 10. the sound card unplugged --------------------------------------------------------------------------------------------

#[test]
fn m16_10_an_unplugged_card_resumes_the_same_position_after_10_s_and_the_next_item_after_60_s() {
    let e = env("unplug", 4, 240.0, "file-paced:@OUT@");
    // The device path goes into the config after the directory is known.
    let out = e.dir.join("out.wav");
    let cfg = e.dir.join("stations").join("office.toml");
    let txt = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("@OUT@", &out.display().to_string());
    std::fs::write(&cfg, txt).unwrap();
    let mut child = spawn_with(&e, &[("HOTCLOCK_TEST_HOOKS", "1".into())]);
    let first = wait_for("the first item", 30.0, || {
        rows(&e).into_iter().find(|r| r.origin == "rotation")
    });
    let cp = |min_pos: i64| {
        wait_for("a checkpoint", 30.0, || {
            state::read_checkpoint_readonly(&e.state_db).filter(|c| {
                c.position_ms.unwrap_or(0) >= min_pos && wall_ms() - c.at_wall_ms < 2500
            })
        })
    };
    let c0 = cp(4000);
    println!(
        "10: on air {:?} at {} ms",
        first.title,
        c0.position_ms.unwrap()
    );

    // 10 s unplugged.
    let t_unplug = Instant::now();
    ctl(&e, &["hook", "unplug"]);
    std::thread::sleep(Duration::from_secs(10));
    ctl(&e, &["hook", "plug"]);
    let outage = t_unplug.elapsed().as_secs_f64();
    std::thread::sleep(Duration::from_secs(6));
    let c1 = cp(0);
    let wall_dt = (c1.at_wall_ms - c0.at_wall_ms) as f64 / 1000.0;
    let pos_dt = (c1.position_ms.unwrap() - c0.position_ms.unwrap()) as f64 / 1000.0;
    let paused = wall_dt - pos_dt;
    println!(
        "10: after a 10 s outage (unplugged {outage:.1} s, back and running {:.1} s later): wall {wall_dt:.1} s, item position {pos_dt:.1} s, so the position was held for {paused:.1} s; same item {}; rows {}",
        6.0,
        c1.item_id == c0.item_id,
        rows(&e).iter().filter(|r| r.origin == "rotation").count()
    );
    assert_eq!(c1.item_id, c0.item_id, "the same item resumes");
    assert!(pos_dt > 1.0, "it is playing again");
    assert!(
        (8.0..=18.0).contains(&paused),
        "the position paused for {paused} s"
    );
    assert!(
        child.try_wait().unwrap().is_none(),
        "the process was not restarted"
    );
    assert!(!has_alert(&e, "restarted"));
    let n_before = rows(&e).iter().filter(|r| r.origin == "rotation").count();

    // 60 s unplugged.
    let t_unplug = Instant::now();
    ctl(&e, &["hook", "unplug"]);
    std::thread::sleep(Duration::from_secs(60));
    ctl(&e, &["hook", "plug"]);
    let t_plug = Instant::now();
    let next = wait_for("the next item", 20.0, || {
        let r = rows(&e);
        let rot: Vec<_> = r.iter().filter(|r| r.origin == "rotation").collect();
        (rot.len() > n_before).then(|| rot.last().cloned().cloned().unwrap())
    });
    let lag = t_plug.elapsed().as_secs_f64();
    // The interrupted item's row is closed when its (30 ms) fade has been heard: wait for it.
    let prev = wait_for("the interrupted item's row to close", 10.0, || {
        rows(&e)
            .into_iter()
            .filter(|r| r.origin == "rotation")
            .rev()
            .nth(1)
            .filter(|r| r.end_reason.is_some())
    });
    println!(
        "10: after a 60 s outage (unplugged {:.1} s): the next item {:?} began {lag:.1} s after the plug; the interrupted one ended {:?}; process alive {}",
        t_unplug.elapsed().as_secs_f64(),
        next.title,
        prev.end_reason,
        child.try_wait().unwrap().is_none()
    );
    assert_ne!(next.item_id, prev.item_id);
    assert_eq!(prev.end_reason.as_deref(), Some("device"));
    assert!(
        child.try_wait().unwrap().is_none(),
        "the process was not restarted"
    );
    assert!(!has_alert(&e, "restarted"));
    child.kill().unwrap();
    let _ = child.wait();
}
