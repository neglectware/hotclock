//! The dry run of a playlist (architecture 4.9, 7.3 `GET /playlists/{id}/preview`; milestone M5): every block's
//! wall time for a chosen T0, the silent gaps, and, for a chosen "now", the blocks that are already past
//! and what their late policy will do. It reads the programming only, plays nothing and fires nothing.

use crate::programming::{
    Anchor, Block, BlockKind, Entry, LatePolicy, Playlist, Programming, Source, Trigger, Until,
    WaitUntil,
};
use common::timespec::{When, Zone};
use serde_json::{json, Value};

/// The default late window of a timed block (4.9), seconds.
pub const DEFAULT_LATE_WINDOW_S: i64 = 60;

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub position: usize,
    pub depth: usize,
    pub block_id: i64,
    pub name: String,
    pub kind: String,
    pub trigger: &'static str,
    /// The wall time of the block, when it has one for this T0 (a timed block, or an in-sequence block that
    /// the cursor reaches at T0).
    pub at_ms: Option<i64>,
    /// The time as the playlist says it.
    pub when: String,
    /// How a time written as a recurrence or a date is placed: the first occurrence at or after T0.
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gap {
    pub from_ms: i64,
    pub to_ms: i64,
    pub seconds: i64,
    pub before_block: i64,
    pub name: String,
}

/// A block whose time is before "now" at the chosen T0.
#[derive(Debug, Clone, PartialEq)]
pub struct Past {
    pub block_id: i64,
    pub name: String,
    pub kind: String,
    pub at_ms: i64,
    pub late_by_s: i64,
    pub policy: &'static str,
    /// `run` (it acts now, in order) or `skip` (logged `missed`).
    pub action: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct Preview {
    pub rows: Vec<Row>,
    pub gaps: Vec<Gap>,
    pub past: Vec<Past>,
    /// Warnings that need a T0 (4.4): a block in the past at start, a block that can never fire.
    pub warnings: Vec<(i64, &'static str, String)>,
}

fn stopping(k: &BlockKind) -> bool {
    matches!(
        k,
        BlockKind::Stop { .. } | BlockKind::FadeStop { .. } | BlockKind::End
    )
}

/// The wall time of a timed block for `t0`, and a note when it is not exact.
fn timed_at(b: &Block, zone: &Zone, t0: i64) -> Option<(i64, Option<String>)> {
    let t = b.timed.as_ref()?;
    match &t.spec.when {
        When::Relative { offset_ms } => Some((t0 + offset_ms, None)),
        When::Local { .. } | When::Date { .. } => t
            .spec
            .occurrences(zone, Some(t0), i64::MIN / 4, i64::MAX / 4)
            .first()
            .map(|x| (*x, None)),
        When::Recurrence { .. } => t
            .spec
            .first_after(zone, Some(t0), t0 - 1, 400 * 86_400_000)
            .map(|x| (x, Some("the first occurrence at or after T0".into()))),
    }
}

fn takes_time(e: &Entry) -> bool {
    match e {
        Entry::Item { .. } | Entry::Album { .. } => true,
        Entry::Block(b) => {
            b.enabled
                && match &b.kind {
                    BlockKind::Play(_) | BlockKind::Insert { .. } => b.timed.is_none(),
                    BlockKind::Wait(WaitUntil::DurationS(d)) => *d > 0,
                    BlockKind::Wait(_) => true,
                    _ => false,
                }
        }
    }
}

fn silence_after(b: &Block) -> Option<i64> {
    match &b.kind {
        BlockKind::FadeStop { seconds } => Some((seconds * 1000.0).round() as i64),
        BlockKind::Stop { fade_ms, .. } => Some(*fade_ms),
        _ => None,
    }
}

/// The dry run of `playlist_id` for `t0_ms`. `now_ms` adds the list of blocks already past.
pub fn dry_run(
    prog: &Programming,
    playlist_id: i64,
    t0_ms: i64,
    now_ms: Option<i64>,
    zone: &Zone,
) -> Option<Preview> {
    let pl = prog.playlists.get(&playlist_id)?;
    let mut pv = Preview::default();
    walk(prog, pl, t0_ms, zone, 0, &mut pv, &mut vec![playlist_id]);
    // The silent gaps: after a stop or fade_stop until the next timed flow block (4.4).
    let mut prev_end: Option<(i64, i64)> = None; // (end of the silence, block)
    let mut timed_flow: Vec<&Row> = pv
        .rows
        .iter()
        .filter(|r| r.depth == 0 && r.trigger == "timed" && r.at_ms.is_some())
        .collect();
    timed_flow.sort_by_key(|r| r.at_ms);
    let mut gaps = Vec::new();
    for r in timed_flow {
        let Some(Entry::Block(b)) = pl.entries.get(r.position) else {
            continue;
        };
        if !b.kind.is_flow() || !b.enabled {
            continue;
        }
        let at = r.at_ms.unwrap_or(0);
        if let Some((end, _)) = prev_end {
            if at > end {
                gaps.push(Gap {
                    from_ms: end,
                    to_ms: at,
                    seconds: (at - end) / 1000,
                    before_block: b.id,
                    name: b.name.clone(),
                });
            }
            prev_end = None;
        }
        if let Some(ms) = silence_after(b) {
            prev_end = Some((at + ms, b.id));
        }
    }
    pv.gaps = gaps;
    if let Some(now) = now_ms {
        for r in &pv.rows {
            let (Some(at), "timed") = (r.at_ms, r.trigger) else {
                continue;
            };
            if at >= now {
                continue;
            }
            let Some(Entry::Block(b)) = pl.entries.get(r.position).filter(|_| r.depth == 0) else {
                continue;
            };
            let Some(t) = &b.timed else { continue };
            let window = t.late_window_s.unwrap_or(DEFAULT_LATE_WINDOW_S);
            let late_by = (now - at) / 1000;
            let (policy, run) = match t.late_policy {
                LatePolicy::Skip => ("skip", false),
                LatePolicy::RunNow => ("run_now", true),
                LatePolicy::RunIfWithin => ("run_if_within", late_by <= window),
                LatePolicy::Default if stopping(&b.kind) => ("skip", false),
                LatePolicy::Default => ("run_if_within", late_by <= window),
            };
            pv.past.push(Past {
                block_id: b.id,
                name: b.name.clone(),
                kind: b.kind.type_name().into(),
                at_ms: at,
                late_by_s: late_by,
                policy,
                action: if run { "run" } else { "skip" },
            });
            let p = Programming::past_at_start(
                playlist_id,
                b,
                format!(
                    "`{}` is {} s in the past at this start; its late policy ({policy}) will {}",
                    b.name,
                    late_by,
                    if run { "run it now" } else { "skip it" }
                ),
            );
            pv.warnings.push((b.id, p.code, p.message));
        }
    }
    // A relative block that falls after an absolute stop or end can never fire.
    let abs_end: Option<(i64, i64)> = pv
        .rows
        .iter()
        .filter(|r| r.depth == 0 && r.trigger == "timed")
        .filter_map(|r| {
            let Entry::Block(b) = pl.entries.get(r.position)? else {
                return None;
            };
            let t = b.timed.as_ref()?;
            if stopping(&b.kind) && matches!(t.spec.when, When::Local { .. } | When::Date { .. }) {
                r.at_ms.map(|a| (a, b.id))
            } else {
                None
            }
        })
        .min();
    if let Some((end, id)) = abs_end {
        for r in &pv.rows {
            if r.depth == 0 && r.trigger == "timed" && r.block_id != id {
                let Some(Entry::Block(b)) = pl.entries.get(r.position) else {
                    continue;
                };
                if matches!(
                    b.timed.as_ref().map(|t| &t.spec.when),
                    Some(When::Relative { .. })
                ) && r.at_ms.is_some_and(|a| a > end)
                {
                    pv.warnings.push((
                        b.id,
                        "never_fires",
                        format!("`{}` falls after the stop at a fixed time and can never fire for this T0", b.name),
                    ));
                }
            }
        }
    }
    Some(pv)
}

fn walk(
    prog: &Programming,
    pl: &Playlist,
    t0: i64,
    zone: &Zone,
    depth: usize,
    pv: &mut Preview,
    seen: &mut Vec<i64>,
) {
    // In sequence, the cursor reaches a block at T0 while nothing before it takes time.
    let mut clock_known = true;
    for (position, e) in pl.entries.iter().enumerate() {
        let Entry::Block(b) = e else {
            if takes_time(e) {
                clock_known = false;
            }
            continue;
        };
        if matches!(b.kind, BlockKind::InsertEvery(_)) {
            continue;
        }
        let trigger = match b.trigger {
            Trigger::Timed => "timed",
            Trigger::Sequence => "sequence",
            Trigger::First => "first",
            Trigger::Standing => "standing",
        };
        let (at, note, when) = match &b.timed {
            Some(t) => {
                let (at, note) = timed_at(b, zone, t0).map_or((None, None), |(a, n)| (Some(a), n));
                (at, note, t.spec.describe())
            }
            None => (
                (clock_known && depth == 0 && !matches!(b.kind, BlockKind::Start(_))).then_some(t0),
                None,
                match b.kind {
                    BlockKind::Start(_) => "start".to_string(),
                    _ => "in sequence".to_string(),
                },
            ),
        };
        pv.rows.push(Row {
            position,
            depth,
            block_id: b.id,
            name: b.name.clone(),
            kind: b.kind.type_name().to_string(),
            trigger,
            at_ms: at,
            when,
            note,
        });
        if b.enabled
            && b.timed.is_none()
            && b.kind.is_flow()
            && !matches!(b.kind, BlockKind::Start(_))
        {
            // A flow block in sequence takes the cursor beyond T0 unless it takes no time.
            if takes_time(e) || matches!(b.kind, BlockKind::Play(_)) {
                clock_known =
                    clock_known && matches!(&b.kind, BlockKind::Play(p) if p.until == Until::Next);
            }
        }
        if let BlockKind::Play(pb) = &b.kind {
            if let Source::Playlist(child) = pb.source {
                if !seen.contains(&child) && depth < 3 {
                    if let Some(c) = prog.playlists.get(&child) {
                        seen.push(child);
                        // Its own times count from its own start, which is not known here.
                        walk_nested(prog, c, depth + 1, pv, seen, b.id);
                        seen.pop();
                    }
                }
            }
        }
    }
}

fn walk_nested(
    prog: &Programming,
    pl: &Playlist,
    depth: usize,
    pv: &mut Preview,
    seen: &mut Vec<i64>,
    parent: i64,
) {
    for (position, e) in pl.entries.iter().enumerate() {
        let Entry::Block(b) = e else { continue };
        if matches!(b.kind, BlockKind::InsertEvery(_) | BlockKind::Start(_)) {
            continue;
        }
        pv.rows.push(Row {
            position,
            depth,
            block_id: b.id,
            name: b.name.clone(),
            kind: b.kind.type_name().to_string(),
            trigger: match b.trigger {
                Trigger::Timed => "timed",
                _ => "sequence",
            },
            at_ms: None,
            when: match &b.timed {
                Some(t) => format!("{} (from the start of `{}`)", t.spec.describe(), pl.name),
                None => "in sequence".into(),
            },
            note: Some(format!("nested in block {parent}")),
        });
        if let BlockKind::Play(pb) = &b.kind {
            if let Source::Playlist(child) = pb.source {
                if !seen.contains(&child) && depth < 4 {
                    if let Some(c) = prog.playlists.get(&child) {
                        seen.push(child);
                        walk_nested(prog, c, depth + 1, pv, seen, b.id);
                        seen.pop();
                    }
                }
            }
        }
    }
}

impl Preview {
    pub fn to_json(&self, zone: &Zone) -> Value {
        let iso = |ms: i64| zone.local_string(ms);
        json!({
            "blocks": self.rows.iter().map(|r| json!({
                "position": r.position + 1, "depth": r.depth, "block_id": r.block_id, "name": r.name,
                "type": r.kind, "trigger": r.trigger,
                "at": r.at_ms.map(&iso), "at_ms": r.at_ms, "when": r.when, "note": r.note,
            })).collect::<Vec<_>>(),
            "silent_gaps": self.gaps.iter().map(|g| json!({
                "from": iso(g.from_ms), "to": iso(g.to_ms), "seconds": g.seconds,
                "before_block": g.before_block, "block": g.name,
                "message": format!("silent for {} s before `{}`", g.seconds, g.name),
            })).collect::<Vec<_>>(),
            "past": self.past.iter().map(|p| json!({
                "block_id": p.block_id, "name": p.name, "type": p.kind, "at": iso(p.at_ms),
                "late_by_s": p.late_by_s, "policy": p.policy, "action": p.action,
            })).collect::<Vec<_>>(),
            "warnings": self.warnings.iter().map(|(b, c, m)| json!({
                "severity": "warning", "code": c, "block": b, "message": m,
            })).collect::<Vec<_>>(),
        })
    }
}

/// The occurrences of a playlist's `manual` start block around a time, for the "expected" line.
pub fn nominal_occurrences(
    prog: &Programming,
    playlist_id: i64,
    zone: &Zone,
    after_ms: i64,
    upto_ms: i64,
) -> Vec<(i64, i64)> {
    let Some(pl) = prog.playlists.get(&playlist_id) else {
        return vec![];
    };
    let Some((b, sb)) = pl.start_block() else {
        return vec![];
    };
    let mut v: Vec<(i64, i64)> = sb
        .schedule
        .iter()
        .flat_map(|s| s.occurrences(zone, None, after_ms, upto_ms))
        .map(|t| (b.id, t))
        .collect();
    v.sort_by_key(|x| x.1);
    v.dedup();
    let _ = Anchor::Manual;
    v
}
