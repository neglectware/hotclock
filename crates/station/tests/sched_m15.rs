//! M1.5 acceptance tests of the scheduler that need no audio: the deck and the seam, continuous
//! re-randomization, the no-repeat window, relaxation, `insert_every`, pinned entries, album
//! continuity. They run on a virtual clock through `station::sim`, i.e. the code the station runs.

use library::playlists::Mode;
use station::catalog::testing::{album_row, catalog, row};
use station::catalog::Catalog;
use station::deck::Unit;
use station::programming::*;
use station::sched::*;
use station::sim::{run, seam_rank_correlation, seam_violations, spacing_ms, spearman};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

const MIN: i64 = 60_000;
const T0: i64 = 1_800_000_000_000;

fn music(n: i64, ms: i64) -> Vec<library::tracks::TrackRow> {
    (1..=n)
        .map(|i| {
            row(
                i,
                &format!("Song {i}"),
                &format!("Artist {i}"),
                "music",
                "music",
                ms,
            )
        })
        .collect()
}

fn liners(first_id: i64, n: i64) -> Vec<library::tracks::TrackRow> {
    (0..n)
        .map(|i| {
            row(
                first_id + i,
                &format!("Liner {i}"),
                "Station",
                "liner",
                "liner",
                8_000,
            )
        })
        .collect()
}

fn shuffle_playlist(id: i64, n: i64, no_repeat: i64, blocks: Vec<Block>) -> Playlist {
    let mut entries: Vec<Entry> = (1..=n)
        .map(|i| Entry::Item {
            item_id: i,
            weight: 1.0,
        })
        .collect();
    entries.extend(blocks.into_iter().map(Entry::Block));
    Playlist {
        id,
        name: format!("pl{id}"),
        mode: Mode::Shuffle,
        no_repeat_minutes: no_repeat,
        separation: Separation {
            title_minutes: 0,
            artist_minutes: 0,
            album_minutes: 0,
        },
        station_blocks: true,
        station_id: None,
        enabled: true,
        entries,
    }
}

fn insert_every(id: i64, n: u32, category_id: i64) -> Block {
    Block {
        id,
        name: "liners".into(),
        priority: 100,
        enabled: true,
        trigger: Trigger::Standing,
        timed: None,
        problem: None,
        kind: BlockKind::InsertEvery(InsertEvery {
            n,
            source: Source::Category {
                id: Some(category_id),
                name: None,
            },
            count_kind: "music".into(),
            min_gap_minutes: 0,
            resets_on: vec!["id".into()],
            inherit: true,
        }),
    }
}

fn prog(pls: Vec<Playlist>) -> Arc<Programming> {
    Arc::new(Programming {
        playlists: pls
            .into_iter()
            .map(|p| (p.id, p))
            .collect::<BTreeMap<_, _>>(),
        station_blocks: vec![],
    })
}

fn sched(p: Arc<Programming>, playlist: i64, seed: u64) -> Scheduler {
    let mut s = Scheduler::new(SchedConfig::default(), p, seed);
    s.start_program(playlist, T0);
    s
}

// ---- 2. deck and seam ---------------------------------------------------------------------------------

#[test]
fn m15_2_every_item_once_per_pass_with_the_seam_and_the_spacing_over_1000_passes() {
    // (window, K, cut to fit, the milestone's spacing: more than this many items apart). At 120
    // minutes the window is cut to 65 (0.65 of the 100-minute pass) and K to floor(20 / 3) = 6
    // (review of M1.5); the milestone's "at least 11 items apart" still holds, kept by the pick-time
    // check (65 minutes is 13 items).
    for (window, k_expected, capped, k_milestone) in
        [(30, 5usize, false, 5usize), (120, 6, true, 10)]
    {
        let cat = catalog(music(20, 5 * MIN));
        let p = prog(vec![shuffle_playlist(1, 20, window, vec![])]);
        let mut s = sched(p.clone(), 1, 42);
        let info = s.rotation_info(p.playlists.get(&1).unwrap(), &cat);
        assert_eq!(
            (info.k, info.capped),
            (k_expected, capped),
            "window {window}"
        );
        if capped {
            // The window is cut to 0.65 of the 100-minute pass (review of M1.5, the owner's rule): the
            // milestone's "55 minutes" was the seam's own guarantee, which is still `seam_minutes`.
            assert_eq!(
                info.message().unwrap(),
                "this playlist can keep a track about 65 minutes apart; the window asks for 120"
            );
            assert!((info.achievable_minutes - 65.0).abs() < 1e-9);
            assert!((info.seam_minutes - 35.0).abs() < 1e-9);
            // The setting stays as set; the cap and its reason are reported beside it.
            assert_eq!(info.no_repeat_configured, 120);
            assert!((info.no_repeat_effective - 65.0).abs() < 1e-9);
            assert_eq!(
                info.no_repeat_reason.as_deref(),
                Some("capped: 20 tracks, 100 min total")
            );
        } else {
            assert!(info.message().is_none() && info.no_repeat_reason.is_none());
            assert_eq!(info.no_repeat_effective, 30.0);
        }
        let plays = run(
            &mut s,
            &cat,
            &EffectiveTiming,
            T0,
            i64::MAX,
            20 * 1000,
            |_, _, _, _| {},
        );
        assert_eq!(plays.len(), 20_000);
        // Every item exactly once per pass.
        let mut per_pass: BTreeMap<u64, Vec<i64>> = BTreeMap::new();
        for pl in &plays {
            per_pass
                .entry(pl.entry.deck.as_ref().unwrap().gen)
                .or_default()
                .push(pl.item_id());
        }
        assert_eq!(per_pass.len(), 1000);
        for (g, v) in &per_pass {
            let set: HashSet<_> = v.iter().collect();
            assert_eq!((v.len(), set.len()), (20, 20), "pass {g}");
        }
        // The seam, and the spacing: any two plays of one item at least K + 1 apart.
        let (pairs, bad) = seam_violations(&plays, "pl:1", k_expected);
        assert_eq!((pairs, bad), (999, 0), "window {window}");
        let mut last: HashMap<i64, usize> = HashMap::new();
        let mut min_apart = usize::MAX;
        for (i, p) in plays.iter().enumerate() {
            if let Some(j) = last.insert(p.item_id(), i) {
                min_apart = min_apart.min(i - j);
            }
        }
        assert!(
            min_apart > k_milestone,
            "window {window}: two plays only {min_apart} items apart"
        );
        println!("2: window {window} min: K {k_expected} (effective window {:.0} min), closest repeat {min_apart} items apart (limit more than {k_milestone}), seam pairs {pairs} violations {bad}", info.achievable_minutes);
    }
}

// ---- 2b. it keeps randomizing --------------------------------------------------------------------------

#[test]
fn m15_2b_passes_differ_and_their_correlation_is_the_seams_own() {
    let n = 20usize;
    let cat = catalog(music(n as i64, 5 * MIN));
    let p = prog(vec![shuffle_playlist(1, n as i64, 30, vec![])]);
    let mut s = sched(p, 1, 7);
    let plays = run(
        &mut s,
        &cat,
        &EffectiveTiming,
        T0,
        i64::MAX,
        n * 1000,
        |_, _, _, _| {},
    );
    let passes: Vec<Vec<i64>> = plays
        .chunks(n)
        .map(|c| c.iter().map(|p| p.item_id()).collect())
        .collect();
    assert_eq!(passes.len(), 1000);
    for w in passes.windows(2) {
        assert_ne!(w[0], w[1], "two consecutive passes had the same order");
    }
    // Spearman rank correlation between consecutive passes, against its exact expectation under the
    // seam (review of M1.5): the milestone's "within 0.05 of 0" cannot hold with a seam, because the
    // last K of a pass are barred from the first K of the next and so land in its later ranks.
    // `sim::seam_rank_correlation` derives the expectation for the deck's draw (0.188 for K = 5 of
    // 20); the measured mean must lie within 4 standard errors of it.
    let rhos: Vec<f64> = passes
        .windows(2)
        .map(|w| spearman(&w[0], &w[1]).unwrap())
        .collect();
    let m = rhos.len() as f64;
    let mean_corr = rhos.iter().sum::<f64>() / m;
    let sd = (rhos.iter().map(|r| (r - mean_corr).powi(2)).sum::<f64>() / (m - 1.0)).sqrt();
    let se = sd / m.sqrt();
    let expected = seam_rank_correlation(n, 5);
    println!(
        "2b: mean rank correlation between consecutive passes {mean_corr:.4}, expected under the seam {expected:.4} (K = 5 of 20); difference {:.4} (limit 4 standard errors, {:.4})",
        mean_corr - expected,
        4.0 * se
    );
    assert!(
        (mean_corr - expected).abs() < 4.0 * se,
        "{mean_corr} vs {expected}"
    );
}

/// Every item in every position, worst deviation from uniform, and Pearson's statistic over the
/// item-by-position table.
fn position_deviation(passes: &[Vec<i64>], n: usize) -> (f64, f64) {
    let mut max_dev = 0.0f64;
    let mut chi2 = 0.0f64;
    for pos in 0..n {
        let mut counts: HashMap<i64, usize> = HashMap::new();
        for p in passes {
            *counts.entry(p[pos]).or_default() += 1;
        }
        let expect = passes.len() as f64 / n as f64;
        for i in 1..=n as i64 {
            let c = *counts.get(&i).unwrap_or(&0) as f64;
            max_dev = max_dev.max((c - expect).abs() / expect);
            chi2 += (c - expect).powi(2) / expect;
        }
    }
    (max_dev, chi2)
}

#[test]
fn m15_2b_every_position_is_uniform() {
    // Every position is uniform, the first K included (review of M1.5): an item is among the barred
    // K with probability K/n, and averaged over that the draw puts it in each place with probability
    // 1/n. The count in one item-by-position cell over N passes is about Binomial(N, 1/n), relative
    // standard deviation sqrt((n - 1) / N): 13.8 percent at N = 1,000, where the worst of 400 cells
    // of a perfect generator is already about 40 percent off, so the milestone's 20 percent bound is
    // noise there. At N = 10,000 it is 4.36 percent and 20 percent is 4.6 standard deviations (a
    // perfect generator fails it with probability about 0.002 over the 400 cells). Pearson's
    // statistic over the table is printed beside its expectation, n(n - 1) = 380.
    let n = 20usize;
    let cat = catalog(music(n as i64, 5 * MIN));
    let p = prog(vec![shuffle_playlist(1, n as i64, 30, vec![])]);
    let mut s = sched(p, 1, 31);
    let plays = run(
        &mut s,
        &cat,
        &EffectiveTiming,
        T0,
        i64::MAX,
        n * 10_000,
        |_, _, _, _| {},
    );
    let passes: Vec<Vec<i64>> = plays
        .chunks(n)
        .map(|c| c.iter().map(|p| p.item_id()).collect())
        .collect();
    let (dev1000, _) = position_deviation(&passes[..1000], n);
    let (dev, chi2) = position_deviation(&passes, n);
    println!(
        "2b: worst item-by-position deviation from uniform, all 20 positions: {:.1} percent over 1,000 passes (not asserted), {:.1} percent over 10,000 (limit 20); Pearson statistic {chi2:.0} (expected 380 for a uniform draw)",
        dev1000 * 100.0,
        dev * 100.0
    );
    assert!(dev < 0.20, "{dev}");
    assert!(chi2 < 380.0 * 1.5, "{chi2}");
}

#[test]
fn m15_2c_thirty_days_of_the_office_playlist_never_repeats_inside_the_window() {
    // 18 items of real-looking, uneven lengths (3:20 to 8:10), a liner every 4, 60-minute window.
    let mut rows: Vec<_> = (1..=18)
        .map(|i| {
            let ms = (200 + (i * 37) % 291) * 1000;
            row(i, &format!("Track {i}"), "Artist", "music", "music", ms)
        })
        .collect();
    rows.extend(liners(101, 3));
    let cat = catalog(rows);
    let p = prog(vec![shuffle_playlist(
        1,
        18,
        60,
        vec![insert_every(1, 4, 2)],
    )]);
    let mut s = sched(p, 1, 2026);
    let until = T0 + 30 * 24 * 60 * MIN;
    let plays = run(
        &mut s,
        &cat,
        &EffectiveTiming,
        T0,
        until,
        usize::MAX,
        |_, _, _, _| {},
    );
    let mut last: HashMap<i64, i64> = HashMap::new();
    let (mut violations, mut relaxed_marked) = (0, 0);
    let mut min_gap = i64::MAX;
    for pl in plays.iter().filter(|p| p.entry.origin == Origin::Rotation) {
        if let Some(prev) = last.insert(pl.item_id(), pl.start_ms) {
            let gap = pl.start_ms - prev;
            min_gap = min_gap.min(gap);
            if gap < 60 * MIN {
                if pl.relaxed() {
                    relaxed_marked += 1;
                } else {
                    violations += 1;
                }
            }
        }
    }
    let relaxed_total = plays.iter().filter(|p| p.relaxed()).count();
    println!(
        "2c: {} plays in 30 virtual days, shortest gap between two plays of a track {:.1} min, \
         unmarked violations {violations}, rule_relaxed rows {relaxed_total} (of which inside the window {relaxed_marked})",
        plays.len(),
        min_gap as f64 / MIN as f64
    );
    assert_eq!(
        violations, 0,
        "a track repeated inside the window without rule_relaxed"
    );
    let (pairs, bad) = seam_violations(
        &plays,
        "pl:1",
        s.rotation_info(s.program_playlist().unwrap(), &cat).k,
    );
    assert_eq!(bad, 0, "seam violations over {pairs} passes");
}

// ---- 3. relaxation never blocks -------------------------------------------------------------------------

#[test]
fn m15_3_a_three_item_playlist_with_a_window_it_cannot_meet_keeps_playing_and_records_it() {
    // Since the review of M1.5 the no-repeat window itself is cut to what a playlist can keep (0.65
    // of a pass, and at most n - 2 items: 5 minutes here, asked 120), so it no longer relaxes; the separation rules are
    // not cut, and one artist with `sep_artist_minutes` 120 over three 5-minute items can never be
    // met. That is the rule that must relax and never block.
    let rows: Vec<_> = (1..=3)
        .map(|i| {
            row(
                i,
                &format!("Song {i}"),
                "One Artist",
                "music",
                "music",
                5 * MIN,
            )
        })
        .collect();
    let cat = catalog(rows);
    let mut pl = shuffle_playlist(1, 3, 120, vec![]);
    pl.separation.artist_minutes = 120;
    let p = prog(vec![pl]);
    let info = sched(p.clone(), 1, 3).rotation_info(p.playlists.get(&1).unwrap(), &cat);
    println!(
        "3: window asked 120 min, kept {:.2} min (cut {})",
        info.achievable_minutes, info.capped
    );
    assert!(info.capped && (info.achievable_minutes - 5.0).abs() < 1e-9);
    let mut s = sched(p, 1, 3);
    let until = T0 + 24 * 60 * MIN;
    let plays = run(
        &mut s,
        &cat,
        &EffectiveTiming,
        T0,
        until,
        usize::MAX,
        |_, _, _, _| {},
    );
    // No gap: every item starts where the last one ended.
    for w in plays.windows(2) {
        assert_eq!(w[1].start_ms - w[0].start_ms, 5 * MIN);
    }
    assert!(plays.len() >= 24 * 12 - 1, "{}", plays.len());
    let relaxed = plays.iter().filter(|p| p.relaxed()).count();
    println!(
        "3: {} plays over 24 virtual hours with no gap; {relaxed} carry rule_relaxed",
        plays.len()
    );
    assert!(relaxed > plays.len() / 2);
    // It still randomizes: the order of passes is not one fixed cycle.
    let orders: HashSet<Vec<i64>> = plays
        .chunks(3)
        .map(|c| c.iter().map(|p| p.item_id()).collect())
        .collect();
    assert!(orders.len() > 3, "{orders:?}");
}

// ---- 4. insert_every -----------------------------------------------------------------------------------

#[test]
fn m15_4_a_liner_after_every_fourth_song_rotating_through_all_three_and_no_gap() {
    let mut rows = music(10, 4 * MIN);
    rows.extend(liners(101, 3));
    let cat = catalog(rows);
    let p = prog(vec![shuffle_playlist(
        1,
        10,
        30,
        vec![insert_every(1, 4, 2)],
    )]);
    let mut s = sched(p, 1, 11);
    let plays = run(
        &mut s,
        &cat,
        &EffectiveTiming,
        T0,
        T0 + 8 * 60 * MIN,
        usize::MAX,
        |_, _, _, _| {},
    );
    let kinds: Vec<bool> = plays
        .iter()
        .map(|p| p.entry.origin == Origin::Insert)
        .collect();
    // Songs 1-4, liner, 5-8, liner...
    for (i, is_liner) in kinds.iter().enumerate() {
        assert_eq!(*is_liner, i % 5 == 4, "play {i}");
    }
    let liner_ids: Vec<i64> = plays
        .iter()
        .filter(|p| p.entry.origin == Origin::Insert)
        .map(|p| p.item_id())
        .collect();
    assert!(liner_ids.len() >= 20);
    for g in liner_ids.chunks_exact(3) {
        let set: HashSet<_> = g.iter().collect();
        assert_eq!(
            set.len(),
            3,
            "a liner repeated before all three had played: {g:?}"
        );
    }
    // 8 virtual hours: exactly one liner per four songs.
    let songs = kinds.iter().filter(|l| !**l).count();
    let liners_n = kinds.iter().filter(|l| **l).count();
    println!("4: {songs} songs, {liners_n} liners in 8 virtual hours (one after every fourth)");
    assert!((liners_n as i64 - (songs / 4) as i64).abs() <= 1);
    // No gap: on the effective timing each item follows the one before.
    for w in plays.windows(2) {
        let dur = if w[0].entry.origin == Origin::Insert {
            8_000
        } else {
            4 * MIN
        };
        assert_eq!(w[1].start_ms - w[0].start_ms, dur);
    }
    // The office playlist of 4.4 runs 24 virtual hours the same way.
    let cat2 = {
        let mut rows = music(18, 5 * MIN);
        rows.extend(liners(101, 3));
        catalog(rows)
    };
    let p2 = prog(vec![shuffle_playlist(
        1,
        18,
        120,
        vec![insert_every(1, 4, 2)],
    )]);
    let mut s2 = sched(p2, 1, 5);
    let plays2 = run(
        &mut s2,
        &cat2,
        &EffectiveTiming,
        T0,
        T0 + 24 * 60 * MIN,
        usize::MAX,
        |_, _, _, _| {},
    );
    for w in plays2.windows(2) {
        let dur = if w[0].entry.origin == Origin::Insert {
            8_000
        } else {
            5 * MIN
        };
        assert_eq!(
            w[1].start_ms - w[0].start_ms,
            dur,
            "a gap in the office playlist"
        );
    }
    assert!(plays2.len() > 24 * 10);
}

#[test]
fn an_insert_never_repeats_a_third_non_music_item_in_a_row_and_min_gap_holds() {
    let mut rows = music(6, 4 * MIN);
    rows.extend(liners(101, 3));
    let cat = catalog(rows);
    let mut b1 = insert_every(1, 1, 2);
    let mut b2 = insert_every(2, 1, 2);
    let mut b3 = insert_every(3, 1, 2);
    b1.priority = 300;
    b2.priority = 200;
    b3.priority = 100;
    let p = prog(vec![shuffle_playlist(1, 6, 0, vec![b1, b2, b3])]);
    let mut s = sched(p, 1, 1);
    let plays = run(
        &mut s,
        &cat,
        &EffectiveTiming,
        T0,
        T0 + 6 * 60 * MIN,
        usize::MAX,
        |_, _, _, _| {},
    );
    let mut run_len = 0;
    for pl in &plays {
        if pl.entry.origin == Origin::Insert {
            run_len += 1;
            assert!(run_len <= 2, "three inserts in a row");
        } else {
            run_len = 0;
        }
    }
    // min_gap_minutes: a liner at most every 20 minutes even though it is due every song.
    let mut ie = insert_every(1, 1, 2);
    if let BlockKind::InsertEvery(x) = &mut ie.kind {
        x.min_gap_minutes = 20;
    }
    let p = prog(vec![shuffle_playlist(1, 6, 0, vec![ie])]);
    let mut s = sched(p, 1, 1);
    let plays = run(
        &mut s,
        &cat,
        &EffectiveTiming,
        T0,
        T0 + 6 * 60 * MIN,
        usize::MAX,
        |_, _, _, _| {},
    );
    let starts: Vec<i64> = plays
        .iter()
        .filter(|p| p.entry.origin == Origin::Insert)
        .map(|p| p.start_ms)
        .collect();
    assert!(starts.len() > 10);
    for w in starts.windows(2) {
        assert!(
            w[1] - w[0] >= 20 * MIN,
            "liners {} minutes apart",
            (w[1] - w[0]) / MIN
        );
    }
}

// ---- 8. pinned entries and reshuffle -----------------------------------------------------------------------

#[test]
fn m15_8_pinned_entries_keep_their_index_across_ten_reshuffles() {
    let cat = catalog(music(20, 5 * MIN));
    let p = prog(vec![shuffle_playlist(1, 20, 30, vec![])]);
    let mut s = sched(p, 1, 8);
    s.fill(T0, &cat, &EffectiveTiming);
    let first = s.head().unwrap().id;
    s.on_started(first, T0, &cat);
    s.fill(T0, &cat, &EffectiveTiming);
    let a = s
        .insert(15, At::Index(2), T0, &cat, &EffectiveTiming)
        .unwrap()
        .entry_id
        .unwrap();
    let b = s
        .insert(16, At::Index(5), T0, &cat, &EffectiveTiming)
        .unwrap()
        .entry_id
        .unwrap();
    for i in 0..10 {
        s.reshuffle(T0 + i * 1000, &cat, &EffectiveTiming).unwrap();
        let q = &s.st.queue;
        assert_eq!(q[2].id, a, "pinned entry a moved on reshuffle {i}");
        assert_eq!(q[5].id, b, "pinned entry b moved on reshuffle {i}");
        assert!(q.len() >= 6);
    }
}

#[test]
fn m15_1_a_reshuffle_never_changes_the_committed_entry_and_returns_the_next_up() {
    let cat = catalog(music(20, 5 * MIN));
    let p = prog(vec![shuffle_playlist(1, 20, 30, vec![])]);
    let mut changed_soft = 0;
    for trial in 0..200u64 {
        let mut s = sched(p.clone(), 1, 100 + trial);
        s.fill(T0, &cat, &EffectiveTiming);
        let first = s.head().unwrap().id;
        s.on_started(first, T0, &cat);
        // A random time inside the item: sometimes the head is committed (within 20 s), often not.
        let at = T0 + (trial as i64 * 7919) % (5 * MIN);
        s.fill(at, &cat, &EffectiveTiming);
        let on_air = s.on_air().cloned().unwrap();
        let before: Vec<i64> = s.st.queue.iter().map(|e| e.item_id).collect();
        let head_before = s.head().cloned().unwrap();
        let eff = s.reshuffle(at, &cat, &EffectiveTiming).unwrap();
        assert_eq!(
            s.on_air().unwrap(),
            &on_air,
            "trial {trial}: on air changed"
        );
        if head_before.committed {
            assert_eq!(
                s.head().unwrap().id,
                head_before.id,
                "trial {trial}: the committed entry changed"
            );
            assert!(!eff.head_changed);
        }
        if before[1..]
            != s.st
                .queue
                .iter()
                .skip(1)
                .map(|e| e.item_id)
                .collect::<Vec<_>>()[..]
        {
            changed_soft += 1;
        }
        // Every item still appears at most once per pass in the plan.
        let ids: Vec<i64> = std::iter::once(on_air.item_id)
            .chain(s.st.queue.iter().map(|e| e.item_id))
            .collect();
        let uniq: HashSet<_> = ids.iter().collect();
        assert!(
            uniq.len() >= ids.len().saturating_sub(1) - 3,
            "trial {trial}: {ids:?}"
        );
    }
    println!("1: 200 reshuffles; the soft plan changed in {changed_soft}; on air and committed never did");
    assert!(changed_soft > 150, "a reshuffle should redraw the plan");
}

// ---- 9. adjacency ------------------------------------------------------------------------------------------------

fn album12() -> Vec<library::tracks::TrackRow> {
    // Two discs of six; TRACKTOTAL is the disc's own count.
    let mut v = vec![];
    for d in 1..=2i64 {
        for t in 1..=6i64 {
            v.push(album_row(
                100 + d * 10 + t,
                &format!("D{d}T{t}"),
                "Album",
                Some(d),
                Some(t),
                Some(6),
                3 * MIN,
            ));
        }
    }
    v
}

fn continuity_flags(s: &Scheduler) -> Vec<bool> {
    s.st.queue.iter().map(|e| e.continuity_from_prev).collect()
}

#[test]
fn m15_9_play_album_sets_continuity_on_exactly_the_eleven_boundaries_and_the_rules_clear_it() {
    let mut rows = music(4, 4 * MIN);
    rows.extend(album12());
    let cat = catalog(rows);
    let key = cat.get(111).unwrap().meta.album_key.clone();
    let p = prog(vec![shuffle_playlist(1, 4, 0, vec![])]);
    let mut s = sched(p.clone(), 1, 1);
    s.fill(T0, &cat, &EffectiveTiming);
    let e = s
        .play_album(&key, true, At::End, T0, &cat, &EffectiveTiming)
        .unwrap();
    let first = e.entry_id.unwrap();
    let start = s.st.queue.iter().position(|q| q.id == first).unwrap();
    let run: Vec<&QEntry> = s.st.queue[start..start + 12].iter().collect();
    let items: Vec<i64> = run.iter().map(|e| e.item_id).collect();
    assert_eq!(
        items,
        vec![111, 112, 113, 114, 115, 116, 121, 122, 123, 124, 125, 126],
        "disc and track order"
    );
    let flags = continuity_flags(&s);
    let in_run: Vec<bool> = flags[start..start + 12].to_vec();
    assert_eq!(in_run.iter().filter(|f| **f).count(), 11);
    assert!(
        !in_run[0],
        "the first track of the run is not continued from what came before"
    );
    assert!(
        in_run[6],
        "track 1 of disc 2 follows the last track of disc 1"
    );

    // continuity: normal clears every one.
    let mut s2 = sched(p.clone(), 1, 1);
    s2.fill(T0, &cat, &EffectiveTiming);
    s2.play_album(&key, false, At::End, T0, &cat, &EffectiveTiming)
        .unwrap();
    assert!(continuity_flags(&s2).iter().all(|f| !f));

    // A manual insert between two tracks breaks the pair.
    let mut s3 = sched(p.clone(), 1, 1);
    s3.fill(T0, &cat, &EffectiveTiming);
    s3.play_album(&key, true, At::End, T0, &cat, &EffectiveTiming)
        .unwrap();
    let start3 = s3.st.queue.iter().position(|q| q.item_id == 111).unwrap();
    s3.insert(1, At::Index(start3 + 3), T0, &cat, &EffectiveTiming)
        .unwrap();
    let f = continuity_flags(&s3);
    assert_eq!(
        f[start3..start3 + 13].iter().filter(|x| **x).count(),
        10,
        "the insert splits one of the 11 adjacent pairs"
    );
    assert!(!f[start3 + 3] && !f[start3 + 4]);

    // A different album_key: two tracks of different albums are never adjacent.
    let mut rows = album12();
    rows.push(album_row(
        300,
        "Other 1",
        "Other",
        Some(1),
        Some(7),
        None,
        3 * MIN,
    ));
    let cat2 = catalog(rows);
    let (a, b) = (cat2.meta(116).unwrap(), cat2.meta(300).unwrap());
    assert!(!cat2.adjacent(a, b));
    // A missing track number is never adjacent; a missing disc tag means disc 1.
    let rows = vec![
        album_row(1, "a", "X", None, Some(1), None, MIN),
        album_row(2, "b", "X", None, Some(2), None, MIN),
        album_row(3, "c", "X", None, None, None, MIN),
        album_row(4, "d", "X", Some(1), Some(3), None, MIN),
    ];
    let cat3 = catalog(rows);
    let m = |i| cat3.meta(i).unwrap();
    assert!(cat3.adjacent(m(1), m(2)), "a missing disc tag is disc 1");
    assert!(
        !cat3.adjacent(m(2), m(3)) && !cat3.adjacent(m(3), m(4)),
        "a missing track number is never adjacent"
    );
    // Disc 2 track 1 follows the last track of disc 1 only when it is the last track.
    let rows = vec![
        album_row(1, "a", "X", Some(1), Some(1), None, MIN),
        album_row(2, "b", "X", Some(1), Some(2), None, MIN),
        album_row(3, "c", "X", Some(2), Some(1), None, MIN),
    ];
    let cat4 = catalog(rows);
    assert!(
        cat4.adjacent(cat4.meta(2).unwrap(), cat4.meta(3).unwrap()),
        "highest track the library holds is the last"
    );
    assert!(!cat4.adjacent(cat4.meta(1).unwrap(), cat4.meta(3).unwrap()));
}

#[test]
fn m15_9_a_shuffle_never_sets_continuity_but_a_sequential_walk_and_an_album_block_do() {
    let mut rows = album12();
    rows.extend(music(6, 4 * MIN));
    let cat = catalog(rows);
    // Shuffle over the 12 album tracks: 200 randomized trials, adjacent picks meet by chance, never gapless.
    let entries: Vec<Entry> = (0..12)
        .map(|i| Entry::Item {
            item_id: 111 + (i / 6) * 10 + i % 6,
            weight: 1.0,
        })
        .collect();
    let mut pl = shuffle_playlist(1, 0, 0, vec![]);
    pl.entries = entries;
    let p = prog(vec![pl]);
    let mut chance_adjacent = 0;
    for trial in 0..200u64 {
        let mut s = sched(p.clone(), 1, 500 + trial);
        let plays = run(
            &mut s,
            &cat,
            &EffectiveTiming,
            T0,
            i64::MAX,
            60,
            |_, _, _, _| {},
        );
        for w in plays.windows(2) {
            assert!(
                !w[1].entry.continuity_from_prev,
                "shuffle formed a run in trial {trial}"
            );
            if cat.adjacent(
                cat.meta(w[0].item_id()).unwrap(),
                cat.meta(w[1].item_id()).unwrap(),
            ) {
                chance_adjacent += 1;
            }
        }
    }
    println!("9: 200 shuffle trials, {chance_adjacent} adjacent pairs met by chance, none gapless");
    assert!(
        chance_adjacent > 0,
        "the trial should have met adjacent tracks"
    );

    // A sequential playlist of the album sets it.
    let mut pl = shuffle_playlist(2, 0, 0, vec![]);
    pl.mode = Mode::Sequential;
    pl.entries = (0..12)
        .map(|i| Entry::Item {
            item_id: 111 + (i / 6) * 10 + i % 6,
            weight: 1.0,
        })
        .collect();
    let p2 = prog(vec![pl]);
    let mut s = sched(p2, 2, 1);
    let plays = run(
        &mut s,
        &cat,
        &EffectiveTiming,
        T0,
        i64::MAX,
        12,
        |_, _, _, _| {},
    );
    let cont = plays
        .iter()
        .filter(|p| p.entry.continuity_from_prev)
        .count();
    assert_eq!(cont, 11, "a sequential walk over the album");

    // An album block in a shuffle playlist plays as one contiguous run, whenever it comes up.
    let key = cat.get(111).unwrap().meta.album_key.clone();
    let mut pl = shuffle_playlist(3, 6, 0, vec![]);
    pl.entries.push(Entry::Album {
        album_key: key,
        gapless: true,
        weight: 1.0,
    });
    let p3 = prog(vec![pl]);
    for trial in 0..50u64 {
        let mut s = sched(p3.clone(), 3, 900 + trial);
        let plays = run(
            &mut s,
            &cat,
            &EffectiveTiming,
            T0,
            i64::MAX,
            60,
            |_, _, _, _| {},
        );
        let mut seen_blocks = 0;
        let mut i = 0;
        while i < plays.len() {
            if plays[i].item_id() >= 111 && plays[i].item_id() <= 126 {
                let ids: Vec<i64> = plays[i..(i + 12).min(plays.len())]
                    .iter()
                    .map(|p| p.item_id())
                    .collect();
                if ids.len() == 12 {
                    assert_eq!(
                        ids,
                        vec![111, 112, 113, 114, 115, 116, 121, 122, 123, 124, 125, 126],
                        "trial {trial}"
                    );
                    assert_eq!(
                        plays[i..i + 12]
                            .iter()
                            .filter(|p| p.entry.continuity_from_prev)
                            .count(),
                        11
                    );
                    seen_blocks += 1;
                }
                i += 12;
            } else {
                i += 1;
            }
        }
        assert!(
            seen_blocks >= 1,
            "trial {trial}: the album block never came up"
        );
    }
}

// ---- restart of the scheduler state ---------------------------------------------------------------------------------

#[test]
fn the_scheduler_state_round_trips_through_json_and_continues_the_same_order() {
    let cat = catalog(music(20, 5 * MIN));
    let p = prog(vec![shuffle_playlist(1, 20, 60, vec![])]);
    let mut a = sched(p.clone(), 1, 77);
    let plays = run(
        &mut a,
        &cat,
        &EffectiveTiming,
        T0,
        i64::MAX,
        30,
        |_, _, _, _| {},
    );
    let json = serde_json::to_string(&a.snapshot()).unwrap();
    let st: SchedState = serde_json::from_str(&json).unwrap();
    let mut b = Scheduler::restore(SchedConfig::default(), p, st);
    let now = plays.last().unwrap().start_ms;
    let qa: Vec<i64> = a.st.queue.iter().map(|e| e.item_id).collect();
    let qb: Vec<i64> = b.st.queue.iter().map(|e| e.item_id).collect();
    assert_eq!(qa, qb);
    let more_a = run(
        &mut a,
        &cat,
        &EffectiveTiming,
        now,
        i64::MAX,
        40,
        |_, _, _, _| {},
    );
    let more_b = run(
        &mut b,
        &cat,
        &EffectiveTiming,
        now,
        i64::MAX,
        40,
        |_, _, _, _| {},
    );
    assert_eq!(
        more_a.iter().map(|p| p.item_id()).collect::<Vec<_>>(),
        more_b.iter().map(|p| p.item_id()).collect::<Vec<_>>()
    );
    let _ = (
        spacing_ms(&plays, Origin::Rotation),
        Unit::Item(1),
        Catalog::default(),
    );
}

#[test]
fn m15_2b_without_a_seam_consecutive_passes_are_uncorrelated() {
    // no_repeat_minutes 0 means only the deck: K = 0, so nothing but the shuffle acts.
    let n = 20usize;
    let cat = catalog(music(n as i64, 5 * MIN));
    let p = prog(vec![shuffle_playlist(1, n as i64, 0, vec![])]);
    let mut s = sched(p, 1, 17);
    let plays = run(
        &mut s,
        &cat,
        &EffectiveTiming,
        T0,
        i64::MAX,
        n * 2000,
        |_, _, _, _| {},
    );
    let passes: Vec<Vec<i64>> = plays
        .chunks(n)
        .map(|c| c.iter().map(|p| p.item_id()).collect())
        .collect();
    let rank = |v: &Vec<i64>| -> HashMap<i64, f64> {
        v.iter().enumerate().map(|(i, x)| (*x, i as f64)).collect()
    };
    let mut sum = 0.0;
    for w in passes.windows(2) {
        let (a, b) = (rank(&w[0]), rank(&w[1]));
        let d2: f64 = a.iter().map(|(k, ra)| (ra - b[k]).powi(2)).sum();
        sum += 1.0 - 6.0 * d2 / (n as f64 * ((n * n) as f64 - 1.0));
    }
    let mean_corr = sum / (passes.len() - 1) as f64;
    println!("2b: K = 0: mean rank correlation between consecutive passes {mean_corr:.4}");
    assert!(mean_corr.abs() < 0.05, "{mean_corr}");
}

/// "Not so long that the playlist can't randomize a little over time" (owner, 4.3): a playlist whose
/// window was cut to fit keeps its no-repeat spacing and its order still drifts away from any earlier
/// pass. Before the review the seam's `K` was capped at `n / 2`, which freezes the halves: the first
/// half of every pass was the first half of the pass before, as a set, forever (rank correlation 0.75
/// between passes 100 apart on these lengths).
#[test]
fn a_capped_rotation_keeps_its_window_and_forgets_its_order_over_passes() {
    let secs = [
        826, 544, 619, 274, 437, 998, 975, 1028, 230, 208, 124, 141, 264, 216, 540, 307, 174, 174,
    ];
    let rows: Vec<_> = secs
        .iter()
        .enumerate()
        .map(|(i, s)| {
            row(
                i as i64 + 1,
                &format!("Song {i}"),
                &format!("Artist {i}"),
                "music",
                "music",
                *s as i64 * 1000,
            )
        })
        .collect();
    let cat = catalog(rows);
    let p = prog(vec![shuffle_playlist(1, 18, 120, vec![])]);
    let mut s = sched(p.clone(), 1, 99);
    let info = s.rotation_info(p.playlists.get(&1).unwrap(), &cat);
    let plays = run(
        &mut s,
        &cat,
        &EffectiveTiming,
        T0,
        i64::MAX,
        18 * 1500,
        |_, _, _, _| {},
    );
    let passes = station::sim::pass_orders(&plays, "pl:1");
    let lag = |l: usize| -> f64 {
        let v: Vec<f64> = (0..passes.len() - l)
            .filter_map(|i| spearman(&passes[i], &passes[i + l]))
            .collect();
        v.iter().sum::<f64>() / v.len() as f64
    };
    let mut last: HashMap<i64, i64> = HashMap::new();
    let mut min_gap = i64::MAX;
    for pl in &plays {
        if let Some(prev) = last.insert(pl.item_id(), pl.start_ms) {
            min_gap = min_gap.min(pl.start_ms - prev);
        }
    }
    let relaxed = plays.iter().filter(|p| p.relaxed()).count();
    let (l1, l10, l100) = (lag(1), lag(10), lag(100));
    println!(
        "randomizes over time: 18 office lengths, window 120 kept at {:.1} min (K {}); shortest gap {:.1} min; relaxed {relaxed}; rank correlation at lag 1 {l1:.3}, lag 10 {l10:.3} (limit 0.05), lag 100 {l100:.3} (limit 0.05)",
        info.achievable_minutes,
        info.k,
        min_gap as f64 / MIN as f64
    );
    assert!(min_gap as f64 >= info.achievable_minutes * MIN as f64);
    assert_eq!(relaxed, 0);
    assert!(l10.abs() < 0.05 && l100.abs() < 0.05);
}

#[test]
fn an_insert_every_block_with_n_below_one_inserts_nothing() {
    let mut rows = music(6, 4 * MIN);
    rows.extend(liners(101, 2));
    let cat = catalog(rows);
    let lid = cat.category_id_by_name("liner").unwrap();
    let p = prog(vec![shuffle_playlist(
        1,
        6,
        0,
        vec![insert_every(9, 0, lid)],
    )]);
    let mut s = sched(p, 1, 1);
    let plays = run(
        &mut s,
        &cat,
        &EffectiveTiming,
        T0,
        T0 + 60 * MIN,
        usize::MAX,
        |_, _, _, _| {},
    );
    let inserts = plays
        .iter()
        .filter(|p| p.entry.origin == Origin::Insert)
        .count();
    println!(
        "insert_every n = 0: {inserts} inserts in {} plays",
        plays.len()
    );
    assert_eq!(inserts, 0);
}
