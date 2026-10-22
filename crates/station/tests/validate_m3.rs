//! M3: the validator's rules for timed blocks and scheduled starts (4.4). Each error and warning has a
//! playlist that raises it, and the clean playlist raises nothing.

use common::timespec::{TimeSpec, Zone};
use library::playlists::Mode;
use station::programming::*;
use std::collections::BTreeMap;

fn timed(spec: TimeSpec, mode: BoundaryMode, early: i64, late: i64) -> Timed {
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

fn src_cat(id: i64) -> Source {
    Source::Category {
        id: Some(id),
        name: None,
    }
}

fn play(id: i64, trigger: Trigger, at: Option<i64>, source: Source) -> Block {
    Block::new(
        id,
        "play",
        BlockKind::Play(PlayBlock::next(source)),
        trigger,
        at.map(|ms| timed(TimeSpec::relative(ms), BoundaryMode::SoftThenHard, 0, 30)),
    )
}

fn stop(id: i64, at: i64) -> Block {
    Block::new(
        id,
        "stop",
        BlockKind::Stop {
            how: StopHow::Now,
            fade_ms: 2000,
        },
        Trigger::Timed,
        Some(timed(TimeSpec::relative(at), BoundaryMode::Hard, 0, 0)),
    )
}

fn fade(id: i64, at: i64, secs: f64) -> Block {
    Block::new(
        id,
        "fade",
        BlockKind::FadeStop { seconds: secs },
        Trigger::Timed,
        Some(timed(TimeSpec::relative(at), BoundaryMode::Hard, 0, 0)),
    )
}

fn pl(id: i64, name: &str, mode: Mode, blocks: Vec<Block>) -> Playlist {
    let mut entries: Vec<Entry> = (1..=3)
        .map(|i| Entry::Item {
            item_id: i,
            weight: 1.0,
        })
        .collect();
    entries.extend(blocks.into_iter().map(Entry::Block));
    Playlist {
        id,
        name: name.into(),
        mode,
        no_repeat_minutes: 60,
        separation: Separation::default(),
        station_blocks: true,
        station_id: None,
        enabled: true,
        entries,
    }
}

fn prog(pls: Vec<Playlist>, sb: Vec<Block>) -> Programming {
    Programming {
        playlists: pls
            .into_iter()
            .map(|p| (p.id, p))
            .collect::<BTreeMap<_, _>>(),
        station_blocks: sb,
    }
}

struct Refs3;
impl Refs for Refs3 {
    fn category_exists(&self, id: Option<i64>, _: Option<&str>) -> bool {
        id.is_some_and(|i| i <= 3)
    }
    fn item_exists(&self, id: i64) -> bool {
        id <= 100
    }
    fn category_playable(&self, id: Option<i64>, _: Option<&str>) -> Option<usize> {
        id.map(|i| if i == 3 { 1 } else { 40 })
    }
}

fn codes(p: &Programming) -> Vec<(&'static str, bool)> {
    let z = Zone::named("America/New_York").unwrap();
    let mut v: Vec<_> = p
        .validate_with(&Refs3, Some(&z))
        .into_iter()
        .map(|x| (x.code, x.error))
        .collect();
    v.sort();
    v
}

fn id_insert(id: i64) -> Block {
    Block::new(
        id,
        "ID",
        BlockKind::Insert { source: src_cat(2) },
        Trigger::Timed,
        Some(timed(
            TimeSpec::recurrence("0 * * * *").unwrap(),
            BoundaryMode::SoftThenHard,
            60,
            30,
        )),
    )
}

#[test]
fn the_clean_playlists_raise_nothing() {
    let p = prog(
        vec![
            pl(1, "Office", Mode::Shuffle, vec![id_insert(10)]),
            pl(
                2,
                "Show",
                Mode::Sequential,
                vec![
                    play(20, Trigger::Sequence, None, src_cat(1)),
                    fade(21, 880_000, 7.0),
                    stop(22, 900_000),
                ],
            ),
        ],
        vec![id_insert(30)],
    );
    assert!(codes(&p).is_empty(), "{:?}", codes(&p));
    // The Eve program: the fade ends at 14:47, the elevator starts at 15:01: 14 s of silence by design.
    let eve = prog(
        vec![pl(
            2,
            "Eve",
            Mode::Sequential,
            vec![
                play(20, Trigger::Sequence, None, src_cat(1)),
                fade(21, 880_000, 7.0),
                play(23, Trigger::Timed, Some(901_000), src_cat(1)),
                stop(22, 1_800_000),
            ],
        )],
        vec![],
    );
    let z = Zone::named("America/New_York").unwrap();
    let probs = eve.validate_with(&Refs3, Some(&z));
    assert_eq!(probs.len(), 1, "{probs:?}");
    assert!(probs[0].message.contains("14 s"), "{}", probs[0].message);
    let clean = prog(
        vec![pl(1, "Office", Mode::Shuffle, vec![id_insert(10)])],
        vec![id_insert(30)],
    );
    assert!(codes(&clean).is_empty(), "{:?}", codes(&clean));
}

#[test]
fn errors() {
    // An in-sequence flow block in a shuffle playlist.
    let p = prog(
        vec![pl(
            1,
            "A",
            Mode::Shuffle,
            vec![play(20, Trigger::Sequence, None, src_cat(1))],
        )],
        vec![],
    );
    assert_eq!(codes(&p), vec![("in_sequence_in_shuffle", true)]);
    // A timed flow block in a shuffle playlist is allowed.
    let p = prog(
        vec![pl(1, "A", Mode::Shuffle, vec![stop(20, 60_000)])],
        vec![],
    );
    assert!(codes(&p).is_empty());
    // Two timed flow blocks at the same time.
    let p = prog(
        vec![pl(
            1,
            "A",
            Mode::Sequential,
            vec![
                stop(20, 60_000),
                play(21, Trigger::Timed, Some(60_000), src_cat(1)),
            ],
        )],
        vec![],
    );
    assert_eq!(codes(&p), vec![("same_time", true)]);
    // Times that run backward in list order.
    let p = prog(
        vec![pl(
            1,
            "A",
            Mode::Sequential,
            vec![
                play(20, Trigger::Timed, Some(120_000), src_cat(1)),
                stop(21, 60_000),
            ],
        )],
        vec![],
    );
    assert_eq!(codes(&p), vec![("times_backward", true)]);
    // Recurring blocks are exempt from the order rule.
    let p = prog(
        vec![pl(
            1,
            "A",
            Mode::Sequential,
            vec![stop(20, 120_000), id_insert(21)],
        )],
        vec![],
    );
    assert!(codes(&p).is_empty());
    // A negative relative offset (the row does not read).
    let mut neg = stop(20, 0);
    neg.timed = None;
    neg.problem = Some("a negative relative offset".into());
    let p = prog(vec![pl(1, "A", Mode::Sequential, vec![neg])], vec![]);
    assert_eq!(codes(&p), vec![("negative_offset", true)]);
    // A source that does not exist: category, item, playlist.
    let p = prog(
        vec![pl(
            1,
            "A",
            Mode::Sequential,
            vec![
                play(20, Trigger::Sequence, None, src_cat(9)),
                Block::new(
                    21,
                    "ID",
                    BlockKind::Insert {
                        source: Source::Item(500),
                    },
                    Trigger::Timed,
                    Some(timed(
                        TimeSpec::recurrence("0 * * * *").unwrap(),
                        BoundaryMode::Soft,
                        0,
                        30,
                    )),
                ),
                Block::new(
                    22,
                    "go",
                    BlockKind::StartPlaylist {
                        playlist_id: 77,
                        how: StartHow::Segue,
                    },
                    Trigger::Timed,
                    Some(timed(
                        TimeSpec::relative(5000),
                        BoundaryMode::SoftThenHard,
                        0,
                        30,
                    )),
                ),
            ],
        )],
        vec![],
    );
    assert_eq!(
        codes(&p),
        vec![
            ("source_missing", true),
            ("source_missing", true),
            ("source_missing", true)
        ]
    );
    // A nesting cycle, and nesting deeper than 4.
    let cyc = prog(
        vec![
            pl(
                1,
                "A",
                Mode::Sequential,
                vec![play(20, Trigger::Sequence, None, Source::Playlist(2))],
            ),
            pl(
                2,
                "B",
                Mode::Sequential,
                vec![play(21, Trigger::Sequence, None, Source::Playlist(1))],
            ),
        ],
        vec![],
    );
    let c = codes(&cyc);
    assert!(c.contains(&("nesting_cycle", true)), "{c:?}");
    let deep = prog(
        (1..=6)
            .map(|i| {
                let blocks = if i < 6 {
                    vec![play(
                        20 + i,
                        Trigger::Sequence,
                        None,
                        Source::Playlist(i + 1),
                    )]
                } else {
                    vec![]
                };
                pl(i, &format!("L{i}"), Mode::Sequential, blocks)
            })
            .collect(),
        vec![],
    );
    let c = codes(&deep);
    assert!(c.contains(&("nesting_depth", true)), "{c:?}");
    let four = prog(
        (1..=4)
            .map(|i| {
                let blocks = if i < 4 {
                    vec![play(
                        20 + i,
                        Trigger::Sequence,
                        None,
                        Source::Playlist(i + 1),
                    )]
                } else {
                    vec![]
                };
                pl(i, &format!("L{i}"), Mode::Sequential, blocks)
            })
            .collect(),
        vec![],
    );
    assert!(
        !codes(&four).iter().any(|c| c.0.starts_with("nesting")),
        "four deep runs"
    );
    // insert_every with n below 1.
    let ie = Block::new(
        20,
        "liners",
        BlockKind::InsertEvery(InsertEvery {
            n: 0,
            source: src_cat(2),
            count_kind: "music".into(),
            min_gap_minutes: 0,
            resets_on: vec![],
            inherit: true,
        }),
        Trigger::Standing,
        None,
    );
    let p = prog(vec![pl(1, "A", Mode::Shuffle, vec![ie])], vec![]);
    assert_eq!(codes(&p), vec![("insert_every_n", true)]);
    // Two playlists with recurring start blocks at the same time.
    let start = |id: i64, cron: &str| {
        Block::new(
            id,
            "start",
            BlockKind::Start(StartBlock {
                schedule: vec![TimeSpec::recurrence(cron).unwrap()],
                anchor: Anchor::Absolute,
                how: StartHow::Segue,
                mode: BoundaryMode::Soft,
                early_s: 0,
                late_s: 300,
                arm_window_min: 60,
                fallback_grace_s: None,
            }),
            Trigger::First,
            None,
        )
    };
    let p = prog(
        vec![
            pl(1, "A", Mode::Shuffle, vec![start(20, "0 18 * * *")]),
            pl(2, "B", Mode::Shuffle, vec![start(21, "0 18 * * 1-5")]),
        ],
        vec![],
    );
    assert_eq!(codes(&p), vec![("start_clash", true)]);
    let p = prog(
        vec![
            pl(1, "A", Mode::Shuffle, vec![start(20, "0 18 * * *")]),
            pl(2, "B", Mode::Shuffle, vec![start(21, "0 20 * * *")]),
        ],
        vec![],
    );
    assert!(codes(&p).is_empty());
    let p = prog(
        vec![
            pl(1, "A", Mode::Shuffle, vec![start(20, "0 18 * * 6,0")]),
            pl(2, "B", Mode::Shuffle, vec![start(21, "0 18 * * 1-5")]),
        ],
        vec![],
    );
    assert!(codes(&p).is_empty(), "different days");
    // A flow block in the station blocks, and a station block with a relative time.
    let p = prog(vec![], vec![stop(30, 5000)]);
    let c = codes(&p);
    assert!(
        c.contains(&("station_flow_block", true)) && c.contains(&("station_relative_time", true)),
        "{c:?}"
    );
}

#[test]
fn warnings() {
    // A silent gap: a timed flow block reached early after a stop.
    let p = prog(
        vec![pl(
            1,
            "A",
            Mode::Sequential,
            vec![
                stop(20, 10_000),
                play(21, Trigger::Timed, Some(24_000), src_cat(1)),
            ],
        )],
        vec![],
    );
    let z = Zone::named("America/New_York").unwrap();
    let probs = p.validate_with(&Refs3, Some(&z));
    assert_eq!(probs.len(), 1);
    assert!(probs[0].message.contains("12 s"), "{}", probs[0].message);
    assert!(!probs[0].error && probs[0].code == "silent_gap");
    // A block after an `end` never fires.
    let end = Block::new(20, "end", BlockKind::End, Trigger::Sequence, None);
    let p = prog(
        vec![pl(
            1,
            "A",
            Mode::Sequential,
            vec![end, play(21, Trigger::Sequence, None, src_cat(1))],
        )],
        vec![],
    );
    assert_eq!(codes(&p), vec![("never_fires", false)]);
    // A nonexistent local time.
    let gap = Block::new(
        20,
        "once",
        BlockKind::Insert { source: src_cat(2) },
        Trigger::Timed,
        Some(timed(
            TimeSpec::date("2026-03-08T02:30:00").unwrap(),
            BoundaryMode::Soft,
            0,
            30,
        )),
    );
    let p = prog(vec![pl(1, "A", Mode::Shuffle, vec![gap])], vec![]);
    assert_eq!(codes(&p), vec![("dst_nonexistent", false)]);
    let ok = Block::new(
        20,
        "once",
        BlockKind::Insert { source: src_cat(2) },
        Trigger::Timed,
        Some(timed(
            TimeSpec::date("2026-03-08T03:30:00").unwrap(),
            BoundaryMode::Soft,
            0,
            30,
        )),
    );
    assert!(codes(&prog(vec![pl(1, "A", Mode::Shuffle, vec![ok])], vec![])).is_empty());
    // A play source with fewer than 2 playable items.
    let p = prog(
        vec![pl(
            1,
            "A",
            Mode::Sequential,
            vec![play(20, Trigger::Sequence, None, src_cat(3))],
        )],
        vec![],
    );
    assert_eq!(codes(&p), vec![("thin_source", false)]);
}

#[test]
fn block_rows_that_do_not_read_say_why() {
    use library::playlists::BlockRow;
    let row = |kind: &str, trigger: &str, params: serde_json::Value| BlockRow {
        id: 1,
        name: "b".into(),
        kind: kind.into(),
        trigger: trigger.into(),
        priority: 100,
        params,
        enabled: true,
    };
    use serde_json::json;
    let b = Block::from_row(&row(
        "insert",
        "timed",
        json!({"source": {"category_id": 2}, "when": {"kind": "recurrence", "cron": "99 * * * *"}}),
    ));
    assert!(
        b.problem.as_deref().unwrap().contains("minute"),
        "{:?}",
        b.problem
    );
    let b = Block::from_row(&row(
        "insert",
        "timed",
        json!({"source": {"category_id": 2}}),
    ));
    assert!(b.problem.as_deref().unwrap().contains("when"));
    let b = Block::from_row(&row(
        "insert",
        "timed",
        json!({"source": {"category_id": 2}, "when": {"kind": "relative", "offset_ms": -5}}),
    ));
    assert!(b.problem.as_deref().unwrap().contains("negative"));
    let b = Block::from_row(&row(
        "insert",
        "timed",
        json!({"source": {"category_id": 2}, "mode": "now-ish", "when": {"kind": "relative", "offset_ms": 5}}),
    ));
    assert!(b.problem.as_deref().unwrap().contains("mode"));
    let b = Block::from_row(&row(
        "insert",
        "timed",
        json!({"when": {"kind": "relative", "offset_ms": 5}}),
    ));
    assert!(b.problem.as_deref().unwrap().contains("source"));
    let ok = Block::from_row(&row(
        "insert",
        "timed",
        json!({"source": {"category_id": 2}, "when": {"kind": "recurrence", "cron": "0 * * * *"}}),
    ));
    assert!(
        ok.problem.is_none()
            && ok
                .timed
                .as_ref()
                .is_some_and(|t| t.early_s == 60 && t.late_s == 30)
    );
    let ok = Block::from_row(&row(
        "start",
        "first",
        json!({"schedule": [{"cron": "45 18 * * 1"}], "anchor": "manual"}),
    ));
    assert!(
        matches!(&ok.kind, BlockKind::Start(s) if s.anchor == Anchor::Manual && s.arm_window_min == 60),
        "{:?}",
        ok.kind
    );
}
