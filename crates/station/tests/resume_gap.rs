//! The boundary out of an item that was resumed in place after a restart (1.7) is as gapless as any other.
//! Found by the accelerated soak (M1.8): after a `kill -9` the resumed item ended and the next item began 239 ms
//! later, where every boundary of an uninterrupted run is exact.

use engine::render::{render_tone, ToneRender};
use engine::wav::WavFormat;
use library::playlists::{NewBlock, NewPlaylist};
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use station::offline::Offline;
use station::runtime::{Station, StationOptions};

const T0: i64 = 1_800_000_000_000;

#[test]
fn the_boundary_out_of_a_resumed_item_has_no_gap() {
    let dir = fixtures::testdir::process_dir().join(format!("rg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let lib = Library::open(Paths::from_data_dir(&dir.join("data"))).unwrap();
    let mut ids = vec![];
    for i in 0..4 {
        let p = dir.join("src").join(format!("t{i}.wav"));
        render_tone(&ToneRender {
            out: p.clone(),
            rate: 48_000,
            seconds: 45.0,
            hz: 300.0 + 90.0 * f64::from(i),
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
    let mut liners = vec![];
    for i in 0..2 {
        let p = dir.join("src").join(format!("l{i}.wav"));
        render_tone(&ToneRender {
            out: p.clone(),
            rate: 48_000,
            seconds: 8.0,
            hz: 1000.0 + 100.0 * f64::from(i),
            level_dbfs: -20.0,
            format: WavFormat::F32,
        })
        .unwrap();
        match import_file(
            &lib,
            &p,
            &ImportOptions {
                category: Some("liner".into()),
                source: "test".into(),
            },
        ) {
            ImportOutcome::Done { item_id, .. } => liners.push(item_id),
            o => panic!("{o:?}"),
        }
    }
    let cid = lib.category_id("liner").unwrap().unwrap();
    let mut pl = NewPlaylist::shuffle("Seq");
    pl.no_repeat_minutes = 0;
    let pid = lib.create_playlist(&pl).unwrap();
    for i in &ids {
        lib.add_item_to_playlist(pid, *i, 1.0).unwrap();
    }
    lib.add_block_to_playlist(
        pid,
        &NewBlock::insert_every("liners", 2, serde_json::json!({"category_id": cid})),
    )
    .unwrap();
    let _ = liners;
    let state = dir.join("stations/office/state.db");
    let open = |wall: i64| {
        let mut o = StationOptions::new("office", lib.paths.media.clone());
        o.default_playlist = Some("Seq".into());
        let (st, mixer) =
            Station::open(o, Some(state.clone()), Some(lib.paths.db.clone()), wall).unwrap();
        let mut off = Offline::new(st, mixer, wall);
        off.keep_samples = false;
        off.start().unwrap();
        off
    };
    let mut worst_all = 0i64;
    for (k, at) in [5.0f64, 12.0, 20.0, 31.0, 41.0, 44.5, 50.0, 58.0, 70.0]
        .into_iter()
        .enumerate()
    {
        let _ = std::fs::remove_dir_all(dir.join("stations"));
        let mut a = open(T0);
        a.run_until(at, &[]).unwrap();
        a.station.flush();
        drop(a);
        // Back 3 s later, and run through several boundaries.
        let mut b = open(T0 + (at * 1000.0) as i64 + 3_000);
        b.run_until(200.0, &[]).unwrap();
        b.station.flush();
        let rows = station::state::read_as_run(&state).unwrap();
        let plays: Vec<_> = rows.iter().filter(|r| r.origin != "restart").collect();
        let mut worst = 0i64;
        for w in plays.windows(2) {
            if w[0].end_reason.as_deref() == Some("crash") {
                continue;
            }
            let gap = w[1].started_ms - w[0].ended_ms.unwrap();
            if gap > 5 {
                println!(
                    "restart at {at} s: {} ({}) -> {} ({}): {gap} ms",
                    w[0].title, w[0].origin, w[1].title, w[1].origin
                );
            }
            worst = worst.max(gap);
        }
        println!(
            "case {k}: restart at {at} s, {} plays, worst gap {worst} ms",
            plays.len()
        );
        worst_all = worst_all.max(worst);
    }
    assert!(
        worst_all <= 5,
        "a gap of {worst_all} ms after a resumed item"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
