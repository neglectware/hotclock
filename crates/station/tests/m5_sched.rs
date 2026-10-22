//! M5 acceptance tests of the block engine that need no audio: the Monday Eve program, waits, loops,
//! program actions, nesting, back-timing. Virtual clock, `station::sim`.

#[path = "support_m5/mod.rs"]
mod common_m5;
use common_m5::*;
use station::sched::*;
use station::sim::run_timed;
use station::timed::ActKind;

#[test]
fn ita_monday_blocks_fire_at_their_times_with_the_button_press_as_t0() {
    let (p, _) = ita_prog();
    let cat = ita_cat();
    for shift in [0, 15] {
        let t0 = local("2027-01-18T18:45:00") + shift * SEC;
        let mut s = sched_for(p.clone(), MONDAY, t0, 7);
        let until = local("2027-01-19T01:00:00");
        let (plays, acts, _) = run_timed(
            &mut s,
            &cat,
            &EffectiveTiming,
            t0,
            until,
            usize::MAX,
            |_, _, _, _| {},
        );
        let z = zone();
        for a in &acts {
            println!("{}  {:?}", z.local_string(a.at_ms), a.act.kind);
        }
        let first = &plays[0];
        assert_eq!(first.start_ms, t0, "the first item starts at the press");
        assert!(plays
            .iter()
            .take_while(|p| p.start_ms < t0 + 880 * SEC)
            .all(|p| p.item_id() <= 6));
        let fade = acts
            .iter()
            .find(|a| matches!(a.act.kind, ActKind::FadeStop { .. }))
            .expect("fade");
        assert_eq!(fade.act.target_ms, t0 + 880 * SEC);
        let osc = acts
            .iter()
            .find(|a| matches!(a.act.kind, ActKind::Side(_)))
            .expect("osc");
        assert_eq!(osc.act.target_ms, t0 + 900 * SEC);
        let elevator = plays.iter().find(|p| p.item_id() >= 11).expect("elevator");
        assert_eq!(
            elevator.start_ms,
            t0 + 901 * SEC,
            "the elevator bed starts at T0 + 15:01"
        );
        let stop = acts
            .iter()
            .find(|a| matches!(a.act.kind, ActKind::Stop { .. }))
            .expect("stop");
        assert_eq!(
            stop.act.target_ms,
            local("2027-01-19T00:30:00"),
            "the stop does not move with T0"
        );
        // Nothing plays after the stop.
        assert!(plays.iter().all(|p| p.start_ms < stop.act.target_ms));
    }
}

// ---- helpers -------------------------------------------------------------------------------------------

use serde_json::json;
use station::programming::*;

fn run(
    p: std::sync::Arc<Programming>,
    pl: i64,
    t0: i64,
    until: i64,
    seed: u64,
) -> (
    Scheduler,
    Vec<station::sim::SimPlay>,
    Vec<station::sim::SimAct>,
) {
    let cat = ita_cat();
    let mut s = sched_for(p, pl, t0, seed);
    let (plays, acts, _) = run_timed(
        &mut s,
        &cat,
        &EffectiveTiming,
        t0,
        until,
        usize::MAX,
        |_, _, _, _| {},
    );
    (s, plays, acts)
}

fn rows_of<'a>(s: &'a Scheduler, name: &str) -> Vec<&'a station::timed::LogRow> {
    s.st.log.rows.iter().filter(|r| r.name == name).collect()
}

fn t0() -> i64 {
    local("2027-01-18T18:45:00")
}

fn rel(ms: i64) -> serde_json::Value {
    json!({"kind": "relative", "offset_ms": ms})
}

// ---- 9. the block engine -------------------------------------------------------------------------------

#[test]
fn engine_a_timed_flow_block_preempts_a_running_play_and_passes_over_the_steps_between() {
    // [play pre] [osc in sequence] [timed fade at +60 s] [stop timed at +300 s]: the fade preempts the play at
    // its time; the in-sequence osc block that lay between is "passed" only if the cursor had not reached it.
    let blocks = vec![
        block(
            1,
            "Music",
            "play",
            "sequence",
            json!({"source": {"playlist_id": PRE}, "until": "forever"}),
        ),
        block(
            2,
            "Mute",
            "osc_out",
            "sequence",
            json!({"host": "127.0.0.1", "port": 9, "address": "/x", "args": []}),
        ),
        block(
            3,
            "Fade",
            "fade_stop",
            "timed",
            json!({"when": rel(60_000), "seconds": 7}),
        ),
    ];
    let p = prog(
        vec![
            shuffle(PRE, "pre", 1..=6),
            sequential(MONDAY, "Show", blocks),
        ],
        vec![],
    );
    let (s, plays, acts) = run(p, MONDAY, t0(), t0() + 20 * MIN, 1);
    // `forever` holds the cursor at the play, so the osc block in sequence is never reached; the timed fade
    // preempts at its time and passes over it.
    assert!(acts
        .iter()
        .any(|a| matches!(a.act.kind, ActKind::FadeStop { .. })
            && a.act.target_ms == t0() + 60 * SEC));
    assert!(
        acts.iter().all(|a| !matches!(a.act.kind, ActKind::Side(_))),
        "the osc block never ran"
    );
    let passed: Vec<_> = rows_of(&s, "Mute");
    assert_eq!(passed.len(), 1, "{passed:?}");
    assert_eq!(passed[0].how, "passed");
    // Nothing plays after the fade.
    assert!(plays.iter().all(|p| p.start_ms < t0() + 60 * SEC));
}

#[test]
fn engine_a_timed_flow_block_reached_early_waits_while_the_preceding_play_keeps_playing() {
    let blocks = vec![
        block(
            1,
            "Music",
            "play",
            "sequence",
            json!({"source": {"playlist_id": PRE}, "until": "next"}),
        ),
        block(
            2,
            "Later",
            "play",
            "timed",
            json!({"when": rel(10 * 60_000), "source": {"playlist_id": BED}, "until": "next"}),
        ),
    ];
    let p = prog(
        vec![
            shuffle(PRE, "pre", 1..=6),
            shuffle(BED, "bed", 11..=15),
            sequential(MONDAY, "Show", blocks),
        ],
        vec![],
    );
    let (_, plays, _) = run(p, MONDAY, t0(), t0() + 14 * MIN, 2);
    let first_bed = plays
        .iter()
        .find(|p| p.item_id() >= 11)
        .expect("the bed started");
    let t = first_bed.start_ms - t0();
    println!(
        "the second play started {} s after T0 (window 600 to 630 s)",
        t / SEC
    );
    assert!((600 * SEC..=630 * SEC).contains(&t));
    // Before it only the first source played.
    assert!(plays
        .iter()
        .take_while(|p| p.item_id() < 11)
        .all(|p| p.item_id() <= 6));
    assert!(
        plays
            .iter()
            .filter(|p| p.start_ms < first_bed.start_ms)
            .count()
            >= 3
    );
}

#[test]
fn engine_a_timed_side_block_never_moves_the_cursor() {
    let blocks = vec![
        block(
            1,
            "Music",
            "play",
            "sequence",
            json!({"source": {"playlist_id": PRE}, "until": "next"}),
        ),
        block(
            2,
            "Osc",
            "osc_out",
            "timed",
            json!({"when": rel(90_000), "host": "127.0.0.1", "port": 9, "address": "/x", "args": []}),
        ),
        block(
            3,
            "Stop",
            "stop",
            "timed",
            json!({"when": rel(5 * 60_000), "how": "now", "fade_ms": 1000}),
        ),
    ];
    let p = prog(
        vec![
            shuffle(PRE, "pre", 1..=6),
            sequential(MONDAY, "Show", blocks),
        ],
        vec![],
    );
    let cat = ita_cat();
    let mut s = sched_for(p, MONDAY, t0(), 3);
    let (plays, acts, _) = run_timed(
        &mut s,
        &cat,
        &EffectiveTiming,
        t0(),
        t0() + 8 * MIN,
        usize::MAX,
        |sc, _, _, now| {
            if now > t0() + 100 * SEC && now < t0() + 110 * SEC {
                // The osc block fired and the cursor still stands at the stop.
                let pr = sc.st.program.as_ref().unwrap();
                assert_eq!(
                    pr.seq_pos, 2,
                    "the cursor stands at the timed stop, not past the osc block"
                );
            }
        },
    );
    assert!(acts
        .iter()
        .any(|a| matches!(a.act.kind, ActKind::Side(_)) && a.act.target_ms == t0() + 90 * SEC));
    assert!(plays.len() >= 2);
}

#[test]
fn engine_wait_for_the_operator_holds_until_continue_and_the_music_goes_on() {
    let blocks = vec![
        block(
            1,
            "Music",
            "play",
            "sequence",
            json!({"source": {"playlist_id": PRE}, "until": "next"}),
        ),
        block(2, "Hold", "wait", "sequence", json!({"until": "operator"})),
        block(
            3,
            "Then",
            "osc_out",
            "sequence",
            json!({"host": "127.0.0.1", "port": 9, "address": "/go", "args": []}),
        ),
        block(4, "End", "end", "sequence", json!({})),
    ];
    let p = prog(
        vec![
            shuffle(PRE, "pre", 1..=6),
            sequential(MONDAY, "Show", blocks),
        ],
        vec![],
    );
    let cat = ita_cat();
    let mut s = sched_for(p, MONDAY, t0(), 4);
    let mut released_at = None;
    let (plays, acts, _) = run_timed(
        &mut s,
        &cat,
        &EffectiveTiming,
        t0(),
        t0() + 30 * MIN,
        usize::MAX,
        |sc, _, _, now| {
            if now >= t0() + 12 * MIN && released_at.is_none() {
                assert!(
                    sc.waiting_for_operator().is_some(),
                    "waiting for the operator at {now}"
                );
                sc.continue_program(now, &ita_cat(), &EffectiveTiming)
                    .unwrap();
                released_at = Some(now);
            }
        },
    );
    let r = released_at.expect("released");
    println!(
        "released at T0 + {} s; plays {}",
        (r - t0()) / SEC,
        plays.len()
    );
    // Music played through the wait: items up to at least the release, with no gap longer than an item.
    assert!(plays.iter().filter(|p| p.start_ms < r).count() >= 3);
    let osc = acts
        .iter()
        .find(|a| matches!(a.act.kind, ActKind::Side(_)))
        .expect("the block after the wait ran");
    assert!(
        osc.act.target_ms >= r,
        "nothing after the wait ran before Continue"
    );
}

#[test]
fn engine_wait_for_a_duration_and_a_time() {
    for until in [json!({"duration_s": 120}), json!({"at": rel(120_000)})] {
        let blocks = vec![
            block(
                1,
                "Music",
                "play",
                "sequence",
                json!({"source": {"playlist_id": PRE}, "until": "next"}),
            ),
            block(2, "Hold", "wait", "sequence", json!({"until": until})),
            block(
                3,
                "Then",
                "osc_out",
                "sequence",
                json!({"host": "127.0.0.1", "port": 9, "address": "/go", "args": []}),
            ),
        ];
        let p = prog(
            vec![
                shuffle(PRE, "pre", 1..=6),
                sequential(MONDAY, "Show", blocks),
            ],
            vec![],
        );
        let (_, _, acts) = run(p, MONDAY, t0(), t0() + 10 * MIN, 5);
        let osc = acts
            .iter()
            .find(|a| matches!(a.act.kind, ActKind::Side(_)))
            .expect("ran");
        // The act waits for the boundary where the previous step ended: the first one at or after 120 s.
        let d = (osc.act.target_ms - t0()) / SEC;
        println!("wait {until}: the next block acted at T0 + {d} s");
        assert!((120..=330).contains(&d), "{d}");
    }
}

#[test]
fn engine_a_loop_of_an_insert_and_a_counted_play_runs_ten_times_and_moves_on() {
    // [insert liner] [play pre count 3] [loop times 10] [osc]: 10 passes of one liner and three songs.
    let mut rows = ita_rows();
    rows.push(station::catalog::testing::row(
        101,
        "Liner",
        "Station",
        "liner",
        "liner",
        8 * SEC,
    ));
    let cat = station::catalog::testing::catalog(rows);
    let blocks = vec![
        block(
            1,
            "Liner",
            "insert",
            "sequence",
            json!({"source": {"item_id": 101}}),
        ),
        block(
            2,
            "Three",
            "play",
            "sequence",
            json!({"source": {"playlist_id": PRE}, "until": {"count": 3}}),
        ),
        block(3, "Again", "loop", "sequence", json!({"times": 10})),
        block(
            4,
            "Done",
            "osc_out",
            "sequence",
            json!({"host": "127.0.0.1", "port": 9, "address": "/done", "args": []}),
        ),
    ];
    let p = prog(
        vec![
            shuffle(PRE, "pre", 1..=6),
            sequential(MONDAY, "Show", blocks),
        ],
        vec![],
    );
    let mut s = sched_for(p, MONDAY, t0(), 6);
    let (plays, acts, _) = run_timed(
        &mut s,
        &cat,
        &EffectiveTiming,
        t0(),
        t0() + 12 * 3600 * SEC,
        200,
        |_, _, _, _| {},
    );
    let liners = plays.iter().filter(|p| p.item_id() == 101).count();
    let songs = plays.iter().filter(|p| p.item_id() <= 6).count();
    println!(
        "loop: {liners} liners, {songs} songs, {} act(s)",
        acts.len()
    );
    // The done message is made after the tenth pass; the plays before it are 10 x (1 + 3).
    let done_at = acts
        .iter()
        .find(|a| matches!(a.act.kind, ActKind::Side(_)))
        .map(|a| a.act.target_ms)
        .expect("done");
    let before: Vec<_> = plays.iter().filter(|p| p.start_ms < done_at).collect();
    assert_eq!(before.iter().filter(|p| p.item_id() == 101).count(), 10);
    assert_eq!(before.iter().filter(|p| p.item_id() <= 6).count(), 30);
}

// ---- nesting ---------------------------------------------------------------------------------------------

/// Item 1 plays 200 s... use the catalog's lengths: pre items are 210..260 s (id * 10 + 200).
fn len_of(id: i64) -> i64 {
    (200 + id * 10) * SEC
}

#[test]
fn engine_a_nested_playlists_timed_blocks_count_from_its_own_start_and_lose_to_the_parents() {
    // Parent: item 1, then a nested play of CHILD; a timed stop of the parent at the same wall time as the
    // child's timed fade (its own start + 60 s). The parent's block wins; the child's never acts.
    let child_blocks = vec![
        block(
            11,
            "Child music",
            "play",
            "sequence",
            json!({"source": {"playlist_id": BED}, "until": "next"}),
        ),
        block(
            12,
            "Child osc",
            "osc_out",
            "timed",
            json!({"when": rel(30_000), "host": "127.0.0.1", "port": 9, "address": "/child", "args": []}),
        ),
        block(
            13,
            "Child fade",
            "fade_stop",
            "timed",
            json!({"when": rel(60_000), "seconds": 3}),
        ),
    ];
    let child_start = len_of(1); // item 1 plays alone before the nested playlist (no overlap in this timing)
    let parent_blocks = vec![Block::new(
        1,
        "unused",
        BlockKind::End,
        Trigger::Sequence,
        None,
    )];
    let _ = parent_blocks;
    let mut parent = sequential(
        MONDAY,
        "Parent",
        vec![
            block(
                21,
                "Nested",
                "play",
                "sequence",
                json!({"source": {"playlist_id": 7}, "until": "forever"}),
            ),
            block(
                22,
                "Parent stop",
                "stop",
                "timed",
                json!({"when": rel(child_start + 60_000), "how": "now", "fade_ms": 1000}),
            ),
        ],
    );
    parent.entries.insert(
        0,
        Entry::Item {
            item_id: 1,
            weight: 1.0,
        },
    );
    let p = prog(
        vec![
            shuffle(BED, "bed", 11..=15),
            sequential(7, "Child", child_blocks),
            parent,
        ],
        vec![],
    );
    let (_, plays, acts) = run(p, MONDAY, t0(), t0() + 30 * MIN, 8);
    let z = zone();
    for a in &acts {
        println!(
            "{}  {} {:?}",
            z.local_string(a.at_ms),
            a.act.name,
            a.act.kind
        );
    }
    // The child's osc is 30 s after the child starts (item 1 ends), not 30 s after T0.
    let osc = acts
        .iter()
        .find(|a| a.act.name == "Child osc")
        .expect("child osc");
    assert_eq!(osc.act.target_ms, t0() + child_start + 30 * SEC);
    // At the shared moment the parent's stop acts and the child's fade does not.
    let stop = acts
        .iter()
        .find(|a| a.act.name == "Parent stop")
        .expect("parent stop");
    assert_eq!(stop.act.target_ms, t0() + child_start + 60 * SEC);
    assert!(
        acts.iter().all(|a| a.act.name != "Child fade"),
        "the child's block lost"
    );
    assert!(plays
        .iter()
        .all(|p| p.start_ms < t0() + child_start + 60 * SEC));
}

#[test]
fn engine_nesting_four_deep_runs_and_a_fifth_level_and_a_cycle_are_refused() {
    // L1 plays L2 plays L3 plays L4 (four levels: allowed), L4 plays a shuffle.
    let mk = |id: i64, child: i64| {
        sequential(
            id,
            &format!("L{id}"),
            vec![block(
                id * 10,
                "go",
                "play",
                "sequence",
                json!({"source": {"playlist_id": child}, "until": "forever"}),
            )],
        )
    };
    let p = prog(
        vec![
            mk(1, 2),
            mk(2, 3),
            mk(3, 4),
            mk(4, PRE),
            shuffle(PRE, "pre", 1..=6),
        ],
        vec![],
    );
    let problems = p.validate();
    assert!(
        problems.iter().all(|x| !x.error),
        "four levels are fine: {problems:?}"
    );
    let (_, plays, _) = run(p.clone(), 1, t0(), t0() + 15 * MIN, 9);
    assert!(!plays.is_empty(), "a program four deep runs");
    // A fifth level is refused.
    let p5 = prog(
        vec![
            mk(1, 2),
            mk(2, 3),
            mk(3, 4),
            mk(4, 5),
            mk(5, PRE),
            shuffle(PRE, "pre", 1..=6),
        ],
        vec![],
    );
    let errs: Vec<_> = p5.validate().into_iter().filter(|x| x.error).collect();
    assert!(errs.iter().any(|e| e.code == "nesting_depth"), "{errs:?}");
    // A cycle is refused.
    let cyc = prog(vec![mk(1, 2), mk(2, 1)], vec![]);
    let errs: Vec<_> = cyc.validate().into_iter().filter(|x| x.error).collect();
    assert!(errs.iter().any(|e| e.code == "nesting_cycle"), "{errs:?}");
}

#[test]
fn engine_start_playlist_replaces_the_program_and_cancels_its_pending_blocks() {
    // The program has a timed osc at +10 min; at +2 min a timed start_playlist replaces it with BED: the osc
    // belonged to the replaced program and never acts.
    let blocks = vec![
        block(
            1,
            "Music",
            "play",
            "sequence",
            json!({"source": {"playlist_id": PRE}, "until": "next"}),
        ),
        block(
            2,
            "Switch",
            "start_playlist",
            "timed",
            json!({"when": rel(120_000), "playlist_id": BED, "how": "segue"}),
        ),
        block(
            3,
            "Late osc",
            "osc_out",
            "timed",
            json!({"when": rel(10 * 60_000), "host": "127.0.0.1", "port": 9, "address": "/late", "args": []}),
        ),
    ];
    let p = prog(
        vec![
            shuffle(PRE, "pre", 1..=6),
            shuffle(BED, "bed", 11..=15),
            sequential(MONDAY, "Show", blocks),
        ],
        vec![],
    );
    let (s, plays, acts) = run(p, MONDAY, t0(), t0() + 20 * MIN, 10);
    assert!(
        acts.iter().all(|a| a.act.name != "Late osc"),
        "the replaced program's block never fired: {acts:?}"
    );
    let first_bed = plays.iter().find(|p| p.item_id() >= 11).expect("bed");
    assert!(
        first_bed.start_ms >= t0() + 120 * SEC
            && first_bed.start_ms <= t0() + 120 * SEC + 330 * SEC
    );
    assert_eq!(s.st.program.as_ref().unwrap().playlist_id, BED);
    assert!(plays
        .iter()
        .filter(|p| p.start_ms > first_bed.start_ms)
        .all(|p| p.item_id() >= 11));
}

#[test]
fn engine_a_loop_with_a_body_that_takes_no_time_is_refused() {
    let blocks = vec![
        block(
            1,
            "Osc",
            "osc_out",
            "sequence",
            json!({"host": "127.0.0.1", "port": 9, "address": "/x", "args": []}),
        ),
        block(2, "Spin", "loop", "sequence", json!({"times": "forever"})),
    ];
    let p = prog(vec![sequential(MONDAY, "Spin", blocks)], vec![]);
    let errs: Vec<_> = p.validate().into_iter().filter(|x| x.error).collect();
    assert!(errs.iter().any(|e| e.code == "loop_no_time"), "{errs:?}");
}

// ---- program actions ---------------------------------------------------------------------------------------

#[test]
fn actions_fire_skip_abort_and_the_late_start_policy() {
    let (p, _) = ita_prog();
    let cat = ita_cat();
    // fire: the osc block now.
    let mut s = sched_for(p.clone(), MONDAY, t0(), 11);
    s.fill(t0(), &cat, &EffectiveTiming);
    let acts = s
        .fire_block(103, t0() + 30 * SEC, &cat, &EffectiveTiming)
        .expect("fire");
    assert!(matches!(acts[0].kind, ActKind::Side(_)));
    assert_eq!(acts[0].target_ms, t0() + 30 * SEC);
    // It does not fire again at its own time.
    let (_, acts2) = {
        let (plays, acts, _) = run_timed(
            &mut s,
            &cat,
            &EffectiveTiming,
            t0() + 30 * SEC,
            t0() + 20 * MIN,
            usize::MAX,
            |_, _, _, _| {},
        );
        (plays, acts)
    };
    assert!(
        acts2
            .iter()
            .all(|a| !matches!(a.act.kind, ActKind::Side(_))),
        "fired by hand, not again"
    );
    // skip: the next timed flow block (the fade) is skipped and logged.
    let mut s = sched_for(p.clone(), MONDAY, t0(), 12);
    s.fill(t0(), &cat, &EffectiveTiming);
    let name = s.skip_next_block(t0() + 10 * SEC).expect("skip");
    assert_eq!(name, "Fade out");
    let (_, acts, _) = run_timed(
        &mut s,
        &cat,
        &EffectiveTiming,
        t0() + 10 * SEC,
        t0() + 20 * MIN,
        usize::MAX,
        |_, _, _, _| {},
    );
    assert!(
        acts.iter()
            .all(|a| !matches!(a.act.kind, ActKind::FadeStop { .. })),
        "the skipped fade did not act"
    );
    assert!(rows_of(&s, "Fade out").iter().any(|r| r.how == "skipped"));
    // abort: the program ends; no later block fires.
    let mut s = sched_for(p.clone(), MONDAY, t0(), 13);
    s.cfg.default_playlist = Some(PRE);
    s.fill(t0(), &cat, &EffectiveTiming);
    s.abort_program(t0() + 60 * SEC, &cat, &EffectiveTiming)
        .expect("abort");
    let (_, acts, _) = run_timed(
        &mut s,
        &cat,
        &EffectiveTiming,
        t0() + 60 * SEC,
        t0() + 20 * MIN,
        usize::MAX,
        |_, _, _, _| {},
    );
    assert!(acts
        .iter()
        .all(|a| !matches!(a.act.kind, ActKind::FadeStop { .. } | ActKind::Side(_))));
    assert_eq!(s.st.program.as_ref().unwrap().playlist_id, PRE);
}

#[test]
fn actions_a_late_join_runs_only_what_its_policy_allows() {
    // Joined with T0 = nominal: at 18:50 nothing is past; at 19:00:30 the fade (a stopping block) is skipped,
    // the mute (30 s late, inside its 60 s window) and the bed (29 s late) run.
    let (p, ex) = ita_prog();
    let _ = ex;
    let z = zone();
    let nominal = local("2027-01-18T18:45:00");
    let pv = station::preview::dry_run(&p, MONDAY, nominal, Some(local("2027-01-18T18:50:00")), &z)
        .unwrap();
    assert!(
        pv.past.is_empty(),
        "nothing is past at 18:50: {:?}",
        pv.past
    );
    let pv = station::preview::dry_run(&p, MONDAY, nominal, Some(local("2027-01-18T19:00:30")), &z)
        .unwrap();
    let by_name = |n: &str| {
        pv.past
            .iter()
            .find(|x| x.name == n)
            .unwrap_or_else(|| panic!("{n}: {:?}", pv.past))
    };
    assert_eq!(by_name("Fade out").action, "skip");
    assert_eq!(by_name("Mute the Wing").action, "run");
    assert_eq!(by_name("Elevator bed").action, "run");
    // And the scheduler does what the dry run says: started at 19:00:30 with T0 nominal.
    let cat = ita_cat();
    let mut s = sched_for(p.clone(), MONDAY, nominal, 14);
    let start = local("2027-01-18T19:00:30");
    let (plays, acts, _) = run_timed(
        &mut s,
        &cat,
        &EffectiveTiming,
        start,
        start + 10 * MIN,
        usize::MAX,
        |_, _, _, _| {},
    );
    assert!(
        acts.iter().any(|a| matches!(a.act.kind, ActKind::Side(_))),
        "the mute ran"
    );
    assert!(
        acts.iter()
            .all(|a| !matches!(a.act.kind, ActKind::FadeStop { .. })),
        "the fade was skipped"
    );
    assert!(plays.iter().any(|p| p.item_id() >= 11), "the bed started");
    let missed: Vec<_> = rows_of(&s, "Fade out");
    assert!(missed.iter().any(|r| r.how == "missed"), "{missed:?}");
}

#[test]
fn actions_the_stop_in_the_past_is_skipped_for_a_late_night_start() {
    // Started at 00:45 with the stop written for 00:30 on the same day: past at the start, late policy skip.
    let blocks = vec![
        block(
            1,
            "Music",
            "play",
            "sequence",
            json!({"source": {"playlist_id": PRE}, "until": "next"}),
        ),
        block(
            2,
            "Stop",
            "stop",
            "timed",
            json!({"when": {"kind": "local", "at": "00:30", "day_offset": 0}, "how": "now", "fade_ms": 3000, "late_policy": "skip"}),
        ),
    ];
    let p = prog(
        vec![
            shuffle(PRE, "pre", 1..=6),
            sequential(MONDAY, "Show", blocks),
        ],
        vec![],
    );
    let start = local("2027-01-19T00:45:00");
    let cat = ita_cat();
    let mut s = sched_for(p, MONDAY, start, 15);
    let (plays, acts, _) = run_timed(
        &mut s,
        &cat,
        &EffectiveTiming,
        start,
        start + 30 * MIN,
        usize::MAX,
        |_, _, _, _| {},
    );
    assert!(acts
        .iter()
        .all(|a| !matches!(a.act.kind, ActKind::Stop { .. })));
    assert!(plays.len() >= 6, "the music keeps playing: {}", plays.len());
}

#[test]
fn actions_expected_arm_and_the_fallback_start() {
    // A manual start block expects Mondays at 18:45. A hand start at 18:46 consumes the occurrence; with the
    // fallback enabled and nobody pressing, the program starts itself at nominal + grace.
    let (p, _) = ita_prog();
    let mut s = sched_for(p, 99, 0, 1); // a program that is not the Monday one
    s.st.program = None;
    let e = s.expected(local("2027-01-18T12:00:00")).expect("expected");
    assert_eq!(e.at_ms, local("2027-01-18T18:45:00"));
    assert!(e.manual && !e.armed);
    // An hour late the occurrence is still offered (up to arm_window_min after its time); after that the next Monday is.
    let e = s
        .expected(local("2027-01-18T19:30:00"))
        .expect("still expected");
    assert_eq!(e.at_ms, local("2027-01-18T18:45:00"));
    let e = s.expected(local("2027-01-18T20:00:00")).expect("next week");
    assert_eq!(e.at_ms, local("2027-01-25T18:45:00"));
    // A hand start consumes it.
    s.consume_occurrences(MONDAY, local("2027-01-18T18:46:00"));
    let e = s.expected(local("2027-01-18T18:47:00")).expect("next");
    assert_eq!(e.at_ms, local("2027-01-25T18:45:00"));
    // The fallback: a block that enables it, armed by default; disarming turns it off for that occurrence.
    let blk = block(
        300,
        "Expected",
        "start",
        "first",
        json!({"schedule": [{"cron": "45 18 * * 1"}], "anchor": "manual", "fallback_start": {"enabled": true, "grace_s": 120}}),
    );
    let mut pl = sequential(
        MONDAY,
        "Eve",
        vec![
            blk,
            block(
                301,
                "m",
                "play",
                "sequence",
                json!({"source": {"playlist_id": PRE}}),
            ),
        ],
    );
    pl.station_blocks = false;
    let p2 = prog(vec![shuffle(PRE, "pre", 1..=6), pl], vec![]);
    let mut s = sched_for(p2.clone(), PRE, 0, 1);
    s.st.program = None;
    assert!(
        s.fallback_due(local("2027-01-18T18:46:00")).is_none(),
        "not before the grace"
    );
    let due = s
        .fallback_due(local("2027-01-18T18:47:30"))
        .expect("due after the grace");
    assert_eq!((due.0, due.2), (MONDAY, local("2027-01-18T18:45:00")));
    s.set_armed(MONDAY, Some(due.2), false, local("2027-01-18T18:40:00"))
        .unwrap();
    assert!(
        s.fallback_due(local("2027-01-18T18:47:30")).is_none(),
        "disarmed"
    );
    s.set_armed(MONDAY, Some(due.2), true, local("2027-01-18T18:40:00"))
        .unwrap();
    s.mark_fallback_fired(due.1, due.2);
    assert!(
        s.fallback_due(local("2027-01-18T18:48:00")).is_none(),
        "fired once"
    );
    // Arming a block that does not enable the fallback is refused.
    let (p, _) = ita_prog();
    let mut s3 = sched_for(p, PRE, 0, 1);
    s3.st.program = None;
    assert!(s3
        .set_armed(MONDAY, None, true, local("2027-01-18T12:00:00"))
        .is_err());
}

#[test]
fn engine_a_play_that_ends_naturally_hands_over_to_start_playlist_with_a_normal_segue() {
    // Acceptance 3: back-timed play to T0 + 10 min, then start_playlist(elevator, segue).
    let blocks = vec![
        block(
            1,
            "Music",
            "play",
            "sequence",
            json!({
                "source": {"playlist_id": PRE}, "until": {"at": rel(10 * 60_000)}, "end": "natural",
                "back_time": {"mode": "best_fit", "tolerance_s": 45}
            }),
        ),
        block(
            2,
            "Then",
            "start_playlist",
            "sequence",
            json!({"playlist_id": BED, "how": "segue"}),
        ),
    ];
    let p = prog(
        vec![
            shuffle(PRE, "pre", 1..=6),
            shuffle(BED, "bed", 11..=15),
            sequential(MONDAY, "Show", blocks),
        ],
        vec![],
    );
    let (s, plays, _) = run(p, MONDAY, t0(), t0() + 20 * MIN, 16);
    for p in &plays {
        println!(
            "  T0+{:>4} s item {} {:?} flags {:?}",
            (p.start_ms - t0()) / SEC,
            p.item_id(),
            p.entry.origin,
            p.entry.flags
        );
    }
    let first_bed = plays
        .iter()
        .find(|p| p.item_id() >= 11)
        .expect("bed started");
    let last_pre = plays.iter().rfind(|p| p.item_id() <= 6).unwrap();
    let end = last_pre.start_ms + ita_cat().meta(last_pre.item_id()).unwrap().play_ms;
    println!(
        "the last pre-show item ends at T0 + {} s, the bed starts at T0 + {} s",
        (end - t0()) / SEC,
        (first_bed.start_ms - t0()) / SEC
    );
    assert!((end - (t0() + 600 * SEC)).abs() <= 45 * SEC);
    // A normal segue: the bed starts at the pre-show item's boundary (its effective length), no gap, no cut.
    assert_eq!(
        first_bed.start_ms,
        last_pre.start_ms + ita_cat().meta(last_pre.item_id()).unwrap().effective_ms
    );
    assert_eq!(s.st.program.as_ref().unwrap().playlist_id, BED);
}

#[test]
fn end_at_makes_a_one_off_program_that_ends_a_song_on_the_time_and_hands_over() {
    use station::program::EndThen;
    for mode_sequential in [false, true] {
        let mut pre = shuffle(PRE, "pre", 1..=6);
        if mode_sequential {
            pre.mode = library::playlists::Mode::Sequential;
        }
        let p = prog(vec![pre, shuffle(BED, "bed", 11..=15)], vec![]);
        let cat = ita_cat();
        let mut s = sched_for(p, PRE, t0() - 3600 * SEC, 77);
        let at = t0() + 12 * MIN;
        let id = s
            .make_end_at(PRE, at, EndThen::Playlist(BED))
            .expect("end-at");
        s.switch_with(
            id,
            SwitchHow::Now,
            t0(),
            t0(),
            "api",
            None,
            &cat,
            &EffectiveTiming,
        )
        .expect("switch");
        let (plays, _, _) = run_timed(
            &mut s,
            &cat,
            &EffectiveTiming,
            t0(),
            t0() + 25 * MIN,
            usize::MAX,
            |_, _, _, _| {},
        );
        let last_pre = plays
            .iter()
            .rfind(|p| p.item_id() <= 6)
            .expect("pre-show items");
        let end = last_pre.start_ms + cat.meta(last_pre.item_id()).unwrap().play_ms;
        let first_bed = plays.iter().find(|p| p.item_id() >= 11).expect("bed");
        println!("sequential={mode_sequential}: last pre-show item ends {} ms from the time; bed starts {} s after it", end - at, (first_bed.start_ms - end) / SEC);
        assert!((end - at).abs() <= 21, "{}", end - at);
        assert!(
            first_bed.start_ms >= end - 12 * SEC && first_bed.start_ms <= end + 1,
            "a normal segue into the bed"
        );
    }
}

#[test]
fn a_back_timed_play_without_an_end_time_still_plays_and_a_wait_holds_it() {
    // What the editor can save before its end time is given (the validator refuses to enable it): the fill
    // must still deal songs, and the operator wait must hold the cursor with them playing.
    let blocks = vec![
        block(
            1,
            "Expected",
            "start",
            "first",
            json!({"schedule": [{"cron": "45 18 * * 1"}], "anchor": "manual"}),
        ),
        block(
            2,
            "Music",
            "play",
            "sequence",
            json!({"source": {"playlist_id": PRE}, "until": "next", "end": "natural", "back_time": {"mode": "best_fit", "tolerance_s": 45}}),
        ),
        block(3, "Hold", "wait", "sequence", json!({"until": "operator"})),
        block(
            4,
            "Again",
            "loop",
            "sequence",
            json!({"to": 1, "times": "forever"}),
        ),
    ];
    let p = prog(
        vec![
            shuffle(PRE, "pre", 1..=6),
            sequential(MONDAY, "Extras", blocks),
        ],
        vec![],
    );
    let (s, plays, _) = run(p, MONDAY, t0(), t0() + 10 * MIN, 21);
    assert!(plays.len() >= 3, "songs play: {}", plays.len());
    assert!(s.waiting_for_operator().is_some());
}

#[test]
fn a_back_timed_play_until_a_time_over_three_songs_plays() {
    let blocks = vec![
        block(
            1,
            "Expected",
            "start",
            "first",
            json!({"schedule": [{"cron": "45 18 * * 1"}], "anchor": "manual"}),
        ),
        block(
            2,
            "Music",
            "play",
            "sequence",
            json!({"source": {"playlist_id": PRE}, "until": {"at": rel(600_000)}, "end": "natural", "back_time": {"mode": "best_fit", "tolerance_s": 45}}),
        ),
        block(3, "Hold", "wait", "sequence", json!({"until": "operator"})),
        block(
            4,
            "Again",
            "loop",
            "sequence",
            json!({"to": 1, "times": "forever"}),
        ),
    ];
    let p = prog(
        vec![
            shuffle(PRE, "pre", 1..=3),
            sequential(MONDAY, "Extras", blocks),
        ],
        vec![],
    );
    let (s, plays, _) = run(p, MONDAY, t0(), t0() + 12 * MIN, 22);
    println!("{} plays; bt {:?}", plays.len(), s.st.bt_display);
    assert!(plays.len() >= 2);
}

#[test]
fn a_back_timed_play_over_songs_longer_than_the_window_deals_one_and_reports_the_fallback() {
    let rows: Vec<_> = (1..=3i64)
        .map(|i| {
            station::catalog::testing::row(
                i,
                &format!("Long {i}"),
                &format!("A{i}"),
                "music",
                "music",
                (760 + i * 5) * SEC,
            )
        })
        .collect();
    let cat = station::catalog::testing::catalog(rows);
    let blocks = vec![
        block(
            1,
            "Expected",
            "start",
            "first",
            json!({"schedule": [{"cron": "45 18 * * 1"}], "anchor": "manual"}),
        ),
        block(
            2,
            "Music",
            "play",
            "sequence",
            json!({"source": {"playlist_id": PRE}, "until": {"at": rel(600_000)}, "end": "natural", "back_time": {"mode": "best_fit", "tolerance_s": 45}}),
        ),
        block(3, "Hold", "wait", "sequence", json!({"until": "operator"})),
    ];
    let p = prog(
        vec![
            shuffle(PRE, "pre", 1..=3),
            sequential(MONDAY, "Extras", blocks),
        ],
        vec![],
    );
    let mut s = sched_for(p, MONDAY, t0(), 23);
    let (plays, _, _) = run_timed(
        &mut s,
        &cat,
        &EffectiveTiming,
        t0(),
        t0() + 12 * MIN,
        usize::MAX,
        |_, _, _, _| {},
    );
    println!("{} plays; bt {:?}", plays.len(), s.st.bt_display);
    assert!(!plays.is_empty(), "one song is dealt even though none fits");
    assert_eq!(
        s.st.bt_display.as_ref().map(|b| b.state.as_str()),
        Some("fallback")
    );
}

#[test]
fn a_program_started_while_a_long_song_is_on_air_plans_its_first_item_for_now() {
    let rows: Vec<_> = (1..=3i64)
        .map(|i| {
            station::catalog::testing::row(
                i,
                &format!("Long {i}"),
                &format!("A{i}"),
                "music",
                "music",
                900 * SEC,
            )
        })
        .collect();
    let cat = station::catalog::testing::catalog(rows);
    let blocks = vec![block(
        2,
        "Music",
        "play",
        "sequence",
        json!({"source": {"playlist_id": PRE}, "until": {"at": rel(600_000)}, "end": "natural", "back_time": {"mode": "best_fit", "tolerance_s": 45}}),
    )];
    let p = prog(
        vec![
            shuffle(PRE, "pre", 1..=3),
            sequential(MONDAY, "Show", blocks),
        ],
        vec![],
    );
    let mut s = sched_for(p, PRE, t0() - 60 * SEC, 31);
    s.fill(t0() - 60 * SEC, &cat, &EffectiveTiming);
    let head = s.head().unwrap().clone();
    s.on_started(head.id, t0() - 60 * SEC, &cat);
    s.fill(t0(), &cat, &EffectiveTiming);
    s.switch_with(
        MONDAY,
        SwitchHow::Now,
        t0(),
        t0(),
        "ui",
        None,
        &cat,
        &EffectiveTiming,
    )
    .unwrap();
    let first = s.head().expect("something is queued at once");
    println!(
        "the first item of the new program is planned for T0 + {} ms",
        first.planned_start_ms.unwrap() - t0()
    );
    assert_eq!(first.planned_start_ms, Some(t0()));
}
