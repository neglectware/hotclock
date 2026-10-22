//! M5 acceptance 10 and (k): each error and warning of the validator of 4.4 that the block engine adds is raised by
//! its own test playlist and by nothing else, and the Monday program has exactly one warning and no errors.

#[path = "support_m5/mod.rs"]
mod common_m5;
use common_m5::*;
use serde_json::json;
use station::programming::*;

fn codes(p: &std::sync::Arc<Programming>, pl: i64) -> Vec<(bool, &'static str)> {
    p.validate()
        .into_iter()
        .filter(|x| x.playlist == Some(pl))
        .map(|x| (x.error, x.code))
        .collect()
}

fn one(blocks: Vec<Block>) -> std::sync::Arc<Programming> {
    prog(
        vec![
            shuffle(PRE, "pre", 1..=6),
            sequential(MONDAY, "Show", blocks),
        ],
        vec![],
    )
}

fn osc(id: i64, trigger: &str) -> Block {
    let mut params = json!({"host": "127.0.0.1", "port": 9, "address": "/x", "args": []});
    if trigger == "timed" {
        params["when"] = rel(60_000);
    }
    block(id, "osc", "osc_out", trigger, params)
}

fn rel(ms: i64) -> serde_json::Value {
    json!({"kind": "relative", "offset_ms": ms})
}

fn play(id: i64) -> Block {
    block(
        id,
        "play",
        "play",
        "sequence",
        json!({"source": {"playlist_id": PRE}, "until": "next"}),
    )
}

#[test]
fn the_monday_program_has_one_warning_and_no_errors() {
    let (p, _) = ita_prog();
    let found = codes(&p, MONDAY);
    println!("(k) Monday: {found:?}");
    assert_eq!(found, vec![(false, "silent_gap")]);
    let msg: Vec<_> = p
        .validate()
        .into_iter()
        .filter(|x| x.playlist == Some(MONDAY))
        .map(|x| x.message)
        .collect();
    assert!(msg[0].contains("14 s"), "{msg:?}");
}

#[test]
fn loop_checks() {
    // A loop over a body with a play is fine; with only a message in it, it takes no time.
    assert!(codes(
        &one(vec![
            play(1),
            block(2, "l", "loop", "sequence", json!({"times": 3}))
        ]),
        MONDAY
    )
    .is_empty());
    assert_eq!(
        codes(
            &one(vec![
                osc(1, "sequence"),
                block(2, "l", "loop", "sequence", json!({"times": "forever"}))
            ]),
            MONDAY
        ),
        vec![(true, "loop_no_time")]
    );
    // A wait with a duration of zero takes no time either; one with a duration does.
    assert_eq!(
        codes(
            &one(vec![
                block(
                    1,
                    "w",
                    "wait",
                    "sequence",
                    json!({"until": {"duration_s": 0}})
                ),
                block(2, "l", "loop", "sequence", json!({"times": 2}))
            ]),
            MONDAY
        ),
        vec![(true, "loop_no_time")]
    );
    assert!(codes(
        &one(vec![
            block(
                1,
                "w",
                "wait",
                "sequence",
                json!({"until": {"duration_s": 5}})
            ),
            block(2, "l", "loop", "sequence", json!({"times": 2}))
        ]),
        MONDAY
    )
    .is_empty());
    // A loop that goes forward, to itself or to position 0, or runs 0 times.
    for params in [
        json!({"to": 2, "times": 2}),
        json!({"to": 3, "times": 2}),
        json!({"to": 0, "times": 2}),
        json!({"times": 0}),
    ] {
        let p = one(vec![
            play(1),
            block(2, "l", "loop", "sequence", params.clone()),
        ]);
        assert_eq!(codes(&p, MONDAY), vec![(true, "loop_target")], "{params}");
    }
    // To an earlier row.
    assert!(codes(
        &one(vec![
            play(1),
            block(2, "w", "wait", "sequence", json!({"until": "operator"})),
            block(3, "l", "loop", "sequence", json!({"to": 1, "times": 2}))
        ]),
        MONDAY
    )
    .is_empty());
}

#[test]
fn gain_and_back_time_checks() {
    assert_eq!(
        codes(
            &one(vec![block(
                1,
                "g",
                "set_gain",
                "sequence",
                json!({"gain_db": 3})
            )]),
            MONDAY
        ),
        vec![(true, "gain_range")]
    );
    assert!(codes(
        &one(vec![block(
            1,
            "g",
            "set_gain",
            "sequence",
            json!({"gain_db": -6, "ramp_ms": 500})
        )]),
        MONDAY
    )
    .is_empty());
    let nat = |until: serde_json::Value| {
        block(
            1,
            "bt",
            "play",
            "sequence",
            json!({"source": {"playlist_id": PRE}, "until": until, "end": "natural", "back_time": {"mode": "best_fit", "tolerance_s": 45}}),
        )
    };
    // No end time anywhere.
    assert_eq!(
        codes(&one(vec![nat(json!("next"))]), MONDAY),
        vec![(true, "back_time_no_end")]
    );
    // An end time from a duration, from an until time, from a later timed flow block.
    assert!(codes(&one(vec![nat(json!({"duration_s": 600}))]), MONDAY).is_empty());
    assert!(codes(&one(vec![nat(json!({"at": rel(600_000)}))]), MONDAY).is_empty());
    assert!(codes(
        &one(vec![
            nat(json!("next")),
            block(
                2,
                "f",
                "fade_stop",
                "timed",
                json!({"when": rel(600_000), "seconds": 5})
            )
        ]),
        MONDAY
    )
    .is_empty());
    // A window shorter than the shortest item (refs know the lengths).
    struct R;
    impl Refs for R {
        fn category_exists(&self, _: Option<i64>, _: Option<&str>) -> bool {
            true
        }
        fn item_exists(&self, _: i64) -> bool {
            true
        }
        fn playlist_min_ms(&self, _: i64) -> Option<i64> {
            Some(200_000)
        }
    }
    let p = one(vec![nat(json!({"at": rel(120_000)}))]);
    let found: Vec<_> = p
        .validate_with(&R, None)
        .into_iter()
        .map(|x| (x.error, x.code))
        .collect();
    assert_eq!(found, vec![(false, "back_time_short")]);
    let p = one(vec![nat(json!({"at": rel(300_000)}))]);
    assert!(p.validate_with(&R, None).is_empty());
}

#[test]
fn blocks_that_cannot_read_are_unreadable_and_in_sequence_blocks_need_a_sequential_playlist() {
    for (kind, params) in [
        ("osc_out", json!({"host": "", "port": 9, "address": "/x"})),
        (
            "osc_out",
            json!({"host": "h", "port": 70000, "address": "/x"}),
        ),
        ("osc_out", json!({"host": "h", "port": 9, "address": "x"})),
        (
            "osc_out",
            json!({"host": "h", "port": 9, "address": "/x", "args": [{"type": "q", "value": 1}]}),
        ),
        (
            "midi_out",
            json!({"port": "hw:1,0", "message": {"type": "cc", "channel": 0, "data1": 1, "data2": 1}}),
        ),
        (
            "midi_out",
            json!({"port": "hw:1,0", "message": {"type": "cc", "channel": 1, "data1": 200, "data2": 1}}),
        ),
        ("http_request", json!({"method": "POST", "url": "ftp://x"})),
        ("wait", json!({})),
        (
            "play",
            json!({"source": {"playlist_id": 1}, "until": {"count": 0}}),
        ),
        (
            "play",
            json!({"source": {"playlist_id": 1}, "end": "sideways"}),
        ),
        ("nonsense", json!({})),
    ] {
        let p = one(vec![block(1, "x", kind, "sequence", params.clone())]);
        let found = codes(&p, MONDAY);
        assert!(
            found.contains(&(true, "unreadable")),
            "{kind} {params}: {found:?}"
        );
    }
    // The same side block in a shuffle playlist cannot be in sequence; timed it is fine.
    let mut sh = shuffle(5, "shuf", 1..=3);
    sh.entries.push(Entry::Block(osc(9, "sequence")));
    let p = prog(vec![sh.clone()], vec![]);
    assert_eq!(codes(&p, 5), vec![(true, "in_sequence_in_shuffle")]);
    sh.entries.pop();
    sh.entries.push(Entry::Block(osc(9, "timed")));
    assert!(codes(&prog(vec![sh], vec![]), 5).is_empty());
}

#[test]
fn the_dry_run_of_the_monday_program_lists_blocks_3_to_6_and_the_gap_and_fires_nothing() {
    let (p, _) = ita_prog();
    let z = zone();
    let t0 = local("2027-01-18T18:45:00");
    let pv = station::preview::dry_run(&p, MONDAY, t0, None, &z).unwrap();
    let at = |name: &str| {
        pv.rows
            .iter()
            .find(|r| r.name == name)
            .and_then(|r| r.at_ms)
            .map(|m| z.local_string(m))
    };
    println!(
        "rows: {:?}",
        pv.rows
            .iter()
            .map(|r| (
                r.position + 1,
                r.name.clone(),
                r.at_ms.map(|m| z.local_string(m))
            ))
            .collect::<Vec<_>>()
    );
    assert_eq!(at("Fade out").unwrap(), "2027-01-18T18:59:40.000-05:00");
    assert_eq!(
        at("Mute the Wing").unwrap(),
        "2027-01-18T19:00:00.000-05:00"
    );
    assert_eq!(at("Elevator bed").unwrap(), "2027-01-18T19:00:01.000-05:00");
    assert_eq!(
        at("Stop after midnight").unwrap(),
        "2027-01-19T00:30:00.000-05:00"
    );
    assert_eq!(pv.gaps.len(), 1);
    assert_eq!(pv.gaps[0].seconds, 14);
    assert_eq!(pv.gaps[0].name, "Elevator bed");
    // The in-sequence play is reached at T0.
    assert_eq!(
        at("Pre-show music").unwrap(),
        "2027-01-18T18:45:00.000-05:00"
    );
    assert!(pv.past.is_empty());
    // Nothing fired: the dry run reads the programming only (it has no scheduler to fire anything).
}
