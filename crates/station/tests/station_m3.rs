//! M3 acceptance tests that need audio: the station (scheduler, conductor, mixer and `state.db`) rendered
//! offline on a virtual clock over a real library of generated tones. Wall time is UTC (the station's
//! default zone) and starts at 2027-01-15T08:00:00Z.

use engine::render::{render_tone, ToneRender};
use engine::wav::WavFormat;
use library::playlists::{Mode, NewBlock, NewPlaylist};
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use serde_json::json;
use station::offline::Offline;
use station::runtime::{Station, StationOptions};
use std::path::{Path, PathBuf};

const RATE: u32 = 48_000;
const T0: i64 = 1_800_000_000_000;

fn tmpdir(name: &str) -> PathBuf {
    let d = fixtures::testdir::process_dir().join(format!("m3-{}-{name}", std::process::id()));
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
    src: PathBuf,
}

impl World {
    fn new(name: &str) -> World {
        let dir = tmpdir(name);
        let lib = Library::open(Paths::from_data_dir(&dir.join("data"))).unwrap();
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        World { dir, lib, src }
    }

    /// Import tones of `seconds` at `hz_from + 100 * i` into `category`; returns `(item id, hz)`.
    fn tones(
        &self,
        tag: &str,
        category: &str,
        n: usize,
        seconds: f64,
        hz_from: f64,
    ) -> Vec<(i64, f64)> {
        let mut v = vec![];
        for i in 0..n {
            let hz = hz_from + 100.0 * i as f64;
            let p = tone(&self.src, &format!("{tag}{i}.wav"), hz, seconds);
            let opts = ImportOptions {
                category: Some(category.into()),
                source: "test".into(),
            };
            match import_file(&self.lib, &p, &opts) {
                ImportOutcome::Done { item_id, .. } => v.push((item_id, hz)),
                other => panic!("import: {other:?}"),
            }
        }
        v
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

    fn category(&self, name: &str) -> i64 {
        self.lib.category_id(name).unwrap().unwrap()
    }

    fn opts(&self, default_playlist: &str) -> StationOptions {
        let mut o = StationOptions::new("office", self.lib.paths.media.clone());
        o.default_playlist = Some(default_playlist.into());
        o.record_sidecar = true;
        o
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("stations").join("office").join("state.db")
    }

    fn open(&self, default_playlist: &str, seed: u64, with_state: bool, wall: i64) -> Offline {
        let mut o = self.opts(default_playlist);
        o.seed = seed;
        let (st, mixer) = Station::open(
            o,
            with_state.then(|| self.state_path()),
            Some(self.lib.paths.db.clone()),
            wall,
        )
        .unwrap();
        Offline::new(st, mixer, wall)
    }
}

fn frame_of(wall_ms: i64) -> i64 {
    (wall_ms - T0) * i64::from(RATE) / 1000
}

fn rms_db(x: &[f32]) -> f64 {
    if x.is_empty() {
        return -200.0;
    }
    let p: f64 = x.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / x.len() as f64;
    10.0 * p.max(1e-20).log10()
}

// ---- 2. the hard fallback: the ID's attack lands on T + late within one frame ----------------------------------

#[test]
fn m3_2_audio_a_hard_id_lands_its_attack_on_t_plus_late_within_one_frame_with_a_1_5_s_fade() {
    let w = World::new("hard-id");
    // 25 s tones: a boundary every 25 s or so, none of them inside a window of [T - 4 s, T + 3 s] of
    // the two-minute targets after the first cut (checked below against the log, not assumed).
    let music = w.tones("m", "music", 6, 25.0, 300.0);
    let ids = w.tones("i", "id", 2, 3.0, 4000.0);
    let pl = w.playlist(
        "Rot",
        Mode::Shuffle,
        &music.iter().map(|m| m.0).collect::<Vec<_>>(),
    );
    let _ = pl;
    let id_cat = w.category("id");
    w.lib
        .add_station_block(
            "office",
            &NewBlock {
                name: "top of the hour".into(),
                kind: "insert".into(),
                trigger: "timed".into(),
                priority: 100,
                params: json!({
                    "source": {"category_id": id_cat},
                    "when": {"kind": "recurrence", "cron": "*/2 * * * *"},
                    "mode": "soft_then_hard", "window_early_s": 4, "window_late_s": 3
                }),
            },
        )
        .unwrap();
    let mut off = w.open("Rot", 4, false, T0);
    off.start().unwrap();
    off.run_until(11.0 * 60.0, &[]).unwrap();
    let rows: Vec<_> = off
        .station
        .sched
        .st
        .log
        .rows
        .iter()
        .filter(|r| r.kind == "insert" && r.fired_ms.is_some())
        .cloned()
        .collect();
    let hard: Vec<_> = rows.iter().filter(|r| r.how == "hard").collect();
    println!("2 audio: {} IDs, {} hard", rows.len(), hard.len());
    assert!(rows.len() >= 4 && !hard.is_empty());
    let id_hz: Vec<f64> = ids.iter().map(|i| i.1).collect();
    let _ = id_hz;
    for r in &hard {
        let want = frame_of(r.target_ms + 3000);
        // The voice of the ID: its record in the sidecar whose attack frame is nearest the hard time.
        let rec = off
            .station
            .cond
            .sidecar
            .voices
            .iter()
            .filter(|v| v.item.starts_with("i"))
            .min_by_key(|v| (v.attack_frame as i64 - want).abs())
            .unwrap();
        let err = rec.attack_frame as i64 - want;
        // The item before it fades out over 1.5 s ending at the attack.
        let before = off
            .station
            .cond
            .sidecar
            .voices
            .iter()
            .filter(|v| v.item.starts_with('m'))
            .filter_map(|v| v.stop.as_ref().map(|s| (v, s)))
            .find(|(_, s)| (s.at as i64 + i64::from(s.frames) - want).abs() < 4800);
        let fade = before.map(|(_, s)| s.frames);
        println!("2 audio: target {} s: attack frame error {err} frames; the item before fades over {:?} frames ({:.2} s)", (r.target_ms - T0) / 1000, fade, f64::from(fade.unwrap_or(0)) / 48000.0);
        assert!(err.abs() <= 1, "attack {err} frames off");
        assert!(
            fade.is_some_and(|f| (f as i64 - 72_000).abs() <= 1),
            "fade {fade:?}"
        );
    }
}

// ---- 6b. a timed stop at a random time, mid-track ---------------------------------------------------------------------

#[test]
fn m3_6b_audio_a_timed_stop_now_ends_the_running_play_within_one_period() {
    let w = World::new("stop-now");
    let music = w.tones("m", "music", 5, 40.0, 300.0);
    let _ = music;
    let cid = w.category("music");
    let mut rng = 987u64;
    let mut worst = 0i64;
    for trial in 0..8u64 {
        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let at_ms = 15_000 + ((rng >> 33) as i64 % 60_000);
        let name = format!("Show{trial}");
        let mut p = NewPlaylist::shuffle(&name);
        p.mode = Mode::Sequential;
        let pl = w.lib.create_playlist(&p).unwrap();
        w.lib
            .add_block_to_playlist(
                pl,
                &NewBlock {
                    name: "fill".into(),
                    kind: "play".into(),
                    trigger: "sequence".into(),
                    priority: 100,
                    params: json!({"source": {"category_id": cid}, "until": "next"}),
                },
            )
            .unwrap();
        w.lib
            .add_block_to_playlist(
                pl,
                &NewBlock {
                    name: "mute".into(),
                    kind: "osc_out".into(),
                    trigger: "sequence".into(),
                    priority: 100,
                    params: json!({}),
                },
            )
            .unwrap();
        w.lib
            .add_block_to_playlist(
                pl,
                &NewBlock {
                    name: "stop".into(),
                    kind: "stop".into(),
                    trigger: "timed".into(),
                    priority: 100,
                    params: json!({"when": {"kind": "relative", "offset_ms": at_ms}, "how": "now", "fade_ms": 2000}),
                },
            )
            .unwrap();
        let mut off = w.open(&name, trial, false, T0);
        off.start().unwrap();
        off.run_until(at_ms as f64 / 1000.0 + 6.0, &[]).unwrap();
        let want = frame_of(T0 + at_ms);
        let stops: Vec<_> = off
            .station
            .cond
            .sidecar
            .voices
            .iter()
            .filter_map(|v| v.stop.as_ref())
            .filter(|s| s.frames == 96_000)
            .collect();
        assert!(!stops.is_empty(), "trial {trial}: no stop of 2 s");
        let err = stops
            .iter()
            .map(|s| s.at as i64 - want)
            .min_by_key(|e| e.abs())
            .unwrap();
        worst = worst.max(err.abs());
        assert!(
            err.abs() <= 1024,
            "trial {trial}: stop {err} frames from its time"
        );
        // Level before: the tone; at T + 2 s the decay is complete and nothing follows.
        let left = off.left();
        let pre = &left[(want as usize - 24_000)..(want as usize - 2_400)];
        let post = &left[(want as usize + 96_000 + 4_800)..];
        if rms_db(pre) <= -30.0 {
            for r in &off.station.as_run {
                eprintln!(
                    "as-run {:>7.2}s {} {:?} {:?}",
                    (r.started_ms - T0) as f64 / 1000.0,
                    r.title,
                    r.end_reason,
                    r.flags
                );
            }
            eprintln!(
                "at {at_ms} want {want}; alerts {:?}",
                off.station
                    .alerts()
                    .active()
                    .iter()
                    .map(|a| a.code.clone())
                    .collect::<Vec<_>>()
            );
        }
        assert!(
            rms_db(pre) > -30.0,
            "trial {trial}: playing before the stop ({:.1} dB)",
            rms_db(pre)
        );
        assert!(
            rms_db(post) < -90.0,
            "trial {trial}: silent after ({:.1} dB)",
            rms_db(post)
        );
        // The station is stopped and the in-sequence block was passed over.
        assert_eq!(off.station.transport.name(), "stopped");
        let passed: Vec<_> = off
            .station
            .sched
            .st
            .log
            .rows
            .iter()
            .filter(|r| r.how == "passed")
            .collect();
        assert_eq!(passed.len(), 1, "{passed:?}");
        assert_eq!(passed[0].name, "mute");
    }
    println!("6b audio: 8 random stops, worst distance of the stop from its time {worst} frames (limit 1024, one period)");
}

// ---- the owner's case, with audio and a restart across the switch time ---------------------------------------------------

#[test]
fn m3_ita_audio_a_restart_across_the_switch_time_still_switches_once_at_a_boundary() {
    let w = World::new("eve-restart");
    let a = w.tones("a", "music", 4, 20.0, 300.0);
    let b = w.tones("b", "music", 4, 20.0, 1300.0);
    let a_ids: Vec<i64> = a.iter().map(|x| x.0).collect();
    let b_ids: Vec<i64> = b.iter().map(|x| x.0).collect();
    let pa = w.playlist("UE Elevator Music", Mode::Shuffle, &a_ids);
    let pb = w.playlist("UE Library Tweener", Mode::Shuffle, &b_ids);
    let _ = pa;
    // The tweener starts at 08:01:00 UTC (60 s in), segue, never a cut.
    w.lib
        .add_block_to_playlist(
            pb,
            &NewBlock {
                name: "start".into(),
                kind: "start".into(),
                trigger: "first".into(),
                priority: 100,
                params: json!({"schedule": [{"cron": "1 8 * * *"}], "anchor": "absolute", "how": "segue"}),
            },
        )
        .unwrap();
    let hz_of = |id: i64| -> f64 { a.iter().chain(b.iter()).find(|x| x.0 == id).unwrap().1 };
    for (label, kill_s, down_s) in [
        ("none", 0.0, 0.0),
        ("before it is committed", 28.0, 3.0),
        ("after it is committed", 52.0, 2.0),
        ("after the switch", 85.0, 2.0),
    ] {
        let _ = std::fs::remove_dir_all(w.dir.join("stations"));
        let mut off = w.open("UE Elevator Music", 5, true, T0);
        off.start().unwrap();
        let mut rows;
        let mut restart_wall = T0;
        if kill_s == 0.0 {
            off.run_until(130.0, &[]).unwrap();
            rows = off.station.as_run.clone();
        } else {
            off.run_until(kill_s, &[]).unwrap();
            let before: Vec<_> = off.station.as_run.clone();
            off.station.flush();
            drop(off);
            restart_wall = T0 + ((kill_s + down_s) * 1000.0) as i64;
            let mut o = w.opts("UE Elevator Music");
            o.seed = 5;
            let (st, mixer) = Station::open(
                o,
                Some(w.state_path()),
                Some(w.lib.paths.db.clone()),
                restart_wall,
            )
            .unwrap();
            let mut b2 = Offline::new(st, mixer, restart_wall);
            b2.start().unwrap();
            b2.run_until(130.0 - kill_s - down_s, &[]).unwrap();
            rows = before;
            rows.extend(b2.station.as_run.clone());
            off = b2;
        }
        let items: Vec<(f64, i64)> = rows
            .iter()
            .filter(|r| !r.flags.iter().any(|f| f == "resumed"))
            .filter_map(|r| r.item_id.map(|i| ((r.started_ms - T0) as f64 / 1000.0, i)))
            .collect();
        let first_b = items.iter().find(|(_, i)| b_ids.contains(i)).copied();
        let a_after = first_b.map(|(t, _)| {
            items
                .iter()
                .filter(|(s, i)| *s > t && a_ids.contains(i))
                .count()
        });
        let log: Vec<_> = off
            .station
            .sched
            .st
            .log
            .rows
            .iter()
            .filter(|r| r.kind == "start" && r.fired_ms.is_some())
            .collect();
        println!("eve audio: restart {label}: the tweener's first item at {:?} s (hz {:?}), elevator items after it: {a_after:?}, start fired {} time(s), restarted at +{:.0} s", first_b.map(|x| x.0), first_b.map(|x| hz_of(x.1)), log.len(), (restart_wall - T0) as f64 / 1000.0);
        let (t, _) = first_b.expect("the tweener plays");
        assert!((60.0..=60.0 + 300.0).contains(&t), "{t}");
        assert_eq!(a_after, Some(0), "the program stays switched");
        assert_eq!(log.len(), 1, "one start in the block log");
        // The switch was at a boundary: the outgoing item ran to its end (within a decoder frame of 20 s
        // after its own start) except across a restart that cut the process, not the item.
        let prev = items.iter().rev().find(|(s, _)| *s < t).copied().unwrap();
        assert!(
            t - prev.0 >= 19.0,
            "the outgoing item was cut: {:.1} s of {:.1}",
            t - prev.0,
            20.0
        );
        assert_ne!(prev.1, items.iter().find(|(s, _)| *s == t).unwrap().1);
    }
}

// ---- 5. clock steps, with audio ------------------------------------------------------------------------------------

#[test]
fn m3_5_audio_a_step_forward_fires_the_skipped_id_once_now_and_a_step_back_does_not_fire_it_again()
{
    let w = World::new("clock-step");
    let music = w.tones("m", "music", 6, 25.0, 300.0);
    w.tones("i", "id", 2, 3.0, 4000.0);
    w.playlist(
        "Rot",
        Mode::Shuffle,
        &music.iter().map(|m| m.0).collect::<Vec<_>>(),
    );
    let id_cat = w.category("id");
    w.lib
        .add_station_block(
            "office",
            &NewBlock {
                name: "ten past".into(),
                kind: "insert".into(),
                trigger: "timed".into(),
                priority: 100,
                params: json!({
                    "source": {"category_id": id_cat},
                    "when": {"kind": "recurrence", "cron": "*/10 * * * *"},
                    "mode": "soft_then_hard", "window_early_s": 4, "window_late_s": 3
                }),
            },
        )
        .unwrap();
    let mut off = w.open("Rot", 9, false, T0);
    off.start().unwrap();
    // The ID of 08:10 (600 s) is 20 s away; the clock then jumps 10 minutes forward.
    off.run_until(580.0, &[]).unwrap();
    let rows_before: usize = off
        .station
        .sched
        .st
        .log
        .rows
        .iter()
        .filter(|r| r.kind == "insert")
        .count();
    off.step_clock(600_000);
    off.run_until(640.0, &[]).unwrap();
    let rows: Vec<_> = off
        .station
        .sched
        .st
        .log
        .rows
        .iter()
        .filter(|r| r.kind == "insert" && r.target_ms == T0 + 600_000)
        .cloned()
        .collect();
    println!(
        "5 audio: +10 min at 580 s: rows for the skipped ID: {:?}; alerts {:?}",
        rows.iter()
            .map(|r| (
                r.how.clone(),
                r.fired_ms.map(|f| (f - (T0 + 1_180_000)) as f64 / 1000.0)
            ))
            .collect::<Vec<_>>(),
        off.station
            .alerts()
            .active()
            .iter()
            .map(|a| a.code.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].how, "late_run");
    let lag = rows[0].fired_ms.unwrap() - (T0 + 580_000 + 600_000);
    assert!((0..=3_000).contains(&lag), "fired {lag} ms after the step");
    assert!(off
        .station
        .alerts()
        .active()
        .iter()
        .any(|a| a.code == "clock_jump"));
    let _ = rows_before;
    // The ID of "08:20" (wall 1200 s) is 20 s ahead of the stepped clock: it fires in its window.
    off.run_until(680.0, &[]).unwrap();
    // Now the clock goes back 15 minutes, past the 08:20 ID that has fired.
    off.step_clock(-900_000);
    let n_before = off
        .station
        .sched
        .st
        .log
        .rows
        .iter()
        .filter(|r| r.kind == "insert" && r.fired_ms.is_some())
        .count();
    // Wall now runs from about 08:10:00 again; run through 08:20 (the second time) and on to 08:25.
    off.run_until(680.0 + 900.0, &[]).unwrap();
    let rows: Vec<_> = off
        .station
        .sched
        .st
        .log
        .rows
        .iter()
        .filter(|r| r.kind == "insert")
        .cloned()
        .collect();
    let n_after = rows.iter().filter(|r| r.fired_ms.is_some()).count();
    println!("5 audio: -15 min after the IDs of 08:10 and 08:20 fired: {n_before} fired before the step, {n_after} after 15 more minutes (the wall passed 08:20 again)");
    assert_eq!(
        rows.iter()
            .filter(|r| r.target_ms == T0 + 1_200_000)
            .count(),
        1,
        "the 08:20 ID has one row"
    );
    assert_eq!(n_after, n_before, "nothing refires after a step back");
}
