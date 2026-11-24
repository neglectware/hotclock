//! The automatic unload of a stopped program (`programs.unload_after_min`, `station::unload`), on the virtual clock
//! of the offline renderer with real audio: a small show (music, then a timed stop at T0 + 60 s) on a part-time
//! station with no default playlist, as on the box's eve. Every wait below is virtual time.

#[path = "support_m5/mod.rs"]
mod common_m5;

use common_m5::{local, MIN, SEC};
use engine::render::{render_tone, ToneRender};
use engine::source::Source as _;
use engine::wav::WavFormat;
use library::playlists::{Mode, NewBlock, NewPlaylist};
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use serde_json::json;
use station::offline::Offline;
use station::runtime::{Op, Station, StationOptions};
use station::sched::{At, SwitchHow};
use std::path::PathBuf;

const RATE: u32 = 48_000;

struct World {
    dir: PathBuf,
    lib: Library,
    show: i64,
    music: Vec<i64>,
}

impl Drop for World {
    fn drop(&mut self) {
        if std::env::var_os("HOTCLOCK_KEEP_TEST_DIRS").is_none_or(|v| v != "1") {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

impl World {
    fn new(name: &str) -> World {
        let dir =
            fixtures::testdir::process_dir().join(format!("unload-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let lib = Library::open(Paths::from_data_dir(&dir.join("data"))).unwrap();
        let mut music = vec![];
        for i in 0..4 {
            let p = dir.join("src").join(format!("m{i}.wav"));
            render_tone(&ToneRender {
                out: p.clone(),
                rate: RATE,
                seconds: 45.0,
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
                ImportOutcome::Done { item_id, .. } => music.push(item_id),
                other => panic!("import: {other:?}"),
            }
        }
        let mut pl = NewPlaylist::shuffle("Show music");
        pl.no_repeat_minutes = 0;
        let pre = lib.create_playlist(&pl).unwrap();
        for i in &music {
            lib.add_item_to_playlist(pre, *i, 1.0).unwrap();
        }
        let show = lib
            .create_playlist(&NewPlaylist {
                name: "The show".into(),
                mode: Mode::Sequential,
                no_repeat_minutes: 0,
                separation: serde_json::Value::Object(Default::default()),
                station_id: None,
            })
            .unwrap();
        let blocks = [
            (
                "Expected Mondays",
                "start",
                "first",
                json!({"schedule": [{"cron": "45 18 * * 1"}], "anchor": "manual", "arm_window_min": 60, "fallback_start": {"enabled": false}}),
            ),
            (
                "Music",
                "play",
                "sequence",
                json!({"source": {"playlist_id": pre}, "until": "next"}),
            ),
            (
                "Stop",
                "stop",
                "timed",
                json!({"when": {"kind": "relative", "offset_ms": 60_000}, "how": "now", "fade_ms": 2000}),
            ),
        ];
        for (n, k, t, p) in blocks {
            lib.add_block_to_playlist(
                show,
                &NewBlock {
                    name: n.into(),
                    kind: k.into(),
                    trigger: t.into(),
                    priority: 100,
                    params: p,
                },
            )
            .unwrap();
        }
        World {
            dir,
            lib,
            show,
            music,
        }
    }

    fn open(&self, wall: i64, unload_after_min: Option<u32>) -> Offline {
        let mut o = StationOptions::new("eve", self.lib.paths.media.clone());
        o.default_playlist = None;
        o.stopped_is_normal = true;
        o.time_zone = "America/New_York".into();
        o.seed = 7;
        o.unload_after_min = unload_after_min;
        let sp = self.dir.join("stations").join("eve").join("state.db");
        let (mut st, mixer) =
            Station::open(o, Some(sp), Some(self.lib.paths.db.clone()), wall).unwrap();
        st.side = Box::new(station::side::SharedRecording::default());
        let mut off = Offline::new(st, mixer, wall);
        off.keep_samples = false;
        off
    }
}

fn press(show: i64) -> Op {
    Op::StartProgram {
        playlist_id: show,
        t0: station::program::T0Req::Now,
        how: SwitchHow::Now,
        source: "deck",
    }
}

/// Monday 18:44 in New York: the press is at 5 s, so the show's timed stop is at 65 s and silent by 67.1 s.
fn origin() -> i64 {
    local("2027-01-18T18:44:00")
}
const PRESS_S: f64 = 5.0;
const STOP_S: f64 = 65.0;

fn loaded(off: &Offline) -> bool {
    off.station.sched.st.program.is_some()
}

/// The rows the End program path wrote: `(name, how)`.
fn end_rows(off: &Offline) -> Vec<(String, String)> {
    off.station
        .sched
        .st
        .log
        .rows
        .iter()
        .filter(|r| r.kind == "end_program")
        .map(|r| (r.name.clone(), r.how.clone()))
        .collect()
}

#[test]
fn a_program_that_ends_on_its_own_unloads_after_the_setting() {
    let w = World::new("ends");
    let mut off = w.open(origin(), Some(2));
    off.start().unwrap();
    off.run_until(STOP_S + 10.0, &[(PRESS_S, press(w.show))])
        .unwrap();
    assert_eq!(
        off.station.transport.name(),
        "stopped",
        "the show's stop block stopped it"
    );
    assert!(loaded(&off), "the program is still loaded after its stop");
    let due = off.station.unload_due_ms().expect("a countdown runs");
    // The countdown starts when the stop's fade has ended (65 s + 2 s fade), at the first step after it.
    let silent_at = wall_s_plain(STOP_S + 2.0);
    println!(
        "stop at {STOP_S} s, unload due {} s after the fade ended",
        (due - silent_at) as f64 / 1000.0
    );
    assert!(
        (due - silent_at - 2 * MIN).abs() < 200,
        "due 2 min after the fade: {}",
        due - silent_at
    );
    off.run_until(STOP_S + 2.0 + 119.0, &[]).unwrap();
    assert!(loaded(&off), "still loaded 1:59 into the countdown");
    assert!(end_rows(&off).is_empty());
    off.run_until(STOP_S + 2.0 + 121.0, &[]).unwrap();
    assert!(!loaded(&off), "unloaded 2 min after the stop");
    assert!(
        off.station.sched.st.queue.is_empty(),
        "the queue is cleared"
    );
    assert_eq!(off.station.transport.name(), "stopped");
    assert!(off.station.announce_clear, "the displays are to be told");
    assert_eq!(
        end_rows(&off),
        [(
            "End program and stop the audio".to_string(),
            "automatic".to_string()
        )]
    );
    let row = off
        .station
        .sched
        .st
        .log
        .rows
        .iter()
        .find(|r| r.kind == "end_program")
        .unwrap();
    println!(
        "log row: {} {} at {} ({})",
        row.name,
        row.how,
        row.fired_ms.unwrap(),
        row.detail
    );
    assert!(
        row.detail
            .contains("The show: unloaded automatically after 2 min"),
        "{}",
        row.detail
    );
    assert!(
        off.station.unload_due_ms().is_none(),
        "no countdown after the unload"
    );
    // Nothing starts again and nothing is logged twice.
    off.run_until(STOP_S + 2.0 + 30.0 * 60.0, &[]).unwrap();
    assert_eq!(end_rows(&off).len(), 1);
    assert!(off.station.cond.live_voices() == 0);
}

#[test]
fn the_operators_stop_unloads_after_the_setting() {
    let w = World::new("manual-stop");
    let mut off = w.open(origin(), Some(1));
    off.start().unwrap();
    // Stop at 30 s, long before the show's own stop (65 s): the timed stop never acts on a stopped station's audio.
    off.run_until(
        40.0,
        &[
            (PRESS_S, press(w.show)),
            (30.0, Op::Stop { fade_s: Some(1.5) }),
        ],
    )
    .unwrap();
    assert!(loaded(&off));
    let due = off
        .station
        .unload_due_ms()
        .expect("the operator's Stop starts a countdown");
    assert!(
        (due - wall_s_plain(31.5) - MIN).abs() < 500,
        "{}",
        due - wall_s_plain(31.5)
    );
    off.run_until(31.5 + 59.0, &[]).unwrap();
    assert!(loaded(&off), "still loaded at 0:59");
    off.run_until(31.5 + 61.0, &[]).unwrap();
    assert!(!loaded(&off), "unloaded after 1 min");
    assert_eq!(end_rows(&off)[0].1, "automatic");
}

#[test]
fn a_start_resume_cue_or_keep_loaded_during_the_countdown_cancels_it() {
    let w = World::new("cancel");
    let mut off = w.open(origin(), Some(1));
    off.start().unwrap();
    // Stopped at 20 s, resumed at 50 s: the countdown ends with the resume.
    off.run_until(
        55.0,
        &[
            (PRESS_S, press(w.show)),
            (20.0, Op::Stop { fade_s: Some(1.0) }),
            (50.0, Op::Resume),
        ],
    )
    .unwrap();
    assert_eq!(off.station.transport.name(), "playing");
    assert!(
        off.station.unload_due_ms().is_none(),
        "a resume ends the countdown"
    );
    assert!(off.station.sched.st.unload.since_ms.is_none());
    // The show's own stop at 65 s starts a new countdown; a cue (an item added to the queue) at 100 s cancels it
    // for this stop: the program stays loaded past the minute.
    off.run_until(
        100.5,
        &[(
            100.0,
            Op::Insert {
                item_id: w.music[0],
                at: At::Next,
            },
        )],
    )
    .unwrap();
    assert!(
        off.station.unload_due_ms().is_none(),
        "a cue cancels the countdown"
    );
    assert!(off.station.sched.st.unload.kept);
    off.run_until(STOP_S + 2.0 + 5.0 * 60.0, &[]).unwrap();
    assert!(loaded(&off), "kept loaded for this stop");
    assert_eq!(off.station.transport.name(), "stopped");
    assert!(end_rows(&off).is_empty());
    // A start, then a stop: a fresh countdown, which Keep loaded cancels.
    let t = STOP_S + 2.0 + 5.0 * 60.0;
    off.run_until(
        t + 20.0,
        &[
            (t + 1.0, Op::Resume),
            (t + 10.0, Op::Stop { fade_s: Some(1.0) }),
        ],
    )
    .unwrap();
    assert!(
        off.station.unload_due_ms().is_some(),
        "a new stop, a new countdown"
    );
    off.station.keep_loaded().unwrap();
    assert!(off.station.unload_due_ms().is_none());
    assert!(off.station.keep_loaded().is_err(), "nothing left to cancel");
    off.run_until(t + 20.0 + 3.0 * 60.0, &[]).unwrap();
    assert!(loaded(&off), "Keep loaded holds");
    // A program start after the stop (the next show) plays as before, and its own stop counts afresh.
    off.run_until(t + 200.0 + 70.0, &[(t + 200.0, press(w.show))])
        .unwrap();
    assert_eq!(
        off.station.transport.name(),
        "stopped",
        "the new run stopped at its own T0 + 60 s"
    );
    let t0 = off.station.sched.st.program.as_ref().unwrap().t0_ms;
    assert_eq!(off.station.sched.st.unload.program, Some((w.show, t0)));
    assert!(off.station.unload_due_ms().is_some());
}

#[test]
fn never_keeps_the_program_loaded() {
    let w = World::new("never");
    let mut off = w.open(origin(), None);
    off.start().unwrap();
    off.run_until(STOP_S + 2.0 + 61.0 * 60.0, &[(PRESS_S, press(w.show))])
        .unwrap();
    assert!(loaded(&off), "loaded 61 min after the stop");
    assert!(off.station.unload_due_ms().is_none());
    assert!(end_rows(&off).is_empty());
    // Switched to 1 min while it stands stopped (the live setting): the minute counts from the stop.
    let mut v = serde_json::Map::new();
    v.insert("unload_after_min".into(), json!(1));
    off.station.apply_live(&v).unwrap();
    off.run_until(STOP_S + 2.0 + 61.0 * 60.0 + 1.0, &[])
        .unwrap();
    assert!(!loaded(&off), "the new setting applies at once");
}

#[test]
fn a_restart_during_the_countdown_still_unloads_on_time() {
    let w = World::new("restart");
    let mut off = w.open(origin(), Some(5));
    off.start().unwrap();
    off.run_until(STOP_S + 2.0 + 120.0, &[(PRESS_S, press(w.show))])
        .unwrap();
    let due = off.station.unload_due_ms().unwrap();
    off.station.flush();
    drop(off);
    // Down for a minute: back at 3 min into the countdown.
    let back = wall_s_plain(STOP_S + 2.0 + 180.0);
    let mut off = w.open(back, Some(5));
    off.start().unwrap();
    off.run_until(0.1, &[]).unwrap();
    assert_eq!(
        off.station.transport.name(),
        "stopped",
        "it comes back stopped"
    );
    assert!(loaded(&off), "and loaded");
    assert_eq!(
        off.station.unload_due_ms(),
        Some(due),
        "the same deadline after the restart"
    );
    off.run_until(119.0, &[]).unwrap();
    assert!(loaded(&off), "loaded until the deadline");
    off.run_until(121.0, &[]).unwrap();
    assert!(
        !loaded(&off),
        "unloaded at the deadline, 2 min after the restart"
    );
    let row = off
        .station
        .sched
        .st
        .log
        .rows
        .iter()
        .find(|r| r.kind == "end_program")
        .unwrap();
    println!(
        "deadline {due}, unloaded at {} ({} ms late)",
        row.fired_ms.unwrap(),
        row.fired_ms.unwrap() - due
    );
    assert!((0..100).contains(&(row.fired_ms.unwrap() - due)));
    // The unload outlives another restart.
    off.station.flush();
    drop(off);
    let mut off = w.open(back + 10 * MIN, Some(5));
    off.start().unwrap();
    off.run_until(1.0, &[]).unwrap();
    assert!(!loaded(&off), "still unloaded after a restart");
    assert_eq!(end_rows(&off).len(), 1);
}

fn wall_s_plain(s: f64) -> i64 {
    origin() + (s * 1000.0) as i64
}

#[test]
fn a_program_stopped_longer_than_the_setting_unloads_at_startup() {
    let w = World::new("overdue");
    let mut off = w.open(origin(), Some(5));
    off.start().unwrap();
    off.run_until(STOP_S + 2.0 + 60.0, &[(PRESS_S, press(w.show))])
        .unwrap();
    assert!(loaded(&off));
    off.station.flush();
    drop(off);
    // Back 20 minutes after the stop: overdue by 15.
    let mut off = w.open(wall_s_plain(STOP_S + 2.0 + 20.0 * 60.0), Some(5));
    off.start().unwrap();
    assert!(loaded(&off), "loaded until the first step");
    off.run_until(0.05, &[]).unwrap();
    assert!(!loaded(&off), "unloaded at the first step");
    assert_eq!(end_rows(&off)[0].1, "automatic");
    assert!(off.station.sched.st.queue.is_empty());
}

/// What the automatic unload leaves is what the operator's End program and stop the audio leaves: the same queue,
/// program, transport, scheduler state and block log (but for who did it).
#[test]
fn the_end_state_is_the_one_of_the_operators_end_program() {
    let w = World::new("same");
    let run = |auto: bool| {
        let mut off = w.open(origin(), Some(1));
        off.start().unwrap();
        off.run_until(STOP_S + 10.0, &[(PRESS_S, press(w.show))])
            .unwrap();
        if auto {
            off.run_until(STOP_S + 2.0 + 61.0, &[]).unwrap();
        } else {
            // The "⋯" menu's End program and stop the audio, one second into the countdown.
            let mixer = &mut off.mixer;
            let mut send = |c: engine::rt::Command| mixer.command(&c.kind);
            off.station.program_abort(true, &mut send).unwrap();
            off.run_until(STOP_S + 2.0 + 61.0, &[]).unwrap();
        }
        let s = &off.station;
        let st = &s.sched.st;
        let log: Vec<_> = st
            .log
            .rows
            .iter()
            .map(|r| (r.block_id, r.name.clone(), r.kind.clone(), r.run_id))
            .collect();
        let hows: Vec<_> = st
            .log
            .rows
            .iter()
            .filter(|r| r.kind == "end_program")
            .map(|r| r.how.clone())
            .collect();
        let shape = json!({
            "program": st.program.is_some(), "closed": st.program_closed, "queue": st.queue.len(),
            "on_air": st.on_air.is_some(), "scheduled": st.scheduled_program, "transport": s.transport.name(),
            "seq_acts": st.seq_acts.len(), "bt": st.bt_display.is_some(), "unload": st.unload.since_ms.is_some(),
            "voices": s.cond.live_voices(), "announce": s.announce_clear, "now": s.on_air_info().is_some(),
            "upcoming": s.sched.upcoming(wall_s_plain(STOP_S + 70.0), 48).len(),
        });
        drop(off);
        let _ = std::fs::remove_dir_all(w.dir.join("stations"));
        (shape, log, hows)
    };
    let (manual, log_m, how_m) = run(false);
    let (auto, log_a, how_a) = run(true);
    println!("manual: {manual}\nauto:   {auto}");
    assert_eq!(manual, auto);
    assert_eq!(log_m, log_a, "the same block log rows");
    assert_eq!(how_m, ["operator"]);
    assert_eq!(how_a, ["automatic"]);
    assert_eq!(manual["program"], false);
    assert_eq!(manual["queue"], 0);
    let _ = SEC;
}
