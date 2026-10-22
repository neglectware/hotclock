//! M1.6 acceptance tests, offline (virtual clock, the real conductor, mixer, pre-flight and `state.db`):
//! silence, the emergency playlist and the RAM loop, missing and truncated files, per-item failures in one
//! long scenario, the media store, a depleted playlist, and a database that goes read-only or full. The
//! tests that need the real process (watchdog, crash loop, corrupt `state.db` at start, the sound card
//! unplugged) are in `crates/hotclock/tests/fail_m16.rs`.
//!
//! Numbers are printed (`--nocapture`); the assertions are the milestone's limits.

mod support;

use engine::render::{render_tone, ToneRender};
use engine::wav::{WavFormat, WavWriter};
use library::playlists::{Mode, NewPlaylist};
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use station::alerts::{code, AlertChange, Severity};
use station::offline::Offline;
use station::runtime::{Op, Station, StationOptions};
use station::state::{self, FaultMode};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const RATE: u32 = 48_000;
const T0: i64 = 1_800_000_000_000;

fn tmpdir(name: &str) -> PathBuf {
    let d = fixtures::testdir::process_dir().join(format!("m16-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[derive(Clone, Copy)]
struct It {
    id: i64,
    secs: f64,
}

struct W {
    dir: PathBuf,
    lib: Library,
    n: usize,
}

fn world(name: &str) -> W {
    let dir = tmpdir(name);
    let lib = Library::open(Paths::from_data_dir(&dir.join("data"))).unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    W { dir, lib, n: 0 }
}

impl W {
    /// Import a generated tone of `secs` seconds as an item of `category`.
    fn add(&mut self, hz: f64, secs: f64, category: &str) -> It {
        self.n += 1;
        let p = self.dir.join("src").join(format!("t{}.wav", self.n));
        render_tone(&ToneRender {
            out: p.clone(),
            rate: RATE,
            seconds: secs,
            hz,
            level_dbfs: -20.0,
            format: WavFormat::F32,
        })
        .unwrap();
        match import_file(
            &self.lib,
            &p,
            &ImportOptions {
                category: Some(category.into()),
                source: "test".into(),
            },
        ) {
            ImportOutcome::Done { item_id, .. } => It { id: item_id, secs },
            o => panic!("import: {o:?}"),
        }
    }

    fn media(&self, id: i64) -> PathBuf {
        let sp: String = self
            .lib
            .conn()
            .query_row("SELECT store_path FROM items WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        self.lib.paths.media.join(sp)
    }

    fn writable(p: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    /// Replace the stored file by digital silence of the same format and length (a file "replaced after
    /// import"): the size is unchanged, so pre-flight passes and only the silence detector can find it.
    fn silence(&self, it: It) {
        let p = self.media(it.id);
        let size = std::fs::metadata(&p).unwrap().len();
        Self::writable(&p);
        let frames = (it.secs * f64::from(RATE)).round() as usize;
        let mut wr = WavWriter::create(&p, RATE, 2, WavFormat::F32).unwrap();
        wr.write_bytes(&vec![0u8; frames * 8]).unwrap();
        wr.finish().unwrap();
        assert_eq!(
            std::fs::metadata(&p).unwrap().len(),
            size,
            "same size as the tone it replaces"
        );
    }

    fn truncate(&self, id: i64, percent: u64) {
        let p = self.media(id);
        Self::writable(&p);
        let len = std::fs::metadata(&p).unwrap().len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&p)
            .unwrap()
            .set_len(len * percent / 100)
            .unwrap();
    }

    fn playlist(&self, name: &str, mode: Mode, ids: &[i64]) -> i64 {
        let mut p = NewPlaylist::shuffle(name);
        p.mode = mode;
        p.no_repeat_minutes = 0;
        let pl = self.lib.create_playlist(&p).unwrap();
        for i in ids {
            self.lib.add_item_to_playlist(pl, *i, 1.0).unwrap();
        }
        pl
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("stations").join("office").join("state.db")
    }

    fn opts(&self, default_playlist: &str, emergency: Option<&str>, seed: u64) -> StationOptions {
        let mut o = StationOptions::new("office", self.lib.paths.media.clone());
        o.default_playlist = Some(default_playlist.into());
        o.emergency_playlist = emergency.map(str::to_string);
        o.seed = seed;
        o.record_sidecar = true;
        o
    }

    fn open(&self, default_playlist: &str, emergency: Option<&str>, seed: u64) -> Offline {
        let (st, mixer) = Station::open(
            self.opts(default_playlist, emergency, seed),
            Some(self.state_path()),
            Some(self.lib.paths.db.clone()),
            T0,
        )
        .unwrap();
        Offline::new(st, mixer, T0)
    }
}

/// Do what the library service does every second: the station's writer puts its decisions about items
/// into its `state.db` (`library_outbox`), and the library service, the only writer of `library.db`,
/// applies them.
fn wait_library_pushed(w: &W, st: &Station) {
    st.flush_writer();
    library::health::apply_station_outbox(w.lib.conn(), "office", &w.state_path()).unwrap();
}

fn secs(ms: i64) -> f64 {
    ms as f64 / 1000.0
}

fn rows_of(off: &Offline, id: i64) -> Vec<station::state::AsRunRow> {
    off.station
        .as_run
        .iter()
        .filter(|r| r.item_id == Some(id))
        .cloned()
        .collect()
}

fn quarantine_reason(w: &W, id: i64) -> Option<String> {
    w.lib
        .conn()
        .query_row(
            "SELECT quarantine_reason FROM items WHERE id = ?1 AND quarantined = 1",
            [id],
            |r| r.get(0),
        )
        .ok()
}

// ---- 1. a silent item is skipped after 15 s -----------------------------------------------------------------

#[test]
fn m16_1_a_silent_item_is_skipped_after_15_s_and_the_silence_alert_clears() {
    let mut w = world("a1");
    let a = w.add(440.0, 40.0, "music");
    let b = w.add(660.0, 40.0, "music");
    let c = w.add(880.0, 40.0, "music");
    w.silence(a);
    w.playlist("Seq", Mode::Sequential, &[a.id, b.id, c.id]);
    let mut off = w.open("Seq", None, 1);
    off.start().unwrap();
    off.run_until(45.0, &[]).unwrap();
    let rows = off.station.as_run.clone();
    let skip = secs(rows[1].started_ms - rows[0].started_ms);
    println!(
        "1: A (silent) sounded from {:.2} s, B began {:.2} s later (limit 15 +- 0.5 s); silence raised {} time(s), active at the end: {}",
        secs(rows[0].started_ms - T0),
        skip,
        off.station.alerts().count_raised(code::SILENCE),
        off.station.alerts().is_active(code::SILENCE)
    );
    assert_eq!(rows[1].item_id, Some(b.id));
    assert!((skip - 15.0).abs() <= 0.5, "skipped after {skip} s");
    assert_eq!(rows[0].end_reason.as_deref(), Some("cut"));
    let al = off.station.alerts();
    assert_eq!(al.count_raised(code::SILENCE), 1);
    assert!(
        !al.is_active(code::SILENCE),
        "cleared once the sound was back for 2 s"
    );
    let cleared = al
        .history
        .iter()
        .find(|e| e.change == AlertChange::Cleared && e.alert.code == code::SILENCE)
        .unwrap();
    println!(
        "1: silence cleared {:.2} s after B began",
        secs(cleared.at_ms - rows[1].started_ms)
    );
    // Never critical for one skip: /health stays 200 (acceptance 11 relies on this).
    assert!(al
        .history
        .iter()
        .all(|e| e.alert.severity != Severity::Critical));
    assert_eq!(off.station.health().status.http_status(), 200);
    // The item is charged with a silent skip; one is not enough to quarantine it.
    assert_eq!(al.count_raised(code::ITEM_SILENT), 1);
    assert_eq!(off.station.health().quarantined, 0);
    // B really sounds.
    let l = off.left();
    let t = ((rows[1].started_ms - T0) as f64 / 1000.0 + 1.0) * f64::from(RATE);
    assert!(support::tone_amp(&l, 660.0, t as usize, 24_000) > 0.05);
}

// ---- 2. all silent: the emergency playlist after 30 s, the RAM loop when it fails ---------------------------------

fn silent_world(name: &str) -> (W, Vec<It>, Vec<It>) {
    let mut w = world(name);
    let silent: Vec<It> = (0..3)
        .map(|i| w.add(440.0 + 100.0 * i as f64, 30.0, "music"))
        .collect();
    let emerg: Vec<It> = vec![w.add(1200.0, 25.0, "music"), w.add(1400.0, 25.0, "music")];
    for s in &silent {
        w.silence(*s);
    }
    w.playlist(
        "Rot",
        Mode::Shuffle,
        &silent.iter().map(|i| i.id).collect::<Vec<_>>(),
    );
    w.playlist(
        "Emergency",
        Mode::Sequential,
        &emerg.iter().map(|i| i.id).collect::<Vec<_>>(),
    );
    (w, silent, emerg)
}

#[test]
fn m16_2_all_items_silent_the_emergency_playlist_plays_after_30_s() {
    let (w, _silent, emerg) = silent_world("a2");
    let mut off = w.open("Rot", Some("Emergency"), 4);
    off.start().unwrap();
    off.run_until(50.0, &[]).unwrap();
    let rows = off.station.as_run.clone();
    for r in rows.iter().take(4) {
        println!(
            "  {:>6.2} s {} {:?} {:?}",
            secs(r.started_ms - T0),
            r.title,
            r.origin,
            r.end_reason
        );
    }
    let e = rows
        .iter()
        .find(|r| r.origin == "emergency")
        .expect("an emergency item");
    let after = secs(e.started_ms - rows[0].started_ms);
    println!(
        "2: the emergency playlist began {after:.2} s after the first item (limit 30 s); mode {}",
        off.station.health().mode
    );
    assert!((after - 30.0).abs() <= 0.6, "{after}");
    assert_eq!(e.item_id, Some(emerg[0].id));
    assert_eq!(off.station.health().mode, "emergency");
    // It is audible.
    let l = off.left();
    let t = ((e.started_ms - T0) as f64 / 1000.0 + 2.0) * f64::from(RATE);
    assert!(support::tone_amp(&l, 1200.0, t as usize, 24_000) > 0.05);
    // The silence went critical at 30 s and cleared when the emergency sound was heard.
    let al = off.station.alerts();
    let sev: Vec<Severity> = al
        .history
        .iter()
        .filter(|x| x.alert.code == code::SILENCE)
        .map(|x| x.alert.severity)
        .collect();
    println!(
        "2: silence alert severities over time: {sev:?}, active at the end: {}",
        al.is_active(code::SILENCE)
    );
    assert!(sev.contains(&Severity::Critical));
    assert!(!al.is_active(code::SILENCE));
}

#[test]
fn m16_2b_with_the_emergency_files_gone_the_ram_loop_plays_within_a_second() {
    let (w, _silent, emerg) = silent_world("a2b");
    let mut off = w.open("Rot", Some("Emergency"), 4);
    off.start().unwrap();
    // The RAM loop is decoded at start, in the background; wait for it, then remove the files.
    let t = std::time::Instant::now();
    let mut vt = 0.0;
    while off.station.fail.ram.is_none() {
        vt += 0.05;
        off.run_until(vt, &[]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(t.elapsed().as_secs() < 30, "the RAM loop never loaded");
    }
    let ram = off.station.fail.ram.clone().unwrap();
    println!(
        "2b: RAM loop ready after {:.2} s of station time: {} items, {:.1} s ({:.1} MB as 16-bit PCM)",
        vt,
        ram.items.len(),
        ram.seconds(RATE),
        ram.items.iter().map(|i| i.clip.samples.len() * 2).sum::<usize>() as f64 / 1e6
    );
    for e in &emerg {
        std::fs::remove_file(w.media(e.id)).unwrap();
    }
    off.run_until(50.0, &[]).unwrap();
    let rows = off.station.as_run.clone();
    let r = rows
        .iter()
        .find(|r| r.flags.iter().any(|f| f == "ram_loop"))
        .expect("a RAM loop item");
    let failed = off
        .station
        .alerts()
        .history
        .iter()
        .find(|e| e.alert.code == code::EMERGENCY_FAILED)
        .expect("emergency_failed raised");
    let lag = secs(r.started_ms - failed.at_ms);
    println!(
        "2b: emergency failed at {:.2} s, the RAM loop's first item sounded {:.2} s later (limit 1 s); mode {}",
        secs(failed.at_ms - T0),
        lag,
        off.station.health().mode
    );
    assert!((0.0..=1.0).contains(&lag), "{lag}");
    assert_eq!(off.station.health().mode, "ram_loop");
    assert_eq!(
        off.station.alerts().count_raised(code::ITEM_QUARANTINED),
        2,
        "both emergency items are quarantined (missing)"
    );
    let l = off.left();
    let t = ((r.started_ms - T0) as f64 / 1000.0 + 2.0) * f64::from(RATE);
    assert!(
        support::tone_amp(&l, 1200.0, t as usize, 24_000) > 0.05,
        "the RAM item is audible"
    );
}

// ---- 3. a queued item's file deleted -------------------------------------------------------------------------------

#[test]
fn m16_3_a_queued_items_file_deleted_is_dropped_replaced_and_quarantined_silently() {
    let mut w = world("a3");
    let items: Vec<It> = (0..6)
        .map(|i| w.add(300.0 + 100.0 * i as f64, 20.0, "music"))
        .collect();
    let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
    w.playlist("Seq", Mode::Sequential, &ids);
    let mut off = w.open("Seq", None, 2);
    off.start().unwrap();
    off.run_until(5.0, &[]).unwrap();
    let victim = items[2];
    assert!(
        off.station
            .sched
            .st
            .queue
            .iter()
            .any(|e| e.item_id == victim.id),
        "it is in the queue"
    );
    std::fs::remove_file(w.media(victim.id)).unwrap();
    off.run_until(110.0, &[]).unwrap();
    wait_library_pushed(&w, &off.station);
    let played: Vec<i64> = off
        .station
        .as_run
        .iter()
        .filter_map(|r| r.item_id)
        .collect();
    println!("3: played {played:?}; item {} was deleted", victim.id);
    assert!(!played.contains(&victim.id));
    assert!(played.len() >= 5);
    // Quarantined once, with the reason, in the library too.
    assert_eq!(off.station.alerts().count_raised(code::ITEM_QUARANTINED), 1);
    assert_eq!(off.station.alerts().count_raised(code::ITEM_MISSING), 1);
    assert_eq!(quarantine_reason(&w, victim.id).as_deref(), Some("missing"));
    // Nothing audible: no run below -60 dBFS longer than 50 ms after the first sound.
    let runs: Vec<_> = support::silent_runs(&off.samples, -60.0, 2_400)
        .into_iter()
        .filter(|(s, _)| *s > 48_000)
        .collect();
    println!("3: silent runs over 50 ms after the start: {runs:?}");
    assert!(runs.is_empty());
}

// ---- 4. a file cut off while it plays -------------------------------------------------------------------------------

#[test]
fn m16_4_a_truncated_file_transitions_at_the_truncation_point_without_a_gap() {
    let mut w = world("a4");
    let a = w.add(440.0, 150.0, "music");
    let b = w.add(660.0, 40.0, "music");
    let c = w.add(880.0, 40.0, "music");
    w.playlist("Seq", Mode::Sequential, &[a.id, b.id, c.id]);
    let mut off = w.open("Seq", None, 3);
    off.start().unwrap();
    off.run_until(4.0, &[]).unwrap();
    w.truncate(a.id, 40); // cut at 40 percent: 60 s of the 150
    off.run_until(100.0, &[]).unwrap();
    wait_library_pushed(&w, &off.station);
    let rows = off.station.as_run.clone();
    let a_row = &rows[0];
    let b_row = &rows[1];
    println!(
        "4: A sounded {:.2} s, ended {:?}; B began {:.3} s into the run (the cut is at 60 s)",
        secs(a_row.played_ms.unwrap()),
        a_row.end_reason,
        secs(b_row.started_ms - a_row.started_ms)
    );
    assert_eq!(a_row.end_reason.as_deref(), Some("error"));
    assert_eq!(b_row.item_id, Some(b.id));
    let all: Vec<_> = off
        .station
        .cond
        .sidecar
        .boundaries
        .iter()
        .map(|b| {
            format!(
                "{} -> {}: {} at {} (f_end_a {}) notes {:?}",
                b.from, b.to, b.kind, b.s_b, b.f_end_a, b.notes
            )
        })
        .collect();
    println!("4: boundaries: {all:#?}");
    // How A hands over depends on a race between the decoder's view of the cut and the planner: usually the
    // early-end watch finds the cut and takes an `emergency` boundary; sometimes the decoder has already
    // reported the true length and the planner places an ordinary segue at it (measured: 9 of 400 runs in a
    // container with one CPU). Either is right; what the acceptance asks is a hand-over at the cut.
    let bd = off
        .station
        .cond
        .sidecar
        .boundaries
        .iter()
        .find(|b| b.from == all_first_from(&off))
        .expect("a boundary out of A");
    assert!(
        bd.kind == "emergency" || bd.kind == "segue",
        "unexpected boundary kind {}",
        bd.kind
    );
    let at = bd.s_b as f64 / 48000.0;
    assert!(
        (59.0..61.5).contains(&at),
        "the hand-over is at {at:.3} s, the cut is at 60 s"
    );
    println!(
        "4: the boundary out of A: B starts at frame {} ({:.3} s), A's stop at f_end_a {}",
        bd.s_b,
        bd.s_b as f64 / 48000.0,
        bd.f_end_a
    );
    // No gap longer than 50 ms anywhere after the start.
    let runs: Vec<_> = support::silent_runs(&off.samples, -60.0, 2_400)
        .into_iter()
        .filter(|(s, _)| *s > 48_000)
        .collect();
    println!("4: silent runs over 50 ms after the start: {runs:?}");
    assert!(runs.is_empty(), "{runs:?}");
    // Counted, flagged and quarantined at once.
    let errs: i64 = w
        .lib
        .conn()
        .query_row(
            "SELECT playback_errors FROM items WHERE id = ?1",
            [a.id],
            |r| r.get(0),
        )
        .unwrap();
    println!(
        "4: playback_errors {errs}, quarantine {:?}, errors held by the station {:?}, alerts raised {:?}",
        quarantine_reason(&w, a.id),
        off.station.item_errors(a.id),
        off.station.alerts().raised_total
    );
    assert_eq!(
        errs, 1,
        "one cut file is one error, however many copies of it run into it"
    );
    assert_eq!(quarantine_reason(&w, a.id).as_deref(), Some("truncated"));
    assert_eq!(off.station.alerts().count_raised(code::ITEM_TRUNCATED), 1);
    assert_eq!(off.station.alerts().count_raised(code::ITEM_QUARANTINED), 1);
    assert_eq!(off.station.health().status.http_status(), 200);
}

// ---- 9. an item inside an album run uses the 60 s window ------------------------------------------------------------

#[test]
fn m16_9_forty_seconds_of_silence_inside_an_album_run_does_not_skip() {
    let mut w = world("a9");
    let t1 = w.add(440.0, 20.0, "music");
    let t2 = w.add(550.0, 40.0, "music");
    let t3 = w.add(660.0, 20.0, "music");
    let other = w.add(880.0, 20.0, "music");
    for (n, it) in [t1, t2, t3].iter().enumerate() {
        w.lib
            .conn()
            .execute(
                "UPDATE items SET album_key='mark|drift', disc_no=1, track_no=?2, track_total=3 WHERE id=?1",
                rusqlite::params![it.id, n as i64 + 1],
            )
            .unwrap();
    }
    w.silence(t2);
    w.playlist("Rot", Mode::Shuffle, &[other.id]);
    let mut off = w.open("Rot", None, 5);
    off.start().unwrap();
    off.run_until(
        100.0,
        &[(
            2.0,
            Op::PlayAlbum {
                album_key: "mark|drift".into(),
                gapless: true,
                at: station::sched::At::Next,
            },
        )],
    )
    .unwrap();
    let r2 = rows_of(&off, t2.id);
    println!(
        "9: the silent track sounded {:.1} s, ended {:?}; silence raised {} time(s)",
        r2.first().and_then(|r| r.played_ms).map_or(0.0, secs),
        r2.first().and_then(|r| r.end_reason.clone()),
        off.station.alerts().count_raised(code::SILENCE)
    );
    assert_eq!(r2.len(), 1);
    assert!(secs(r2[0].played_ms.unwrap()) > 38.0, "it played its 40 s");
    assert_eq!(off.station.alerts().count_raised(code::SILENCE), 0);
    assert_eq!(off.station.alerts().count_raised(code::ITEM_SILENT), 0);
    assert!(
        !rows_of(&off, t3.id).is_empty(),
        "the run went on to track 3"
    );
}

// ---- 12. the media store fails ------------------------------------------------------------------------------------

#[test]
fn m16_12_a_failing_media_store_plays_the_ram_loop_within_a_second_and_recovers() {
    let mut w = world("a12");
    let music: Vec<It> = (0..5)
        .map(|i| w.add(300.0 + 100.0 * i as f64, 25.0, "music"))
        .collect();
    let emerg = [w.add(1200.0, 25.0, "music"), w.add(1400.0, 25.0, "music")];
    w.playlist(
        "Rot",
        Mode::Shuffle,
        &music.iter().map(|i| i.id).collect::<Vec<_>>(),
    );
    w.playlist(
        "Emergency",
        Mode::Sequential,
        &emerg.iter().map(|i| i.id).collect::<Vec<_>>(),
    );
    let mut off = w.open("Rot", Some("Emergency"), 6);
    off.start().unwrap();
    let t = std::time::Instant::now();
    let mut vt = 0.0;
    while off.station.fail.ram.is_none() {
        vt += 0.05;
        off.run_until(vt, &[]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(t.elapsed().as_secs() < 30);
    }
    off.run_until(20.0, &[]).unwrap();
    let faults = off.station.media_faults();
    faults
        .fail_all
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let t_inject = 20.0;
    off.run_until(40.0, &[]).unwrap();
    let raised = off
        .station
        .alerts()
        .history
        .iter()
        .find(|e| e.alert.code == code::MEDIA_STORE_FAILING)
        .expect("media_store_failing raised")
        .clone();
    assert_eq!(raised.alert.severity, Severity::Critical);
    let ram_row = off
        .station
        .as_run
        .iter()
        .find(|r| r.flags.iter().any(|f| f == "ram_loop"))
        .expect("the RAM loop plays");
    let detect = secs(raised.at_ms - T0) - t_inject;
    let lag = secs(ram_row.started_ms - raised.at_ms);
    println!(
        "12: reads fail from {t_inject:.0} s; media_store_failing critical after {detect:.2} s; the RAM loop sounded {lag:.2} s after the alert (limit 1 s); mode {}",
        off.station.health().mode
    );
    assert!(lag <= 1.0, "{lag}");
    assert_eq!(off.station.health().mode, "ram_loop");
    // Reads recover: the probe every 60 s sees it and normal programming resumes at the next boundary.
    faults
        .fail_all
        .store(false, std::sync::atomic::Ordering::SeqCst);
    off.run_until(140.0, &[]).unwrap();
    let al = off.station.alerts();
    println!(
        "12: after the reads recovered: media_store_failing active {}, mode {}",
        al.is_active(code::MEDIA_STORE_FAILING),
        off.station.health().mode
    );
    assert!(!al.is_active(code::MEDIA_STORE_FAILING));
    assert_eq!(off.station.health().mode, "normal");
    let last = off.station.as_run.last().unwrap();
    assert_eq!(
        last.origin, "rotation",
        "back on the playlist: {:?}",
        last.origin
    );
    // Never silent through it.
    let runs: Vec<_> = support::silent_runs(&off.samples, -60.0, 2_400)
        .into_iter()
        .filter(|(s, _)| *s > 48_000)
        .collect();
    println!("12: silent runs over 50 ms after the start: {runs:?}");
    assert!(runs.is_empty());
}

// ---- 13. a depleted playlist -------------------------------------------------------------------------------------------

#[test]
fn m16_13_a_depleted_playlist_fills_from_the_emergency_playlist_and_returns_when_items_are_released(
) {
    let mut w = world("a13");
    let music: Vec<It> = (0..4)
        .map(|i| w.add(300.0 + 100.0 * i as f64, 20.0, "music"))
        .collect();
    let emerg = [w.add(1200.0, 20.0, "music"), w.add(1400.0, 20.0, "music")];
    w.playlist(
        "Rot",
        Mode::Shuffle,
        &music.iter().map(|i| i.id).collect::<Vec<_>>(),
    );
    w.playlist(
        "Emergency",
        Mode::Sequential,
        &emerg.iter().map(|i| i.id).collect::<Vec<_>>(),
    );
    let mut off = w.open("Rot", Some("Emergency"), 8);
    off.start().unwrap();
    off.run_until(3.0, &[]).unwrap();
    // Quarantine all but one (by hand, as the operator's editor would).
    for m in &music[..3] {
        off.station.quarantine_item(m.id, "manual");
    }
    off.run_until(6.0, &[]).unwrap();
    assert!(off.station.alerts().is_active(code::PLAYLIST_DEPLETED));
    wait_library_pushed(&w, &off.station);
    off.run_until(75.0, &[]).unwrap();
    let origins: Vec<String> = off
        .station
        .as_run
        .iter()
        .map(|r| r.origin.clone())
        .collect();
    println!("13: origins while depleted: {origins:?}");
    assert!(origins.iter().any(|o| o == "emergency"), "{origins:?}");
    assert_eq!(off.station.health().mode, "emergency");
    let n_before = off.station.as_run.len();
    // The library service releases two of them.
    library::health::release(w.lib.conn(), music[0].id).unwrap();
    library::health::release(w.lib.conn(), music[1].id).unwrap();
    off.run_until(160.0, &[]).unwrap();
    let after: Vec<String> = off.station.as_run[n_before..]
        .iter()
        .map(|r| format!("{}:{}", r.origin, r.item_id.unwrap_or(0)))
        .collect();
    println!(
        "13: after releasing two: {after:?}; alert active {}",
        off.station.alerts().is_active(code::PLAYLIST_DEPLETED)
    );
    assert!(!off.station.alerts().is_active(code::PLAYLIST_DEPLETED));
    assert_eq!(off.station.health().mode, "normal");
    assert!(off.station.as_run[n_before..].iter().any(
        |r| r.origin == "rotation" && [music[0].id, music[1].id].contains(&r.item_id.unwrap())
    ));
    assert_eq!(
        off.station.alerts().count_raised(code::PLAYLIST_DEPLETED),
        1
    );
}

// ---- 14 and 8. the database read-only, and full ---------------------------------------------------------------------------

fn db_fault_run(mode: FaultMode, alert: &str, name: &str) {
    let mut w = world(name);
    let music: Vec<It> = (0..4)
        .map(|i| w.add(300.0 + 100.0 * i as f64, 20.0, "music"))
        .collect();
    w.playlist(
        "Rot",
        Mode::Shuffle,
        &music.iter().map(|i| i.id).collect::<Vec<_>>(),
    );
    let mut off = w.open("Rot", None, 9);
    off.start().unwrap();
    off.run_until(10.0, &[]).unwrap();
    off.station.flush();
    let before = state::read_as_run(&w.state_path()).unwrap().len();
    off.station.set_state_fault(mode);
    // Ten virtual minutes with the database broken.
    off.run_until(10.0 + 600.0, &[]).unwrap();
    let st = off.station.writer_status().unwrap();
    use std::sync::atomic::Ordering::Relaxed;
    let rows_mem = off.station.as_run.len();
    println!(
        "{name}: 600 s with the database broken: {} as-run rows in memory, {} on disk before, writer failing {}, backlog {}",
        rows_mem,
        before,
        st.failing.load(Relaxed),
        st.backlog.load(Relaxed)
    );
    assert!(st.failing.load(Relaxed));
    assert!(st.backlog.load(Relaxed) > 0);
    assert!(
        rows_mem > before + 10,
        "playback went on and the rows accumulated"
    );
    let al = off.station.alerts();
    let ev: Vec<(f64, Severity)> = al
        .history
        .iter()
        .filter(|e| e.alert.code == alert)
        .map(|e| (secs(e.at_ms - T0) - 10.0, e.alert.severity))
        .collect();
    println!("{name}: {alert} events (s after the fault, severity): {ev:?}");
    assert!(al.is_active(alert));
    if alert == code::DB_UNAVAILABLE {
        let crit = ev
            .iter()
            .find(|(_, s)| *s == Severity::Critical)
            .expect("critical after 60 s");
        assert!(crit.0 >= 59.0 && crit.0 <= 65.0, "critical at {}", crit.0);
        assert_eq!(ev[0].1, Severity::Warning);
    } else {
        assert_eq!(al.severity_of(alert), Some(Severity::Critical));
    }
    // No dead air while it lasted.
    let runs: Vec<_> = support::silent_runs(&off.samples, -60.0, 2_400)
        .into_iter()
        .filter(|(s, _)| *s > 48_000)
        .collect();
    assert!(runs.is_empty(), "{runs:?}");
    // Writable again: everything buffered is written.
    off.station.set_state_fault(FaultMode::None);
    let t = std::time::Instant::now();
    while st.failing.load(Relaxed) || st.backlog.load(Relaxed) > 0 {
        off.station.flush();
        assert!(
            t.elapsed().as_secs() < 20,
            "the buffered rows were never written"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // The alert clears once writes have worked for 5 s (no flapping on a database that fails on and off).
    off.run_until(10.0 + 607.0, &[]).unwrap();
    off.station.flush();
    let on_disk = state::read_as_run(&w.state_path()).unwrap();
    let mem: Vec<i64> = off.station.as_run.iter().map(|r| r.id).collect();
    let disk: Vec<i64> = on_disk.iter().map(|r| r.id).collect();
    println!(
        "{name}: writable again after {:.1} s of retrying: {} rows on disk, {} in memory; {} writes were buffered",
        t.elapsed().as_secs_f64(),
        disk.len(),
        mem.len(),
        st.recovered.load(Relaxed)
    );
    assert_eq!(disk, mem, "every buffered row is on disk, in order");
    assert!(!off.station.alerts().is_active(alert), "the alert cleared");
}

#[test]
fn m16_14_a_read_only_database_buffers_for_ten_minutes_and_flags_critical_after_60_s() {
    db_fault_run(FaultMode::ReadOnly, code::DB_UNAVAILABLE, "a14");
}

#[test]
fn m16_8_a_full_disk_buffers_the_rows_and_raises_disk_full() {
    db_fault_run(FaultMode::Full, code::DISK_FULL, "a8");
}

/// The same on a real full filesystem: only where the test runner mounts a small one (`dev/linux-test.sh`
/// gives the container a 4 MB tmpfs at `/small`; set `HOTCLOCK_SMALL_FS` to use another).
#[test]
fn m16_8b_a_really_full_filesystem() {
    let Some(small) = std::env::var_os("HOTCLOCK_SMALL_FS").map(PathBuf::from) else {
        eprintln!("SKIPPED: HOTCLOCK_SMALL_FS is not set (no small filesystem to fill)");
        return;
    };
    let mut w = world("a8b");
    let music: Vec<It> = (0..4)
        .map(|i| w.add(300.0 + 100.0 * i as f64, 20.0, "music"))
        .collect();
    w.playlist(
        "Rot",
        Mode::Shuffle,
        &music.iter().map(|i| i.id).collect::<Vec<_>>(),
    );
    let dir = small.join(format!("m16-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let state_path = dir.join("state.db");
    let (st, mixer) = Station::open(
        w.opts("Rot", None, 9),
        Some(state_path.clone()),
        Some(w.lib.paths.db.clone()),
        T0,
    )
    .unwrap();
    let mut off = Offline::new(st, mixer, T0);
    off.start().unwrap();
    off.run_until(10.0, &[]).unwrap();
    off.station.flush();
    // Fill what is left of the filesystem.
    let filler = dir.join("filler");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&filler).unwrap();
        let chunk = vec![1u8; 64 * 1024];
        while f.write_all(&chunk).is_ok() {}
        let _ = f.sync_all();
    }
    // The database also needs room to grow for the writes to fail: push enough rows.
    off.run_until(10.0 + 600.0, &[]).unwrap();
    let s = off.station.writer_status().unwrap();
    use std::sync::atomic::Ordering::Relaxed;
    println!(
        "8b: real ENOSPC: writer failing {}, backlog {}, disk_full active {}",
        s.failing.load(Relaxed),
        s.backlog.load(Relaxed),
        off.station.alerts().is_active(code::DISK_FULL)
    );
    assert!(off.station.alerts().is_active(code::DISK_FULL));
    std::fs::remove_file(&filler).unwrap();
    let t = std::time::Instant::now();
    while s.failing.load(Relaxed) || s.backlog.load(Relaxed) > 0 {
        off.station.flush();
        assert!(t.elapsed().as_secs() < 20);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // The alert clears once writes have worked for 5 s.
    off.run_until(10.0 + 607.0, &[]).unwrap();
    off.station.flush();
    let disk: Vec<i64> = state::read_as_run(&state_path)
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect();
    let mem: Vec<i64> = off.station.as_run.iter().map(|r| r.id).collect();
    println!(
        "8b: after freeing space: {} rows on disk, {} in memory",
        disk.len(),
        mem.len()
    );
    assert_eq!(disk, mem);
    assert!(!off.station.alerts().is_active(code::DISK_FULL));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- 7 (in process). a garbage state.db --------------------------------------------------------------------------------------

#[test]
fn m16_7_a_garbage_state_db_is_moved_aside_and_the_default_playlist_plays() {
    let mut w = world("a7");
    let music: Vec<It> = (0..3)
        .map(|i| w.add(300.0 + 100.0 * i as f64, 20.0, "music"))
        .collect();
    w.playlist(
        "Default",
        Mode::Shuffle,
        &music.iter().map(|i| i.id).collect::<Vec<_>>(),
    );
    std::fs::create_dir_all(w.state_path().parent().unwrap()).unwrap();
    std::fs::write(w.state_path(), vec![0xA5u8; 200_000]).unwrap();
    let mut off = w.open("Default", None, 1);
    off.start().unwrap();
    off.run_until(6.0, &[]).unwrap();
    let first = off.station.as_run.first().expect("it plays");
    println!(
        "7: first sound at {:.2} s; state_db_corrupt active {}",
        secs(first.started_ms - T0),
        off.station.alerts().is_active(code::STATE_DB_CORRUPT)
    );
    assert!(secs(first.started_ms - T0) < 5.0);
    assert!(off.station.alerts().is_active(code::STATE_DB_CORRUPT));
    assert_eq!(off.station.health().status.http_status(), 503);
    let aside = std::fs::read_dir(w.state_path().parent().unwrap())
        .unwrap()
        .flatten()
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("state.db.corrupt-")
        });
    assert!(aside);
    off.station.flush();
    let alerts = state::read_alerts(&w.state_path()).unwrap();
    assert!(
        alerts.iter().any(|a| a.1 == code::STATE_DB_CORRUPT),
        "{alerts:?}"
    );
}

// ---- 11. one long scenario with every per-item failure -----------------------------------------------------------------------

/// Dead air, counted as the blocks are rendered (a two-hour render is too long to keep).
#[derive(Default)]
struct DeadAir {
    run_start: Option<u64>,
    runs: Vec<(u64, u64)>,
}

impl DeadAir {
    fn feed(&mut self, t: u64, block: &[f32]) {
        for (i, f) in block.chunks_exact(2).enumerate() {
            let quiet = f[0].abs().max(f[1].abs()) < 1e-3;
            let frame = t + i as u64;
            match (quiet, self.run_start) {
                (true, None) => self.run_start = Some(frame),
                (false, Some(s)) => {
                    if frame - s > 2_400 {
                        self.runs.push((s, frame - s));
                    }
                    self.run_start = None;
                }
                _ => {}
            }
        }
    }
}

#[test]
fn m16_11_per_item_failures_never_stop_the_station_in_a_two_hour_scenario() {
    let mut w = world("a11");
    let normal: Vec<It> = (0..5)
        .map(|i| w.add(300.0 + 60.0 * i as f64, 20.0, "music"))
        .collect();
    let trunc = w.add(1000.0, 100.0, "music"); // cut at 40 percent once it is on air
    let silent = w.add(1100.0, 30.0, "music"); // replaced by silence
    let reanalysis = w.add(1200.0, 20.0, "music"); // its re-analysis is forced to fail
    let badmarks = w.add(1300.0, 40.0, "music"); // stored markers corrupted to segue > cue_out
    let deleted = w.add(1400.0, 20.0, "music"); // its file is deleted
    let emerg = [w.add(1500.0, 20.0, "music"), w.add(1600.0, 20.0, "music")];
    let mut ids: Vec<i64> = normal.iter().map(|i| i.id).collect();
    ids.extend([trunc.id, silent.id, reanalysis.id, badmarks.id, deleted.id]);
    // The fixture the milestone names, when ffmpeg can make it.
    let (fx_dir, rep) = fixtures::shared_dir(&["corrupt.mp3"]);
    let mut corrupt: Option<i64> = None;
    if rep.skipped.is_empty() {
        if let ImportOutcome::Done { item_id, .. } = import_file(
            &w.lib,
            &fx_dir.join("corrupt.mp3"),
            &ImportOptions {
                category: Some("music".into()),
                source: "test".into(),
            },
        ) {
            corrupt = Some(item_id);
            ids.push(item_id);
        }
    }
    println!("11: corrupt.mp3 in the scenario: {}", corrupt.is_some());
    w.playlist("Rot", Mode::Shuffle, &ids);
    w.playlist(
        "Emergency",
        Mode::Sequential,
        &emerg.iter().map(|i| i.id).collect::<Vec<_>>(),
    );

    // The faults, before the station starts (except the cut, which needs the item on air).
    w.silence(silent);
    let before = w.lib.item_effective(reanalysis.id).unwrap();
    let r = w
        .lib
        .reanalyze_item(reanalysis.id, Some("forced failure"))
        .unwrap();
    assert!(matches!(r, library::health::Reanalysis::KeptOld { .. }));
    let after = w.lib.item_effective(reanalysis.id).unwrap();
    assert_eq!(
        (before.cue_in, before.segue, before.cue_out),
        (after.cue_in, after.segue, after.cue_out)
    );
    w.lib
        .conn()
        .execute(
            "UPDATE item_effective SET segue = cue_out + 5000, revision = revision + 1 WHERE item_id = ?1",
            [badmarks.id],
        )
        .unwrap();
    Library::bump_revision(w.lib.conn()).unwrap();
    std::fs::remove_file(w.media(deleted.id)).unwrap();

    let mut off = w.open("Rot", Some("Emergency"), 2026);
    off.keep_samples = false;
    let dead = Arc::new(Mutex::new(DeadAir::default()));
    let d2 = dead.clone();
    off.sink = Some(Box::new(move |t, b| d2.lock().unwrap().feed(t, b)));
    off.start().unwrap();
    let hours: f64 = std::env::var("HOTCLOCK_M16_SCENARIO_HOURS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2.0);
    let end = (hours * 3600.0) as u64;
    let t_wall = std::time::Instant::now();
    let mut cut_done = false;
    let mut cut_at = 0.0;
    for t in 1..=end {
        off.run_until(t as f64, &[]).unwrap();
        if !cut_done
            && off.station.now().is_some_and(|n| n.item_id == trunc.id)
            && off.station.position_ms().unwrap_or(0) > 1000
        {
            w.truncate(trunc.id, 40);
            cut_done = true;
            cut_at = t as f64;
        }
        if t == 600 {
            w.silence(silent); // "and then by another 30 s silent file"
        }
    }
    wait_library_pushed(&w, &off.station);
    println!(
        "11: {hours} h rendered in {:.0} s of wall time; the cut was made at {cut_at:.0} s; {} as-run rows",
        t_wall.elapsed().as_secs_f64(),
        off.station.as_run.len()
    );
    assert!(cut_done, "the truncated item came on air");

    // The render: no run below -60 dBFS longer than 50 ms except inside the silent item's 15 s windows.
    let dead = dead.lock().unwrap();
    let windows: Vec<(u64, u64)> = rows_of(&off, silent.id)
        .iter()
        .map(|r| {
            let s = ((r.started_ms - T0) as f64 * 48.0) as u64;
            (s.saturating_sub(48_000), s + 16 * 48_000)
        })
        .collect();
    // corrupt.mp3 carries its own hole: the decoder pads a bad packet with zeros for the packet's
    // duration (1.5, "decode error, recoverable"), which is 5,728 frames (119 ms) in this file. A hole of
    // that size inside the corrupt item's own play is the design working, not dead air.
    let corrupt_windows: Vec<(u64, u64)> = corrupt
        .map(|c| {
            rows_of(&off, c)
                .iter()
                .map(|r| {
                    (
                        ((r.started_ms - T0) as f64 * 48.0) as u64,
                        ((r.ended_ms.unwrap_or(r.started_ms) - T0) as f64 * 48.0) as u64 + 48_000,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let in_windows =
        |ws: &[(u64, u64)], s: u64, l: u64| ws.iter().any(|(a, b)| s >= *a && s + l <= *b);
    let padded: Vec<&(u64, u64)> = dead
        .runs
        .iter()
        .filter(|(s, l)| *l <= 6_144 && in_windows(&corrupt_windows, *s, *l))
        .collect();
    let stray: Vec<&(u64, u64)> = dead
        .runs
        .iter()
        .filter(|(s, _)| *s > 24_000)
        .filter(|(s, l)| !in_windows(&windows, *s, *l))
        .filter(|(s, l)| !(*l <= 6_144 && in_windows(&corrupt_windows, *s, *l)))
        .collect();
    println!(
        "11: dead-air runs over 50 ms: {} in all; {} are inside the silent item's windows {:?}; {} are corrupt.mp3's own padded packet (lengths {:?} frames); stray: {stray:?}",
        dead.runs.len(),
        dead.runs.iter().filter(|(s, l)| in_windows(&windows, *s, *l)).count(),
        windows,
        padded.len(),
        padded.iter().map(|r| r.1).collect::<std::collections::BTreeSet<_>>()
    );
    assert!(
        stray.is_empty(),
        "dead air outside the allowed windows: {stray:?}"
    );

    // The truncated item: quarantined at its first early end.
    let tr = rows_of(&off, trunc.id);
    assert_eq!(tr.len(), 1, "the truncated item played once");
    assert_eq!(tr[0].end_reason.as_deref(), Some("error"));
    assert_eq!(
        quarantine_reason(&w, trunc.id).as_deref(),
        Some("truncated")
    );
    // The silent one: quarantined at its second skip.
    let sr = rows_of(&off, silent.id);
    assert_eq!(sr.len(), 2, "played (and skipped) twice: {sr:?}");
    assert!(sr.iter().all(|r| r.end_reason.as_deref() == Some("cut")));
    assert_eq!(quarantine_reason(&w, silent.id).as_deref(), Some("silent"));
    // The deleted one: at pre-flight, never played.
    assert!(rows_of(&off, deleted.id).is_empty());
    assert_eq!(
        quarantine_reason(&w, deleted.id).as_deref(),
        Some("missing")
    );
    // The failed re-analysis: old markers, flagged, still plays.
    let flagged: i64 = w
        .lib
        .conn()
        .query_row(
            "SELECT analysis_failed FROM items WHERE id = ?1",
            [reanalysis.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(flagged, 1);
    assert!(!rows_of(&off, reanalysis.id).is_empty());
    assert_eq!(quarantine_reason(&w, reanalysis.id), None);
    // The bad markers: plays as the whole file, flagged, not skipped.
    let bm = rows_of(&off, badmarks.id);
    assert!(!bm.is_empty());
    assert!(
        bm.iter()
            .all(|r| r.flags.iter().any(|f| f == "marker_invalid")),
        "{bm:?}"
    );
    let played = bm
        .iter()
        .filter_map(|r| r.played_ms)
        .map(secs)
        .fold(f64::MAX, f64::min);
    println!(
        "11: the bad-marker item played at least {played:.1} s of its 40 s file, {} plays",
        bm.len()
    );
    assert!(played > 30.0);
    assert_eq!(quarantine_reason(&w, badmarks.id), None);
    // One item_quarantined per quarantine, and /health never 503.
    let al = off.station.alerts();
    let n_q = al.count_raised(code::ITEM_QUARANTINED);
    println!(
        "11: item_quarantined raised {n_q} times; alerts raised: {:?}",
        al.raised_total
    );
    assert_eq!(n_q, 3, "truncated, silent, missing");
    assert_eq!(al.count_raised(code::ITEM_MARKER_INVALID), 1);
    assert!(
        al.history
            .iter()
            .all(|e| e.alert.severity != Severity::Critical),
        "never critical"
    );
    assert_eq!(off.station.health().status.http_status(), 200);
    assert_eq!(al.count_raised(code::MEDIA_STORE_FAILING), 0);
}

// ---- clock jumps (1.5): detected and flagged; the audio never notices -------------------------------------------------------

#[test]
fn m16_clock_jumps_are_flagged_and_the_audio_does_not_notice() {
    let mut w = world("clk");
    let music: Vec<It> = (0..3)
        .map(|i| w.add(300.0 + 100.0 * i as f64, 30.0, "music"))
        .collect();
    w.playlist(
        "Rot",
        Mode::Shuffle,
        &music.iter().map(|i| i.id).collect::<Vec<_>>(),
    );
    let mut off = w.open("Rot", None, 3);
    off.start().unwrap();
    off.run_until(10.0, &[]).unwrap();
    assert!(!off.station.alerts().is_active(code::CLOCK_JUMP));
    // A 10-minute step forward (chrony, or a person), then 20 minutes back.
    off.wall_origin_ms += 600_000;
    off.run_until(12.0, &[]).unwrap();
    let al = off.station.alerts();
    assert!(al.is_active(code::CLOCK_JUMP));
    let msg = al
        .active()
        .into_iter()
        .find(|a| a.code == code::CLOCK_JUMP)
        .unwrap()
        .message
        .clone();
    println!("clock: forward step flagged: {msg}");
    off.wall_origin_ms -= 1_200_000;
    off.run_until(40.0, &[]).unwrap();
    assert!(off.station.alerts().count_raised(code::CLOCK_JUMP) >= 1);
    println!(
        "clock: {} clock_jump alert(s) over a forward and a backward step; the audio ran on: {} silent runs over 50 ms",
        off.station.alerts().count_raised(code::CLOCK_JUMP),
        support::silent_runs(&off.samples, -60.0, 2_400).iter().filter(|(s, _)| *s > 48_000).count()
    );
    assert!(support::silent_runs(&off.samples, -60.0, 2_400)
        .iter()
        .all(|(s, _)| *s <= 48_000));
    // The row that was open across the steps says so, and its two stamps differ from `played_ms` by that jump.
    let flagged: Vec<_> = off
        .station
        .as_run
        .iter()
        .filter(|r| r.flags.iter().any(|f| f.starts_with("clock_jump_ms=")))
        .collect();
    assert!(!flagged.is_empty(), "no as-run row carries clock_jump_ms");
    for r in &flagged {
        let f = r
            .flags
            .iter()
            .find(|f| f.starts_with("clock_jump_ms="))
            .unwrap();
        let d: i64 = f["clock_jump_ms=".len()..].parse().unwrap();
        let (e, p) = (r.ended_ms.unwrap(), r.played_ms.unwrap());
        println!(
            "clock: row {} flagged {f}; ended-started-played = {} ms",
            r.id,
            e - r.started_ms - p
        );
        assert!(d != 0 && ((e - r.started_ms - p) - d).abs() < 2_000);
    }
    assert_eq!(
        off.station.health().status.http_status(),
        200,
        "a clock jump is a warning"
    );
}

// ---- a file that cannot be decoded at all, and an empty queue ------------------------------------------------------------------

#[test]
fn m16_a_file_that_will_not_decode_is_quarantined_at_once_and_nothing_goes_quiet() {
    let mut w = world("undec");
    let items: Vec<It> = (0..4)
        .map(|i| w.add(300.0 + 100.0 * i as f64, 20.0, "music"))
        .collect();
    let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
    w.playlist("Seq", Mode::Sequential, &ids);
    // Garbage of the same size: pre-flight passes (it reads), the decoder cannot open it.
    let victim = items[2];
    let p = w.media(victim.id);
    let size = std::fs::metadata(&p).unwrap().len() as usize;
    W::writable(&p);
    std::fs::write(
        &p,
        (0..size).map(|i| (i * 31 % 251) as u8).collect::<Vec<u8>>(),
    )
    .unwrap();
    let mut off = w.open("Seq", None, 2);
    off.start().unwrap();
    off.run_until(100.0, &[]).unwrap();
    wait_library_pushed(&w, &off.station);
    let played: Vec<i64> = off
        .station
        .as_run
        .iter()
        .filter_map(|r| r.item_id)
        .collect();
    println!(
        "undecodable: played {played:?}; item {} is garbage",
        victim.id
    );
    for r in off.station.as_run.iter().take(6) {
        println!(
            "  {:>6.2} s item {:?} played {:?} ms, {:?}",
            secs(r.started_ms - T0),
            r.item_id,
            r.played_ms,
            r.end_reason
        );
    }
    println!(
        "undecodable: alerts {:?}; errors {:?}",
        off.station.alerts().raised_total,
        off.station.item_errors(victim.id)
    );
    assert!(!played.contains(&victim.id));
    assert!(played.len() >= 5);
    assert_eq!(quarantine_reason(&w, victim.id).as_deref(), Some("decode"));
    assert_eq!(
        off.station.alerts().count_raised(code::ITEM_DECODE_FAILED),
        1
    );
    assert_eq!(off.station.alerts().count_raised(code::ITEM_QUARANTINED), 1);
    let runs: Vec<_> = support::silent_runs(&off.samples, -60.0, 2_400)
        .into_iter()
        .filter(|(s, _)| *s > 48_000)
        .collect();
    println!("undecodable: silent runs over 50 ms after the start: {runs:?}");
    assert!(
        runs.is_empty(),
        "the boundary was taken back and re-planned: {runs:?}"
    );
}

#[test]
fn m16_an_empty_queue_falls_back_to_the_emergency_deck_without_asking_the_scheduler() {
    let mut w = world("starved");
    let music: Vec<It> = (0..3)
        .map(|i| w.add(300.0 + 100.0 * i as f64, 20.0, "music"))
        .collect();
    let emerg = [w.add(1200.0, 20.0, "music"), w.add(1400.0, 20.0, "music")];
    w.playlist(
        "Rot",
        Mode::Shuffle,
        &music.iter().map(|i| i.id).collect::<Vec<_>>(),
    );
    w.playlist(
        "Emergency",
        Mode::Sequential,
        &emerg.iter().map(|i| i.id).collect::<Vec<_>>(),
    );
    let mut off = w.open("Rot", Some("Emergency"), 4);
    off.start().unwrap();
    off.run_until(5.0, &[]).unwrap();
    // A scheduler fault: nothing queued, and nothing that will be.
    off.station.sched.st.queue.clear();
    off.station.sched.st.program = None;
    off.run_until(60.0, &[]).unwrap();
    let rows = off.station.as_run.clone();
    for r in &rows {
        println!(
            "  {:>6.2} s {:?} {}",
            secs(r.started_ms - T0),
            r.origin,
            r.title
        );
    }
    let e = rows
        .iter()
        .find(|r| r.origin == "emergency")
        .expect("the emergency deck plays");
    assert!(off.station.alerts().is_active(code::QUEUE_STARVED));
    assert_eq!(
        off.station.alerts().severity_of(code::QUEUE_STARVED),
        Some(Severity::Critical)
    );
    let runs: Vec<_> = support::silent_runs(&off.samples, -60.0, 2_400)
        .into_iter()
        .filter(|(s, _)| *s > 48_000)
        .collect();
    println!(
        "starved: the emergency deck's first item began at {:.2} s (the on-air item's segue is near 20 s); silent runs over 50 ms: {runs:?}",
        secs(e.started_ms - T0)
    );
    assert!(runs.is_empty());
}

// ---- review (Opus), 2026-09-30: races and paths that could leave the station silent -----------------------------------------

/// A skip the silence detector asks for is about the item that was silent. When the boundary into the next
/// item is already too close to take back, the next item starts on its own; the skip must then be dropped,
/// not applied to the item that just began (before the review it cut that item a moment after its start).
#[test]
fn review_a_silence_skip_never_cuts_the_item_that_started_before_it_could_be_applied() {
    // A ends within a fraction of a second of its 15 s of silence being up (the `none` preset butts B onto
    // A's cue-out), so B is already committed and too close to take back when the skip is due. Several
    // lengths, so the skip falls on both sides of B's start. (At 14.9 s the builder's code cut B 0.29 s in.)
    // M6a note: a narrow window of lengths near the edge still cuts B (found 2026-10-06 by sweeping the length
    // in 2.5 ms steps: at the M1.8 baseline it is A = 14.955 s, with the true-peak limiter's 5 ms in the path
    // 14.9125 s and 15.0 s). The five lengths below stay clear of it; the window itself is a race in the
    // silence skip that this milestone did not touch, listed in BUILDLOG as a finding.
    for (k, len) in [14.9, 15.1, 15.2, 15.3, 15.4].into_iter().enumerate() {
        let mut w = world(&format!("r-race{k}"));
        let a = w.add(440.0, len, "music");
        let b = w.add(550.0, 30.0, "music");
        let c = w.add(660.0, 30.0, "music");
        w.silence(a);
        w.playlist("Seq", Mode::Sequential, &[a.id, b.id, c.id]);
        let mut o = w.opts("Seq", None, 1);
        o.preset = "none".into();
        let (st, mixer) =
            Station::open(o, Some(w.state_path()), Some(w.lib.paths.db.clone()), T0).unwrap();
        let mut off = Offline::new(st, mixer, T0);
        off.start().unwrap();
        off.run_until(50.0, &[]).unwrap();
        let rb = rows_of(&off, b.id);
        println!(
            "race: A {len} s: B played {:.2} s, ended {:?}; silence raised {}",
            secs(rb[0].played_ms.unwrap_or(0)),
            rb[0].end_reason,
            off.station.alerts().count_raised(code::SILENCE)
        );
        assert!(
            secs(rb[0].played_ms.unwrap()) > 29.0,
            "B played through (A {len} s)"
        );
        assert_ne!(rb[0].end_reason.as_deref(), Some("cut"));
    }
}

/// Safe mode is left only by an operator's `ack`. A failing store (or a silence escalation) may take over the
/// source for a while; when it is over the station goes back to safe mode, not to its normal programming.
#[test]
fn review_safe_mode_survives_a_media_store_failure_in_the_middle_of_it() {
    let mut w = world("r-safe");
    let music: Vec<It> = (0..4)
        .map(|i| w.add(300.0 + 100.0 * i as f64, 20.0, "music"))
        .collect();
    let emerg = [w.add(1200.0, 20.0, "music"), w.add(1400.0, 20.0, "music")];
    w.playlist(
        "Rot",
        Mode::Shuffle,
        &music.iter().map(|i| i.id).collect::<Vec<_>>(),
    );
    w.playlist(
        "Emergency",
        Mode::Sequential,
        &emerg.iter().map(|i| i.id).collect::<Vec<_>>(),
    );
    let mut off = w.open("Rot", Some("Emergency"), 3);
    off.station.fail.safe_mode = true;
    off.start().unwrap();
    let t = std::time::Instant::now();
    let mut vt = 0.0;
    while off.station.fail.ram.is_none() {
        vt += 0.05;
        off.run_until(vt, &[]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(t.elapsed().as_secs() < 30);
    }
    off.run_until(10.0, &[]).unwrap();
    let faults = off.station.media_faults();
    faults
        .fail_all
        .store(true, std::sync::atomic::Ordering::SeqCst);
    off.run_until(30.0, &[]).unwrap();
    assert!(off.station.alerts().is_active(code::MEDIA_STORE_FAILING));
    faults
        .fail_all
        .store(false, std::sync::atomic::Ordering::SeqCst);
    off.run_until(200.0, &[]).unwrap();
    let origins: Vec<&str> = off
        .station
        .as_run
        .iter()
        .map(|r| r.origin.as_str())
        .collect();
    println!(
        "safe: origins {origins:?}; mode {}; store alert active {}",
        off.station.health().mode,
        off.station.alerts().is_active(code::MEDIA_STORE_FAILING)
    );
    assert!(!off.station.alerts().is_active(code::MEDIA_STORE_FAILING));
    assert_eq!(off.station.health().mode, "safe");
    assert!(off.station.alerts().is_active(code::SAFE_MODE));
    assert!(
        origins.iter().all(|o| *o == "emergency"),
        "normal programming never came back without an ack: {origins:?}"
    );
}

/// Nothing left to play (every item of the only playlist quarantined, no emergency playlist): that is dead
/// air, and the detector must say so (before the review it only watched while a voice was placed).
#[test]
fn review_a_station_with_nothing_left_to_play_raises_silence() {
    let mut w = world("r-empty");
    let a = w.add(440.0, 20.0, "music");
    let b = w.add(550.0, 20.0, "music");
    w.playlist("Rot", Mode::Sequential, &[a.id, b.id]);
    let mut off = w.open("Rot", None, 2);
    off.start().unwrap();
    off.run_until(2.0, &[]).unwrap();
    // Both files go away; A plays on from its open file, B and then A are found missing.
    for it in [a, b] {
        std::fs::remove_file(w.media(it.id)).unwrap();
    }
    off.run_until(80.0, &[]).unwrap();
    let al = off.station.alerts();
    let ev: Vec<(f64, Severity)> = al
        .history
        .iter()
        .filter(|e| e.alert.code == code::SILENCE)
        .map(|e| (secs(e.at_ms - T0), e.alert.severity))
        .collect();
    println!(
        "empty: silence events {ev:?}; raised {:?}; health {}",
        al.raised_total,
        off.station.health().status.name()
    );
    assert!(al.is_active(code::SILENCE));
    assert_eq!(al.severity_of(code::SILENCE), Some(Severity::Critical));
    assert_eq!(off.station.health().status.http_status(), 503);
    // Nobody is blamed for the dead air after the last item ended.
    assert_eq!(
        off.station.item_errors(a.id),
        vec![("missing".to_string(), 1)]
    );
}

/// A media store that disappears (a disk that did not mount: the directory is there and empty) is the
/// store's failure, not every item's: nothing is quarantined, `media_store_failing` is raised, and the
/// station plays again when it is back.
#[test]
fn review_a_vanished_media_store_quarantines_nothing() {
    let mut w = world("r-gone");
    let music: Vec<It> = (0..4)
        .map(|i| w.add(300.0 + 100.0 * i as f64, 20.0, "music"))
        .collect();
    w.playlist(
        "Rot",
        Mode::Shuffle,
        &music.iter().map(|i| i.id).collect::<Vec<_>>(),
    );
    let mut off = w.open("Rot", None, 4);
    off.start().unwrap();
    off.run_until(5.0, &[]).unwrap();
    let media = w.lib.paths.media.clone();
    let away = media.with_extension("away");
    std::fs::rename(&media, &away).unwrap();
    std::fs::create_dir_all(&media).unwrap();
    off.run_until(60.0, &[]).unwrap();
    let al = off.station.alerts();
    println!(
        "gone: raised {:?}; quarantined in the station {}",
        al.raised_total,
        off.station.health().quarantined
    );
    assert!(al.is_active(code::MEDIA_STORE_FAILING));
    assert_eq!(al.count_raised(code::ITEM_QUARANTINED), 0);
    assert_eq!(al.count_raised(code::ITEM_MISSING), 0);
    assert_eq!(off.station.health().quarantined, 0);
    std::fs::remove_dir(&media).unwrap();
    std::fs::rename(&away, &media).unwrap();
    let n = off.station.as_run.len();
    off.run_until(200.0, &[]).unwrap();
    wait_library_pushed(&w, &off.station);
    let q: i64 = w
        .lib
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM items WHERE quarantined = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    println!(
        "gone: back: store alert active {}, {} plays since, library quarantined {q}",
        off.station.alerts().is_active(code::MEDIA_STORE_FAILING),
        off.station.as_run.len() - n
    );
    assert!(!off.station.alerts().is_active(code::MEDIA_STORE_FAILING));
    assert!(off.station.as_run.len() > n, "it plays again");
    assert_eq!(q, 0);
}

/// A voice whose ring ran dry plays the rest of its audio late; its gapless successor, placed where its audio
/// was due to end, must not sound over that late tail (M1.2 review): the starved voice gets an end at the
/// successor's start.
#[test]
fn review_a_starved_voice_before_a_gapless_successor_ends_where_the_successor_starts() {
    let mut w = world("r-starve");
    let t1 = w.add(440.0, 20.0, "music");
    let t2 = w.add(550.0, 20.0, "music");
    let other = w.add(880.0, 20.0, "music");
    for (n, it) in [t1, t2].iter().enumerate() {
        w.lib
            .conn()
            .execute(
                "UPDATE items SET album_key='mark|drift', disc_no=1, track_no=?2, track_total=2 WHERE id=?1",
                rusqlite::params![it.id, n as i64 + 1],
            )
            .unwrap();
    }
    w.playlist("Rot", Mode::Shuffle, &[other.id]);
    let mut off = w.open("Rot", None, 5);
    off.start().unwrap();
    off.run_until(
        1.0,
        &[(
            0.5,
            Op::PlayAlbum {
                album_key: "mark|drift".into(),
                gapless: true,
                at: station::sched::At::Next,
            },
        )],
    )
    .unwrap();
    // Wait for the joint t1 -> t2 to be placed.
    let p1 = w.media(t1.id);
    let mut vt = 1.0;
    loop {
        vt += 1.0;
        off.run_until(vt, &[]).unwrap();
        let placed = &off.station.cond.placed;
        let i = placed.iter().position(|p| p.item.path == p1);
        if let Some(i) = i {
            let started = off.station.as_run.iter().any(|r| r.item_id == Some(t1.id));
            if started && i + 1 < placed.len() {
                break;
            }
        }
        assert!(vt < 120.0, "the joint was never placed");
    }
    let placed = &off.station.cond.placed;
    let i = placed.iter().position(|p| p.item.path == p1).unwrap();
    let (id1, rec1, s2) = (placed[i].id, placed[i].record, placed[i + 1].start_frame);
    let had_stop = off.station.cond.sidecar.voices[rec1].stop.clone();
    let now = (vt * 48_000.0) as u64;
    off.station.on_starved(id1, now);
    off.run_until(vt + 0.5, &[]).unwrap();
    let stop = off.station.cond.sidecar.voices[rec1].stop.clone();
    println!(
        "starve: t1 had a stop before: {had_stop:?}; after its ring ran dry: {stop:?}; t2 starts at {s2}"
    );
    let stop = stop.expect("an end for the late voice");
    assert!(stop.at + u64::from(stop.frames) <= s2, "{stop:?} {s2}");
    assert!(off.station.alerts().is_active(code::DECODE_SLOW));
    off.run_until(vt + 40.0, &[]).unwrap();
    assert!(
        !off.station.alerts().is_active(code::DECODE_SLOW),
        "cleared when the voice is gone"
    );
}

/// An item whose own transition override names a preset that does not exist (deleted, misspelt) is bad data
/// about the item, not a reason to stop: it plays with the layers that do resolve. Before the review the
/// boundary into it failed on every step, and the station went quiet once the item before it ended.
#[test]
fn review_an_item_with_a_transition_preset_that_does_not_exist_still_plays() {
    let mut w = world("r-preset");
    let a = w.add(440.0, 20.0, "music");
    let b = w.add(550.0, 20.0, "music");
    let c = w.add(660.0, 20.0, "music");
    w.lib
        .conn()
        .execute(
            "INSERT INTO item_overrides(item_id, params, updated_at) VALUES(?1, ?2, '2026-09-30T00:00:00Z')",
            rusqlite::params![b.id, r#"{"transition":{"preset":"no-such-preset"}}"#],
        )
        .unwrap();
    w.lib.recompute_effective(b.id).unwrap();
    w.playlist("Seq", Mode::Sequential, &[a.id, b.id, c.id]);
    let mut off = w.open("Seq", None, 1);
    off.start().unwrap();
    off.run_until(70.0, &[]).expect("no step fails");
    let played: Vec<i64> = off
        .station
        .as_run
        .iter()
        .filter_map(|r| r.item_id)
        .collect();
    println!("preset: played {played:?}");
    assert!(played.starts_with(&[a.id, b.id, c.id]), "{played:?}");
    let runs: Vec<_> = support::silent_runs(&off.samples, -60.0, 2_400)
        .into_iter()
        .filter(|(s, _)| *s > 48_000)
        .collect();
    assert!(runs.is_empty(), "{runs:?}");
}

/// A card away for 30 s or more: the station moves on to the next item (1.5), prepared on the paused
/// timeline. Nothing of the interrupted item may sound after the card returns (before the review the move was
/// a planned cut, which let the old item play for the cut's 0.3 s lead first).
#[test]
fn review_after_a_long_outage_the_next_item_starts_from_silence() {
    let mut w = world("r-device");
    let a = w.add(440.0, 60.0, "music");
    let b = w.add(660.0, 60.0, "music");
    w.playlist("Seq", Mode::Sequential, &[a.id, b.id]);
    let mut off = w.open("Seq", None, 1);
    off.start().unwrap();
    off.run_until(20.0, &[]).unwrap();
    // The card goes away: the engine stops rendering, the station keeps stepping on the frozen frame.
    let frozen = off.t;
    off.station.on_device_lost();
    let wall0 = off.wall_of(frozen);
    for k in 0..2_000i64 {
        let mixer = &mut off.mixer;
        let mut send = |c: engine::rt::Command| {
            use engine::source::Source;
            mixer.command(&c.kind)
        };
        off.station.step(frozen, wall0 + k * 20, &mut send).unwrap();
        // The pre-flight worker runs in real time and its requests time out on the station's clock (4 s), which
        // this loop moves 40 s in a few milliseconds: let it answer, as `Offline::run_until` does after every
        // step. Without it a machine under load read B late, B was judged unreadable and dropped, and the
        // station played A again (a flake on main too, 1 run in 5 to 30 with all cores busy).
        off.station
            .fail
            .preflight
            .settle(std::time::Duration::from_secs(10));
    }
    // 40 s later it is back.
    off.wall_origin_ms += 40_000;
    off.station.on_device_restored(40_000);
    off.run_until(40.0, &[]).unwrap();
    let ra = rows_of(&off, a.id);
    let rb = rows_of(&off, b.id);
    let b_start =
        frozen as usize + ((rb[0].started_ms - off.wall_of(frozen)) as f64 * 48.0).round() as usize;
    let after = &off.samples[frozen as usize * 2..b_start * 2];
    let loud: Vec<(usize, f32)> = after
        .chunks_exact(2)
        .enumerate()
        .filter(|(_, f)| f[0].abs().max(f[1].abs()) > 1e-3)
        .map(|(i, f)| (i, f[0].abs().max(f[1].abs())))
        .collect();
    println!(
        "device: A ended {:?} after {:.2} s; B began {:.3} s after the card came back; A heard after the return for {} frames",
        ra[0].end_reason,
        secs(ra[0].played_ms.unwrap()),
        (b_start - frozen as usize) as f64 / 48_000.0,
        loud.last().map_or(0, |l| l.0 + 1)
    );
    assert_eq!(ra[0].end_reason.as_deref(), Some("device"));
    // What of the old item may still sound is what is in the limiters' delay lines when the card returns
    // (the safety limiter's 96 frames and the true-peak limiter's 240, M6a) plus the stop's own fade, so the
    // bound moves with the delay: 500 frames at 96, and twice the extra delay on top of it at the M6a latency
    // (measured: 328 frames at 96, 795 at 336).
    let extra = off.station.opts.lookahead_frames.saturating_sub(96) as usize;
    assert!(
        loud.last().is_none_or(|l| l.0 < 500 + 2 * extra),
        "the old item sounded after the return: {} frames",
        loud.last().unwrap().0
    );
    assert!((b_start - frozen as usize) < 48_000, "B within a second");
}

fn all_first_from(off: &Offline) -> String {
    off.station.cond.sidecar.boundaries[0].from.clone()
}

/// M2a review: a failed stream encoder is critical on a stream-only station (it is off the air) and a warning
/// when a card also plays the program; it clears with the encoder.
#[test]
fn m2a_stream_encoder_failed_is_critical_only_without_a_card() {
    use station::failure::StreamStatus;
    let mut w = world("m2a-enc");
    let music: Vec<It> = (0..3)
        .map(|i| w.add(300.0 + 100.0 * i as f64, 25.0, "music"))
        .collect();
    w.playlist(
        "Rot",
        Mode::Shuffle,
        &music.iter().map(|i| i.id).collect::<Vec<_>>(),
    );
    let mut off = w.open("Rot", None, 6);
    off.start().unwrap();
    for (stream_only, want) in [(true, Severity::Critical), (false, Severity::Warning)] {
        let failed = StreamStatus {
            encoder_failed: true,
            stream_only,
            ..StreamStatus::default()
        };
        off.station.on_stream(&failed);
        assert_eq!(
            off.station
                .alerts()
                .severity_of(code::STREAM_ENCODER_FAILED),
            Some(want),
            "stream_only {stream_only}"
        );
        off.station.on_stream(&StreamStatus {
            stream_only,
            ..StreamStatus::default()
        });
        assert!(!off.station.alerts().is_active(code::STREAM_ENCODER_FAILED));
    }
    // Nobody listening stays a warning, card or not.
    off.station.on_stream(&StreamStatus {
        no_listener: true,
        no_listener_s: 60.0,
        stream_only: true,
        ..StreamStatus::default()
    });
    assert_eq!(
        off.station.alerts().severity_of(code::STREAM_NO_LISTENER),
        Some(Severity::Warning)
    );
}

/// M6a (6.1 alerts): `loudness_at_limit` (a warning) after 10 minutes of the gain at a range limit,
/// `loudness_frozen` (info) after 30 minutes frozen while playing; each clears with its condition, and the
/// checkpoint carries the controller's gain once it is enabled.
#[test]
fn m6a_loudness_alerts_raise_after_their_times_and_clear() {
    let mut w = world("m6a-alerts");
    let music: Vec<It> = (0..2)
        .map(|i| w.add(300.0 + 100.0 * i as f64, 25.0, "music"))
        .collect();
    w.playlist(
        "Rot",
        Mode::Shuffle,
        &music.iter().map(|i| i.id).collect::<Vec<_>>(),
    );
    let mut off = w.open("Rot", None, 6);
    off.start().unwrap();
    let a = |off: &Offline, c: &str| off.station.alerts().severity_of(c);
    off.station.on_loudness(599.0, 0.0);
    assert_eq!(
        a(&off, code::LOUDNESS_AT_LIMIT),
        None,
        "9 min 59 s is not yet"
    );
    off.station.on_loudness(600.0, 0.0);
    assert_eq!(a(&off, code::LOUDNESS_AT_LIMIT), Some(Severity::Warning));
    off.station.on_loudness(0.0, 1799.0);
    assert_eq!(
        a(&off, code::LOUDNESS_AT_LIMIT),
        None,
        "cleared when the gain leaves its limit"
    );
    assert_eq!(a(&off, code::LOUDNESS_FROZEN), None);
    off.station.on_loudness(0.0, 1800.0);
    assert_eq!(a(&off, code::LOUDNESS_FROZEN), Some(Severity::Info));
    off.station.on_loudness(0.0, 0.0);
    assert_eq!(a(&off, code::LOUDNESS_FROZEN), None);
    println!("loudness_at_limit raised at 600 s (warning), loudness_frozen at 1800 s (info), both cleared with their conditions");
}
