//! The 30-day rotation on the real office set (`HOTCLOCK_TESTDATA/office`, never mixed with the other
//! sets): analysed and imported into a scratch library, then 30 virtual days of the office playlist of
//! architecture 4.4 (18 tracks, a liner every 4) through the scheduler with the transition planner as
//! the timing model. Skipped without the test data.

use common::transition::PresetBook;
use engine::render::{render_tone, ToneRender};
use engine::wav::WavFormat;
use library::playlists::{NewBlock, NewPlaylist};
use library::tracks::read_all_tracks;
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use station::catalog::Catalog;
use station::deck::WINDOW_SHARE_OF_PASS;
use station::programming::Programming;
use station::runtime::RuntimeTiming;
use station::sched::{Origin, SchedConfig, Scheduler};
use station::sim::{pass_orders, run, seam_rank_correlation, seam_violations, spearman};
use std::collections::HashMap;
use std::sync::Arc;

const MIN: i64 = 60_000;
const T0: i64 = 1_800_000_000_000;

fn files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    v.sort();
    for p in v {
        if p.is_dir() {
            files(&p, out);
        } else if p.extension().is_some_and(|e| e == "flac") {
            out.push(p);
        }
    }
}

#[test]
fn thirty_virtual_days_on_the_real_office_set() {
    let Some(root) = std::env::var_os("HOTCLOCK_TESTDATA") else {
        eprintln!("SKIPPED: HOTCLOCK_TESTDATA not set");
        return;
    };
    let office = std::path::Path::new(&root).join("office");
    if !office.exists() {
        eprintln!("SKIPPED: no office set");
        return;
    }
    let dir = fixtures::testdir::process_dir().join(format!("office-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let lib = Library::open(Paths::from_data_dir(&dir.join("data"))).unwrap();
    let mut fl = vec![];
    files(&office, &mut fl);
    let mut ids = vec![];
    for f in &fl {
        match import_file(
            &lib,
            f,
            &ImportOptions {
                category: Some("music".into()),
                source: "office".into(),
            },
        ) {
            ImportOutcome::Done { item_id, .. } | ImportOutcome::Duplicate { item_id } => {
                ids.push(item_id)
            }
            o => panic!("{}: {o:?}", f.display()),
        }
    }
    for i in 0..3 {
        let p = dir.join(format!("liner{i}.wav"));
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
                source: "office".into(),
            },
        ) {
            ImportOutcome::Done { .. } => {}
            o => panic!("{o:?}"),
        }
    }
    let cid = lib.category_id("liner").unwrap().unwrap();
    let mut pls = HashMap::new();
    for window in [60, 120] {
        let mut np = NewPlaylist::shuffle(&format!("Office {window}"));
        np.no_repeat_minutes = window;
        let pl = lib.create_playlist(&np).unwrap();
        for i in &ids {
            lib.add_item_to_playlist(pl, *i, 1.0).unwrap();
        }
        lib.add_block_to_playlist(
            pl,
            &NewBlock::insert_every("liners", 4, serde_json::json!({"category_id": cid})),
        )
        .unwrap();
        pls.insert(window, pl);
    }
    let mut cat = Catalog::new(&lib.paths.media);
    cat.apply(read_all_tracks(lib.conn()).unwrap());
    let prog = Arc::new(Programming::from_rows(&lib.playlists().unwrap(), &[]));
    let presets = PresetBook::builtin();
    let timing = RuntimeTiming {
        cat: &cat,
        presets: &presets,
        preset: "plexamp",
        rate: 48_000,
    };
    println!(
        "office set: {} tracks, lengths (s): {:?}",
        ids.len(),
        ids.iter()
            .map(|i| cat.meta(*i).unwrap().file_ms / 1000)
            .collect::<Vec<_>>()
    );
    let sim = |window: i64, share: f64| -> Stats {
        let pl = pls[&window];
        let cfg = SchedConfig {
            window_share: share,
            ..SchedConfig::default()
        };
        let mut s = Scheduler::new(cfg, prog.clone(), 2026);
        s.start_program(pl, T0);
        let info = s.rotation_info(prog.playlists.get(&pl).unwrap(), &cat);
        let plays = run(
            &mut s,
            &cat,
            &timing,
            T0,
            T0 + 30 * 24 * 60 * MIN,
            usize::MAX,
            |_, _, _, _| {},
        );
        let eff_ms = (info.achievable_minutes * MIN as f64) as i64;
        let mut last: HashMap<i64, i64> = HashMap::new();
        let mut gaps: Vec<i64> = vec![];
        let (mut inside_eff, mut unmarked, mut inside_req) = (0, 0, 0);
        for p in plays.iter().filter(|p| p.entry.origin == Origin::Rotation) {
            if let Some(prev) = last.insert(p.item_id(), p.start_ms) {
                let g = p.start_ms - prev;
                gaps.push(g);
                if g < window * MIN {
                    inside_req += 1;
                }
                if g < eff_ms {
                    inside_eff += 1;
                    if !p.relaxed() {
                        println!(
                            "UNMARKED gap {} ms item {} flags {:?} deck {:?}",
                            g,
                            p.item_id(),
                            p.entry.flags,
                            p.entry.deck
                        );
                        unmarked += 1;
                    }
                }
            }
        }
        gaps.sort_unstable();
        let pct = |q: f64| gaps[((gaps.len() - 1) as f64 * q) as usize] as f64 / MIN as f64;
        let key = format!("pl:{pl}");
        let (pairs, bad) = seam_violations(&plays, &key, info.k);
        let passes = pass_orders(&plays, &key);
        let corrs: Vec<f64> = passes
            .windows(2)
            .filter(|w| w[0].len() == info.units && w[1].len() == info.units)
            .filter_map(|w| spearman(&w[0], &w[1]))
            .collect();
        let identical = passes.windows(2).filter(|w| w[0] == w[1]).count();
        let lag10: Vec<f64> = (0..passes.len().saturating_sub(10))
            .filter(|&i| passes[i].len() == info.units && passes[i + 10].len() == info.units)
            .filter_map(|i| spearman(&passes[i], &passes[i + 10]))
            .collect();
        Stats {
            plays: plays.len(),
            tracks: plays
                .iter()
                .filter(|p| p.entry.origin == Origin::Rotation)
                .count(),
            relaxed: plays.iter().filter(|p| p.relaxed()).count(),
            inside_eff,
            unmarked,
            inside_req,
            pcts: [
                pct(0.0),
                pct(0.05),
                pct(0.25),
                pct(0.5),
                pct(0.75),
                pct(0.95),
            ],
            seam: (pairs, bad),
            mean_corr: corrs.iter().sum::<f64>() / corrs.len().max(1) as f64,
            lag10: lag10.iter().sum::<f64>() / lag10.len().max(1) as f64,
            identical,
            info,
        }
    };

    // The share of a pass the window may take: the sweep that chose `WINDOW_SHARE_OF_PASS`.
    println!("share sweep, requested window 120 min:");
    for share in [0.5, 0.6, 0.65, 0.7, 0.8, 1.0] {
        let st = sim(120, share);
        println!(
            "  share {share:.2}: effective {:.1} min, K {}, relaxed {}, min gap {:.1} min, 5th pct {:.1} min, correlation {:.3} (seam alone {:.3}), 10 passes apart {:.3}, identical consecutive passes {}",
            st.info.achievable_minutes,
            st.info.k,
            st.relaxed,
            st.pcts[0],
            st.pcts[1],
            st.mean_corr,
            seam_rank_correlation(st.info.units, st.info.k),
            st.lag10,
            st.identical
        );
    }
    for window in [60i64, 120] {
        let st = sim(window, WINDOW_SHARE_OF_PASS);
        let info = &st.info;
        let [p0, p5, p25, p50, p75, p95] = st.pcts;
        println!(
            "office {window}-minute window: {} plays ({} tracks) in 30 days; effective window {:.1} min (share {WINDOW_SHARE_OF_PASS} of a {:.1}-minute pass, cut {}), K {} (W {}, mean {:.2} min, seam alone {:.1} min); \
             gap between two plays of a track, percentiles min/5/25/50/75/95: {p0:.1}/{p5:.1}/{p25:.1}/{p50:.1}/{p75:.1}/{p95:.1} min; \
             plays inside the effective window {} (unmarked {}); inside the requested {window} min {}; rule_relaxed rows {}; \
             seam violations {} of {} pass pairs; mean rank correlation between consecutive passes {:.3} (seam alone {:.3}), 10 passes apart {:.3} (limit 0.1); identical consecutive passes {}",
            st.plays,
            st.tracks,
            info.achievable_minutes,
            info.units as f64 * info.mean_minutes,
            info.capped,
            info.k,
            info.window_items,
            info.mean_minutes,
            info.seam_minutes,
            st.inside_eff,
            st.unmarked,
            st.inside_req,
            st.relaxed,
            st.seam.1,
            st.seam.0,
            st.mean_corr,
            seam_rank_correlation(info.units, info.k),
            st.lag10,
            st.identical
        );
        assert_eq!(st.seam.1, 0);
        assert!(
            st.lag10.abs() < 0.1,
            "the order does not drift: {}",
            st.lag10
        );
        assert_eq!(st.unmarked, 0);
        assert_eq!(st.identical, 0, "two consecutive passes in the same order");
        // "About zero": at most one relaxed play a day.
        assert!(st.relaxed <= 30, "{} relaxed plays", st.relaxed);
    }
}

struct Stats {
    plays: usize,
    tracks: usize,
    relaxed: usize,
    inside_eff: usize,
    unmarked: usize,
    inside_req: usize,
    pcts: [f64; 6],
    seam: (usize, usize),
    mean_corr: f64,
    lag10: f64,
    identical: usize,
    info: station::sched::Info,
}
