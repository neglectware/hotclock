//! M1.5 acceptance tests that need audio: the station (scheduler + conductor + mixer + `state.db`)
//! rendered offline on a virtual clock over a real library of generated tones.

use engine::render::{render_tone, ToneRender};
use engine::wav::WavFormat;
use library::playlists::{Mode, NewBlock, NewPlaylist};
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use station::offline::Offline;
use station::runtime::{Op, Station, StationOptions};
use station::sched::SwitchHow;
use station::state;
use std::path::{Path, PathBuf};

const RATE: u32 = 48_000;
const T0: i64 = 1_800_000_000_000;

pub fn tmpdir(name: &str) -> PathBuf {
    let d = fixtures::testdir::process_dir().join(format!("m15-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tone(dir: &Path, name: &str, hz: f64, seconds: f64) -> PathBuf {
    let out = dir.join(name);
    render_tone(&ToneRender {
        out: out.clone(),
        rate: RATE,
        seconds,
        hz,
        level_dbfs: -20.0,
        format: WavFormat::F32,
    })
    .unwrap();
    out
}

pub struct World {
    pub dir: PathBuf,
    pub lib: Library,
    /// `(item id, hz)` of the music tones.
    pub music: Vec<(i64, f64)>,
    pub liners: Vec<(i64, f64)>,
}

/// A library with `n` music tones of `seconds` each at 300 Hz steps of 100, and `n_liners` liners.
pub fn world(name: &str, n: usize, seconds: f64, n_liners: usize) -> World {
    let dir = tmpdir(name);
    let lib = Library::open(Paths::from_data_dir(&dir.join("data"))).unwrap();
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut music = vec![];
    for i in 0..n {
        let hz = 300.0 + 100.0 * i as f64;
        let p = tone(&src, &format!("m{i}.wav"), hz, seconds);
        let opts = ImportOptions {
            category: Some("music".into()),
            source: "test".into(),
        };
        match import_file(&lib, &p, &opts) {
            ImportOutcome::Done { item_id, .. } => music.push((item_id, hz)),
            other => panic!("import: {other:?}"),
        }
    }
    let mut liners = vec![];
    for i in 0..n_liners {
        let hz = 2000.0 + 200.0 * i as f64;
        let p = tone(&src, &format!("l{i}.wav"), hz, 6.0);
        let opts = ImportOptions {
            category: Some("liner".into()),
            source: "test".into(),
        };
        match import_file(&lib, &p, &opts) {
            ImportOutcome::Done { item_id, .. } => liners.push((item_id, hz)),
            other => panic!("import: {other:?}"),
        }
    }
    World {
        dir,
        lib,
        music,
        liners,
    }
}

impl World {
    pub fn playlist(&self, name: &str, mode: Mode, no_repeat: i64, ids: &[i64]) -> i64 {
        let mut p = NewPlaylist::shuffle(name);
        p.mode = mode;
        p.no_repeat_minutes = no_repeat;
        let pl = self.lib.create_playlist(&p).unwrap();
        for i in ids {
            self.lib.add_item_to_playlist(pl, *i, 1.0).unwrap();
        }
        pl
    }

    pub fn liner_block(&self, pl: i64, n: i64) {
        let cid = self.lib.category_id("liner").unwrap().unwrap();
        self.lib
            .add_block_to_playlist(
                pl,
                &NewBlock::insert_every("liners", n, serde_json::json!({"category_id": cid})),
            )
            .unwrap();
    }

    pub fn ids(&self) -> Vec<i64> {
        self.music.iter().map(|m| m.0).collect()
    }

    pub fn opts(&self, default_playlist: &str) -> StationOptions {
        let mut o = StationOptions::new("office", self.lib.paths.media.clone());
        o.default_playlist = Some(default_playlist.into());
        o.record_sidecar = true;
        o
    }

    pub fn state_path(&self) -> PathBuf {
        self.dir.join("stations").join("office").join("state.db")
    }

    pub fn open(&self, default_playlist: &str, seed: u64, with_state: bool) -> Offline {
        let mut o = self.opts(default_playlist);
        o.seed = seed;
        let (st, mixer) = Station::open(
            o,
            with_state.then(|| self.state_path()),
            Some(self.lib.paths.db.clone()),
            T0,
        )
        .unwrap();
        Offline::new(st, mixer, T0)
    }
}

/// The amplitude of the component at `hz` in `x[start..start + win]` (a correlation with a complex
/// exponential); `win` should hold whole cycles of the tones present.
pub fn tone_amp(x: &[f32], hz: f64, start: usize, win: usize) -> f64 {
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (k, &v) in x[start..start + win].iter().enumerate() {
        let ph = std::f64::consts::TAU * hz * (start + k) as f64 / f64::from(RATE);
        re += f64::from(v) * ph.cos();
        im += f64::from(v) * ph.sin();
    }
    2.0 * (re * re + im * im).sqrt() / win as f64
}

#[test]
fn a_station_plays_a_rotation_with_a_liner_every_second_song() {
    let w = world("smoke", 4, 12.0, 2);
    let pl = w.playlist("Office", Mode::Shuffle, 0, &w.ids());
    w.liner_block(pl, 2);
    let t = std::time::Instant::now();
    let mut off = w.open("Office", 3, true);
    off.start().unwrap();
    off.run_until(80.0, &[]).unwrap();
    println!("rendered 80 s in {:.2} s", t.elapsed().as_secs_f64());
    let rows = &off.station.as_run;
    for r in rows {
        println!(
            "{:>8.2}s {:?} {} {:?} {:?}",
            (r.started_ms - T0) as f64 / 1000.0,
            r.origin,
            r.title,
            r.end_reason,
            r.played_ms
        );
    }
    assert!(rows.len() >= 5);
    let origins: Vec<&str> = rows.iter().map(|r| r.origin.as_str()).collect();
    assert_eq!(&origins[..3], ["rotation", "rotation", "insert"]);
}

// ---- 1. a reshuffle never touches the air -------------------------------------------------------------

#[test]
fn m15_1_a_reshuffle_at_a_random_time_leaves_the_audio_up_to_the_committed_items_successor_identical(
) {
    // Six 20 s tones (butt joints under the default preset), lock-ahead 8 s: a reshuffle lands sometimes
    // before the next item is committed and sometimes after.
    let w = world("reshuffle", 6, 20.0, 0);
    let pl = w.playlist("Rot", Mode::Shuffle, 0, &w.ids());
    let mut trials = 0;
    let mut changed_after = 0;
    let mut locked_trials = 0;
    let t_all = std::time::Instant::now();
    for seed in 0..5u64 {
        let open = |seed: u64| {
            let mut o = w.opts("Rot");
            o.seed = 500 + seed;
            o.lock_ahead_s = 8;
            let (st, mixer) = Station::open(o, None, Some(w.lib.paths.db.clone()), T0).unwrap();
            Offline::new(st, mixer, T0)
        };
        let _ = pl;
        let mut reference = open(seed);
        reference.start().unwrap();
        reference.run_until(43.0, &[]).unwrap();
        let ref_left = reference.left();
        let bounds: Vec<u64> = reference
            .station
            .cond
            .sidecar
            .boundaries
            .iter()
            .map(|b| b.s_b)
            .collect();
        let ref_items: Vec<i64> = reference
            .station
            .as_run
            .iter()
            .filter_map(|r| r.item_id)
            .collect();
        assert!(bounds.len() >= 2 && ref_items.len() >= 3, "{bounds:?}");
        for k in 0..40u64 {
            let r = 0.5 + ((seed * 40 + k) as f64 * 7.13) % 19.0; // seconds into the first item
            let mut trial = open(seed);
            trial.start().unwrap();
            trial.run_until(43.0, &[(r, Op::Reshuffle)]).unwrap();
            assert!(
                trial.op_log.iter().all(|(_, _, res)| res.is_ok()),
                "{:?}",
                trial.op_log
            );
            let items: Vec<i64> = trial
                .station
                .as_run
                .iter()
                .filter_map(|r| r.item_id)
                .collect();
            // The head is committed once it is within lock-ahead of its start.
            let s2 = bounds[0] as f64 / f64::from(RATE);
            let locked = r >= s2 - 8.0;
            assert_eq!(items[0], ref_items[0], "on air changed (r {r})");
            let bound = if locked {
                assert_eq!(items[1], ref_items[1], "the committed item changed (r {r})");
                locked_trials += 1;
                bounds[1] as usize
            } else {
                if items[1] != ref_items[1] {
                    changed_after += 1;
                }
                bounds[0] as usize
            };
            let left = trial.left();
            assert!(left.len() >= bound && ref_left.len() >= bound);
            let diff = left[..bound]
                .iter()
                .zip(&ref_left[..bound])
                .position(|(a, b)| a != b);
            assert_eq!(
                diff, None,
                "audio differs before frame {bound} (r {r}, locked {locked})"
            );
            trials += 1;
        }
    }
    println!(
        "1: {trials} reshuffles at random times ({locked_trials} after the next item was committed): the audio before the committed item's successor is sample-identical; \
         the unlocked next item changed in {changed_after} of {} ({:.1} s)",
        trials - locked_trials,
        t_all.elapsed().as_secs_f64()
    );
    assert_eq!(trials, 200);
    assert!(locked_trials > 20 && changed_after > 0);
}

fn rms_db(x: &[f32]) -> f64 {
    if x.is_empty() {
        return -200.0;
    }
    let p: f64 = x.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / x.len() as f64;
    10.0 * p.max(1e-20).log10()
}

// ---- 5. playlist switch --------------------------------------------------------------------------------

fn two_playlists(name: &str) -> (World, i64, i64) {
    let w = world(name, 6, 30.0, 0);
    let ids = w.ids();
    let a = w.playlist("A", Mode::Shuffle, 0, &ids[..3]);
    let b = w.playlist("B", Mode::Shuffle, 0, &ids[3..]);
    (w, a, b)
}

fn sam_station(w: &World, seed: u64) -> Offline {
    let mut o = w.opts("A");
    o.seed = seed;
    o.preset = "sam".into();
    let (st, mixer) = Station::open(o, None, Some(w.lib.paths.db.clone()), T0).unwrap();
    Offline::new(st, mixer, T0)
}

#[test]
fn m15_5_switch_now_fades_within_two_seconds_segue_takes_the_next_segue_and_end_follows_the_cue_out(
) {
    let (w, _a, b) = two_playlists("switch");
    let b_items: Vec<i64> = w.ids()[3..].to_vec();
    let a_hz = |off: &Offline| -> f64 {
        let id = off.station.as_run[0].item_id.unwrap();
        w.music.iter().find(|m| m.0 == id).unwrap().1
    };

    // A reference run without a switch: where A's segue lands.
    let mut reference = sam_station(&w, 9);
    reference.start().unwrap();
    reference.run_until(45.0, &[]).unwrap();
    let ref_boundary = reference.station.cond.sidecar.boundaries[0].clone();
    let ref_items: Vec<i64> = reference
        .station
        .as_run
        .iter()
        .filter_map(|r| r.item_id)
        .collect();
    assert!(!b_items.contains(&ref_items[1]));

    // how = now, at 10 s.
    let mut off = sam_station(&w, 9);
    off.start().unwrap();
    off.run_until(
        20.0,
        &[(
            10.0,
            Op::Switch {
                playlist_id: b,
                how: SwitchHow::Now,
            },
        )],
    )
    .unwrap();
    let hz = a_hz(&off);
    assert!(
        b_items.contains(&off.station.as_run[1].item_id.unwrap()),
        "the new item is from B"
    );
    let op_frame = off.op_log[0].0 as usize;
    let left = off.left();
    let first = &off.station.cond.sidecar.voices[0];
    let gone = first.ended_frame.unwrap() as usize;
    let secs_to_silence = (gone - op_frame) as f64 / f64::from(RATE);
    let win = (RATE as usize / 10) * 10; // whole cycles of the 100 Hz-step tones
    let a_late = tone_amp(&left, hz, op_frame + (2.6 * f64::from(RATE)) as usize, win);
    let a_early = tone_amp(&left, hz, op_frame - win - 1000, win);
    println!("5: how=now: A gone {secs_to_silence:.2} s after the action; A level after {:.1} dB vs before", 20.0 * (a_late / a_early).log10());
    assert!(secs_to_silence <= 2.6, "{secs_to_silence}");
    assert!(a_late < a_early * 0.001);

    // how = segue, at 10 s: B's first item is the next segue, exactly where the reference put it.
    let mut off = sam_station(&w, 9);
    off.start().unwrap();
    off.run_until(
        45.0,
        &[(
            10.0,
            Op::Switch {
                playlist_id: b,
                how: SwitchHow::Segue,
            },
        )],
    )
    .unwrap();
    let items: Vec<i64> = off
        .station
        .as_run
        .iter()
        .filter_map(|r| r.item_id)
        .collect();
    assert_eq!(items[0], ref_items[0], "on air unchanged");
    assert!(
        b_items.contains(&items[1]),
        "the next item is from the new playlist"
    );
    let bd = &off.station.cond.sidecar.boundaries[0];
    println!(
        "5: how=segue: boundary at frame {} (reference {}), overlap {} frames",
        bd.s_b, ref_boundary.s_b, bd.overlap_frames
    );
    assert!(
        (bd.s_b as i64 - ref_boundary.s_b as i64).abs() < 48_000,
        "the segue point is A's own"
    );
    assert!(
        off.station.as_run[1].started_ms - T0 > 25_000,
        "B did not start at once"
    );

    // how = end, at 10 s: after the outgoing cue-out, no overlap.
    let mut off = sam_station(&w, 9);
    off.start().unwrap();
    off.run_until(
        45.0,
        &[(
            10.0,
            Op::Switch {
                playlist_id: b,
                how: SwitchHow::End,
            },
        )],
    )
    .unwrap();
    let items: Vec<i64> = off
        .station
        .as_run
        .iter()
        .filter_map(|r| r.item_id)
        .collect();
    assert!(b_items.contains(&items[1]));
    let bd = &off.station.cond.sidecar.boundaries[0];
    println!(
        "5: how=end: s_b {} f_end_a {} overlap {} frames (kind {})",
        bd.s_b, bd.f_end_a, bd.overlap_frames, bd.kind
    );
    assert!(bd.overlap_frames <= 0, "no overlap");
    assert!(bd.s_b as i64 >= bd.f_end_a, "B starts after A's cue-out");
    assert!(bd.s_b as i64 - bd.f_end_a <= 2, "and at once");
}

// ---- 7. library sync --------------------------------------------------------------------------------------

#[test]
fn m15_7_a_renamed_title_shows_in_now_within_three_seconds_and_a_deleted_library_stops_nothing() {
    let w = world("sync", 3, 10.0, 0);
    w.playlist("Rot", Mode::Shuffle, 0, &w.ids());
    let mut off = w.open("Rot", 4, true);
    off.start().unwrap();
    off.run_until(5.0, &[]).unwrap();
    let now = off.station.now().unwrap();
    assert!(now.title.starts_with('m'), "{}", now.title);
    // Rename in the library while the station runs.
    w.lib
        .set_item_title(now.item_id, "Renamed In Library")
        .unwrap();
    off.run_until(5.0 + 2.9, &[]).unwrap();
    let after = off.station.now().unwrap();
    println!(
        "7: title `{}` -> `{}` visible in now within 2.9 s",
        now.title, after.title
    );
    assert_eq!(after.title, "Renamed In Library");

    // Delete library.db entirely: playback goes on across a boundary (items are 10 s).
    let db = w.lib.paths.db.clone();
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", db.display()));
    }
    let n_before = off.station.as_run.len();
    off.run_until(30.0, &[]).unwrap();
    assert!(off.station.library_unavailable);
    assert!(off.station.as_run.len() > n_before, "items kept starting");
    let left = off.left();
    let tail = &left[(22.0 * f64::from(RATE)) as usize..];
    assert!(
        rms_db(tail) > -40.0,
        "audio kept sounding without library.db"
    );
    off.station.flush();
    drop(off);

    // A restart with no library.db at all plays from the track cache.
    let o = w.opts("Rot");
    let (st, mixer) =
        Station::open(o, Some(w.state_path()), Some(db.clone()), T0 + 60_000).unwrap();
    let mut off2 = Offline::new(st, mixer, T0 + 60_000);
    let info = off2.start().unwrap();
    off2.run_until(8.0, &[]).unwrap();
    assert!(off2.station.library_unavailable);
    let tail = &off2.left()[(2.0 * f64::from(RATE)) as usize..];
    println!(
        "7: restart without library.db: {info:?}, level {:.1} dB",
        rms_db(tail)
    );
    assert!(rms_db(tail) > -40.0);
    assert!(off2.station.now().is_some());
}

// ---- 6. restart and resume (offline; the real-process kill test is in the hotclock crate) ------------------------

#[test]
fn m15_6_a_restart_resumes_the_item_on_air_or_starts_the_next_and_the_queue_order_holds() {
    let w = world("resume", 6, 60.0, 0);
    w.playlist("Rot", Mode::Shuffle, 0, &w.ids());
    // Trials: (kill time in the first item, downtime), against a 60 s item whose segue is at ~60 s.
    for (kill_s, down_s, expect_resume) in [
        (12.0, 3.0, true),
        (30.5, 2.0, true),
        (37.0, 1.0, true),
        (44.0, 2.5, false),
        (55.0, 4.0, false),
    ] {
        // Fresh state for each trial.
        let _ = std::fs::remove_dir_all(w.dir.join("stations"));
        let mut a = w.open("Rot", 21, true);
        a.start().unwrap();
        a.run_until(kill_s, &[]).unwrap();
        let queue_at_kill: Vec<i64> = a.station.sched.st.queue.iter().map(|e| e.item_id).collect();
        let on_air = a.station.sched.on_air().unwrap().clone();
        let orig_start = a.station.as_run[0].started_ms;
        a.station.flush();
        drop(a); // the process is gone
        let restart_wall = T0 + ((kill_s + down_s) * 1000.0) as i64;
        let (st, mixer) = Station::open(
            w.opts("Rot"),
            Some(w.state_path()),
            Some(w.lib.paths.db.clone()),
            restart_wall,
        )
        .unwrap();
        let mut b = Offline::new(st, mixer, restart_wall);
        let info = b.start().unwrap();
        b.run_until(6.0, &[]).unwrap();
        let rows = b.station.as_run.clone();
        let first = &rows[0];
        if expect_resume {
            let (item, pos) = info.resumed.expect("resumed");
            assert_eq!(item, on_air.item_id);
            let expected = restart_wall + 300 - orig_start;
            let err = (pos - expected).abs();
            println!("6: killed at {kill_s} s, down {down_s} s: resumed at {pos} ms, expected {expected} ms (error {err} ms)");
            assert!(err < 1000, "{err}");
            assert!(first.flags.iter().any(|f| f == "resumed"));
            assert_eq!(first.item_id, Some(on_air.item_id));
            // The next item is the one that was next.
            let mut off = b;
            off.run_until(80.0 - kill_s - down_s, &[]).unwrap();
            let played: Vec<i64> = off
                .station
                .as_run
                .iter()
                .filter_map(|r| r.item_id)
                .collect();
            assert_eq!(
                played[1], queue_at_kill[0],
                "queue order after the resumed item is unchanged"
            );
        } else {
            assert!(info.resumed.is_none() && info.next_instead, "{info:?}");
            println!(
                "6: killed at {kill_s} s, down {down_s} s: the next item started ({:?})",
                first.item_id
            );
            assert_eq!(
                first.item_id,
                Some(queue_at_kill[0]),
                "the next item of the saved queue plays"
            );
            let after: Vec<i64> = b.station.sched.st.queue.iter().map(|e| e.item_id).collect();
            assert_eq!(
                &after[..3],
                &queue_at_kill[1..4],
                "queue order after it is unchanged"
            );
        }
        // The interrupted row was closed as a crash.
        let db_rows = state::read_as_run(&w.state_path()).unwrap();
        assert_eq!(
            db_rows[0].end_reason.as_deref(),
            Some("crash"),
            "{db_rows:?}"
        );
    }
}

// ---- operator actions take back a committed boundary ---------------------------------------------------------

#[test]
fn operator_actions_after_a_boundary_was_committed_act_on_what_is_sounding() {
    let w = world("ops", 6, 30.0, 0);
    w.playlist("Rot", Mode::Shuffle, 0, &w.ids());
    let hz = |id: i64| w.music.iter().find(|m| m.0 == id).unwrap().1;
    let win = (RATE as usize / 10) * 10;
    // Reference: 30 s items, boundary at ~30.3 s, committed 10 s before (about 20 s).
    for (name, op) in [("remove head", 0), ("insert next", 1), ("switch segue", 2)] {
        let mut a = w.open("Rot", 33, false);
        a.start().unwrap();
        a.run_until(22.0, &[]).unwrap();
        let queue: Vec<i64> = a.station.sched.st.queue.iter().map(|e| e.item_id).collect();
        let head_entry = a.station.sched.head().unwrap().id;
        let other = w
            .ids()
            .into_iter()
            .find(|i| !queue[..2].contains(i) && *i != a.station.sched.on_air().unwrap().item_id)
            .unwrap();
        let o = match op {
            0 => Op::Remove {
                entry_id: head_entry,
            },
            1 => Op::Insert {
                item_id: other,
                at: station::sched::At::Next,
            },
            _ => {
                let pl2 = w.playlist("Solo", Mode::Shuffle, 0, &[other]);
                let _ = pl2;
                Op::Reshuffle
            }
        };
        let mut b = w.open("Rot", 33, false);
        b.start().unwrap();
        b.run_until(70.0, &[(22.0, o)]).unwrap();
        let played: Vec<i64> = b.station.as_run.iter().filter_map(|r| r.item_id).collect();
        let cancelled = b
            .station
            .cond
            .sidecar
            .voices
            .iter()
            .filter(|v| v.end_reason.as_deref() == Some("cancelled"))
            .count();
        println!(
            "ops `{name}`: played {played:?}, queue was {queue:?}, cancelled voices {cancelled}"
        );
        match op {
            0 => {
                assert_eq!(played[1], queue[1], "the removed head never plays");
                assert!(cancelled >= 1);
                let left = b.left();
                let at = (31.0 * f64::from(RATE)) as usize;
                assert!(
                    tone_amp(&left, hz(queue[0]), at, win) < 0.001,
                    "the removed item sounded"
                );
            }
            1 => {
                assert_eq!(played[1], other, "the inserted item plays next");
                assert_eq!(played[2], queue[0]);
                assert!(cancelled >= 1);
            }
            _ => assert_eq!(played[1], queue[0], "a reshuffle leaves the committed head"),
        }
        assert_eq!(played[0], a.station.sched.on_air().unwrap().item_id);
    }
}

#[test]
fn a_play_album_run_is_gapless_and_transport_ops_hold_and_resume() {
    let w = world("album", 4, 15.0, 0);
    // Tag three of the tones as one album.
    for (n, (id, _)) in w.music.iter().take(3).enumerate() {
        w.lib
            .conn()
            .execute(
                "UPDATE items SET album_key='mark|drift', disc_no=1, track_no=?2, track_total=3 WHERE id=?1",
                rusqlite::params![id, n as i64 + 1],
            )
            .unwrap();
    }
    let others = vec![w.music[3].0];
    w.playlist("Rot", Mode::Shuffle, 0, &others);
    let mut off = w.open("Rot", 5, false);
    off.start().unwrap();
    off.run_until(
        58.0,
        &[
            (
                2.0,
                Op::PlayAlbum {
                    album_key: "mark|drift".into(),
                    gapless: true,
                    at: station::sched::At::Next,
                },
            ),
            (47.0, Op::Hold),
        ],
    )
    .unwrap();
    let kinds: Vec<String> = off
        .station
        .cond
        .sidecar
        .boundaries
        .iter()
        .map(|b| b.kind.clone())
        .collect();
    println!(
        "album: boundary kinds {kinds:?}; transport {:?}",
        off.station.transport
    );
    assert_eq!(
        kinds.iter().filter(|k| k.as_str() == "continuity").count(),
        2,
        "{kinds:?}"
    );
    assert_eq!(off.station.transport.name(), "held");
    // After the hold the station is silent until resume; resume starts the next entry.
    let n = off.station.as_run.len();
    off.run_until(80.0, &[(66.0, Op::Resume)]).unwrap();
    assert!(off.station.as_run.len() > n, "resume started an item");
}

// ---- review (Opus), M1.5: actions racing a committed boundary -------------------------------------------

/// Every operator action pressed in the last moments before a committed item starts (inside the mixer's
/// lookahead plus the minimum lead, where its boundary can no longer be taken back safely): nothing
/// plays twice, nothing is left silent that should sound, and no item is stopped twice. Before the
/// review, `next` there planned from the committed item into itself (two voices of it), and an edit
/// took the boundary back after the mixer could already have started the item.
#[test]
fn an_action_racing_the_committed_items_start_never_plays_it_twice_or_leaves_a_gap() {
    use engine::source::Source;
    use station::runtime::OpError;
    // 30.01 s items: B's start then falls 800 frames into a 1024-frame render block, so the last block rendered
    // before the action ends more than the mixer's lookahead (96 frames at M1.8, 336 with the true-peak
    // limiter's 5 ms in the path, M6a) short of it and B's start has not yet been mixed. At 30.0 s it fell 320
    // frames in, which is inside the 336, and the action then met a started item (a different case).
    let w = world("race", 4, 30.01, 0);
    w.playlist("Rot", Mode::Shuffle, 0, &w.ids());
    // Where the first boundary lands.
    let mut r = w.open("Rot", 17, false);
    r.start().unwrap();
    r.run_until(25.0, &[]).unwrap();
    let s_b = r.station.cond.placed.last().unwrap().start_frame;
    let b_item = r.station.sched.head().unwrap().item_id;
    let other = w
        .ids()
        .into_iter()
        .find(|i| {
            *i != b_item
                && *i != r.station.sched.on_air().unwrap().item_id
                && r.station.sched.st.queue.get(1).map(|e| e.item_id) != Some(*i)
        })
        .unwrap();
    let ops: Vec<(&str, Op)> = vec![
        ("next", Op::Next),
        ("fade_next", Op::FadeNext(None)),
        ("cut", Op::Cut),
        ("hold", Op::Hold),
        ("reshuffle", Op::Reshuffle),
        (
            "insert next",
            Op::Insert {
                item_id: other,
                at: station::sched::At::Next,
            },
        ),
        ("remove head", Op::Remove { entry_id: 0 }),
        ("stop", Op::Stop { fade_s: None }),
    ];
    for (name, op) in ops {
        for before in [40u64, 2_000, 9_000, 30_000] {
            let mut b = w.open("Rot", 17, false);
            b.start().unwrap();
            // Render to the last block before the action's frame, then act at that frame.
            let at = s_b - before;
            let block = engine::mixer::MAX_BLOCK as u64;
            b.run_until(((at / block) * block) as f64 / f64::from(RATE), &[])
                .unwrap();
            assert_eq!(b.station.cond.placed.last().unwrap().start_frame, s_b);
            let op = match op {
                Op::Remove { .. } => Op::Remove {
                    entry_id: b.station.sched.head().unwrap().id,
                },
                ref o => o.clone(),
            };
            let wall = b.wall_of(at);
            let res = {
                let mixer = &mut b.mixer;
                let mut send = |c: engine::rt::Command| mixer.command(&c.kind);
                b.station.op(op.clone(), at, wall, &mut send)
            };
            let busy = matches!(res, Err(OpError::Busy));
            // A `Busy` action is retried from the next block, as the API's caller does.
            let retry: Vec<(f64, Op)> = if busy {
                vec![(0.0, op.clone())]
            } else {
                vec![]
            };
            b.run_until(75.0, &retry).unwrap();
            let b_path = b
                .station
                .cat
                .get(b_item)
                .unwrap()
                .item()
                .unwrap()
                .path
                .display()
                .to_string();
            let started_b = b
                .station
                .cond
                .sidecar
                .voices
                .iter()
                .filter(|v| {
                    v.path == b_path
                        && v.started_frame.is_some_and(|f| {
                            f + 30 * u64::from(RATE) > s_b && f < s_b + 25 * u64::from(RATE)
                        })
                })
                .count();
            let played: Vec<i64> = b.station.as_run.iter().filter_map(|r| r.item_id).collect();
            println!(
                "race `{name}` {before} frames before B: {} ; B voices started {started_b}; played {played:?}",
                if busy { "Busy, retried" } else { "done" }
            );
            assert!(started_b <= 1, "`{name}`: the committed item started twice");
            for w2 in played.windows(2) {
                assert_ne!(w2[0], w2[1], "`{name}`: an item played twice in a row");
            }
            if before == 40 {
                assert!(
                    busy || matches!(name, "next" | "fade_next" | "cut" | "stop"),
                    "`{name}` inside the lead must wait for the item to start"
                );
            }
            if before == 30_000 {
                assert!(!busy, "`{name}` 0.6 s ahead is taken back at once");
            }
            if !matches!(name, "hold" | "stop") {
                // No gap: every 50 ms window from 1 s to 55 s holds a tone.
                let left = b.left();
                let win = (RATE / 20) as usize;
                let (at_s, quietest) = (RATE as usize..55 * RATE as usize)
                    .step_by(win)
                    .map(|i| (i as f64 / f64::from(RATE), rms_db(&left[i..i + win])))
                    .fold((0.0, f64::INFINITY), |a, b| if b.1 < a.1 { b } else { a });
                assert!(
                    quietest > -40.0,
                    "`{name}` {before}: a gap ({quietest:.1} dB at {at_s:.2} s, s_b {:.3} s, {} frames rendered)",
                    s_b as f64 / f64::from(RATE),
                    left.len()
                );
            }
        }
    }
}

/// A restart keeps the transport the operator left (1.7: the checkpoint carries it). Before the review
/// a stopped station came back playing the next item, and a held one played on past its hold.
#[test]
fn a_restart_keeps_a_stopped_station_stopped_and_a_held_one_held() {
    let w = world("transport", 4, 60.0, 0);
    w.playlist("Rot", Mode::Shuffle, 0, &w.ids());
    for (name, op) in [("stop", Op::Stop { fade_s: None }), ("hold", Op::Hold)] {
        let _ = std::fs::remove_dir_all(w.dir.join("stations"));
        let mut a = w.open("Rot", 4, true);
        a.start().unwrap();
        a.run_until(10.0, &[(5.0, op)]).unwrap();
        let on_air = a.station.sched.on_air().unwrap().item_id;
        a.station.flush();
        drop(a);
        let restart_wall = T0 + 12_000;
        let (st, mixer) = Station::open(
            w.opts("Rot"),
            Some(w.state_path()),
            Some(w.lib.paths.db.clone()),
            restart_wall,
        )
        .unwrap();
        let mut b = Offline::new(st, mixer, restart_wall);
        let info = b.start().unwrap();
        b.run_until(70.0, &[]).unwrap();
        let played: Vec<i64> = b.station.as_run.iter().filter_map(|r| r.item_id).collect();
        println!(
            "restart after `{name}`: {info:?}, transport {}, played {played:?}",
            b.station.transport.name()
        );
        match name {
            "stop" => {
                assert!(info.silent && info.resumed.is_none());
                assert_eq!(b.station.transport.name(), "stopped");
                assert!(played.is_empty(), "a stopped station played {played:?}");
            }
            _ => {
                assert_eq!(
                    info.resumed.map(|r| r.0),
                    Some(on_air),
                    "the held item resumes"
                );
                assert_eq!(b.station.transport.name(), "held");
                assert_eq!(played, vec![on_air], "nothing follows a held item");
            }
        }
        // `resume` brings it back.
        b.run_until(80.0, &[(71.0, Op::Resume)]).unwrap();
        assert!(
            b.station.as_run.len() > played.len(),
            "`resume` after `{name}` plays"
        );
    }
}

/// A `library.db` replaced under the same name (a restore from backup, a copy renamed over it) is a
/// new file: the station reopens it and sees its contents. Before the review its read-only connection
/// stayed on the old file, whose `data_version` never changes again, so the station never synced again.
#[test]
fn a_replaced_library_db_is_reopened_and_read() {
    let w = world("replace", 3, 10.0, 0);
    w.playlist("Rot", Mode::Shuffle, 0, &w.ids());
    let mut off = w.open("Rot", 4, true);
    off.start().unwrap();
    off.run_until(4.0, &[]).unwrap();
    let now = off.station.now().unwrap();
    let db = w.lib.paths.db.clone();
    let copy = w.dir.join("restored.db");
    w.lib
        .conn()
        .execute(&format!("VACUUM INTO '{}'", copy.display()), [])
        .unwrap();
    {
        let c = rusqlite::Connection::open(&copy).unwrap();
        c.execute(
            "UPDATE items SET title = 'From The Restore' WHERE id = ?1",
            [now.item_id],
        )
        .unwrap();
    }
    for ext in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", db.display()));
    }
    std::fs::rename(&copy, &db).unwrap();
    off.run_until(4.0 + 2.9, &[]).unwrap();
    let after = off.station.now().unwrap();
    println!(
        "replaced library.db: `{}` -> `{}` within 2.9 s",
        now.title, after.title
    );
    assert_eq!(after.title, "From The Restore");
    assert!(!off.station.library_unavailable);
}
