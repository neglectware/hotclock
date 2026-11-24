//! The show time (4.5): blocks timed before or after the moment a program builds to, wherever T0 falls, and
//! moving the show time for one run. Virtual clock, `station::sim`.

#[path = "support_m5/mod.rs"]
mod common_m5;
use common_m5::*;
use serde_json::json;
use station::acts::ShowTimeSet;
use station::programming::*;
use station::sched::*;
use station::sim::run_timed;
use station::timed::ActKind;

const WING: &str = "/ch/02/mix/fader";

/// Eve Monday written against the show time: expected Mondays 18:45, show at 19:00, the pre-show music fades
/// 20 s before it, the Wing fader goes down 9 s before it, the elevator bed starts 1 s after it.
fn show_prog(show_time: Option<&str>) -> std::sync::Arc<Programming> {
    let mut start = json!({"schedule": [{"cron": "45 18 * * 1"}], "anchor": "manual", "arm_window_min": 60,
        "fallback_start": {"enabled": false}});
    if let Some(s) = show_time {
        start["show_time"] = json!(s);
    }
    let show = |ms: i64| json!({"kind": "show", "offset_ms": ms});
    let blocks = vec![
        block(100, "Expected Mondays", "start", "first", start),
        block(
            101,
            "Pre-show music",
            "play",
            "sequence",
            json!({"source": {"playlist_id": PRE}, "until": "next"}),
        ),
        block(
            102,
            "Fade out",
            "fade_stop",
            "timed",
            json!({"when": show(-20_000), "seconds": 7, "curve": "cosine"}),
        ),
        block(
            103,
            "Wing fader down",
            "osc_out",
            "timed",
            json!({"when": show(-9_000), "host": "127.0.0.1", "port": 2223, "address": WING,
                "args": [{"type": "f", "value": 0.2}]}),
        ),
        block(
            104,
            "Elevator bed",
            "play",
            "timed",
            json!({"when": show(1_000), "source": {"playlist_id": BED}, "until": "next"}),
        ),
    ];
    let mut monday = sequential(MONDAY, "Eve Monday", blocks);
    monday.station_blocks = false;
    prog(
        vec![
            shuffle(PRE, "Eve 80s library", 1..=6),
            shuffle(BED, "Eve elevator", 11..=15),
            monday,
        ],
        vec![],
    )
}

fn fade_of(acts: &[station::sim::SimAct]) -> Vec<i64> {
    acts.iter()
        .filter(|a| matches!(a.act.kind, ActKind::FadeStop { .. }))
        .map(|a| a.act.target_ms)
        .collect()
}

fn osc_of(acts: &[station::sim::SimAct]) -> Vec<i64> {
    acts.iter()
        .filter(|a| matches!(a.act.kind, ActKind::Side(_)))
        .map(|a| a.act.target_ms)
        .collect()
}

#[test]
fn blocks_timed_from_the_show_time_land_on_the_clock_wherever_t0_falls() {
    let p = show_prog(Some("19:00"));
    let cat = ita_cat();
    // On time, a few seconds late, and 6 minutes late (the countdown started at 18:51:13).
    for start in [
        "2027-01-18T18:45:00",
        "2027-01-18T18:45:10",
        "2027-01-18T18:51:13",
    ] {
        let t0 = local(start);
        let mut s = sched_for(p.clone(), MONDAY, t0, 7);
        let (plays, acts, _) = run_timed(
            &mut s,
            &cat,
            &EffectiveTiming,
            t0,
            local("2027-01-18T19:10:00"),
            usize::MAX,
            |_, _, _, _| {},
        );
        assert_eq!(
            fade_of(&acts),
            vec![local("2027-01-18T18:59:40")],
            "{start}"
        );
        assert_eq!(osc_of(&acts), vec![local("2027-01-18T18:59:51")], "{start}");
        let bed = plays.iter().find(|p| p.item_id() >= 11).expect("bed");
        assert_eq!(bed.start_ms, local("2027-01-18T19:00:01"), "{start}");
        // The pre-show music played up to the fade and not after it.
        assert!(plays
            .iter()
            .filter(|p| p.item_id() <= 6)
            .all(|p| p.start_ms < local("2027-01-18T18:59:40")));
    }
}

#[test]
fn moving_the_show_time_moves_every_block_timed_from_it_for_this_run_only() {
    let p = show_prog(Some("19:00"));
    let cat = ita_cat();
    let t0 = local("2027-01-18T18:45:00");
    let mut s = sched_for(p.clone(), MONDAY, t0, 7);
    let mut moved = false;
    let (plays, acts, _) = run_timed(
        &mut s,
        &cat,
        &EffectiveTiming,
        t0,
        local("2027-01-18T19:15:00"),
        usize::MAX,
        |s, cat, _, now| {
            // At 18:52 the operator delays the show by 5 minutes.
            if !moved && now >= local("2027-01-18T18:52:00") {
                moved = true;
                let at = s.move_show_time(ShowTimeSet::Delay(5 * MIN)).unwrap();
                assert_eq!(at, local("2027-01-18T19:05:00"));
                s.regenerate(now, cat, &EffectiveTiming, false);
            }
        },
    );
    assert!(moved);
    assert_eq!(fade_of(&acts), vec![local("2027-01-18T19:04:40")]);
    assert_eq!(osc_of(&acts), vec![local("2027-01-18T19:04:51")]);
    let bed = plays.iter().find(|p| p.item_id() >= 11).expect("bed");
    assert_eq!(bed.start_ms, local("2027-01-18T19:05:01"));
    // Nothing acted at the old times.
    assert!(acts
        .iter()
        .all(|a| a.act.target_ms < local("2027-01-18T18:59:00")
            || a.act.target_ms > local("2027-01-18T19:04:00")));
    // The next run starts from the playlist's show time again.
    let t1 = local("2027-01-25T18:45:00");
    s.start_program(MONDAY, t1);
    let (_, acts, _) = run_timed(
        &mut s,
        &cat,
        &EffectiveTiming,
        t1,
        local("2027-01-25T19:10:00"),
        usize::MAX,
        |_, _, _, _| {},
    );
    assert_eq!(fade_of(&acts), vec![local("2027-01-25T18:59:40")]);
}

#[test]
fn move_show_time_by_local_time_instant_and_reset() {
    let p = show_prog(Some("19:00"));
    let t0 = local("2027-01-18T18:45:00");
    let mut s = sched_for(p, MONDAY, t0, 7);
    assert_eq!(
        s.move_show_time(ShowTimeSet::Local("19:02:30".into())),
        Ok(local("2027-01-18T19:02:30"))
    );
    assert_eq!(
        s.st.program.as_ref().unwrap().show_override_ms,
        Some(local("2027-01-18T19:02:30"))
    );
    // A delay adds to where it is now.
    assert_eq!(
        s.move_show_time(ShowTimeSet::Delay(60_000)),
        Ok(local("2027-01-18T19:03:30"))
    );
    // Earlier is allowed down to T0, not before it.
    assert_eq!(
        s.move_show_time(ShowTimeSet::At(local("2027-01-18T18:55:00"))),
        Ok(local("2027-01-18T18:55:00"))
    );
    assert!(s
        .move_show_time(ShowTimeSet::At(local("2027-01-18T18:40:00")))
        .is_err());
    assert!(s
        .move_show_time(ShowTimeSet::Local("25:00".into()))
        .is_err());
    // Put back: the override goes and the playlist's time is in force.
    assert_eq!(
        s.move_show_time(ShowTimeSet::Reset),
        Ok(local("2027-01-18T19:00:00"))
    );
    assert_eq!(s.st.program.as_ref().unwrap().show_override_ms, None);
    // Moving it to the playlist's own time keeps no override.
    s.move_show_time(ShowTimeSet::Local("19:00".into()))
        .unwrap();
    assert_eq!(s.st.program.as_ref().unwrap().show_override_ms, None);
}

#[test]
fn a_block_that_acted_does_not_act_again_when_the_show_time_moves_later() {
    let p = show_prog(Some("19:00"));
    let cat = ita_cat();
    let t0 = local("2027-01-18T18:45:00");
    let mut s = sched_for(p, MONDAY, t0, 7);
    let mut moved = false;
    let (_, acts, _) = run_timed(
        &mut s,
        &cat,
        &EffectiveTiming,
        t0,
        local("2027-01-18T19:20:00"),
        usize::MAX,
        |s, cat, _, now| {
            // 18:59:45: the fade has run, the fader and the bed have not. The show is moved to 19:05.
            if !moved && now >= local("2027-01-18T18:59:45") {
                moved = true;
                s.move_show_time(ShowTimeSet::Local("19:05".into()))
                    .unwrap();
                s.regenerate(now, cat, &EffectiveTiming, false);
            }
        },
    );
    assert!(moved);
    assert_eq!(
        fade_of(&acts),
        vec![local("2027-01-18T18:59:40")],
        "the fade ran once, at the first show time"
    );
    assert_eq!(osc_of(&acts), vec![local("2027-01-18T19:04:51")]);
}

#[test]
fn without_a_program_or_a_show_time_nothing_moves() {
    let p = show_prog(None);
    let t0 = local("2027-01-18T18:45:00");
    let mut s = sched_for(p, MONDAY, t0, 7);
    let e = s.move_show_time(ShowTimeSet::Delay(60_000)).unwrap_err();
    assert!(e.contains("no show time"), "{e}");
    s.st.program = None;
    let e = s.move_show_time(ShowTimeSet::Delay(60_000)).unwrap_err();
    assert!(e.contains("no program"), "{e}");
}

#[test]
fn the_validator_names_a_show_timed_block_without_a_show_time_and_compares_show_times() {
    let codes =
        |p: &Programming| -> Vec<&'static str> { p.validate().iter().map(|x| x.code).collect() };
    let p = show_prog(None);
    let c = codes(&p);
    assert_eq!(
        c.iter().filter(|c| **c == "no_show_time").count(),
        3,
        "{c:?}"
    );
    let p = show_prog(Some("19:00"));
    assert!(!codes(&p).contains(&"no_show_time"));
    // A time of day and a show time at the same instant: two timed flow blocks at the same time.
    let mut pl = p.playlists[&MONDAY].clone();
    pl.entries.push(Entry::Block(block(
        105,
        "Also at 18:59:40",
        "fade_stop",
        "timed",
        json!({"when": {"kind": "local", "at": "18:59:40", "day_offset": 0}, "seconds": 2}),
    )));
    let p2 = prog(
        vec![
            shuffle(PRE, "Eve 80s library", 1..=6),
            shuffle(BED, "Eve elevator", 11..=15),
            pl,
        ],
        vec![],
    );
    assert!(codes(&p2).contains(&"same_time"), "{:?}", codes(&p2));
    // A show time that is not a time of day makes the start block unreadable.
    let bad = block(
        200,
        "start",
        "start",
        "first",
        json!({"schedule": [{"cron": "45 18 * * 1"}], "show_time": "7pm"}),
    );
    assert!(
        bad.problem.as_deref().unwrap_or("").contains("show_time"),
        "{:?}",
        bad.problem
    );
}

#[test]
fn the_preview_puts_show_timed_blocks_on_the_clock() {
    let p = show_prog(Some("19:00"));
    let t0 = local("2027-01-18T18:51:13");
    let pv = station::preview::dry_run(&p, MONDAY, t0, None, &zone()).expect("preview");
    let at = |name: &str| {
        pv.rows
            .iter()
            .find(|r| r.name == name)
            .and_then(|r| r.at_ms)
    };
    assert_eq!(at("Fade out"), Some(local("2027-01-18T18:59:40")));
    assert_eq!(at("Wing fader down"), Some(local("2027-01-18T18:59:51")));
    assert_eq!(at("Elevator bed"), Some(local("2027-01-18T19:00:01")));
}
