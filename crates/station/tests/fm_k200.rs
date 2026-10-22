//! `fm`'s `last_crossing` exit in playout: a station reads the `K200` envelope from its track cache, so the
//! boundary it plans equals the scenario renderer's for the same pair (before the fix a station fell back to
//! the cue-out, because `Item::from_row` carried only `K10`).

mod support;

use library::playlists::{Mode, NewPlaylist};
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use station::offline::Offline;
use station::runtime::{Station, StationOptions};
use station::scenario::{render, Scenario};
use support::*;

const T0: i64 = 1_800_000_000_000;

#[test]
fn a_station_plays_a_pair_under_fm_with_the_scenario_renderers_exit() {
    let Some(fxdir) = fx(&["tone_fade.flac", "the_summoning.flac"]) else {
        return;
    };
    // The renderer's answer.
    let yaml = "station: { preset: fm }\nitems:\n  a: { file: tone_fade.flac }\n  b: { file: the_summoning.flac }\nqueue:\n  - { item: a }\n  - { item: b }\n";
    let sc = Scenario::from_yaml(yaml).unwrap();
    let x = render(&sc, Some(&fxdir)).unwrap();
    let ra = &x.sidecar.voices[0];
    let r_bd = &x.sidecar.boundaries[0];
    let r_exit = r_bd.e_a - ra.start_frame as i64;
    let r_sb = r_bd.s_b as i64 - ra.start_frame as i64;
    let r_cue_out = ra.cue_out_frame as i64 - ra.start_frame as i64;

    // The same pair on a station: imported into a library, read back through the track cache.
    let dir = fixtures::testdir::process_dir().join(format!("fmk200-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let lib = Library::open(Paths::from_data_dir(&dir.join("data"))).unwrap();
    let mut ids = vec![];
    for f in ["tone_fade.flac", "the_summoning.flac"] {
        let opts = ImportOptions {
            category: Some("music".into()),
            source: "test".into(),
        };
        match import_file(&lib, &fxdir.join(f), &opts) {
            ImportOutcome::Done { item_id, .. } => ids.push(item_id),
            other => panic!("import: {other:?}"),
        }
    }
    let mut p = NewPlaylist::shuffle("Fm");
    p.mode = Mode::Sequential;
    let pl = lib.create_playlist(&p).unwrap();
    for i in &ids {
        lib.add_item_to_playlist(pl, *i, 1.0).unwrap();
    }
    let state = dir.join("state.db");
    let open = || {
        let mut o = StationOptions::new("office", lib.paths.media.clone());
        o.default_playlist = Some("Fm".into());
        o.record_sidecar = true;
        o.preset = "fm".into();
        let (st, mixer) =
            Station::open(o, Some(state.clone()), Some(lib.paths.db.clone()), T0).unwrap();
        Offline::new(st, mixer, T0)
    };
    let mut off = open();
    off.start().unwrap();
    off.run_until(40.0, &[]).unwrap();
    let sc_ = &off.station.cond.sidecar;
    let sa = &sc_.voices[0];
    let bd = &sc_.boundaries[0];
    let s_exit = bd.e_a - sa.start_frame as i64;
    let s_sb = bd.s_b as i64 - sa.start_frame as i64;
    let s_cue_out = sa.cue_out_frame as i64 - sa.start_frame as i64;
    println!(
        "fm tone_fade -> the_summoning: renderer exit {r_exit} (cue-out {r_cue_out}), B starts {r_sb}; \
         station exit {s_exit} (cue-out {s_cue_out}), B starts {s_sb}; exit difference {} frames (limit 0)",
        s_exit - r_exit
    );
    assert_ne!(
        r_exit, r_cue_out,
        "the pair must discriminate: the last crossing is not the cue-out"
    );
    assert_eq!(s_exit, r_exit);
    assert_eq!(s_sb, r_sb);

    // Size of the cached K200: measured on the two files, and on a pessimistic 4-minute curve.
    drop(off);
    let cached = station::state::load(&state).unwrap().tracks;
    for t in &cached {
        let n = analysis::env::unpack(&t.k200).unwrap().len();
        println!(
            "track {} ({} ms): K200 {} values, {} bytes cached (raw i16 {}, k10 cached {})",
            t.item_id,
            t.file_ms,
            n,
            t.k200.len(),
            n * 2,
            t.k10.len()
        );
        assert!(!t.k200.is_empty());
    }
    let mut x_ = 12345u64;
    let mut v = -14.0f32;
    let walk: Vec<f32> = (0..12_000)
        .map(|_| {
            x_ = x_
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            v = (v + ((x_ >> 33) % 21) as f32 / 10.0 - 1.0).clamp(-40.0, -6.0);
            v
        })
        .collect();
    let packed = analysis::env::pack(&analysis::env::quantize(&walk));
    println!(
        "4-minute random-walk K200 (+-1 dB per 20 ms, worse than music): 12000 values, raw i16 24000 bytes, packed {} bytes; x1000 tracks = {:.1} MB",
        packed.len(),
        packed.len() as f64 * 1000.0 / 1e6
    );
}
