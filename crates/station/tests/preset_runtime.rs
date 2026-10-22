//! The station's default transition preset changed while it runs (architecture 2.6, resolution step 1): it applies
//! to the next boundary that is not committed, never to one that is, and it survives a restart through
//! `state.db` `meta` (and loses to a file edited while the station was stopped).

use engine::render::{render_tone, ToneRender};
use engine::wav::WavFormat;
use library::playlists::{Mode, NewPlaylist};
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use station::offline::Offline;
use station::runtime::{Station, StationOptions};
use std::path::PathBuf;

const RATE: u32 = 48_000;
const T0: i64 = 1_800_000_000_000;

struct W {
    lib: Library,
    state: PathBuf,
}

fn world(name: &str) -> W {
    let dir =
        fixtures::testdir::process_dir().join(format!("preset-rt-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let lib = Library::open(Paths::from_data_dir(&dir.join("data"))).unwrap();
    let mut ids = vec![];
    for i in 0..4 {
        let out = dir.join("src").join(format!("m{i}.wav"));
        render_tone(&ToneRender {
            out: out.clone(),
            rate: RATE,
            seconds: 30.0,
            hz: 300.0 + 100.0 * i as f64,
            level_dbfs: -20.0,
            format: WavFormat::F32,
        })
        .unwrap();
        match import_file(
            &lib,
            &out,
            &ImportOptions {
                category: Some("music".into()),
                source: "test".into(),
            },
        ) {
            ImportOutcome::Done { item_id, .. } => ids.push(item_id),
            o => panic!("{o:?}"),
        }
    }
    let mut p = NewPlaylist::shuffle("Rot");
    p.mode = Mode::Sequential;
    p.no_repeat_minutes = 0;
    let pl = lib.create_playlist(&p).unwrap();
    for i in &ids {
        lib.add_item_to_playlist(pl, *i, 1.0).unwrap();
    }
    W {
        state: dir.join("stations/office/state.db"),
        lib,
    }
}

fn open(w: &W, preset: &str) -> Offline {
    let mut o = StationOptions::new("office", w.lib.paths.media.clone());
    o.default_playlist = Some("Rot".into());
    o.preset = preset.into();
    let (st, mixer) =
        Station::open(o, Some(w.state.clone()), Some(w.lib.paths.db.clone()), T0).unwrap();
    Offline::new(st, mixer, T0)
}

fn starts(off: &Offline) -> Vec<f64> {
    off.station
        .cond
        .placed
        .iter()
        .map(|p| p.start_frame as f64 / f64::from(RATE))
        .collect()
}

#[test]
fn a_change_applies_to_the_next_uncommitted_boundary_and_not_to_a_committed_one() {
    let w = world("boundary");
    let mut off = open(&w, "none");
    off.keep_samples = false;
    off.start().unwrap();
    // 12 s in: the first boundary (about 30 s) is not committed (commit ahead is 10 s).
    off.run_until(12.0, &[]).unwrap();
    assert_eq!(starts(&off).len(), 1);
    off.station.set_station_preset("basic").unwrap();
    off.run_until(28.0, &[]).unwrap();
    let s = starts(&off);
    assert_eq!(s.len(), 2, "{s:?}");
    // `basic` overlaps the two songs by 5 s: B starts about 25 s after A.
    assert!((s[1] - s[0] - 25.0).abs() < 1.5, "basic overlap: {s:?}");
    // The boundary is committed now: switching back to `none` must not move it.
    off.station.set_station_preset("none").unwrap();
    off.run_until(40.0, &[]).unwrap();
    let s2 = starts(&off);
    assert_eq!(s2[1], s[1], "a committed boundary was re-planned");
    // The next one, still uncommitted when the preset changed, is planned with `none`: no overlap.
    off.run_until(62.0, &[]).unwrap();
    let s3 = starts(&off);
    assert!(s3.len() >= 3, "{s3:?}");
    assert!(
        (s3[2] - s3[1] - 30.0).abs() < 1.5,
        "none: gapless, no overlap: {s3:?}"
    );
    assert!(off.station.set_station_preset("nonsense").is_err());
    assert_eq!(off.station.opts.preset, "none");
}

#[test]
fn the_runtime_default_survives_a_restart_and_a_file_edit_beats_it() {
    let w = world("persist");
    {
        let mut off = open(&w, "plexamp");
        off.start().unwrap();
        off.station.set_station_preset("fm").unwrap();
        off.run_until(1.0, &[]).unwrap();
        assert_eq!(off.station.config_preset, "plexamp");
        assert_eq!(off.station.opts.preset, "fm");
        if let Some(s) = off.station.state_writer() {
            s.flush();
        }
    }
    // Restarted with the same file: the runtime value wins.
    let off = open(&w, "plexamp");
    assert_eq!(off.station.opts.preset, "fm");
    assert_eq!(off.station.cond.cfg.station_preset, "fm");
    assert_eq!(off.station.config_preset, "plexamp");
    drop(off);
    // The file was edited while the station was stopped: the file wins.
    let off = open(&w, "sam");
    assert_eq!(off.station.opts.preset, "sam");
}
