//! M3 acceptance tests of the scheduler that need no audio: the top-of-hour ID and its window, the
//! hard fallback, DST, clock steps, scheduled starts, preemption by timed flow blocks, one-shot changes
//! and the prediction of the upcoming events. They run on a virtual clock through `station::sim`, i.e.
//! the code the station runs.

use common::timespec::{TimeSpec, Zone};
use library::playlists::Mode;
use station::catalog::testing::{catalog, row};
use station::catalog::Catalog;
use station::programming::*;
use station::sched::*;
use station::sim::{run_timed, SimPlay};
use std::collections::BTreeMap;
use std::sync::Arc;

pub const MIN: i64 = 60_000;
pub const SEC: i64 = 1000;

pub fn zone() -> Zone {
    Zone::named("America/New_York").expect("tz database")
}

/// Unix ms of a New York local time.
pub fn local(s: &str) -> i64 {
    let z = zone();
    z.compatible(s.parse().unwrap()).unwrap()
}

/// 18 music tracks of 3:20 to 8:10 (the office set's spread), liners, IDs and sweepers.
pub fn office_rows() -> Vec<library::tracks::TrackRow> {
    let mut v = Vec::new();
    for i in 1..=18i64 {
        let ms = (200 + (i - 1) * 290 / 17) * SEC;
        v.push(row(
            i,
            &format!("Song {i}"),
            &format!("Artist {i}"),
            "music",
            "music",
            ms,
        ));
    }
    for i in 0..3i64 {
        v.push(row(
            101 + i,
            &format!("Liner {i}"),
            "Station",
            "liner",
            "liner",
            (8 + i) * SEC,
        ));
    }
    for i in 0..3i64 {
        v.push(row(
            201 + i,
            &format!("ID {i}"),
            "Station",
            "id",
            "id",
            (5 + i) * SEC,
        ));
    }
    v
}

pub fn shuffle_playlist(
    id: i64,
    name: &str,
    ids: std::ops::RangeInclusive<i64>,
    no_repeat: i64,
    blocks: Vec<Block>,
) -> Playlist {
    let mut entries: Vec<Entry> = ids
        .map(|i| Entry::Item {
            item_id: i,
            weight: 1.0,
        })
        .collect();
    entries.extend(blocks.into_iter().map(Entry::Block));
    Playlist {
        id,
        name: name.into(),
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

pub fn liners_every(id: i64, n: u32) -> Block {
    Block::new(
        id,
        "liners",
        BlockKind::InsertEvery(InsertEvery {
            n,
            source: Source::Category {
                id: Some(2),
                name: None,
            },
            count_kind: "music".into(),
            min_gap_minutes: 0,
            resets_on: vec!["id".into()],
            inherit: true,
        }),
        Trigger::Standing,
        None,
    )
}

pub fn timed(spec: TimeSpec, mode: BoundaryMode, early: i64, late: i64) -> Timed {
    Timed {
        spec,
        mode,
        early_s: early,
        late_s: late,
        fill: None,
        late_policy: LatePolicy::Default,
        late_window_s: None,
    }
}

/// The top-of-hour ID: a timed `insert` from the `id` category (category id 3), every hour at :00.
pub fn hourly_id(id: i64) -> Block {
    Block::new(
        id,
        "top of hour",
        BlockKind::Insert {
            source: Source::Category {
                id: Some(3),
                name: None,
            },
        },
        Trigger::Timed,
        Some(timed(
            TimeSpec::recurrence("0 * * * *").unwrap(),
            BoundaryMode::SoftThenHard,
            60,
            30,
        )),
    )
}

pub fn prog(pls: Vec<Playlist>, station_blocks: Vec<Block>) -> Arc<Programming> {
    Arc::new(Programming {
        playlists: pls
            .into_iter()
            .map(|p| (p.id, p))
            .collect::<BTreeMap<_, _>>(),
        station_blocks,
    })
}

pub fn sched(p: Arc<Programming>, playlist: i64, t0: i64, seed: u64) -> Scheduler {
    let cfg = SchedConfig {
        zone: zone(),
        ..SchedConfig::default()
    };
    let mut s = Scheduler::new(cfg, p, seed);
    s.start_program(playlist, t0);
    s
}

pub fn log_rows(s: &Scheduler, block: i64) -> Vec<station::timed::LogRow> {
    s.st.log
        .rows
        .iter()
        .filter(|r| r.block_id == block)
        .cloned()
        .collect()
}

pub fn kinds(cat: &Catalog, plays: &[SimPlay]) -> Vec<String> {
    plays
        .iter()
        .map(|p| cat.meta(p.item_id()).unwrap().kind.clone())
        .collect()
}

/// Run in chunks of a day and keep every block-log row seen (the scheduler trims its log to 72 hours).
pub fn run_collect(
    s: &mut Scheduler,
    cat: &Catalog,
    timing: &dyn Timing,
    start: i64,
    until: i64,
) -> (
    Vec<SimPlay>,
    Vec<station::sim::SimAct>,
    Vec<station::timed::LogRow>,
) {
    let mut plays = Vec::new();
    let mut acts = Vec::new();
    let mut rows: Vec<station::timed::LogRow> = Vec::new();
    let mut now = start;
    while now < until {
        let to = (now + 24 * 60 * MIN).min(until);
        let (p, a, n) = run_timed(s, cat, timing, now, to, usize::MAX, |_, _, _, _| {});
        plays.extend(p);
        acts.extend(a);
        for r in &s.st.log.rows {
            if !rows.iter().rev().take(400).any(|x| {
                x.block_id == r.block_id && x.target_ms == r.target_ms && x.run_id == r.run_id
            }) {
                rows.push(r.clone());
            }
        }
        if n <= now {
            break;
        }
        now = n.max(to);
    }
    (plays, acts, rows)
}

fn percentile(v: &[i64], p: f64) -> i64 {
    let mut s = v.to_vec();
    s.sort_unstable();
    s[((s.len() - 1) as f64 * p).round() as usize]
}

// ---- 1. the top-of-hour ID over 30 virtual days -------------------------------------------------------

#[test]
fn m3_1_the_top_of_hour_id_fires_every_hour_inside_its_window_over_30_days() {
    let cat = catalog(office_rows());
    let t0 = local("2026-10-20T12:00:00");
    let until = local("2026-11-19T12:00:00");
    let p = prog(
        vec![shuffle_playlist(
            1,
            "Office",
            1..=18,
            60,
            vec![liners_every(10, 4)],
        )],
        vec![hourly_id(20)],
    );
    let mut s = sched(p, 1, t0, 2026);
    let (plays, _, all_rows) = run_collect(&mut s, &cat, &EffectiveTiming, t0, until);
    let rows: Vec<_> = all_rows.into_iter().filter(|r| r.block_id == 20).collect();
    // The window of 30 days of hours: 720 hours plus the repeated one of the fall-back night.
    let fired: Vec<_> = rows.iter().filter(|r| r.fired_ms.is_some()).collect();
    println!(
        "1: {} plays, {} block-log rows, {} fired",
        plays.len(),
        rows.len(),
        fired.len()
    );
    assert!(fired.len() >= 720, "{} fired", fired.len());
    let devs: Vec<i64> = fired.iter().map(|r| r.deviation_ms.unwrap()).collect();
    assert!(
        devs.iter().all(|d| (-60 * SEC..=30 * SEC).contains(d)),
        "outside the window: {:?}",
        devs.iter()
            .filter(|d| !(-60 * SEC..=30 * SEC).contains(d))
            .take(10)
            .collect::<Vec<_>>()
    );
    let soft = fired.iter().filter(|r| r.how == "soft").count();
    let pct = 100.0 * soft as f64 / fired.len() as f64;
    println!(
        "1: soft {soft} of {} = {pct:.1} percent; deviations s: min {} p5 {} p25 {} median {} p75 {} p95 {} max {}",
        fired.len(),
        percentile(&devs, 0.0) / SEC,
        percentile(&devs, 0.05) / SEC,
        percentile(&devs, 0.25) / SEC,
        percentile(&devs, 0.5) / SEC,
        percentile(&devs, 0.75) / SEC,
        percentile(&devs, 0.95) / SEC,
        percentile(&devs, 1.0) / SEC
    );
    for r in fired.iter().filter(|r| r.how != "soft").take(6) {
        let near: Vec<String> = plays
            .iter()
            .filter(|p| p.start_ms > r.target_ms - 25 * MIN && p.start_ms <= r.target_ms + 2 * MIN)
            .map(|p| {
                format!(
                    "{}@{:+}s{}",
                    cat.meta(p.item_id()).unwrap().title,
                    (p.start_ms - r.target_ms) / SEC,
                    if p.entry.flags.iter().any(|f| f == "fitted") {
                        "*"
                    } else {
                        ""
                    }
                )
            })
            .collect();
        println!(
            "hard case {} {}: {}",
            zone().local_string(r.target_ms),
            r.how,
            near.join(" | ")
        );
    }
    assert!(pct >= 95.0, "{pct}");
    // 3. The ID resets the liner counter: no liner immediately follows an ID.
    let k = kinds(&cat, &plays);
    let follows = k
        .windows(2)
        .filter(|w| w[0] == "id" && w[1] == "liner")
        .count();
    println!(
        "3: {} IDs, liners right after an ID: {follows}",
        k.iter().filter(|x| *x == "id").count()
    );
    assert_eq!(follows, 0);
}

// ---- 2. a playlist of 7-minute tracks: the fit is impossible, the ID is hard at T + 30 s ----------------

fn seven_minute_rows() -> Vec<library::tracks::TrackRow> {
    let mut v: Vec<_> = (1..=12i64)
        .map(|i| {
            row(
                i,
                &format!("Seven {i}"),
                &format!("A{i}"),
                "music",
                "music",
                7 * MIN,
            )
        })
        .collect();
    for i in 0..3i64 {
        v.push(row(
            201 + i,
            &format!("ID {i}"),
            "Station",
            "id",
            "id",
            (5 + i) * SEC,
        ));
    }
    v
}

#[test]
fn m3_2_with_only_seven_minute_tracks_every_id_is_hard_at_t_plus_30_s() {
    let cat = catalog(seven_minute_rows());
    // 20 s past the hour: boundaries every 7 minutes never land inside a 90 s window, and after each
    // hard fallback the next boundaries are at +30 s plus multiples of 7 minutes, which never do either.
    let t0 = local("2026-10-20T12:00:20");
    let until = local("2026-10-22T12:00:00");
    let p = prog(
        vec![shuffle_playlist(1, "Sevens", 1..=12, 60, vec![])],
        vec![hourly_id(20)],
    );
    let mut s = sched(p, 1, t0, 5);
    let (plays, _, rows) = run_collect(&mut s, &cat, &EffectiveTiming, t0, until);
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|r| r.block_id == 20 && r.fired_ms.is_some() && r.target_ms > t0)
        .collect();
    println!("2: {} IDs over 48 hours", rows.len());
    assert!(rows.len() >= 47);
    assert!(
        rows.iter()
            .all(|r| r.how == "hard" && r.deviation_ms == Some(30 * SEC)),
        "{:?}",
        rows.iter().find(|r| r.how != "hard")
    );
    // The ID's entry starts at T + 30 s exactly and the item before it was cut there.
    for r in &rows {
        let id_play = plays
            .iter()
            .find(|p| p.start_ms == r.target_ms + 30 * SEC && p.entry.origin == Origin::Insert);
        assert!(id_play.is_some(), "no ID starting at {}", r.target_ms);
    }
}

// ---- 3b. the ID is a station block ---------------------------------------------------------------------

#[test]
fn m3_3b_the_id_fires_the_same_under_two_programs_and_not_under_station_blocks_off() {
    let mut rows = office_rows();
    for i in 0..6i64 {
        rows.push(row(
            301 + i,
            &format!("Other {i}"),
            "Band",
            "music",
            "music",
            (4 + i) * MIN,
        ));
    }
    let cat = catalog(rows);
    let mut off = shuffle_playlist(3, "Live show", 301..=306, 30, vec![]);
    off.station_blocks = false;
    let p = prog(
        vec![
            shuffle_playlist(1, "Office", 1..=18, 60, vec![]),
            shuffle_playlist(2, "Other", 301..=306, 30, vec![]),
            off,
        ],
        vec![hourly_id(20)],
    );
    let t0 = local("2026-10-20T12:00:00");
    let until = local("2026-10-21T12:00:00");
    let mut counts = Vec::new();
    for pl in [1, 2, 3] {
        let mut s = sched(p.clone(), pl, t0, 11);
        let (plays, _, rows) = run_collect(&mut s, &cat, &EffectiveTiming, t0, until);
        let fired = rows
            .iter()
            .filter(|r| r.block_id == 20 && r.fired_ms.is_some())
            .count();
        let ids = plays
            .iter()
            .filter(|p| p.entry.origin == Origin::Insert)
            .count();
        let devs: Vec<i64> = rows
            .iter()
            .filter(|r| r.block_id == 20)
            .filter_map(|r| r.deviation_ms)
            .collect();
        println!(
            "3b: playlist {pl}: {fired} IDs fired, {ids} ID plays, deviations within window: {}",
            devs.iter().all(|d| (-60 * SEC..=30 * SEC).contains(d))
        );
        counts.push((fired, ids));
        if pl == 3 {
            assert_eq!((fired, ids), (0, 0));
        } else {
            assert!(fired >= 24 && fired == ids, "{fired} {ids}");
            assert!(devs.iter().all(|d| (-60 * SEC..=30 * SEC).contains(d)));
        }
    }
    assert_eq!(counts[0].0, counts[1].0);
}

// ---- 4. DST --------------------------------------------------------------------------------------------

#[test]
fn m3_4_the_fall_back_night_fires_both_one_oclock_hours_and_spring_forward_skips_only_the_missing_hour(
) {
    let cat = catalog(office_rows());
    let p = prog(
        vec![shuffle_playlist(
            1,
            "Office",
            1..=18,
            60,
            vec![liners_every(10, 4)],
        )],
        vec![hourly_id(20)],
    );
    for (from, to, expect_hours, label) in [
        (
            "2026-11-01T00:00:00",
            "2026-11-02T00:00:00",
            25usize,
            "fall back",
        ),
        (
            "2026-03-08T00:00:00",
            "2026-03-09T00:00:00",
            23usize,
            "spring forward",
        ),
    ] {
        let t0 = local(from) - 3 * 3_600_000;
        let day_end = local(to);
        let mut s = sched(p.clone(), 1, t0, 77);
        let (_, _, rows) = run_collect(&mut s, &cat, &EffectiveTiming, t0, day_end + 3_600_000);
        let day: Vec<_> = rows
            .iter()
            .filter(|r| r.block_id == 20 && r.target_ms >= local(from) && r.target_ms < day_end)
            .collect();
        let fired: Vec<_> = day.iter().filter(|r| r.fired_ms.is_some()).collect();
        let locals: Vec<String> = fired
            .iter()
            .map(|r| zone().local_string(r.target_ms)[11..19].to_string())
            .collect();
        println!(
            "4: {label}: {} IDs fired ({} expected); local hours {}",
            fired.len(),
            expect_hours,
            locals.join(" ")
        );
        assert_eq!(fired.len(), expect_hours, "{label}");
        assert!(fired
            .windows(2)
            .all(|w| w[1].target_ms - w[0].target_ms == 3_600_000));
        if label == "fall back" {
            let ones: Vec<_> = locals.iter().filter(|l| l.starts_with("01:")).collect();
            assert_eq!(ones.len(), 2, "both 01:00 hours");
        } else {
            assert!(
                !locals.iter().any(|l| l.starts_with("02:")),
                "the nonexistent hour"
            );
            assert!(
                locals.iter().any(|l| l.starts_with("03:"))
                    && locals.iter().any(|l| l.starts_with("01:"))
            );
        }
        assert!(fired
            .iter()
            .all(|r| (-60 * SEC..=30 * SEC).contains(&r.deviation_ms.unwrap())));
    }
}

// ---- 5. clock steps ------------------------------------------------------------------------------------

#[test]
fn m3_5_a_step_forward_fires_the_missed_id_once_now_and_a_step_back_does_not_fire_it_again() {
    let cat = catalog(office_rows());
    let p = prog(
        vec![shuffle_playlist(1, "Office", 1..=18, 60, vec![])],
        vec![hourly_id(20)],
    );
    let t0 = local("2026-10-20T12:00:00");
    // Forward: +10 minutes at T - 2 minutes (T = 14:00).
    let target = local("2026-10-20T14:00:00");
    let mut s = sched(p.clone(), 1, t0, 3);
    let (_, _, _) = run_timed(
        &mut s,
        &cat,
        &EffectiveTiming,
        t0,
        target - 2 * MIN,
        usize::MAX,
        |_, _, _, _| {},
    );
    // The step: what the scheduler thought was T - 2 min is T + 8 min.
    let old = target - 2 * MIN;
    let new = old + 10 * MIN;
    s.on_clock_step(old, new);
    let (plays, _, _) = run_timed(
        &mut s,
        &cat,
        &EffectiveTiming,
        new,
        target + 90 * MIN,
        usize::MAX,
        |_, _, _, _| {},
    );
    let rows = log_rows(&s, 20);
    let at_target: Vec<_> = rows.iter().filter(|r| r.target_ms == target).collect();
    println!(
        "5: step +10 min at T-2: rows for T: {:?}",
        at_target
            .iter()
            .map(|r| (r.how.clone(), r.fired_ms.map(|f| f - new)))
            .collect::<Vec<_>>()
    );
    assert_eq!(at_target.len(), 1);
    assert_eq!(at_target[0].how, "late_run");
    // Fired now: within a couple of seconds of the step (the first boundary of the hard time, 0 s).
    let lag = at_target[0].fired_ms.unwrap() - new;
    assert!(
        (0..=2 * SEC).contains(&lag),
        "fired {lag} ms after the step"
    );
    assert_eq!(
        plays
            .iter()
            .filter(|p| p
                .entry
                .timed
                .as_ref()
                .is_some_and(|t| t.target_ms == target))
            .count(),
        1
    );

    // Backward: after the ID of 14:00 has fired, the clock goes back 10 minutes.
    let mut s = sched(p, 1, t0, 3);
    let (_, _, now) = run_timed(
        &mut s,
        &cat,
        &EffectiveTiming,
        t0,
        target + 2 * MIN,
        usize::MAX,
        |_, _, _, _| {},
    );
    assert!(log_rows(&s, 20)
        .iter()
        .any(|r| r.target_ms == target && r.fired_ms.is_some()));
    let before = log_rows(&s, 20).len();
    s.on_clock_step(now, now - 10 * MIN);
    let (plays, _, _) = run_timed(
        &mut s,
        &cat,
        &EffectiveTiming,
        now - 10 * MIN,
        target + 50 * MIN,
        usize::MAX,
        |_, _, _, _| {},
    );
    let again = log_rows(&s, 20)
        .iter()
        .filter(|r| r.target_ms == target)
        .count();
    let ids_near: usize = plays
        .iter()
        .filter(|p| {
            p.entry.origin == Origin::Insert
                && p.entry
                    .timed
                    .as_ref()
                    .is_some_and(|t| t.target_ms == target)
        })
        .count();
    println!("5: step -10 min after the ID: rows for T {again} (before {before} rows in all), IDs for T played again {ids_near}");
    assert_eq!((again, ids_near), (1, 0));
}

// ---- helpers for programs ---------------------------------------------------------------------------------

fn start_block(id: i64, cron: &str, anchor: Anchor, how: StartHow) -> Block {
    Block::new(
        id,
        "start",
        BlockKind::Start(StartBlock {
            schedule: vec![TimeSpec::recurrence(cron).unwrap()],
            anchor,
            how,
            mode: if how == StartHow::Now {
                BoundaryMode::Hard
            } else {
                BoundaryMode::Soft
            },
            early_s: 0,
            late_s: 300,
            arm_window_min: 60,
            fallback_grace_s: None,
        }),
        Trigger::First,
        None,
    )
}

fn seq_playlist(id: i64, name: &str, entries: Vec<Entry>) -> Playlist {
    Playlist {
        id,
        name: name.into(),
        mode: Mode::Sequential,
        no_repeat_minutes: 0,
        separation: Separation::default(),
        station_blocks: false,
        station_id: None,
        enabled: true,
        entries,
    }
}

fn item(i: i64) -> Entry {
    Entry::Item {
        item_id: i,
        weight: 1.0,
    }
}

fn rows_of(
    ids: std::ops::RangeInclusive<i64>,
    ms: i64,
    category: &str,
    kind: &str,
) -> Vec<library::tracks::TrackRow> {
    ids.map(|i| {
        row(
            i,
            &format!("{category} {i}"),
            &format!("Artist {i}"),
            category,
            kind,
            ms + (i % 5) * 20 * SEC,
        )
    })
    .collect()
}

// ---- 6. scheduled starts ------------------------------------------------------------------------------------

#[test]
fn m3_6_a_scheduled_start_switches_at_the_first_boundary_after_its_time_and_never_cuts() {
    let mut rows = office_rows();
    rows.extend(rows_of(301..=312, 5 * MIN, "evening", "music"));
    let cat = catalog(rows);
    let mut evening = shuffle_playlist(2, "Evening", 301..=312, 30, vec![]);
    evening.entries.insert(
        0,
        Entry::Block(start_block(
            40,
            "0 18 * * *",
            Anchor::Absolute,
            StartHow::Segue,
        )),
    );
    let mut day = shuffle_playlist(1, "Day", 1..=18, 60, vec![liners_every(10, 4)]);
    day.entries.insert(
        0,
        Entry::Block(start_block(
            39,
            "0 6 * * *",
            Anchor::Absolute,
            StartHow::Segue,
        )),
    );
    let p = prog(vec![day, evening], vec![]);
    let t0 = local("2026-10-20T05:00:00");
    let until = local("2026-10-27T06:00:00");
    let mut s = sched(p, 1, t0, 8);
    s.cfg.default_playlist = Some(1);
    let (plays, _, rows) = run_collect(&mut s, &cat, &EffectiveTiming, t0, until);
    let fired: Vec<_> = rows
        .iter()
        .filter(|r| {
            (r.block_id == 40 || r.block_id == 39) && r.fired_ms.is_some() && r.how != "skipped"
        })
        .collect();
    // 7 evenings and 6 mornings: the first 06:00 finds the Day playlist already running.
    println!(
        "6: {} scheduled starts over 7 days (06:00 and 18:00)",
        fired.len()
    );
    assert_eq!(fired.len(), 13);
    assert!(rows.iter().any(|r| r.block_id == 39 && r.how == "skipped"));
    let mut cuts = 0;
    let mut devs = vec![];
    for r in &fired {
        let start = r.fired_ms.unwrap();
        let i = plays
            .iter()
            .position(|p| {
                p.start_ms == start
                    && p.entry
                        .timed
                        .as_ref()
                        .is_some_and(|t| t.block_id == r.block_id)
            })
            .expect("the first item of the new program");
        assert_eq!(plays[i].item_id() >= 301, r.block_id == 40);
        let prev = &plays[i - 1];
        let ended = prev.start_ms + cat.meta(prev.item_id()).unwrap().effective_ms;
        if ended != start {
            cuts += 1;
        }
        assert!(start >= r.target_ms, "before its time");
        devs.push(start - r.target_ms);
    }
    println!(
        "6: switch delays after 18:00 (s): {:?}; cuts: {cuts}",
        devs.iter().map(|d| d / SEC).collect::<Vec<_>>()
    );
    assert_eq!(cuts, 0, "a scheduled start never cuts");
    assert!(devs.iter().all(|d| *d <= 300 * SEC), "{devs:?}");
}

#[test]
fn m3_6_a_manual_program_holds_a_scheduled_start_until_it_ends() {
    let mut rows = office_rows();
    rows.extend(rows_of(301..=312, 5 * MIN, "evening", "music"));
    rows.extend(rows_of(401..=404, 6 * MIN, "show", "music"));
    rows.push(row(405, "Show last", "Band", "show", "music", 3 * MIN));
    rows.push(row(406, "Show long", "Band", "show", "music", 30 * MIN));
    let cat = catalog(rows);
    let mut evening = shuffle_playlist(2, "Evening", 301..=312, 30, vec![]);
    evening.entries.insert(
        0,
        Entry::Block(start_block(
            40,
            "0 18 * * *",
            Anchor::Absolute,
            StartHow::Segue,
        )),
    );
    let mk = |last: i64| {
        let mut show = seq_playlist(
            3,
            "Show",
            vec![item(401), item(402), item(403), item(404), item(last)],
        );
        show.entries.insert(
            0,
            Entry::Block(start_block(
                41,
                "0 17 * * 1",
                Anchor::Manual,
                StartHow::Segue,
            )),
        );
        prog(
            vec![
                shuffle_playlist(1, "Day", 1..=18, 60, vec![]),
                evening.clone(),
                show,
            ],
            vec![],
        )
    };
    // The show runs from 17:33 and ends 18:03:20 (inside the 5-minute window) or 18:30.
    for (last, expect) in [(405, "fires"), (406, "missed")] {
        let p = mk(last);
        let t0 = local("2026-10-20T17:33:00");
        let mut s = sched(p, 3, t0, 2);
        s.cfg.default_playlist = Some(1);
        let (plays, _, rows) = run_collect(
            &mut s,
            &cat,
            &EffectiveTiming,
            t0,
            local("2026-10-20T19:00:00"),
        );
        let r = rows
            .iter()
            .find(|r| r.block_id == 40)
            .expect("a row for the start");
        let show_end = plays
            .iter()
            .filter(|p| p.item_id() >= 401 && p.item_id() <= 406)
            .map(|p| p.start_ms + cat.meta(p.item_id()).unwrap().effective_ms)
            .max()
            .unwrap();
        println!(
            "6: manual show ends {} ; the 18:00 start is {} ({})",
            zone().local_string(show_end),
            r.how,
            r.detail
        );
        // Nothing of the evening playlist played while the show was on.
        assert!(
            plays
                .iter()
                .filter(|p| p.start_ms < show_end)
                .all(|p| p.item_id() >= 401),
            "the show was interrupted"
        );
        match expect {
            "fires" => {
                assert!(r.fired_ms.is_some(), "{r:?}");
                assert_eq!(
                    r.fired_ms,
                    Some(show_end),
                    "it starts when the program ends"
                );
            }
            _ => assert_eq!(r.how, "missed"),
        }
    }
}

// ---- 6b. preemption ---------------------------------------------------------------------------------------

fn play_block(id: i64, category_id: i64, trigger: Trigger, timed: Option<Timed>) -> Block {
    Block::new(
        id,
        "play",
        BlockKind::Play(PlayBlock::next(Source::Category {
            id: Some(category_id),
            name: None,
        })),
        trigger,
        timed,
    )
}

fn stop_block(id: i64, at_ms: i64, how: StopHow) -> Block {
    Block::new(
        id,
        "stop",
        BlockKind::Stop { how, fade_ms: 2000 },
        Trigger::Timed,
        Some(timed(TimeSpec::relative(at_ms), BoundaryMode::Hard, 0, 0)),
    )
}

#[test]
fn m3_6b_a_timed_stop_acts_at_its_time_and_passes_over_the_in_sequence_blocks() {
    let cat = catalog(office_rows());
    let mut rng = 12345u64;
    let mut worst = 0i64;
    for trial in 0..100 {
        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let at = 300 * SEC + ((rng >> 33) as i64 % (50 * 60 * 1000));
        let osc = Block::new(
            31,
            "mute",
            BlockKind::Other("osc_out".into()),
            Trigger::Sequence,
            None,
        );
        let pl = seq_playlist(
            5,
            "Show",
            vec![
                Entry::Block(play_block(30, 1, Trigger::Sequence, None)),
                Entry::Block(osc),
                Entry::Block(stop_block(32, at, StopHow::Now)),
            ],
        );
        let p = prog(vec![pl], vec![]);
        let t0 = local("2026-10-20T18:45:00") + trial * 997;
        let mut s = sched(p, 5, t0, trial as u64);
        let (plays, acts, rows) =
            run_collect(&mut s, &cat, &EffectiveTiming, t0, t0 + 2 * 3_600_000);
        assert_eq!(acts.len(), 1, "trial {trial}");
        let a = &acts[0];
        assert!(matches!(
            a.act.kind,
            station::timed::ActKind::Stop { fade_ms: 2000 }
        ));
        assert_eq!(a.at_ms, t0 + at);
        worst = worst.max((a.at_ms - (t0 + at)).abs());
        // Nothing starts after the stop; the item on air was mid-track.
        assert!(plays.iter().all(|p| p.start_ms <= t0 + at), "trial {trial}");
        let passed: Vec<_> = rows.iter().filter(|r| r.how == "passed").collect();
        assert_eq!(passed.len(), 1, "{passed:?}");
        assert_eq!(passed[0].block_id, 31);
        assert!(rows.iter().any(|r| r.block_id == 32 && r.how == "fired"));
    }
    println!("6b: 100 random stops: every act at its time (worst error {worst} ms), the in-sequence block logged passed each time");
}

#[test]
fn m3_6b_a_timed_play_reached_early_leaves_the_first_play_going_until_its_time() {
    let mut rows = rows_of(1..=10, 4 * MIN, "alpha1", "music");
    rows.extend(rows_of(51..=60, 4 * MIN, "bravo22", "music"));
    let cat = catalog(rows);
    // alpha1 has category id 106, bravo22 107 (the test catalog's rule).
    let a_id = cat.meta(1).unwrap().category_id;
    let b_id = cat.meta(51).unwrap().category_id;
    assert_ne!(a_id, b_id);
    let timed_play = play_block(
        61,
        b_id,
        Trigger::Timed,
        Some(timed(
            TimeSpec::relative(10 * MIN),
            BoundaryMode::SoftThenHard,
            0,
            30,
        )),
    );
    let pl = seq_playlist(
        7,
        "Two fills",
        vec![
            Entry::Block(play_block(60, a_id, Trigger::Sequence, None)),
            Entry::Block(timed_play),
        ],
    );
    let p = prog(vec![pl], vec![]);
    let t0 = local("2026-10-20T20:00:00");
    let mut s = sched(p, 7, t0, 4);
    let (plays, _, rows) = run_collect(&mut s, &cat, &EffectiveTiming, t0, t0 + 40 * MIN);
    let first_b = plays
        .iter()
        .find(|p| p.item_id() >= 51)
        .expect("the second fill starts");
    let row = rows.iter().find(|r| r.block_id == 61).unwrap();
    println!(
        "6b: the timed play (T0 + 10:00) started at T0 + {:.1} s ({}); {} items of the first fill played before it",
        (first_b.start_ms - t0) as f64 / 1000.0,
        row.how,
        plays.iter().filter(|p| p.item_id() <= 10).count()
    );
    assert!(
        plays
            .iter()
            .filter(|p| p.start_ms < first_b.start_ms)
            .all(|p| p.item_id() <= 10),
        "only the first source played until then"
    );
    assert!(first_b.start_ms >= t0 + 10 * MIN && first_b.start_ms <= t0 + 10 * MIN + 30 * SEC);
    // After it, the new source plays and the old one never returns.
    assert!(plays
        .iter()
        .filter(|p| p.start_ms >= first_b.start_ms)
        .all(|p| p.item_id() >= 51));
}

// ---- 7. a one-shot change ---------------------------------------------------------------------------------

#[test]
fn m3_7_a_one_shot_change_fires_once_and_is_gone_from_upcoming() {
    let mut rows = office_rows();
    rows.extend(rows_of(301..=312, 5 * MIN, "evening", "music"));
    let cat = catalog(rows);
    let p = prog(
        vec![
            shuffle_playlist(1, "Day", 1..=18, 60, vec![]),
            shuffle_playlist(2, "Special", 301..=312, 30, vec![]),
        ],
        vec![],
    );
    let t0 = local("2026-10-20T10:00:00");
    let at = local("2026-10-20T13:30:00");
    let mut s = sched(p, 1, t0, 6);
    let id = s
        .add_oneshot(2, at, StartHow::Segue, BoundaryMode::Soft, 0, 300)
        .unwrap();
    let up = s.upcoming(t0, 12);
    assert_eq!(up.iter().filter(|u| u.target_ms == at).count(), 1);
    let (plays, _, _) = run_timed(
        &mut s,
        &cat,
        &EffectiveTiming,
        t0,
        at + 2 * 3_600_000,
        usize::MAX,
        |_, _, _, _| {},
    );
    let hits: Vec<_> = plays
        .iter()
        .filter(|p| p.entry.timed.as_ref().is_some_and(|t| t.kind == "oneshot"))
        .collect();
    println!(
        "7: one-shot {id} fired {} time(s), {:.0} s after its time",
        hits.len(),
        (hits[0].start_ms - at) as f64 / 1000.0
    );
    assert_eq!(hits.len(), 1);
    assert!(hits[0].start_ms >= at && hits[0].start_ms <= at + 300 * SEC);
    assert!(s.st.oneshots.is_empty());
    let up = s.upcoming(hits[0].start_ms + MIN, 12);
    assert!(up.iter().all(|u| u.target_ms != at && u.block_id != -id));
    assert!(
        plays
            .iter()
            .filter(|p| p.start_ms >= hits[0].start_ms)
            .all(|p| p.item_id() >= 301),
        "the program changed and stayed changed"
    );
    // A cancelled one-shot never fires.
    let mut s2 = sched(
        prog(
            vec![
                shuffle_playlist(1, "Day", 1..=18, 60, vec![]),
                shuffle_playlist(2, "Special", 301..=312, 30, vec![]),
            ],
            vec![],
        ),
        1,
        t0,
        6,
    );
    let id = s2
        .add_oneshot(2, at, StartHow::Segue, BoundaryMode::Soft, 0, 300)
        .unwrap();
    assert!(s2.cancel_oneshot(id));
    let (plays, _, _) = run_timed(
        &mut s2,
        &cat,
        &EffectiveTiming,
        t0,
        at + 3_600_000,
        usize::MAX,
        |_, _, _, _| {},
    );
    assert!(plays.iter().all(|p| p.item_id() < 301));
}

// ---- 8. upcoming events predict the fire time -------------------------------------------------------------------

#[test]
fn m3_8_the_upcoming_api_predicts_the_next_id_within_5_s_ten_minutes_ahead() {
    let cat = catalog(office_rows());
    let p = prog(
        vec![shuffle_playlist(
            1,
            "Office",
            1..=18,
            60,
            vec![liners_every(10, 4)],
        )],
        vec![hourly_id(20)],
    );
    let t0 = local("2026-10-20T12:00:00");
    let mut s = sched(p, 1, t0, 99);
    let mut now = t0;
    let mut errs: Vec<i64> = Vec::new();
    let mut with_prediction = 0;
    let mut total = 0;
    for h in 1..=240i64 {
        let target = t0 + h * 3_600_000;
        let sample = target - 10 * MIN;
        let (_, _, n) = run_timed(
            &mut s,
            &cat,
            &EffectiveTiming,
            now,
            sample,
            usize::MAX,
            |_, _, _, _| {},
        );
        now = n.max(sample);
        let up = s.upcoming(sample, 1);
        let next = up
            .iter()
            .find(|u| u.block_id == 20 && u.target_ms == target);
        let (plays, _, n2) = run_timed(
            &mut s,
            &cat,
            &EffectiveTiming,
            now,
            target + 5 * MIN,
            usize::MAX,
            |_, _, _, _| {},
        );
        now = n2.max(target + 5 * MIN);
        let actual = plays
            .iter()
            .find(|p| {
                p.entry
                    .timed
                    .as_ref()
                    .is_some_and(|t| t.target_ms == target)
            })
            .map(|p| p.start_ms);
        total += 1;
        if let (Some(u), Some(a)) = (next, actual) {
            if u.planned {
                with_prediction += 1;
            }
            errs.push((u.predicted_ms - a).abs());
        }
    }
    let within = errs.iter().filter(|e| **e <= 5 * SEC).count();
    let pct = 100.0 * within as f64 / total as f64;
    println!("8: {total} hours; the prediction made 10 minutes ahead was within 5 s in {within} ({pct:.1} percent); planned entries found {with_prediction}; worst error {} ms", errs.iter().max().unwrap_or(&0));
    assert!(pct >= 95.0, "{pct}");
}

// ---- the owner's case: Eve switches between two rotation playlists at set times, and back ----------------------------

fn ita_programming() -> (Arc<Programming>, Catalog) {
    // "UE Elevator Music" (items 1..=12) and "UE Library Tweener" (items 21..=30 plus 5..=8 of the
    // elevator set, so a track can be in both).
    let mut rows = rows_of(1..=12, 4 * MIN, "elevator", "music");
    rows.extend(rows_of(21..=30, 3 * MIN, "tweener", "music"));
    let cat = catalog(rows);
    let mut elevator = shuffle_playlist(1, "UE Elevator Music", 1..=12, 60, vec![]);
    elevator.entries.insert(
        0,
        Entry::Block(start_block(
            50,
            "0 20 * * *",
            Anchor::Absolute,
            StartHow::Segue,
        )),
    );
    let mut tweener = shuffle_playlist(2, "UE Library Tweener", 21..=30, 30, vec![]);
    for i in 5..=8 {
        tweener.entries.push(item(i));
    }
    tweener.entries.insert(
        0,
        Entry::Block(start_block(
            51,
            "45 18 * * *",
            Anchor::Absolute,
            StartHow::Segue,
        )),
    );
    (prog(vec![elevator, tweener], vec![]), cat)
}

#[test]
fn m3_ita_switches_between_two_rotations_at_set_times_and_back() {
    let (p, cat) = ita_programming();
    let t0 = local("2026-10-19T12:00:00");
    let until = local("2026-10-22T12:00:00");
    let mut s = sched(p, 1, t0, 31);
    let (plays, _, rows) = run_collect(&mut s, &cat, &EffectiveTiming, t0, until);
    let sw: Vec<_> = rows
        .iter()
        .filter(|r| {
            (r.block_id == 50 || r.block_id == 51) && r.fired_ms.is_some() && r.how != "skipped"
        })
        .collect();
    println!("eve: {} switches in 3 days", sw.len());
    // 18:45 -> tweener and 20:00 -> elevator, on 19th, 20th, 21st: 6 switches (the 20:00 on the 19th
    // finds the elevator already running at 12:00).
    assert_eq!(sw.len(), 5 + 1);
    let mut lines = vec![];
    for r in &sw {
        let start = r.fired_ms.unwrap();
        let i = plays
            .iter()
            .position(|p| {
                p.start_ms == start
                    && p.entry
                        .timed
                        .as_ref()
                        .is_some_and(|t| t.block_id == r.block_id)
            })
            .unwrap();
        let prev = &plays[i - 1];
        let ended = prev.start_ms + cat.meta(prev.item_id()).unwrap().effective_ms;
        assert_eq!(
            ended, start,
            "a switch is at a segue boundary: the outgoing track plays out"
        );
        assert_ne!(
            plays[i].item_id(),
            prev.item_id(),
            "the outgoing track is not repeated"
        );
        // Nor any of the outgoing playlist's last tracks inside the new playlist's window.
        let recent: Vec<i64> = plays[..i]
            .iter()
            .rev()
            .take_while(|p| start - p.start_ms < 30 * MIN)
            .map(|p| p.item_id())
            .collect();
        assert!(
            !recent.contains(&plays[i].item_id()),
            "{} played within 30 minutes before",
            plays[i].item_id()
        );
        let incoming_lists = if r.block_id == 51 { 21..=30 } else { 1..=12 };
        let next_switch = sw
            .iter()
            .filter_map(|x| x.fired_ms)
            .filter(|f| *f > start)
            .min()
            .unwrap_or(until);
        let wrong = plays
            .iter()
            .filter(|p| {
                p.start_ms >= start && p.start_ms < next_switch && p.entry.origin != Origin::Insert
            })
            .filter(|p| {
                !incoming_lists.contains(&p.item_id())
                    && !(r.block_id == 51 && (5..=8).contains(&p.item_id()))
            })
            .count();
        assert_eq!(wrong, 0);
        lines.push(format!(
            "{} -> item {} (delay {} s)",
            &zone().local_string(r.target_ms)[..16],
            plays[i].item_id(),
            (start - r.target_ms) / SEC
        ));
    }
    println!("eve: {}", lines.join("; "));
}

#[test]
fn m3_ita_resumes_correctly_after_a_restart_across_the_switch_time() {
    let (p, cat) = ita_programming();
    let cfg = || SchedConfig {
        zone: zone(),
        ..SchedConfig::default()
    };
    let t0 = local("2026-10-19T17:00:00");
    // Reference: no restart.
    let mut s = sched(p.clone(), 1, t0, 31);
    let (ref_plays, _, _) = run_collect(
        &mut s,
        &cat,
        &EffectiveTiming,
        t0,
        local("2026-10-19T23:00:00"),
    );
    let ref_switch: Vec<(i64, i64)> = ref_plays
        .iter()
        .filter(|p| p.entry.timed.is_some())
        .map(|p| (p.start_ms, p.item_id()))
        .collect();
    assert_eq!(ref_switch.len(), 2);
    // Restart at three moments: before the 18:45 start is placed, while it is planned in the queue, and
    // after it began; and once more around 20:00. Each time the whole state goes through JSON, as it
    // does through state.db, and a new scheduler is built from it.
    for restart_at in [
        "2026-10-19T18:20:00",
        "2026-10-19T18:42:00",
        "2026-10-19T18:50:00",
        "2026-10-19T19:59:00",
        "2026-10-19T20:02:00",
    ] {
        let at = local(restart_at);
        let mut s = sched(p.clone(), 1, t0, 31);
        let (_, _, now) = run_timed(
            &mut s,
            &cat,
            &EffectiveTiming,
            t0,
            at,
            usize::MAX,
            |_, _, _, _| {},
        );
        let json = serde_json::to_string(&s.st).unwrap();
        let st: SchedState = serde_json::from_str(&json).unwrap();
        let mut s2 = Scheduler::restore(cfg(), p.clone(), st);
        // The station, on start, finds what was on air over and deals what is queued.
        let (plays, _, _) = run_timed(
            &mut s2,
            &cat,
            &EffectiveTiming,
            now.max(at),
            local("2026-10-19T23:00:00"),
            usize::MAX,
            |_, _, _, _| {},
        );
        let sw: Vec<(i64, i64)> = plays
            .iter()
            .filter(|p| p.entry.timed.is_some())
            .map(|p| (p.start_ms, p.item_id()))
            .collect();
        let logged: Vec<_> = s2
            .st
            .log
            .rows
            .iter()
            .filter(|r| {
                (r.block_id == 50 || r.block_id == 51) && r.fired_ms.is_some() && r.how != "skipped"
            })
            .collect();
        println!(
            "eve: restart at {restart_at}: switches after it {:?}; logged {}",
            sw.iter()
                .map(|(t, i)| (zone().local_string(*t)[11..19].to_string(), *i))
                .collect::<Vec<_>>(),
            logged.len()
        );
        // Every switch happened once; the same two as without a restart, at the same times.
        let all: Vec<i64> = logged.iter().map(|r| r.fired_ms.unwrap()).collect();
        let want: Vec<i64> = ref_switch.iter().map(|(t, _)| *t).collect();
        assert_eq!(all, want, "restart at {restart_at}");
        // After the second switch only the elevator plays; between them only the tweener.
        let between: Vec<_> = plays
            .iter()
            .filter(|p| {
                p.start_ms >= want[0] && p.start_ms < want[1] && p.entry.origin != Origin::Insert
            })
            .collect();
        assert!(
            between
                .iter()
                .all(|p| p.item_id() >= 21 || (5..=8).contains(&p.item_id())),
            "restart at {restart_at}"
        );
    }
}
