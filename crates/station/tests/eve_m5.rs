//! M5 acceptance 5: the `ita_monday` fixture rendered offline on a virtual clock with real audio (the
//! conductor, the mixer and `Station::step`), an OSC recorder in place of the console, and the checks
//! (a) to (f), (i) of architecture 15. Wall time is New York local time on 2027-01-18 (a Monday).

#[path = "support_m5/mod.rs"]
mod common_m5;

use common_m5::{local, SEC};
use engine::render::{render_tone, ToneRender};
use engine::source::Source as _;
use engine::wav::WavFormat;
use library::examples::{ItaMonday, OscTarget};
use library::playlists::{Mode, NewPlaylist};
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use station::offline::Offline;
use station::runtime::{Op, Station, StationOptions};
use station::sched::SwitchHow;
use station::side::{SharedRecording, SideAction};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const RATE: u32 = 48_000;

fn tmpdir(name: &str) -> PathBuf {
    let d = fixtures::testdir::process_dir().join(format!("m5-{}-{name}", std::process::id()));
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

struct World {
    dir: PathBuf,
    lib: Library,
    monday: i64,
    pre_hz: Vec<f64>,
    bed_hz: Vec<f64>,
    bed_id: i64,
    ex: ItaMonday,
}

/// Each world holds about 1 GB of audio, so it goes as soon as its test ends, not at process exit.
impl Drop for World {
    fn drop(&mut self) {
        if std::env::var_os("HOTCLOCK_KEEP_TEST_DIRS").is_none_or(|v| v != "1") {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

impl World {
    fn new(name: &str, pre_s: &[f64], bed_s: &[f64]) -> World {
        let dir = tmpdir(name);
        let lib = Library::open(Paths::from_data_dir(&dir.join("data"))).unwrap();
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let import = |tag: &str, secs: &[f64], hz0: f64| -> (Vec<i64>, Vec<f64>) {
            let (mut ids, mut hzs) = (vec![], vec![]);
            for (i, s) in secs.iter().enumerate() {
                let hz = hz0 + 100.0 * i as f64;
                let p = tone(&src, &format!("{tag}{i}.wav"), hz, *s);
                match import_file(
                    &lib,
                    &p,
                    &ImportOptions {
                        category: Some("music".into()),
                        source: "test".into(),
                    },
                ) {
                    ImportOutcome::Done { item_id, .. } => {
                        ids.push(item_id);
                        hzs.push(hz);
                    }
                    other => panic!("import: {other:?}"),
                }
            }
            (ids, hzs)
        };
        let (pre, pre_hz) = import("p", pre_s, 300.0);
        let (bed, bed_hz) = import("b", bed_s, 1500.0);
        let mk = |name: &str, mode: Mode, ids: &[i64]| -> i64 {
            let mut p = NewPlaylist::shuffle(name);
            p.mode = mode;
            p.no_repeat_minutes = 0;
            let id = lib.create_playlist(&p).unwrap();
            for i in ids {
                lib.add_item_to_playlist(id, *i, 1.0).unwrap();
            }
            id
        };
        let pre_id = mk("Eve 80s library", Mode::Shuffle, &pre);
        let bed_id = mk("Eve elevator", Mode::Shuffle, &bed);
        let _idle = mk("Idle", Mode::Sequential, &[]);
        let mut ex = ItaMonday::new(pre_id, bed_id, OscTarget::wing_channel_mute("127.0.0.1", 2));
        ex.station_id = None;
        let monday = ex.create(&lib).unwrap();
        World {
            dir,
            lib,
            monday,
            pre_hz,
            bed_hz,
            bed_id,
            ex,
        }
    }

    fn open(&self, wall: i64, state: bool, seed: u64) -> (Offline, SharedRecording) {
        self.open_with(wall, state, seed, |_| {})
    }

    fn open_with(
        &self,
        wall: i64,
        state: bool,
        seed: u64,
        tweak: impl FnOnce(&mut StationOptions),
    ) -> (Offline, SharedRecording) {
        let mut o = StationOptions::new("eve", self.lib.paths.media.clone());
        o.default_playlist = Some("Idle".into());
        o.record_sidecar = true;
        o.time_zone = "America/New_York".into();
        o.seed = seed;
        tweak(&mut o);
        let sp = self.dir.join("stations").join("eve").join("state.db");
        let (mut st, mixer) = Station::open(
            o,
            state.then(|| sp.clone()),
            Some(self.lib.paths.db.clone()),
            wall,
        )
        .unwrap();
        let rec = SharedRecording::default();
        st.side = Box::new(rec.clone());
        (Offline::new(st, mixer, wall), rec)
    }
}

/// RMS in dBFS of consecutive 20 ms windows of the left channel.
type Sink = Box<dyn FnMut(u64, &[f32]) + Send>;

fn collector() -> (Arc<Mutex<Vec<f32>>>, Sink) {
    let v = Arc::new(Mutex::new(Vec::<f32>::new()));
    let v2 = v.clone();
    let mut acc = 0f64;
    let mut n = 0usize;
    const WIN: usize = RATE as usize / 50;
    (
        v,
        Box::new(move |_t, block: &[f32]| {
            for f in block.chunks_exact(2) {
                acc += f64::from(f[0]).powi(2);
                n += 1;
                if n == WIN {
                    let db = 10.0 * (acc / WIN as f64).max(1e-20).log10();
                    v2.lock().unwrap().push(db as f32);
                    acc = 0.0;
                    n = 0;
                }
            }
        }),
    )
}

fn win_of(frame_or_s: f64) -> usize {
    (frame_or_s * 50.0) as usize
}

/// Run the Monday program started by a button press at `press_s` seconds into a render that begins at
/// `origin`; returns the offline station and what it did.
fn run_monday(
    w: &World,
    origin: i64,
    press_s: f64,
    until_s: f64,
    seed: u64,
) -> (Offline, SharedRecording, Arc<Mutex<Vec<f32>>>) {
    let (mut off, rec) = w.open(origin, false, seed);
    let (levels, sink) = collector();
    off.keep_samples = false;
    off.sink = Some(sink);
    off.start().unwrap();
    let ops = vec![(
        press_s,
        Op::StartProgram {
            playlist_id: w.monday,
            t0: station::program::T0Req::Now,
            how: SwitchHow::Now,
            source: "deck",
        },
    )];
    off.run_until(until_s, &ops).unwrap();
    (off, rec, levels)
}

#[test]
fn ita_monday_audio_the_fade_the_mute_the_elevator_and_the_late_press() {
    // Pre-show items of 2:00 to 2:50, elevator items of 1:30 to 2:10.
    let w = World::new(
        "monday",
        &[120.0, 135.0, 150.0, 125.0, 140.0, 165.0],
        &[90.0, 100.0, 110.0, 130.0, 95.0],
    );
    for (label, press_late_s) in [("press at 18:45:00", 0.0), ("press at 18:45:15", 15.0)] {
        let origin = local("2027-01-18T18:44:00");
        let press_s = 60.0 + press_late_s;
        // Until two minutes past the intro at 19:00:00 plus the shift.
        let until_s = press_s + 16.5 * 60.0;
        let (off, rec, levels) = run_monday(&w, origin, press_s, until_s, 3);
        let sched = &off.station.sched;
        let pr = sched.st.program.as_ref().expect("program");
        let t0 = pr.t0_ms;
        let t0_f = (t0 - origin) as f64 / 1000.0;
        println!(
            "{label}: T0 = {} ({} s into the render)",
            station_zone(t0),
            t0_f
        );
        assert!((t0_f - press_s).abs() < 0.1, "T0 is the press");
        // (a) nothing plays before the press; the first sample comes one lead after it.
        let lv = levels.lock().unwrap().clone();
        let first_sound = lv.iter().position(|d| *d > -60.0).expect("sound") as f64 / 50.0;
        println!("(a) first sound {first_sound:.3} s, press at {t0_f:.3} s");
        assert!(first_sound >= t0_f, "nothing plays before the press");
        assert!(
            first_sound - t0_f < 0.40,
            "the first sample is within the start lead"
        );
        // (b) the master fade.
        let fade_start = t0_f + 880.0;
        let fade_end = fade_start + 7.0;
        let volumes = |a: f64, b: f64| -> Vec<f32> { lv[win_of(a)..win_of(b)].to_vec() };
        let before = volumes(fade_start - 3.0, fade_start - 0.5);
        let pre_db = before.iter().sum::<f32>() / before.len() as f32;
        // No voice boundary may fall inside the fade: the check needs one steady tone.
        let boundaries: Vec<f64> = off
            .station
            .cond
            .sidecar
            .voices
            .iter()
            .map(|v| v.start_frame as f64 / f64::from(RATE))
            .collect();
        println!("voice starts (s): {boundaries:?}");
        let clear = boundaries
            .iter()
            .all(|b| *b < fade_start - 4.0 || *b > fade_end + 0.1);
        assert!(
            clear,
            "a transition falls inside the fade; pick other lengths"
        );
        let mut worst = 0f64;
        // The level of a window is the mean of the squared gain over it: integrate the cosine over the
        // window's own 20 ms, not its value at one instant.
        let gain_at = |t: f64| -> f64 {
            let x = ((t - fade_start) / 7.0).clamp(0.0, 1.0);
            0.5 + 0.5 * (std::f64::consts::PI * x).cos()
        };
        for k in 1..=5 {
            let w = win_of(fade_start + 7.0 * k as f64 / 6.0);
            let (t_a, steps) = (w as f64 / 50.0, 480);
            let mean_sq: f64 = (0..steps)
                .map(|i| gain_at(t_a + (f64::from(i) + 0.5) * 0.02 / f64::from(steps)).powi(2))
                .sum::<f64>()
                / f64::from(steps);
            let want = 10.0 * mean_sq.log10();
            let got = f64::from(lv[w]) - f64::from(pre_db);
            worst = worst.max((got - want).abs());
            println!("(b) checkpoint {k}/6: {got:.3} dB, cosine says {want:.3} dB");
        }
        assert!(worst < 0.1, "cosine within 0.1 dB, worst {worst}");
        let quiet = volumes(fade_end + 0.2, t0_f + 901.0 - 0.5);
        assert!(quiet.iter().all(|d| *d < -90.0), "silent after the fade");
        assert_eq!(
            off.station.transport.name(),
            "playing",
            "the bed has started by the end"
        );
        // (c) the OSC message.
        let arrivals = rec.0.lock().unwrap().arrivals();
        assert_eq!(arrivals.len(), 1, "{arrivals:?}");
        let (at, act) = &arrivals[0];
        assert!(
            matches!(act, SideAction::Osc { port: 2223, address, .. } if address == "/ch/2/mute")
        );
        let err = *at - (t0 + 900 * SEC);
        println!("(c) the OSC mute arrived {err} ms from T0 + 15:00");
        assert!(err.abs() <= 50);
        // (d) the elevator bed.
        let bed_start = off
            .station
            .cond
            .sidecar
            .voices
            .iter()
            .find(|v| v.item.starts_with('b'))
            .expect("bed voice");
        let want_f = ((t0 + 901 * SEC - origin) as f64 / 1000.0 * f64::from(RATE)) as i64;
        let d = bed_start.start_frame as i64 - want_f;
        println!("(d) the first elevator frame is {d} frames from T0 + 15:01 (one period is 1024)");
        assert!(d.abs() <= 1024);
        // The block log: the fade, the mute and the bed fired once, the shift moved all three.
        let rows: Vec<_> = sched
            .st
            .log
            .rows
            .iter()
            .filter(|r| r.fired_ms.is_some())
            .map(|r| (r.name.clone(), r.target_ms - t0, r.how.clone()))
            .collect();
        println!("block log: {rows:?}");
        assert!(rows.iter().any(|r| r.0 == "Fade out" && r.1 == 880 * SEC));
        assert!(rows
            .iter()
            .any(|r| r.0 == "Mute the Wing" && r.1 == 900 * SEC));
        assert!(rows
            .iter()
            .any(|r| r.0 == "Elevator bed" && r.1 == 901 * SEC));
        let _ = (&w.pre_hz, &w.bed_hz);
    }
}

fn station_zone(ms: i64) -> String {
    common_m5::zone().local_string(ms)
}

#[test]
fn ita_monday_audio_nothing_plays_without_a_start_command() {
    let w = World::new("idle", &[120.0, 135.0], &[90.0, 100.0]);
    let origin = local("2027-01-18T18:44:00");
    let (mut off, rec) = w.open(origin, false, 1);
    let (levels, sink) = collector();
    off.keep_samples = false;
    off.sink = Some(sink);
    off.start().unwrap();
    off.run_until(20.0 * 60.0, &[]).unwrap();
    let lv = levels.lock().unwrap();
    let loudest = lv.iter().cloned().fold(f32::MIN, f32::max);
    println!("(a) with no start command: loudest 20 ms window over 20 minutes {loudest:.1} dBFS, {} OSC messages", rec.0.lock().unwrap().jobs.len());
    assert!(loudest < -90.0, "nothing plays");
    assert!(rec.0.lock().unwrap().jobs.is_empty());
    assert!(off.station.cond.sidecar.voices.is_empty());
}

#[test]
fn ita_monday_audio_the_midnight_stop_does_not_move_and_ends_the_program_stopped() {
    let w = World::new(
        "midnight",
        &[300.0, 330.0, 310.0, 340.0],
        &[300.0, 320.0, 340.0, 310.0],
    );
    // The press is at 18:45:15, so everything but the stop shifts by 15 s.
    let origin = local("2027-01-18T18:44:00");
    let press_s = 75.0;
    let stop_s = (local("2027-01-19T00:30:00") - origin) as f64 / 1000.0;
    let (off, rec, levels) = run_monday(&w, origin, press_s, stop_s + 40.0, 9);
    let lv = levels.lock().unwrap();
    // The bed plays until the stop: no window quieter than -60 dBFS between the bed's start and the stop,
    // except the gaps a back-to-back plan may leave, none expected.
    let bed_from = press_s + 901.0 + 2.0;
    let mut quiet = 0usize;
    for i in win_of(bed_from)..win_of(stop_s - 0.5) {
        if lv[i] < -60.0 {
            quiet += 1;
        }
    }
    println!(
        "(e) {} quiet windows between the bed start and the stop (of {})",
        quiet,
        win_of(stop_s - 0.5) - win_of(bed_from)
    );
    assert_eq!(quiet, 0, "the bed plays continuously until the stop");
    // The stop: 3 s fade starting at 00:30:00.000, silent after 00:30:03.1.
    let after = &lv[win_of(stop_s + 3.2)..win_of(stop_s + 39.0)];
    assert!(after.iter().all(|d| *d < -90.0), "silent after the stop");
    let before = lv[win_of(stop_s - 1.0)];
    assert!(
        before > -40.0,
        "still playing a second before the stop: {before}"
    );
    assert_eq!(off.station.transport.name(), "stopped");
    // The program ended stopped: nothing was dealt after the stop.
    assert!(
        off.station.sched.st.queue.is_empty()
            || off
                .station
                .sched
                .st
                .program
                .as_ref()
                .is_some_and(|p| p.halted)
    );
    let rows: Vec<_> = off
        .station
        .sched
        .st
        .log
        .rows
        .iter()
        .filter(|r| r.name == "Stop after midnight")
        .collect();
    println!(
        "(e)/(f) the stop's row: {:?}",
        rows.iter()
            .map(|r| (r.how.clone(), r.target_ms))
            .collect::<Vec<_>>()
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].target_ms,
        local("2027-01-19T00:30:00"),
        "block 6 does not shift with the late press"
    );
    assert_eq!(rec.0.lock().unwrap().jobs.len(), 1);
}

#[test]
fn ita_monday_audio_a_restart_at_18_52_resumes_the_run_and_still_fades_at_18_59_40() {
    let w = World::new(
        "restart",
        &[120.0, 135.0, 150.0, 125.0, 140.0, 165.0],
        &[90.0, 100.0, 110.0, 130.0, 95.0],
    );
    let origin = local("2027-01-18T18:44:00");
    let press_s = 60.0;
    let (mut off, _rec) = w.open(origin, true, 3);
    off.keep_samples = false;
    off.start().unwrap();
    let ops = vec![(
        press_s,
        Op::StartProgram {
            playlist_id: w.monday,
            t0: station::program::T0Req::Now,
            how: SwitchHow::Now,
            source: "deck",
        },
    )];
    let kill_s = (local("2027-01-18T18:52:00") - origin) as f64 / 1000.0;
    off.run_until(kill_s, &ops).unwrap();
    let t0 = off.station.sched.st.program.as_ref().unwrap().t0_ms;
    off.station.flush();
    drop(off);
    // The box is down for 5 s and comes back; state.db resumes the run.
    let wall = local("2027-01-18T18:52:05");
    let (mut off2, rec2) = w.open(wall, true, 3);
    let (levels, sink) = collector();
    off2.keep_samples = false;
    off2.sink = Some(sink);
    off2.start().unwrap();
    let run_s = (local("2027-01-18T19:02:00") - wall) as f64 / 1000.0;
    off2.run_until(run_s, &[]).unwrap();
    let pr = off2
        .station
        .sched
        .st
        .program
        .as_ref()
        .expect("the program resumed");
    println!(
        "(i) after the restart the program is `{}` with T0 {}",
        pr.playlist_id,
        station_zone(pr.t0_ms)
    );
    assert_eq!(pr.t0_ms, t0, "T0 is kept");
    let lv = levels.lock().unwrap();
    let at = |t: i64| ((t - wall) as f64 / 1000.0 * 50.0) as usize;
    // The fade: full level just before 18:59:40 and silent from 18:59:47.3 to 19:00:00.
    let pre = f64::from(lv[at(t0 + 878_000)]);
    let mid = f64::from(lv[at(t0 + 883_500)]);
    let post = lv[at(t0 + 887_300)..at(t0 + 899_000)]
        .iter()
        .cloned()
        .fold(f32::MIN, f32::max);
    println!("(i) 18:59:38 {pre:.1} dB, 18:59:43.5 {mid:.1} dB (cosine -6 dB), after 18:59:47.3 loudest {post:.1} dB");
    assert!(pre > -40.0 && (mid - pre + 6.0).abs() < 1.0 && post < -90.0);
    let jobs = rec2.0.lock().unwrap().arrivals();
    assert_eq!(jobs.len(), 1);
    assert!((jobs[0].0 - (t0 + 900 * SEC)).abs() <= 50);
    let rows: Vec<_> = off2
        .station
        .sched
        .st
        .log
        .rows
        .iter()
        .filter(|r| r.fired_ms.is_some())
        .map(|r| (r.name.clone(), r.how.clone()))
        .collect();
    println!("(i) block log after the restart: {rows:?}");
}

#[test]
fn end_at_audio_a_song_ends_on_the_time_and_the_cut_item_fades_over_two_seconds() {
    let w = World::new(
        "endat",
        &[70.0, 85.0, 95.0, 110.0, 120.0, 65.0],
        &[60.0, 70.0],
    );
    let origin = local("2027-01-18T18:44:00");
    let pre_id = w.lib.playlist_id("Eve 80s library").unwrap().unwrap();
    let bed_id = w.lib.playlist_id("Eve elevator").unwrap().unwrap();
    let (mut off, _rec) = w.open(origin, false, 5);
    off.keep_samples = false;
    off.start().unwrap();
    let at_ms = origin + 60_000 + 9 * 60_000 + 7_000;
    let ops = vec![(
        60.0,
        Op::EndAt {
            playlist_id: pre_id,
            at_ms,
            then: station::program::EndThen::Playlist(bed_id),
        },
    )];
    // Acceptance 2: while the closing item plays, the `ends_at` the countdown reads is the real end.
    let want_s = (at_ms - origin) as f64 / 1000.0;
    off.run_until(want_s - 20.0, &ops).unwrap();
    let info = off
        .station
        .on_air_info()
        .expect("the closing item is on air");
    let ends_at = info.started_ms + (info.bound_out_ms - info.bound_in_ms);
    println!(
        "(2) ends_at reads {} ms from the target end",
        ends_at - at_ms
    );
    assert!((ends_at - at_ms).abs() <= 50);
    off.run_until(60.0 + 10.0 * 60.0 + 60.0, &[]).unwrap();
    println!(
        "op log: {:?}",
        off.op_log
            .iter()
            .map(|(_, _, r)| r.clone())
            .collect::<Vec<_>>()
    );
    let pr = off.station.sched.st.program.as_ref().unwrap();
    println!("program now: playlist {} bt {:?}", pr.playlist_id, pr.bt);
    let voices = &off.station.cond.sidecar.voices;
    for v in voices {
        println!(
            "voice {} start {:.3} s ended {:?} reason {:?} stop {:?}",
            v.item,
            v.start_frame as f64 / 48000.0,
            v.ended_frame.map(|f| f as f64 / 48000.0),
            v.end_reason,
            v.stop.as_ref().map(|s| (s.at as f64 / 48000.0, s.frames))
        );
    }
    let want = (at_ms - origin) as f64 / 1000.0;
    let closing = voices
        .iter()
        .rfind(|v| v.item.starts_with('p'))
        .expect("pre-show voice");
    let end = closing.ended_frame.expect("ended") as f64 / 48000.0;
    println!(
        "the closing pre-show voice ends {:.4} s from the time",
        end - want
    );
    assert!((end - want).abs() <= 1024.0 / 48000.0);
}

#[test]
fn in_sequence_fade_wait_and_play_stop_the_audio_and_bring_it_back_at_unity() {
    use library::playlists::NewBlock;
    use serde_json::json;
    let w = World::new("seqstop", &[40.0, 45.0, 50.0, 42.0], &[30.0, 35.0, 33.0]);
    let pre = w.lib.playlist_id("Eve 80s library").unwrap().unwrap();
    let bed = w.lib.playlist_id("Eve elevator").unwrap().unwrap();
    let mut np = NewPlaylist::shuffle("Seq show");
    np.mode = Mode::Sequential;
    let id = w.lib.create_playlist(&np).unwrap();
    let blk = |name: &str, kind: &str, params: serde_json::Value| NewBlock {
        name: name.into(),
        kind: kind.into(),
        trigger: "sequence".into(),
        priority: 100,
        params,
    };
    for b in [
        blk(
            "Music",
            "play",
            json!({"source": {"playlist_id": pre}, "until": {"duration_s": 100}}),
        ),
        blk("Out", "fade_stop", json!({"seconds": 3})),
        blk("Pause", "wait", json!({"until": {"duration_s": 5}})),
        blk(
            "Bed",
            "play",
            json!({"source": {"playlist_id": bed}, "until": "next"}),
        ),
    ] {
        w.lib.add_block_to_playlist(id, &b).unwrap();
    }
    let origin = local("2027-01-18T20:00:00");
    let (mut off, _rec) = w.open(origin, false, 4);
    let (levels, sink) = collector();
    off.keep_samples = false;
    off.sink = Some(sink);
    off.start().unwrap();
    let ops = vec![(
        10.0,
        Op::StartProgram {
            playlist_id: id,
            t0: station::program::T0Req::Now,
            how: SwitchHow::Now,
            source: "ui",
        },
    )];
    off.run_until(10.0 + 100.0 + 120.0, &ops).unwrap();
    for v in &off.station.cond.sidecar.voices {
        println!(
            "voice {} {:.3} s -> {:?}",
            v.item,
            v.start_frame as f64 / 48000.0,
            v.ended_frame.map(|f| f as f64 / 48000.0)
        );
    }
    let lv = levels.lock().unwrap();
    let bed_start = off
        .station
        .cond
        .sidecar
        .voices
        .iter()
        .find(|v| v.item.starts_with('b'))
        .expect("the bed started");
    let bed_s = bed_start.start_frame as f64 / 48000.0;
    let last_pre = off
        .station
        .cond
        .sidecar
        .voices
        .iter()
        .rfind(|v| v.item.starts_with('p'))
        .unwrap();
    let pre_end = last_pre.ended_frame.unwrap() as f64 / 48000.0;
    println!("the last pre-show voice ended at {pre_end:.2} s, the bed started at {bed_s:.2} s");
    // The fade: audible before, silent for the wait, the bed at its own level (the master is back at unity).
    let silent_from = pre_end + 0.1;
    let silent: Vec<f32> = lv[win_of(silent_from)..win_of(bed_s - 0.1)].to_vec();
    assert!(
        silent.iter().all(|d| *d < -90.0),
        "silent between the fade and the bed"
    );
    assert!(bed_s - pre_end >= 4.5, "the 5 s wait separates them");
    let bed_level = f64::from(lv[win_of(bed_s + 5.0)]);
    println!("the bed's level {bed_level:.2} dB (the tone is -20 dBFS before normalization)");
    assert!(
        bed_level > -30.0,
        "the master gain returned to unity: {bed_level}"
    );
    assert_eq!(off.station.transport.name(), "playing");
    let rows: Vec<_> = off
        .station
        .sched
        .st
        .log
        .rows
        .iter()
        .filter(|r| r.fired_ms.is_some())
        .map(|r| (r.name.clone(), r.how.clone()))
        .collect();
    println!("block log: {rows:?}");
    assert!(rows.iter().any(|r| r.0 == "Out" && r.1 == "fired"));
}

#[test]
fn a_back_timed_wait_loop_program_starts_with_songs_queued() {
    use library::playlists::NewBlock;
    use serde_json::json;
    let w = World::new("extras", &[200.0, 210.0, 220.0], &[30.0, 35.0]);
    let pre = w.lib.playlist_id("Eve 80s library").unwrap().unwrap();
    let mut np = NewPlaylist::shuffle("Extras");
    np.mode = Mode::Sequential;
    let id = w.lib.create_playlist(&np).unwrap();
    let blk = |name: &str, kind: &str, trigger: &str, params: serde_json::Value| NewBlock {
        name: name.into(),
        kind: kind.into(),
        trigger: trigger.into(),
        priority: 100,
        params,
    };
    for b in [
        blk(
            "Expected",
            "start",
            "first",
            json!({"schedule": [{"cron": "45 18 * * 1"}], "anchor": "manual"}),
        ),
        blk(
            "Music",
            "play",
            "sequence",
            json!({"source": {"playlist_id": pre}, "until": {"at": {"kind": "relative", "offset_ms": 600000}}, "end": "natural", "back_time": {"mode": "best_fit", "tolerance_s": 45}}),
        ),
        blk("Hold", "wait", "sequence", json!({"until": "operator"})),
        blk(
            "Again",
            "loop",
            "sequence",
            json!({"to": 1, "times": "forever"}),
        ),
        blk(
            "Stop A",
            "stop",
            "timed",
            json!({"when": {"kind": "relative", "offset_ms": 1200000}, "how": "now", "fade_ms": 1000}),
        ),
        blk(
            "Stop B",
            "stop",
            "timed",
            json!({"when": {"kind": "relative", "offset_ms": 1200000}, "how": "now", "fade_ms": 1000}),
        ),
    ] {
        w.lib.add_block_to_playlist(id, &b).unwrap();
    }
    let waiter = {
        let mut np = NewPlaylist::shuffle("Waiter");
        np.mode = Mode::Sequential;
        let wid = w.lib.create_playlist(&np).unwrap();
        let bed = w.lib.playlist_id("Eve elevator").unwrap().unwrap();
        for b in [
            blk(
                "Start",
                "start",
                "first",
                json!({"schedule": [{"cron": "0 3 * * 2"}], "anchor": "manual"}),
            ),
            blk("Hold", "wait", "sequence", json!({"until": "operator"})),
            blk(
                "Bed",
                "play",
                "sequence",
                json!({"source": {"playlist_id": bed}, "until": "next"}),
            ),
        ] {
            w.lib.add_block_to_playlist(wid, &b).unwrap();
        }
        wid
    };
    let origin = local("2027-01-18T20:00:00");
    let (mut off, _rec) = w.open(origin, false, 4);
    off.keep_samples = false;
    off.start().unwrap();
    let bed = w.lib.playlist_id("Eve elevator").unwrap().unwrap();
    let ops = vec![
        (
            1.0,
            Op::Switch {
                playlist_id: bed,
                how: SwitchHow::Now,
            },
        ),
        (
            3.0,
            Op::StartProgram {
                playlist_id: waiter,
                t0: station::program::T0Req::Now,
                how: SwitchHow::Now,
                source: "ui",
            },
        ),
        (
            10.0,
            Op::StartProgram {
                playlist_id: id,
                t0: station::program::T0Req::Now,
                how: SwitchHow::Now,
                source: "ui",
            },
        ),
    ];
    off.run_until(30.0, &ops).unwrap();
    println!(
        "ops: {:?}",
        off.op_log
            .iter()
            .map(|(_, _, r)| r.clone())
            .collect::<Vec<_>>()
    );
    let pr = off.station.sched.st.program.as_ref().unwrap();
    println!(
        "cursor {} fill {:?} bt {:?} queue {}",
        pr.seq_pos,
        pr.fill,
        pr.bt,
        off.station.sched.st.queue.len()
    );
    assert!(off.op_log.iter().all(|(_, _, r)| r.is_ok()));
    assert!(!off.station.sched.st.queue.is_empty() || off.station.on_air_info().is_some());
}

#[test]
fn after_an_abort_the_station_deals_from_its_default_playlist_again() {
    let w = World::new("abort", &[60.0, 70.0, 80.0], &[30.0, 35.0, 40.0]);
    let origin = local("2027-01-18T20:00:00");
    let mut o = StationOptions::new("eve", w.lib.paths.media.clone());
    o.default_playlist = Some("Eve 80s library".into());
    o.record_sidecar = true;
    o.time_zone = "America/New_York".into();
    let (st, mixer) = Station::open(o, None, Some(w.lib.paths.db.clone()), origin).unwrap();
    let mut off = Offline::new(st, mixer, origin);
    off.keep_samples = false;
    off.station.side = Box::new(SharedRecording::default());
    off.start().unwrap();
    let ops = vec![(
        5.0,
        Op::StartProgram {
            playlist_id: w.monday,
            t0: station::program::T0Req::Now,
            how: SwitchHow::Now,
            source: "ui",
        },
    )];
    off.run_until(20.0, &ops).unwrap();
    println!(
        "before: program {} queue {}",
        off.station.sched.st.program.as_ref().unwrap().playlist_id,
        off.station.sched.st.queue.len()
    );
    // Fire the mute block by hand, then abort.
    let osc_id = off.station.sched.prog.playlists[&w.monday]
        .blocks()
        .find(|b| b.kind.type_name() == "osc_out")
        .unwrap()
        .id;
    {
        let mixer = &mut off.mixer;
        let mut send = |c: engine::rt::Command| mixer.command(&c.kind);
        off.station.program_fire(osc_id, &mut send).unwrap();
        off.station.program_abort(false, &mut send).unwrap();
    }
    off.run_until(60.0, &[]).unwrap();
    let pr = off.station.sched.st.program.as_ref().unwrap();
    println!(
        "after: program {} cursor {} queue {} transport {}",
        pr.playlist_id,
        pr.seq_pos,
        off.station.sched.st.queue.len(),
        off.station.transport.name()
    );
    assert!(
        !off.station.sched.st.queue.is_empty(),
        "songs are queued after the abort"
    );
}

// ---- a press of "Start Playlist as Program" always (re)starts the program from a fresh T0 ----------------------------

/// The owner's "Eve Monday": the six blocks of the fixture with an OSC unmute at T0 + 0 between the start and the
/// pre-show (Start, unmute, Play pre-show in sequence, Fade out and stop at 14:40, mute at 14:50 in the fixture's
/// 15:00, elevator bed, stop after midnight). Returns the playlist id.
fn owner_monday(w: &World, name: &str) -> i64 {
    let mut pl = w.ex.playlist();
    pl.name = name.into();
    let id = w.lib.create_playlist(&pl).unwrap();
    let mut blocks = w.ex.blocks();
    blocks.insert(
        1,
        library::playlists::NewBlock {
            name: "Unmute the Wing".into(),
            kind: "osc_out".into(),
            trigger: "timed".into(),
            priority: 100,
            params: serde_json::json!({
                "when": {"kind": "relative", "offset_ms": 0},
                "host": w.ex.osc.host, "port": w.ex.osc.port, "address": w.ex.osc.address,
                "args": [{"type": "i", "value": 0}]
            }),
        },
    );
    for b in blocks {
        w.lib.add_block_to_playlist(id, &b).unwrap();
    }
    id
}

fn press(playlist_id: i64, source: &'static str) -> Op {
    Op::StartProgram {
        playlist_id,
        t0: station::program::T0Req::Now,
        how: SwitchHow::Now,
        source,
    }
}

/// `(OSC value, arrival ms)` of every OSC message.
fn osc_values(rec: &SharedRecording) -> Vec<(i32, i64)> {
    rec.0
        .lock()
        .unwrap()
        .arrivals()
        .into_iter()
        .filter_map(|(at, a)| match a {
            SideAction::Osc { args, .. } => Some((
                match args.first() {
                    Some(station::side::OscArg::Int(v)) => *v,
                    _ => -1,
                },
                at,
            )),
            _ => None,
        })
        .collect()
}

/// The first voice that starts at or after `t_s` seconds into the render, and whether it is a pre-show item.
fn first_voice_after(off: &Offline, t_s: f64) -> Option<(f64, String)> {
    off.station
        .cond
        .sidecar
        .voices
        .iter()
        .map(|v| (v.start_frame as f64 / f64::from(RATE), v.item.clone()))
        .filter(|(s, _)| *s >= t_s)
        .min_by(|a, b| a.0.total_cmp(&b.0))
}

#[test]
fn a_press_on_a_stopped_station_starts_the_program_and_its_audio() {
    let w = World::new("press-stopped", &[120.0, 135.0, 150.0], &[90.0, 100.0]);
    let monday = owner_monday(&w, "Eve Monday (owner)");
    let origin = local("2027-01-18T18:40:00");
    let (mut off, rec) = w.open(origin, false, 5);
    let (levels, sink) = collector();
    off.keep_samples = false;
    off.sink = Some(sink);
    off.start().unwrap();
    // something plays, then the station is stopped (as after last week's stop after midnight); the press comes later
    let ops = vec![
        (
            1.0,
            Op::Switch {
                playlist_id: w.bed_id,
                how: SwitchHow::Now,
            },
        ),
        (20.0, Op::Stop { fade_s: None }),
        (60.0, press(monday, "deck")),
    ];
    off.run_until(120.0, &ops).unwrap();
    println!(
        "ops: {:?}",
        off.op_log
            .iter()
            .map(|(t, o, r)| (
                t,
                format!("{o:?}").chars().take(40).collect::<String>(),
                r.clone()
            ))
            .collect::<Vec<_>>()
    );
    let pr = off.station.sched.st.program.as_ref().expect("a program");
    assert_eq!(pr.playlist_id, monday);
    assert_eq!(pr.t0_source, "deck");
    let t0_s = (pr.t0_ms - origin) as f64 / 1000.0;
    assert!((t0_s - 60.0).abs() < 0.2, "T0 is the press: {t0_s}");
    assert_eq!(off.station.transport.name(), "playing", "the audio runs");
    let (vs, item) = first_voice_after(&off, 59.0).expect("a voice after the press");
    println!("first voice after the press: {item} at {vs:.3} s");
    assert!(item.starts_with('p'), "a pre-show item plays: {item}");
    assert!(vs - 60.0 < 1.0, "within a second of the press: {vs}");
    let lv = levels.lock().unwrap();
    assert!(
        lv[win_of(62.0)..win_of(119.0)].iter().all(|d| *d > -40.0),
        "sound from the press on"
    );
    let osc = osc_values(&rec);
    assert_eq!(osc.len(), 1, "{osc:?}");
    assert_eq!(osc[0].0, 0, "the unmute");
    assert!((osc[0].1 - pr.t0_ms).abs() <= 50, "the unmute at T0");
}

/// When the voice of `item` stopped sounding: its stop's end, or its own end.
fn voice_end_s(off: &Offline, prefix: char, before_s: f64) -> Option<f64> {
    off.station
        .cond
        .sidecar
        .voices
        .iter()
        .filter(|v| {
            v.item.starts_with(prefix) && (v.start_frame as f64 / f64::from(RATE)) < before_s
        })
        .filter_map(|v| {
            v.stop
                .as_ref()
                .map(|s| (s.at + u64::from(s.frames)) as f64 / f64::from(RATE))
        })
        .reduce(f64::max)
}

#[test]
fn a_press_while_a_plain_playlist_plays_replaces_it_with_the_program() {
    let w = World::new("press-playlist", &[120.0, 135.0, 150.0], &[300.0, 300.0]);
    let monday = owner_monday(&w, "Eve Monday (owner)");
    let origin = local("2027-01-18T18:40:00");
    let (mut off, rec) = w.open(origin, false, 7);
    let (levels, sink) = collector();
    off.keep_samples = false;
    off.sink = Some(sink);
    off.start().unwrap();
    let ops = vec![
        (
            1.0,
            Op::Switch {
                playlist_id: w.bed_id,
                how: SwitchHow::Now,
            },
        ),
        (60.0, press(monday, "ui")),
    ];
    off.run_until(100.0, &ops).unwrap();
    let pr = off.station.sched.st.program.as_ref().expect("a program");
    assert_eq!((pr.playlist_id, pr.t0_source.as_str()), (monday, "ui"));
    let (vs, item) = first_voice_after(&off, 59.0).expect("a voice after the press");
    let bed_end = voice_end_s(&off, 'b', 59.0).expect("the bed was stopped");
    println!("bed fades out by {bed_end:.3} s; first pre-show voice {item} at {vs:.3} s");
    assert!(item.starts_with('p'));
    assert!(
        bed_end <= 62.6,
        "the playlist fades out over the fade-next time: {bed_end}"
    );
    assert!(
        vs >= bed_end - 0.01 && vs - 60.0 < 3.0,
        "the program starts right after: {vs}"
    );
    let lv = levels.lock().unwrap();
    assert!(
        lv[win_of(vs + 0.5)..win_of(99.0)]
            .iter()
            .all(|d| *d > -40.0),
        "the program sounds"
    );
    assert_eq!(off.station.transport.name(), "playing");
    assert_eq!(osc_values(&rec).len(), 1, "the unmute at T0");
}

#[test]
fn a_press_while_held_or_while_another_program_runs_takes_the_station() {
    let w = World::new("press-other", &[120.0, 135.0, 150.0], &[300.0, 300.0]);
    let monday = owner_monday(&w, "Eve Monday (owner)");
    let other = owner_monday(&w, "Another program");
    for (label, held) in [("another program", false), ("held", true)] {
        let origin = local("2027-01-18T18:40:00");
        let (mut off, rec) = w.open(origin, false, 9);
        off.keep_samples = false;
        off.start().unwrap();
        let mut ops = vec![(1.0, press(other, "deck"))];
        if held {
            ops = vec![
                (
                    1.0,
                    Op::Switch {
                        playlist_id: w.bed_id,
                        how: SwitchHow::Now,
                    },
                ),
                (30.0, Op::Hold),
            ];
        }
        ops.push((60.0, press(monday, "deck")));
        off.run_until(90.0, &ops).unwrap();
        let pr = off.station.sched.st.program.as_ref().expect("a program");
        assert_eq!(pr.playlist_id, monday, "{label}");
        let t0_s = (pr.t0_ms - origin) as f64 / 1000.0;
        assert!((t0_s - 60.0).abs() < 0.2, "{label}: T0 is the press");
        let (vs, item) = first_voice_after(&off, 59.0).expect("a voice after the press");
        println!("{label}: first voice after the press {item} at {vs:.3} s");
        assert!(
            item.starts_with('p') && vs - 60.0 < 3.0,
            "{label}: {item} at {vs}"
        );
        assert_eq!(off.station.transport.name(), "playing", "{label}");
        let osc = osc_values(&rec);
        // the other program's unmute at its T0, then Monday's at the press
        assert_eq!(osc.len(), if held { 1 } else { 2 }, "{label}: {osc:?}");
        assert!((osc.last().unwrap().1 - pr.t0_ms).abs() <= 50, "{label}");
    }
}

#[test]
fn a_second_press_restarts_the_same_program_from_a_new_t0() {
    let w = World::new(
        "press-again",
        &[
            120.0, 135.0, 150.0, 125.0, 140.0, 165.0, 130.0, 145.0, 155.0,
        ],
        &[90.0, 100.0],
    );
    let monday = owner_monday(&w, "Eve Monday (owner)");
    let origin = local("2027-01-18T18:40:00");
    let (mut off, rec) = w.open(origin, false, 11);
    let (levels, sink) = collector();
    off.keep_samples = false;
    off.sink = Some(sink);
    off.start().unwrap();
    let ops = vec![
        (60.0, press(monday, "deck")),
        (300.0, press(monday, "deck")),
    ];
    // past the first T0's fade (60 + 880 = 940 s) and through the new one's (300 + 880 = 1180 s, 7 s long)
    off.run_until(1200.0, &ops).unwrap();
    let pr = off.station.sched.st.program.as_ref().expect("a program");
    let t0 = pr.t0_ms;
    let t0_s = (t0 - origin) as f64 / 1000.0;
    assert!(
        (t0_s - 300.0).abs() < 0.2,
        "T0 moved to the second press: {t0_s}"
    );
    // the unmute at T0 + 0 ran once for each press
    let osc = osc_values(&rec);
    println!("OSC: {osc:?}");
    let unmutes: Vec<_> = osc.iter().filter(|o| o.0 == 0).collect();
    assert_eq!(unmutes.len(), 2, "{osc:?}");
    assert!((unmutes[1].1 - t0).abs() <= 50);
    // the first run's fade at 940 s did not happen: sound goes on through it
    let lv = levels.lock().unwrap();
    assert!(
        lv[win_of(930.0)..win_of(1170.0)].iter().all(|d| *d > -40.0),
        "no fade at the first T0's 14:40"
    );
    // the new run's fade out at T0 + 14:40
    assert!(
        lv[win_of(1188.0)..win_of(1199.0)]
            .iter()
            .all(|d| *d < -90.0),
        "silent after the new fade"
    );
    let fades: Vec<_> = off
        .station
        .sched
        .st
        .log
        .rows
        .iter()
        .filter(|r| r.name == "Fade out" && r.fired_ms.is_some())
        .map(|r| r.target_ms - t0)
        .collect();
    assert_eq!(
        fades,
        vec![880 * SEC],
        "the fade re-armed at the new T0 + 14:40"
    );
}

// ---- a planned silence is not starvation ---------------------------------------------------------------------------

#[test]
fn a_program_start_and_its_planned_gap_raise_no_queue_starved_and_play_no_emergency_playlist() {
    let w = World::new(
        "planned-gap",
        &[120.0, 135.0, 150.0, 125.0, 140.0, 165.0, 130.0, 145.0],
        &[90.0, 100.0],
    );
    let monday = owner_monday(&w, "Eve Monday (owner)");
    let origin = local("2027-01-18T18:44:00");
    // an emergency playlist is set, as on a 24/7 station: the planned silences must still not reach it
    let (mut off, _rec) = w.open_with(origin, false, 13, |o| {
        o.emergency_playlist = Some("Eve 80s library".into());
    });
    off.keep_samples = false;
    off.start().unwrap();
    let ops = vec![
        (10.0, Op::Stop { fade_s: None }),
        (60.0, press(monday, "deck")),
    ];
    // through the fade out at T0 + 14:40, the silent gap and the elevator bed at T0 + 15:01
    off.run_until(60.0 + 905.0, &ops).unwrap();
    let a = &off.station.fail.alerts;
    for e in a.history.iter().filter(|e| e.alert.code == "queue_starved") {
        println!(
            "queue_starved {:?} at {:.3} s",
            e.change,
            (e.at_ms - origin) as f64 / 1000.0
        );
    }
    // (the station starts with an empty default playlist and no program: that is starvation on a 24/7 station)
    let after_stop = a
        .history
        .iter()
        .filter(|e| {
            e.alert.code == "queue_starved"
                && matches!(e.change, station::alerts::AlertChange::Raised)
                && e.at_ms - origin > 10_500
        })
        .count();
    assert_eq!(
        after_stop, 0,
        "no starvation once stopped, at the start or in the gap"
    );
    assert!(
        !a.is_active("queue_starved"),
        "the alert from before the stop is over"
    );
    let cleared = a
        .history
        .iter()
        .find(|e| {
            e.alert.code == "queue_starved"
                && !matches!(e.change, station::alerts::AlertChange::Raised)
        })
        .map(|e| (e.at_ms - origin) as f64 / 1000.0);
    assert!(
        cleared.is_some_and(|t| t < 12.0),
        "cleared at the stop: {cleared:?}"
    );
    assert!(
        off.station.fail.alt.is_none(),
        "not on the emergency playlist"
    );
    let bed = first_voice_after(&off, 60.0 + 885.0).expect("the bed after the gap");
    println!("after the fade: {} at {:.3} s", bed.1, bed.0);
    assert!(
        bed.1.starts_with('b'),
        "the elevator bed, not an emergency item: {}",
        bed.1
    );
    assert!((bed.0 - (60.0 + 901.0)).abs() < 0.1, "at T0 + 15:01");
}

#[test]
fn a_part_time_station_with_nothing_to_play_stays_silent_without_alerts() {
    let w = World::new("part-time", &[120.0, 135.0], &[90.0, 100.0]);
    let origin = local("2027-01-18T12:00:00");
    for normal in [true, false] {
        let (mut off, _rec) = w.open_with(origin, false, 3, |o| {
            o.emergency_playlist = Some("Eve 80s library".into());
            o.stopped_is_normal = normal;
        });
        off.keep_samples = false;
        off.start().unwrap();
        off.run_until(30.0, &[]).unwrap();
        let raised = off.station.fail.alerts.count_raised("queue_starved");
        let voices = off.station.cond.sidecar.voices.len();
        println!("stopped_is_normal {normal}: queue_starved raised {raised}, voices {voices}");
        if normal {
            assert_eq!(raised, 0);
            assert_eq!(voices, 0, "silent, no emergency playlist");
        }
    }
}

// ---- the stop after midnight of a program started just after midnight ----------------------------------------------

#[test]
fn the_stop_after_midnight_is_tomorrow_night_shows_its_time_and_fires_by_hand() {
    let w = World::new("fire-stop", &[120.0, 135.0, 150.0], &[90.0, 100.0]);
    let monday = owner_monday(&w, "Eve Monday (owner)");
    // the owner's case: the program was started at 00:07:44 local on the show day
    let origin = local("2027-01-18T00:07:00");
    let (mut off, _rec) = w.open(origin, false, 17);
    off.keep_samples = false;
    off.start().unwrap();
    off.run_until(100.0, &[(44.0, press(monday, "deck"))])
        .unwrap();
    let wall = origin + 100 * SEC;
    let stop_id = off
        .station
        .sched
        .prog
        .playlists
        .get(&monday)
        .unwrap()
        .entries
        .iter()
        .find_map(|e| match e {
            station::programming::Entry::Block(b) if b.name == "Stop after midnight" => Some(b.id),
            _ => None,
        })
        .expect("the stop block");
    // it acts at 00:30 the next night (the night after the show day): neither past nor today
    let up = off.station.sched.upcoming(wall, 49);
    let s = up
        .iter()
        .find(|u| u.block_id == stop_id)
        .expect("listed ahead");
    assert_eq!(
        station_zone(s.target_ms).get(..16),
        Some("2027-01-19T00:30")
    );
    // Fire stops the station now, with its fade, and logs it as fired by hand
    {
        let mixer = &mut off.mixer;
        let mut send = |c: engine::rt::Command| mixer.command(&c.kind);
        off.station
            .program_fire(stop_id, &mut send)
            .expect("Fire works");
    }
    off.run_until(110.0, &[]).unwrap();
    assert_eq!(off.station.transport.name(), "stopped");
    let row = off
        .station
        .sched
        .st
        .log
        .rows
        .iter()
        .find(|r| r.block_id == stop_id)
        .expect("logged");
    assert_eq!(
        (row.how.as_str(), row.detail.as_str()),
        ("fired", "fired by hand")
    );
    // and it does not fire again at its own time
    let up = off.station.sched.upcoming(origin + 110 * SEC, 49);
    assert!(up.iter().all(|u| u.block_id != stop_id), "{up:?}");
}
