//! The `restarted` alert (soak 2, 2026-10-07): it said 3.0 s and 3.9 s against 2.7 s measured, and it never
//! cleared, so both stations sat in `warning` for 72 hours. Now its figure is worded as an upper bound and it
//! clears itself after the station has been on air and healthy for `restart_alert_clear_s`.

use engine::render::{render_tone, ToneRender};
use engine::wav::WavFormat;
use library::playlists::{Mode, NewPlaylist};
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use station::alerts::{code, AlertChange, HealthStatus};
use station::offline::Offline;
use station::runtime::{Station, StationOptions};
use std::path::PathBuf;

const T0: i64 = 1_800_000_000_000;

struct W {
    dir: PathBuf,
    lib: Library,
}

fn world(name: &str) -> W {
    let dir = fixtures::testdir::process_dir().join(format!("ra-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let lib = Library::open(Paths::from_data_dir(&dir.join("data"))).unwrap();
    let mut ids = vec![];
    for i in 0..3 {
        let p = dir.join("src").join(format!("t{i}.wav"));
        render_tone(&ToneRender {
            out: p.clone(),
            rate: 48_000,
            seconds: 120.0,
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
    let mut pl = NewPlaylist::shuffle("Rot");
    pl.mode = Mode::Sequential;
    pl.no_repeat_minutes = 0;
    let pid = lib.create_playlist(&pl).unwrap();
    for i in ids {
        lib.add_item_to_playlist(pid, i, 1.0).unwrap();
    }
    W { dir, lib }
}

impl W {
    fn open(&self, wall: i64, clear_s: f64) -> Offline {
        let mut o = StationOptions::new("office", self.lib.paths.media.clone());
        o.default_playlist = Some("Rot".into());
        o.restart_alert_clear_s = clear_s;
        let (st, mixer) = Station::open(
            o,
            Some(self.dir.join("stations/office/state.db")),
            Some(self.lib.paths.db.clone()),
            wall,
        )
        .unwrap();
        let mut off = Offline::new(st, mixer, wall);
        off.keep_samples = false;
        off.start().unwrap();
        off
    }

    /// Run a station for `s` seconds, "kill" it, and bring a new one up `down_s` later.
    fn restarted(&self, s: f64, down_s: f64, clear_s: f64) -> Offline {
        let _ = std::fs::remove_dir_all(self.dir.join("stations"));
        let mut a = self.open(T0, clear_s);
        a.run_until(s, &[]).unwrap();
        a.station.flush();
        drop(a);
        self.open(T0 + ((s + down_s) * 1000.0) as i64, clear_s)
    }
}

#[test]
fn restarted_clears_after_ten_minutes_on_air_and_keeps_its_history() {
    let w = world("clear");
    let mut b = w.restarted(20.0, 2.5, 600.0);
    b.run_until(5.0, &[]).unwrap();
    let a = b.station.alerts();
    assert!(a.is_active(code::RESTARTED));
    assert_eq!(a.status(), HealthStatus::Warning);
    let msg = a
        .active()
        .into_iter()
        .find(|x| x.code == code::RESTARTED)
        .unwrap()
        .message
        .clone();
    println!("message: {msg}");
    assert!(msg.contains("no more than"), "{msg}");
    assert!(msg.contains("from the last checkpoint"), "{msg}");
    // 9 minutes on air: still there. 10 minutes and a little: gone.
    b.run_until(540.0, &[]).unwrap();
    assert!(b.station.alerts().is_active(code::RESTARTED), "9 minutes");
    b.run_until(620.0, &[]).unwrap();
    let a = b.station.alerts();
    assert!(!a.is_active(code::RESTARTED), "10 minutes");
    assert_eq!(a.status(), HealthStatus::Ok);
    assert_eq!(a.count_raised(code::RESTARTED), 1, "the history keeps it");
    assert!(a
        .history
        .iter()
        .any(|e| e.change == AlertChange::Cleared && e.alert.code == code::RESTARTED));
    // The as-run restart row is untouched.
    b.station.flush();
    let rows = station::state::read_as_run(&w.dir.join("stations/office/state.db")).unwrap();
    assert!(rows.iter().any(|r| r.origin == "restart"), "{rows:?}");
}

#[test]
fn restarted_stays_while_the_station_is_not_healthy_and_with_a_zero_setting() {
    let w = world("stay");
    // 0 keeps it until an operator acknowledges it.
    let mut b = w.restarted(20.0, 2.5, 0.0);
    b.run_until(900.0, &[]).unwrap();
    assert!(b.station.alerts().is_active(code::RESTARTED));
    assert!(b.station.ack(code::RESTARTED));
    assert!(!b.station.alerts().is_active(code::RESTARTED));

    // A critical alert restarts the count: the time is of healthy running.
    let mut c = w.restarted(20.0, 2.5, 60.0);
    c.run_until(30.0, &[]).unwrap();
    c.station.hook_raise_alert("test_critical", true);
    c.run_until(120.0, &[]).unwrap();
    assert!(
        c.station.alerts().is_active(code::RESTARTED),
        "not while a critical alert stands"
    );
    c.station.hook_clear_alert("test_critical");
    c.run_until(150.0, &[]).unwrap();
    assert!(
        c.station.alerts().is_active(code::RESTARTED),
        "only 30 s healthy"
    );
    c.run_until(200.0, &[]).unwrap();
    assert!(
        !c.station.alerts().is_active(code::RESTARTED),
        "60 s healthy"
    );
}

#[test]
fn a_second_restart_raises_it_again_with_the_new_figure() {
    let w = world("twice");
    let mut b = w.restarted(20.0, 2.5, 600.0);
    b.run_until(60.0, &[]).unwrap();
    b.station.flush();
    drop(b);
    let mut c = w.open(T0 + 20_000 + 2_500 + 60_000 + 4_000, 600.0);
    c.run_until(3.0, &[]).unwrap();
    let a = c.station.alerts();
    let m = a
        .active()
        .into_iter()
        .find(|x| x.code == code::RESTARTED)
        .map(|x| x.message.clone())
        .expect("raised again");
    println!("second: {m}");
    assert!(m.contains("no more than"), "{m}");
}
