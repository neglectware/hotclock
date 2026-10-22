//! Helpers shared by the M5 tests (`#[path]`-included, not a test target).
#![allow(dead_code)]

use common::timespec::Zone;
use library::examples::{ItaMonday, OscTarget};
use library::playlists::{BlockRow, Mode, NewBlock};
use station::catalog::testing::{catalog, row};
use station::catalog::Catalog;
use station::programming::*;
use station::sched::*;
use std::collections::BTreeMap;
use std::sync::Arc;

pub const SEC: i64 = 1000;
pub const MIN: i64 = 60_000;

pub fn zone() -> Zone {
    Zone::named("America/New_York").expect("tz database")
}

/// Unix ms of a New York local time.
pub fn local(s: &str) -> i64 {
    zone().compatible(s.parse().unwrap()).unwrap()
}

pub fn block_of(id: i64, nb: &NewBlock) -> Block {
    Block::from_row(&BlockRow {
        id,
        name: nb.name.clone(),
        kind: nb.kind.clone(),
        trigger: nb.trigger.clone(),
        priority: nb.priority,
        params: nb.params.clone(),
        enabled: true,
    })
}

pub fn block(id: i64, name: &str, kind: &str, trigger: &str, params: serde_json::Value) -> Block {
    block_of(
        id,
        &NewBlock {
            name: name.into(),
            kind: kind.into(),
            trigger: trigger.into(),
            priority: 100,
            params,
        },
    )
}

pub fn shuffle(id: i64, name: &str, ids: std::ops::RangeInclusive<i64>) -> Playlist {
    Playlist {
        id,
        name: name.into(),
        mode: Mode::Shuffle,
        no_repeat_minutes: 0,
        separation: Separation {
            title_minutes: 0,
            artist_minutes: 0,
            album_minutes: 0,
        },
        station_blocks: true,
        station_id: None,
        enabled: true,
        entries: ids
            .map(|i| Entry::Item {
                item_id: i,
                weight: 1.0,
            })
            .collect(),
    }
}

pub fn sequential(id: i64, name: &str, blocks: Vec<Block>) -> Playlist {
    Playlist {
        id,
        name: name.into(),
        mode: Mode::Sequential,
        no_repeat_minutes: 0,
        separation: Separation::default(),
        station_blocks: false,
        station_id: None,
        enabled: true,
        entries: blocks.into_iter().map(Entry::Block).collect(),
    }
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

pub fn sched_for(p: Arc<Programming>, playlist: i64, t0: i64, seed: u64) -> Scheduler {
    let cfg = SchedConfig {
        zone: zone(),
        ..SchedConfig::default()
    };
    let mut s = Scheduler::new(cfg, p, seed);
    s.start_program_with(playlist, t0, "ui", None);
    s
}

/// Pre-show tracks 1..=6 (3:20 to 4:10), elevator 11..=15 (2:30 to 3:10).
pub fn ita_rows() -> Vec<library::tracks::TrackRow> {
    let mut v = Vec::new();
    for i in 1..=6i64 {
        v.push(row(
            i,
            &format!("Pre {i}"),
            &format!("A{i}"),
            "music",
            "music",
            (200 + i * 10) * SEC,
        ));
    }
    for i in 11..=15i64 {
        v.push(row(
            i,
            &format!("Bed {i}"),
            &format!("B{i}"),
            "music",
            "music",
            (150 + (i - 10) * 10) * SEC,
        ));
    }
    v
}

pub fn ita_cat() -> Catalog {
    catalog(ita_rows())
}

pub const PRE: i64 = 1;
pub const BED: i64 = 2;
pub const MONDAY: i64 = 3;

/// The Monday program of 4.9 and the two playlists it plays, as a programming. Block ids 100 to 105.
pub fn ita_prog() -> (Arc<Programming>, ItaMonday) {
    let ex = ItaMonday::new(PRE, BED, OscTarget::wing_channel_mute("127.0.0.1", 2));
    let blocks: Vec<Block> = ex
        .blocks()
        .iter()
        .enumerate()
        .map(|(i, nb)| block_of(100 + i as i64, nb))
        .collect();
    let mut monday = sequential(MONDAY, "Eve Monday", blocks);
    monday.station_blocks = false;
    let p = prog(
        vec![
            shuffle(PRE, "Eve 80s library", 1..=6),
            shuffle(BED, "Eve elevator", 11..=15),
            monday,
        ],
        vec![],
    );
    (p, ex)
}
