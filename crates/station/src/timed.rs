//! Timed blocks, scheduled starts and the station blocks (architecture 4.4 to 4.6, 1.5, 1.6; milestone M3).
//!
//! A timed block has a **target** `T` (an instant, from its [`TimeSpec`]) and, for the blocks that act at a
//! boundary (`insert`, `play`, `start_playlist`, a scheduled start), a **window** `[T - early, T + late]`.
//! The scheduler is a plain state machine on a wall time it is given, so everything here runs on the
//! virtual clock of the tests and the soak as it does on the air.
//!
//! * **Planning.** While the queue is dealt, an *event* whose window is within the planning horizon (20
//!   minutes) steers the picks: among the deck's candidates the one whose predicted boundary lands in the
//!   window, closest to `T`, is dealt (a single item, then pairs, then triples, bounded; the no-repeat
//!   rules are relaxed before fit is given up, and the block's `fill` category may close the last gap).
//!   When the slot being dealt starts inside the window the event takes it (the ID, the new playlist's
//!   first item, the `play` source's first item). The entry carries a [`TimedTag`].
//! * **Hard fallback.** An entry whose tag has `hard_at_ms` is cut in at that instant if its natural
//!   boundary falls later: the station plans the boundary as a hard-time transition (2.5). The scheduler
//!   guarantees an entry is at the head of the queue by then ([`Scheduler::ensure_events`]).
//! * **Acts.** A timed `stop how=now`, `fade_stop` and `end` act at `T` whatever is playing
//!   ([`Scheduler::due_acts`]); the station carries them out on exact frames.
//! * **The block log** (5.3) holds one row per block and target, so a block fires once per target even if
//!   the wall clock steps back; a step forward fires what it skipped within the last five minutes once,
//!   now, and logs the rest `missed` ([`Scheduler::on_clock_step`]).

use crate::catalog::{Catalog, TrackMeta};
use crate::deck::Unit;
use crate::programming::{
    Anchor, Block, BlockKind, BoundaryMode, Entry, LatePolicy, Playlist, Source, StartHow, StopHow,
    Until,
};
use crate::sched::{DeckRef, Origin, QEntry, Scheduler, Timing};
use crate::side::{SideAction, SIDE_AHEAD_MS};
use common::timespec::TimeSpec;
use library::playlists::Mode;
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;

/// A timed block enters the planning horizon this long before its window (4.6).
pub const HORIZON_MS: i64 = 20 * 60_000;
/// How long before its hard time an event that has no entry yet is forced to the head of the queue.
pub const PREP_MS: i64 = 15_000;
/// A clock step forward fires the blocks it skipped within this long of the old time, once, now (1.5).
pub const JUMP_FIRE_MS: i64 = 5 * 60_000;
/// Block-log rows older than this are dropped.
const LOG_KEEP_MS: i64 = 72 * 3_600_000;
/// How far ahead events are listed: the plan is dealt half an hour deep, and its last item can be a long one.
const EVENTS_AHEAD_MS: i64 = 3 * 3_600_000;
/// How far back an unfired event is still looked for (a station that was down across its time).
const LOOKBACK_MS: i64 = 2 * 3_600_000;
/// The default late window of a timed block (4.9): `run_if_within` 60 s.
const DEFAULT_LATE_WINDOW_S: i64 = 60;
/// Candidates examined per step of the fit search, and the work done in one search.
const FIT_CANDIDATES: usize = 40;
const FIT_INNER: usize = 12;
const FIT_BUDGET: usize = 200_000;

/// What a queue entry owes to the timed block that put it there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimedTag {
    pub block_id: i64,
    pub run_id: Option<i64>,
    pub name: String,
    /// `insert`, `play`, `switch` (a `start_playlist` block), `start` (a scheduled start) or `oneshot`.
    pub kind: String,
    pub target_ms: i64,
    pub lo_ms: i64,
    pub hi_ms: i64,
    pub mode: BoundaryMode,
    /// The entry is cut in at this instant when its natural boundary falls later.
    pub hard_at_ms: Option<i64>,
    /// The entry was placed late (a restart or a clock step); it acts as soon as it can.
    #[serde(default)]
    pub late: bool,
    /// The playlist entry (position) of the block, for a program block.
    #[serde(default)]
    pub entry_idx: Option<usize>,
    /// The cursor level of the block (0 is the playlist the program started with).
    #[serde(default)]
    pub depth: Option<usize>,
}

/// One row of the block log (5.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogRow {
    pub run_id: Option<i64>,
    pub block_id: i64,
    pub name: String,
    pub kind: String,
    pub target_ms: i64,
    pub planned_ms: i64,
    pub fired_ms: Option<i64>,
    /// `fired`, `soft`, `hard`, `late_run`, `skipped`, `passed`, `missed` or `failed`.
    pub how: String,
    pub deviation_ms: Option<i64>,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BlockLog {
    pub rows: Vec<LogRow>,
}

impl BlockLog {
    pub fn has(&self, run_id: Option<i64>, block_id: i64, target_ms: i64) -> bool {
        self.rows
            .iter()
            .rev()
            .any(|r| r.block_id == block_id && r.target_ms == target_ms && r.run_id == run_id)
    }

    /// Add a row unless the block already has one for this target (the once-per-target index of 5.3).
    pub fn push(&mut self, row: LogRow) -> bool {
        if self.has(row.run_id, row.block_id, row.target_ms) {
            return false;
        }
        self.rows.push(row);
        true
    }

    fn trim(&mut self, now_ms: i64) {
        let cut = now_ms - LOG_KEEP_MS;
        if self.rows.first().is_some_and(|r| r.target_ms < cut) {
            self.rows.retain(|r| r.target_ms >= cut);
        }
    }

    /// Rows in a range of targets, newest last.
    pub fn between(&self, from_ms: i64, to_ms: i64) -> Vec<&LogRow> {
        self.rows
            .iter()
            .filter(|r| r.target_ms >= from_ms && r.target_ms < to_ms)
            .collect()
    }
}

/// A one-shot scheduled change (7.2): start a playlist once, at a time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OneShot {
    pub id: i64,
    pub playlist_id: i64,
    pub at_ms: i64,
    pub how: StartHow,
    pub mode: BoundaryMode,
    pub early_s: i64,
    pub late_s: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvOrigin {
    Station,
    Program,
    Start,
    OneShot,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EvKind {
    Insert(Source),
    Play(Source),
    /// `start_playlist`, a scheduled start or a one-shot: the program is replaced.
    Switch {
        playlist_id: i64,
        how: StartHow,
    },
    /// `stop how=now` or `fade_stop`: ends the audio at `T` (mid-track).
    StopNow {
        fade_ms: i64,
    },
    /// `stop how=boundary` or `after_current`: the item on air plays to its end.
    StopBoundary {
        fade_ms: i64,
        hard_after_late: bool,
    },
    End,
    /// A side effect (`osc_out`, `midi_out`, `http_request`) at `T`.
    Side(SideAction),
    /// `set_gain`: the master gain ramps to `gain_db` (never above 0).
    Gain {
        gain_db: f64,
        ramp_ms: i64,
    },
    /// `set_mute`.
    Mute {
        on: bool,
        ramp_ms: i64,
    },
    /// `fade_stop`: the master gain ramps to silence over `ms` (2.4), then every voice stops.
    FadeStop {
        ms: i64,
    },
}

impl EvKind {
    /// Acts at `T` whatever is playing and never puts an item in the queue.
    pub fn is_act(&self) -> bool {
        matches!(
            self,
            EvKind::StopNow { .. }
                | EvKind::StopBoundary { .. }
                | EvKind::End
                | EvKind::Side(_)
                | EvKind::Gain { .. }
                | EvKind::Mute { .. }
                | EvKind::FadeStop { .. }
        )
    }

    /// Ends the audio or the program: the default late policy skips it (4.9).
    pub fn is_stopping(&self) -> bool {
        matches!(
            self,
            EvKind::StopNow { .. }
                | EvKind::StopBoundary { .. }
                | EvKind::End
                | EvKind::FadeStop { .. }
        )
    }

    /// A side block: acts at its time and never moves the cursor.
    pub fn is_side(&self) -> bool {
        matches!(
            self,
            EvKind::Side(_) | EvKind::Gain { .. } | EvKind::Mute { .. }
        )
    }
}

/// One occurrence of a timed block.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub block_id: i64,
    pub name: String,
    pub kind: EvKind,
    pub origin: EvOrigin,
    pub run_id: Option<i64>,
    pub entry_idx: Option<usize>,
    pub playlist: Option<i64>,
    pub target: i64,
    pub lo: i64,
    pub hi: i64,
    pub mode: BoundaryMode,
    pub priority: i64,
    pub flow: bool,
    pub fill: Option<Source>,
    pub late_policy: LatePolicy,
    pub late_window_s: Option<i64>,
    pub fallback_window_ms: Option<i64>,
    pub order: usize,
    pub type_name: String,
    /// The level of the cursor the block belongs to: 0 is the playlist the program started with, deeper
    /// ones are playlists nested in it (4.4). A parent's block beats a child's at the same moment.
    pub depth: usize,
    /// That level's own start, for the check that the level still exists as it was.
    pub level_t0: i64,
}

impl Event {
    fn kind_tag(&self) -> &'static str {
        match (&self.kind, self.origin) {
            (EvKind::Insert(_), _) => "insert",
            (EvKind::Play(_), _) => "play",
            (EvKind::Switch { .. }, EvOrigin::Start) => "start",
            (EvKind::Switch { .. }, EvOrigin::OneShot) => "oneshot",
            (EvKind::Switch { .. }, _) => "switch",
            _ => "act",
        }
    }

    fn rank(&self) -> (u8, u8, usize, i64, usize) {
        (
            u8::from(!self.flow),
            match self.origin {
                EvOrigin::Program => 0,
                EvOrigin::OneShot | EvOrigin::Start => 1,
                EvOrigin::Station => 2,
            },
            self.depth,
            -self.priority,
            self.order,
        )
    }
}

#[derive(Default)]
pub struct EventCache {
    key: Option<(i64, i64, usize, usize, usize)>,
    events: Arc<Vec<Event>>,
}

/// What the station must do at an exact time (a timed `stop`, `fade_stop` or `end`).
#[derive(Debug, Clone, PartialEq)]
pub struct TimedAct {
    pub block_id: i64,
    pub name: String,
    pub target_ms: i64,
    pub kind: ActKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActKind {
    /// Fade everything that sounds over `fade_ms` from the target and stop (`stop how=now`, `fade_stop`).
    Stop { fade_ms: i64 },
    /// The item on air plays to its end and nothing follows (`stop how=boundary`).
    Hold,
    /// `Hold` was not enough: stop now (the hard stop 30 s later of `boundary`).
    HardStop { fade_ms: i64 },
    /// The program ended; the queue was cleared for the next one.
    EndProgram,
    /// `fade_stop` (2.4): ramp the master gain to silence over `fade_ms` from the target, then stop every
    /// voice and leave the station stopped.
    FadeStop { fade_ms: i64 },
    /// `set_gain`.
    Gain { gain_db: f64, ramp_ms: i64 },
    /// `set_mute`.
    Mute { on: bool, ramp_ms: i64 },
    /// A message for the outside world, to be sent at the target.
    Side(SideAction),
}

/// A line of `schedule/upcoming` (7.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Upcoming {
    pub block_id: i64,
    pub name: String,
    pub kind: String,
    pub source: String,
    pub target_ms: i64,
    /// When the block is expected to act: the queue's predicted start of its entry, else `target_ms`.
    pub predicted_ms: i64,
    pub mode: BoundaryMode,
    pub lo_ms: i64,
    pub hi_ms: i64,
    pub planned: bool,
    /// `program`, `station`, `start` or `oneshot`.
    pub origin: &'static str,
}

fn event_kind(b: &Block) -> Option<EvKind> {
    Some(match &b.kind {
        BlockKind::Insert { source } => EvKind::Insert(source.clone()),
        BlockKind::Play(p) => EvKind::Play(p.source.clone()),
        BlockKind::StartPlaylist { playlist_id, how } => EvKind::Switch {
            playlist_id: *playlist_id,
            how: *how,
        },
        BlockKind::Stop { how, fade_ms } => match how {
            StopHow::Now => EvKind::StopNow { fade_ms: *fade_ms },
            StopHow::Boundary => EvKind::StopBoundary {
                fade_ms: *fade_ms,
                hard_after_late: true,
            },
            StopHow::AfterCurrent => EvKind::StopBoundary {
                fade_ms: *fade_ms,
                hard_after_late: false,
            },
        },
        BlockKind::FadeStop { seconds } => EvKind::FadeStop {
            ms: (seconds * 1000.0).round() as i64,
        },
        BlockKind::End => EvKind::End,
        BlockKind::Side(a) => EvKind::Side(a.clone()),
        BlockKind::SetGain { gain_db, ramp_ms } => EvKind::Gain {
            gain_db: *gain_db,
            ramp_ms: *ramp_ms,
        },
        BlockKind::SetMute { on, ramp_ms } => EvKind::Mute {
            on: *on,
            ramp_ms: *ramp_ms,
        },
        _ => return None,
    })
}

fn window(mode: BoundaryMode, target: i64, early_ms: i64, late_ms: i64) -> (i64, i64) {
    match mode {
        BoundaryMode::SoftThenHard => (target - early_ms, target + late_ms),
        BoundaryMode::Soft | BoundaryMode::HardWait => (target, target + late_ms),
        BoundaryMode::Hard => (target, target),
    }
}

/// The cost of a boundary at `end` for a window: how far past the end of the window it falls (a cut for a
/// hard fallback, lateness otherwise), and how far from the target. Inside the window the first is 0.
fn fit_cost(end: i64, lo: i64, hi: i64, target: i64) -> f64 {
    let over = if end > hi { end - hi } else { 0 };
    let _ = lo;
    over as f64 * 1e7 + (end - target).abs() as f64
}

struct FitCtx<'a> {
    cat: &'a Catalog,
    timing: &'a dyn Timing,
    lo: i64,
    hi: i64,
    target: i64,
    /// What the event's own first item is like (its length decides the boundary into it).
    ev_meta: &'a TrackMeta,
    liner: Option<(&'a TrackMeta, u32, String, Vec<String>)>,
    fill: Vec<&'a TrackMeta>,
    budget: Cell<usize>,
    memo: std::cell::RefCell<HashMap<(i64, i64), i64>>,
}

impl FitCtx<'_> {
    fn adv(&self, a: &TrackMeta, b: &TrackMeta) -> i64 {
        let key = (a.item_id, b.item_id);
        if let Some(v) = self.memo.borrow().get(&key) {
            return *v;
        }
        let v = self.timing.advance_ms(a, b, false);
        self.memo.borrow_mut().insert(key, v);
        v
    }

    /// The best score of a chain that starts with `x` (placed at `start_x`), with `depth` more picks
    /// allowed, `count` music items since the last liner (counting `x`), and `used` already in the chain.
    fn after(
        &self,
        x: &TrackMeta,
        start_x: i64,
        count: u32,
        depth: usize,
        used: &mut Vec<i64>,
        cands: &[&TrackMeta],
    ) -> Option<f64> {
        if self.budget.get() == 0 {
            return None;
        }
        self.budget.set(self.budget.get() - 1);
        // The boundary into the event: if it is inside the window or past it, the event takes it.
        let b_ev = start_x + self.adv(x, self.ev_meta);
        if b_ev >= self.lo {
            return Some(fit_cost(b_ev, self.lo, self.hi, self.target));
        }
        // Before the window. A liner that is due takes the slot first.
        if let Some((l, n, _, _)) = &self.liner {
            if count >= *n {
                let start_l = start_x + self.adv(x, l);
                return self.after(l, start_l, 0, depth, used, cands);
            }
        }
        if depth == 0 {
            return None;
        }
        let mut best: Option<f64> = None;
        let mut tried = 0;
        // Fill items may close the gap (4.6, step 3): they are not music and do not count.
        for f in &self.fill {
            let s = start_x + self.adv(x, f);
            if let Some(v) = self.after(f, s, count, depth - 1, used, cands) {
                best = Some(best.map_or(v, |b| b.min(v)));
            }
        }
        for y in cands {
            if used.contains(&y.item_id) {
                continue;
            }
            tried += 1;
            if tried > FIT_INNER {
                break;
            }
            let s = start_x + self.adv(x, y);
            used.push(y.item_id);
            let c =
                if self.liner.as_ref().is_some_and(|(_, _, kind, resets)| {
                    y.kind == *kind && !resets.contains(&y.kind)
                }) {
                    count + 1
                } else {
                    count
                };
            let v = self.after(y, s, c, depth - 1, used, cands);
            used.pop();
            if let Some(v) = v {
                best = Some(best.map_or(v, |b| b.min(v)));
            }
        }
        best
    }
}

impl Scheduler {
    // ---- the events -------------------------------------------------------------------------------------

    /// The unfired occurrences of every armed timed block from a while ago to the planning horizon.
    pub(crate) fn events(&mut self, now_ms: i64) -> Arc<Vec<Event>> {
        let bucket = now_ms.div_euclid(5000);
        let key = (
            bucket,
            self.st.program.as_ref().map_or(0, |p| p.run_id),
            Arc::as_ptr(&self.prog) as usize,
            self.st.oneshots.len(),
            {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                self.levels().hash(&mut h);
                h.finish() as usize
            },
        );
        if self.ev_cache.key == Some(key) {
            return self.ev_cache.events.clone();
        }
        let after = bucket * 5000 - LOOKBACK_MS;
        let upto = bucket * 5000 + EVENTS_AHEAD_MS;
        let v = Arc::new(self.build_events(after, upto));
        self.ev_cache = EventCache {
            key: Some(key),
            events: v.clone(),
        };
        v
    }

    #[allow(clippy::too_many_arguments)]
    fn block_events(
        &self,
        b: &Block,
        origin: EvOrigin,
        run_id: Option<i64>,
        t0: Option<i64>,
        entry_idx: Option<usize>,
        playlist: Option<i64>,
        order: usize,
        depth: usize,
        after: i64,
        upto: i64,
        out: &mut Vec<Event>,
    ) {
        let (Some(t), true) = (&b.timed, b.enabled) else {
            return;
        };
        let Some(kind) = event_kind(b) else { return };
        let mode = match (&kind, t.mode) {
            (k, _) if k.is_act() => BoundaryMode::Hard,
            (_, m) => m,
        };
        let (early, late) = if kind.is_act() {
            (0, 0)
        } else {
            (t.early_ms(), t.late_ms())
        };
        for target in t.spec.occurrences(&self.cfg.zone, t0, after, upto) {
            if self.st.log.has(run_id, b.id, target) {
                continue;
            }
            let (lo, hi) = window(mode, target, early, late);
            let mode = match &kind {
                EvKind::Switch {
                    how: StartHow::Now, ..
                } => BoundaryMode::Hard,
                _ => mode,
            };
            let (lo, hi) = if mode == BoundaryMode::Hard {
                (target, target)
            } else {
                (lo, hi)
            };
            out.push(Event {
                block_id: b.id,
                name: b.name.clone(),
                kind: kind.clone(),
                origin,
                run_id,
                entry_idx,
                playlist,
                target,
                lo,
                hi,
                mode,
                priority: b.priority,
                flow: b.kind.is_flow(),
                fill: t.fill.clone(),
                late_policy: t.late_policy,
                late_window_s: t.late_window_s,
                fallback_window_ms: None,
                order,
                type_name: b.kind.type_name().to_string(),
                depth,
                level_t0: t0.unwrap_or(0),
            });
        }
    }

    /// The levels of the cursor, outermost first: `(playlist, T0 of the level)`.
    pub(crate) fn levels(&self) -> Vec<(i64, i64)> {
        match &self.st.program {
            None => vec![],
            Some(p) => p
                .stack
                .iter()
                .map(|c| (c.playlist_id, c.t0_ms))
                .chain(std::iter::once((p.playlist_id, p.t0_ms)))
                .collect(),
        }
    }

    /// Is the level an event belongs to still the one it was made for?
    pub(crate) fn event_valid(&self, ev: &Event) -> bool {
        if ev.origin != EvOrigin::Program {
            return true;
        }
        self.levels()
            .get(ev.depth)
            .is_some_and(|(pid, t0)| Some(*pid) == ev.playlist && *t0 == ev.level_t0)
    }

    pub(crate) fn build_events(&self, after: i64, upto: i64) -> Vec<Event> {
        let mut out = Vec::new();
        let prog = self.st.program.clone();
        let levels = self.levels();
        let root = levels
            .first()
            .and_then(|(id, _)| self.prog.playlists.get(id));
        if let (Some(pr), Some(root)) = (&prog, root) {
            for (depth, (pid, t0)) in levels.iter().enumerate() {
                let Some(pl) = self.prog.playlists.get(pid) else {
                    continue;
                };
                for (i, e) in pl.entries.iter().enumerate() {
                    if let Entry::Block(b) = e {
                        self.block_events(
                            b,
                            EvOrigin::Program,
                            Some(pr.run_id),
                            Some(*t0),
                            Some(i),
                            Some(pl.id),
                            i,
                            depth,
                            after,
                            upto,
                            &mut out,
                        );
                    }
                }
            }
            if root.station_blocks {
                for (i, b) in self.prog.station_blocks.iter().enumerate() {
                    self.block_events(
                        b,
                        EvOrigin::Station,
                        None,
                        None,
                        None,
                        None,
                        1000 + i,
                        0,
                        after,
                        upto,
                        &mut out,
                    );
                }
            }
        } else if prog.is_none() {
            for (i, b) in self.prog.station_blocks.iter().enumerate() {
                self.block_events(
                    b,
                    EvOrigin::Station,
                    None,
                    None,
                    None,
                    None,
                    1000 + i,
                    0,
                    after,
                    upto,
                    &mut out,
                );
            }
        }
        // Scheduled starts: every enabled playlist whose `start` block is anchored `absolute`.
        for (pid, b, sb) in self.prog.scheduled_starts() {
            if sb.anchor != Anchor::Absolute {
                continue;
            }
            for spec in &sb.schedule {
                for target in spec.occurrences(&self.cfg.zone, None, after, upto) {
                    if self.st.log.has(None, b.id, target) {
                        continue;
                    }
                    let (lo, hi) = window(sb.mode, target, sb.early_s * 1000, sb.late_s * 1000);
                    out.push(Event {
                        block_id: b.id,
                        name: b.name.clone(),
                        kind: EvKind::Switch {
                            playlist_id: pid,
                            how: sb.how,
                        },
                        origin: EvOrigin::Start,
                        run_id: None,
                        entry_idx: None,
                        playlist: Some(pid),
                        target,
                        lo,
                        hi,
                        mode: sb.mode,
                        priority: b.priority,
                        flow: true,
                        fill: None,
                        late_policy: LatePolicy::RunIfWithin,
                        late_window_s: Some(sb.late_s),
                        fallback_window_ms: Some(sb.late_s * 1000),
                        order: 500 + pid as usize,
                        type_name: "start".into(),
                        depth: 0,
                        level_t0: 0,
                    });
                }
            }
        }
        for o in &self.st.oneshots {
            if o.at_ms <= after || o.at_ms > upto {
                continue;
            }
            let id = -o.id;
            if self.st.log.has(None, id, o.at_ms) {
                continue;
            }
            let (lo, hi) = window(o.mode, o.at_ms, o.early_s * 1000, o.late_s * 1000);
            out.push(Event {
                block_id: id,
                name: format!("one-shot change {}", o.id),
                kind: EvKind::Switch {
                    playlist_id: o.playlist_id,
                    how: o.how,
                },
                origin: EvOrigin::OneShot,
                run_id: None,
                entry_idx: None,
                playlist: Some(o.playlist_id),
                target: o.at_ms,
                lo,
                hi,
                mode: o.mode,
                priority: 100,
                flow: true,
                fill: None,
                late_policy: LatePolicy::RunIfWithin,
                late_window_s: Some(o.late_s),
                fallback_window_ms: Some(o.late_s * 1000),
                order: 400,
                type_name: "start".into(),
                depth: 0,
                level_t0: 0,
            });
        }
        out.sort_by_key(|e| (e.target, e.rank()));
        out
    }

    pub(crate) fn event_logged(&self, ev: &Event) -> bool {
        self.st.log.has(ev.run_id, ev.block_id, ev.target)
    }

    pub(crate) fn event_placed(&self, ev: &Event, queue: &[QEntry]) -> bool {
        queue.iter().chain(self.st.on_air.iter()).any(|e| {
            e.timed.as_ref().is_some_and(|t| {
                t.block_id == ev.block_id && t.target_ms == ev.target && t.run_id == ev.run_id
            })
        })
    }

    pub(crate) fn log_event(&mut self, ev: &Event, how: &str, fired: Option<i64>, detail: &str) {
        let row = LogRow {
            run_id: ev.run_id,
            block_id: ev.block_id,
            name: ev.name.clone(),
            kind: ev.type_name.clone(),
            target_ms: ev.target,
            planned_ms: ev.target,
            fired_ms: fired,
            how: how.into(),
            deviation_ms: fired.map(|f| f - ev.target),
            detail: detail.into(),
        };
        if self.st.log.push(row) {
            self.ev_cache = EventCache::default();
        }
    }

    /// Does the late policy of the block let it act `late_by` ms after its target (4.9)?
    fn late_allowed(&self, ev: &Event, now_ms: i64) -> bool {
        if self.st.jump_fire.contains(&(ev.block_id, ev.target)) {
            return true;
        }
        let late_by = now_ms - ev.target;
        let default_window = match ev.origin {
            EvOrigin::Start | EvOrigin::OneShot => ev.fallback_window_ms.unwrap_or(300_000),
            _ => DEFAULT_LATE_WINDOW_S * 1000,
        };
        let window = ev.late_window_s.map_or(default_window, |s| s * 1000);
        match ev.late_policy {
            LatePolicy::RunNow => true,
            LatePolicy::Skip => false,
            LatePolicy::RunIfWithin => late_by <= window,
            LatePolicy::Default => {
                if ev.kind.is_stopping() {
                    false
                } else {
                    late_by <= window
                }
            }
        }
    }

    // ---- helpers over the queue ---------------------------------------------------------------------------

    /// The item the queue's last entry (or the one on air) is, and when it starts.
    pub(crate) fn tail<'c>(
        &self,
        queue: &[QEntry],
        cat: &'c Catalog,
    ) -> Option<(&'c TrackMeta, i64)> {
        let (id, start) = queue
            .last()
            .map(|e| (e.item_id, e.planned_start_ms))
            .or_else(|| {
                self.st
                    .on_air
                    .as_ref()
                    .map(|e| (e.item_id, self.st.on_air_start_ms))
            })?;
        Some((cat.meta(id)?, start?))
    }

    /// The meta of the item a source would deal next (its deck's next unit, else its first).
    fn peek_meta<'c>(&self, src: &Source, cat: &'c Catalog) -> Option<&'c TrackMeta> {
        let (key, pool, _) = self.source_pool(src, cat)?;
        let first = self
            .st
            .decks
            .get(&key)
            .and_then(|d| d.remaining().into_iter().next())
            .and_then(|u| match u {
                Unit::Item(i) => Some(i),
                Unit::Album(k) => cat.album_tracks(&k).first().map(|t| t.meta.item_id),
            });
        let id = first.or_else(|| {
            pool.iter().find_map(|(u, _)| match u {
                Unit::Item(i) => Some(*i),
                Unit::Album(k) => cat.album_tracks(k).first().map(|t| t.meta.item_id),
            })
        })?;
        cat.meta(id)
    }

    fn tag_for(&self, ev: &Event, hard_at: Option<i64>, late: bool) -> TimedTag {
        TimedTag {
            block_id: ev.block_id,
            run_id: ev.run_id,
            name: ev.name.clone(),
            kind: ev.kind_tag().into(),
            target_ms: ev.target,
            lo_ms: ev.lo,
            hi_ms: ev.hi,
            mode: ev.mode,
            hard_at_ms: hard_at,
            late,
            entry_idx: ev.entry_idx,
            depth: (ev.origin == EvOrigin::Program).then_some(ev.depth),
        }
    }

    // ---- dealing for events ---------------------------------------------------------------------------------

    /// Deal one entry from `src` as a rotation of its own deck (a `play` source), with the rules of a
    /// source playlist when it is one.
    pub(crate) fn deal_fill(
        &mut self,
        src: &Source,
        queue: &[QEntry],
        slot_start: i64,
        cat: &Catalog,
    ) -> Option<Vec<QEntry>> {
        let (key, pool, pid) = self.source_pool(src, cat)?;
        if pool.is_empty() {
            return None;
        }
        let pl = pid.and_then(|p| self.prog.playlists.get(&p)).cloned();
        let chain = self.chain(queue, cat);
        let (pick, relaxed) = self.deal(&key, &pool, pl.as_ref(), &chain, slot_start, cat)?;
        if relaxed {
            self.relaxed_picks += 1;
        }
        let dref = DeckRef {
            key,
            gen: pick.gen,
            seq: pick.seq,
        };
        let mut out = match &pick.unit {
            Unit::Item(id) => vec![QEntry::new(self.take_id(), *id, Origin::Rotation)],
            Unit::Album(k) => self.expand_album(k, true, Origin::Rotation, cat),
        };
        for e in &mut out {
            e.deck = Some(dref.clone());
            if relaxed {
                e.flags.push("rule_relaxed".into());
            }
        }
        Some(out)
    }

    /// Place the event's own entry. `None` when its source has nothing playable (logged `skipped`).
    #[allow(clippy::too_many_arguments)]
    fn place_event(
        &mut self,
        ev: &Event,
        queue: &[QEntry],
        slot_start: i64,
        hard_at: Option<i64>,
        late: bool,
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> Option<Vec<QEntry>> {
        if ev.origin == EvOrigin::Program
            && matches!(ev.kind, EvKind::Play(_) | EvKind::Switch { .. })
            && self
                .st
                .program
                .as_ref()
                .is_some_and(|p| ev.depth < p.stack.len())
        {
            self.unwind_to(ev.depth);
        }
        let tag = self.tag_for(ev, hard_at, late);
        match &ev.kind {
            EvKind::Insert(src) => {
                let (key, pool, _) = self.source_pool(src, cat)?;
                if pool.is_empty() {
                    self.log_event(ev, "skipped", None, "its source has nothing playable");
                    return None;
                }
                let chain = self.chain(queue, cat);
                let (pick, _) = self.deal(&key, &pool, None, &chain, slot_start, cat)?;
                let Unit::Item(id) = pick.unit else {
                    return None;
                };
                let mut e = QEntry::new(self.take_id(), id, Origin::Insert);
                e.block_id = Some(ev.block_id);
                e.playlist_id = self.st.program.as_ref().map(|p| p.playlist_id);
                e.deck = Some(DeckRef {
                    key,
                    gen: pick.gen,
                    seq: pick.seq,
                });
                e.timed = Some(tag);
                Some(vec![e])
            }
            EvKind::Play(src) => {
                let idx = ev.entry_idx;
                let before = self.st.program.clone();
                let pl = self.program_playlist().cloned();
                if let (Some(pr), Some(pl), Some(idx)) = (self.st.program.as_mut(), &pl, idx) {
                    let until_next = matches!(
                        pl.entries.get(idx),
                        Some(Entry::Block(Block { kind: BlockKind::Play(pb), .. })) if pb.until == Until::Next
                    );
                    pr.fill = Some(idx);
                    pr.halted = false;
                    pr.silenced = false;
                    pr.hold_until_ms = None;
                    if until_next {
                        pr.seq_pos = idx + 1;
                        pr.hold = None;
                    } else {
                        pr.seq_pos = idx;
                        pr.hold = Some(crate::sched::Hold {
                            since_ms: slot_start,
                            count: 0,
                        });
                    }
                }
                let Some(mut out) = self.deal_fill(src, queue, slot_start, cat) else {
                    self.st.program = before;
                    self.log_event(ev, "skipped", None, "its source has nothing playable");
                    return None;
                };
                let pid = self.st.program.as_ref().map(|p| p.playlist_id);
                let snap = self.snap();
                for e in &mut out {
                    e.playlist_id = pid;
                    e.prog_after = snap.clone();
                }
                out[0].origin = Origin::Block;
                out[0].block_id = Some(ev.block_id);
                out[0].timed = Some(tag);
                Some(out)
            }
            EvKind::Switch { playlist_id, how } => {
                let pid = *playlist_id;
                let ok = self
                    .prog
                    .playlists
                    .get(&pid)
                    .is_some_and(|p| p.enabled && !self.playable_units(p, cat).is_empty());
                if !ok {
                    self.log_event(ev, "skipped", None, "the playlist has nothing playable");
                    return None;
                }
                if matches!(ev.origin, EvOrigin::Start | EvOrigin::OneShot)
                    && self
                        .st
                        .program
                        .as_ref()
                        .is_some_and(|p| p.playlist_id == pid)
                    && self.st.on_air.is_some()
                {
                    self.log_event(ev, "skipped", None, "the playlist is already running");
                    return None;
                }
                let before = self.st.program.clone();
                self.start_program_with(pid, slot_start, "schedule", None);
                let mut out = self.generate_normal(queue, now_ms, cat, timing, true)?;
                let first = out.first_mut()?;
                first.prog_before = before;
                first.timed = Some(tag);
                first.origin = Origin::Block;
                first.block_id = Some(ev.block_id);
                if *how == StartHow::End {
                    first.transition = Some(crate::sched::END_TRANSITION.into());
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// The best-fit choice for a slot (4.6): among the deck's candidates, the one whose boundary lands in
    /// the event's window closest to `T`, looking one, two or three items ahead.
    #[allow(clippy::too_many_arguments)]
    fn fit_pick(
        &mut self,
        ev: &Event,
        queue: &[QEntry],
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> Option<Vec<QEntry>> {
        let program = self.st.program.clone()?;
        let pl = self.prog.playlists.get(&program.playlist_id)?.clone();
        // Only the rotation of a `shuffle` playlist or of a `play` fill is steered.
        let (key, pool, rules_pl): (String, Vec<(Unit, f64)>, Option<Playlist>) =
            match (pl.mode, program.fill) {
                (Mode::Shuffle, _) => (
                    format!("pl:{}", pl.id),
                    self.playable_units(&pl, cat),
                    Some(pl.clone()),
                ),
                (Mode::Sequential, Some(fi)) => {
                    let Some(Entry::Block(Block {
                        kind: BlockKind::Play(pb),
                        ..
                    })) = pl.entries.get(fi)
                    else {
                        return None;
                    };
                    let (k, p, pid) = self.source_pool(&pb.source, cat)?;
                    (k, p, pid.and_then(|i| self.prog.playlists.get(&i)).cloned())
                }
                _ => return None,
            };
        if pool.is_empty() {
            return None;
        }
        let (prev, prev_start) = self.tail(queue, cat)?;
        let ev_meta = match &ev.kind {
            EvKind::Insert(s) | EvKind::Play(s) => self.peek_meta(s, cat)?,
            EvKind::Switch { playlist_id, .. } => {
                let p = self.prog.playlists.get(playlist_id)?;
                let pool = self.playable_units(p, cat);
                let id = match &pool.first()?.0 {
                    Unit::Item(i) => *i,
                    Unit::Album(k) => cat.album_tracks(k).first()?.meta.item_id,
                };
                cat.meta(id)?
            }
            _ => return None,
        };
        // The liner that `insert_every` would put in (the first block that applies).
        let liner = self.insert_blocks().into_iter().next().and_then(|(_, ie)| {
            let (_, lpool, _) = self.source_pool(&ie.source, cat)?;
            let lmeta = lpool.iter().find_map(|(u, _)| match u {
                Unit::Item(i) => cat.meta(*i),
                _ => None,
            })?;
            Some((lmeta, ie.n, ie.count_kind.clone(), ie.resets_on.clone()))
        });
        let liner_count = liner.as_ref().map_or(0, |(_, _, _, _)| {
            self.insert_blocks()
                .into_iter()
                .next()
                .map_or(0, |(b, ie)| self.planned_counter(b, ie, queue, cat).count)
        });
        // The count `after` starts from is the one that includes the candidate.
        let fill_metas: Vec<&TrackMeta> = ev
            .fill
            .as_ref()
            .and_then(|s| self.source_pool(s, cat))
            .map(|(_, p, _)| {
                p.iter()
                    .filter_map(|(u, _)| match u {
                        Unit::Item(i) => cat.meta(*i),
                        _ => None,
                    })
                    .take(10)
                    .collect()
            })
            .unwrap_or_default();
        let ctx = FitCtx {
            cat,
            timing,
            lo: ev.lo,
            hi: ev.hi,
            target: ev.target,
            ev_meta,
            liner,
            fill: fill_metas,
            budget: Cell::new(FIT_BUDGET),
            memo: Default::default(),
        };
        let _ = ctx.cat;
        // The deck, brought up to date, and its rest of the pass in order.
        let k = self
            .rotation_of(
                &pool,
                rules_pl.as_ref().map_or(0, |p| p.no_repeat_minutes),
                cat,
            )
            .k;
        let mut deck = self.st.decks.remove(&key).unwrap_or_default();
        if !deck.prepare(&pool, k, &mut self.st.rng) {
            self.st.decks.insert(key, deck);
            return None;
        }
        let remaining = deck.remaining();
        let chain = self.chain(queue, cat);
        let mut off_pass: Vec<Unit> = pool
            .iter()
            .map(|(u, _)| u.clone())
            .filter(|u| !remaining.contains(u))
            .collect();
        {
            let last = |u: &Unit| -> i64 {
                let Some(first) = Self::unit_first_item(cat, u) else {
                    return i64::MIN;
                };
                chain
                    .recs
                    .iter()
                    .rev()
                    .find(|r| r.item_id == first)
                    .map_or(i64::MIN, |r| r.start_ms)
            };
            off_pass.sort_by_key(|u| last(u));
        }
        let slot_est = (prev_start + timing.advance_ms(prev, prev, false)).max(now_ms);
        let (eff_ms, sep) = match &rules_pl {
            Some(p) => {
                let r = self.rotation_of(&pool, p.no_repeat_minutes, cat);
                let e = (r.effective_minutes * 60_000.0).round() as i64;
                (
                    if e > 0 {
                        e + crate::sched::PREDICTION_SLACK_MS
                    } else {
                        e
                    },
                    p.separation.clone(),
                )
            }
            None => (0, Default::default()),
        };
        let horizon = slot_est
            - eff_ms
                .max(sep.title_minutes * 60_000)
                .max(sep.artist_minutes * 60_000)
                .max(sep.album_minutes * 60_000);
        let recent: Vec<&crate::sched::PlayRec> = chain
            .recs
            .iter()
            .filter(|r| r.start_ms > horizon - 1 && r.start_ms <= slot_est)
            .collect();
        let item_of = |u: &Unit| Self::unit_first_item(cat, u);
        let eligible = |u: &Unit, level: u8| -> bool {
            let Some(first) = item_of(u) else {
                return false;
            };
            let Some(m) = cat.meta(first) else {
                return false;
            };
            let within = |r: &&crate::sched::PlayRec, min: i64| {
                min > 0 && slot_est - r.start_ms < min * 60_000
            };
            // The same levels as `deal`: 0 all rules, 1 without the album rule, 2 also without the
            // artist rule, 3 also without the title rule, 4 without the item window as well.
            if level < 4
                && eff_ms > 0
                && recent
                    .iter()
                    .any(|r| r.item_id == first && slot_est - r.start_ms < eff_ms)
            {
                return false;
            }
            if level < 3
                && recent
                    .iter()
                    .any(|r| r.title_key == m.title_key() && within(r, sep.title_minutes))
            {
                return false;
            }
            if level < 2
                && !m.artist.is_empty()
                && recent.iter().any(|r| {
                    r.artist == crate::catalog::norm(&m.artist) && within(r, sep.artist_minutes)
                })
            {
                return false;
            }
            if level < 1
                && !m.album_key.is_empty()
                && recent
                    .iter()
                    .any(|r| r.album_key == m.album_key && within(r, sep.album_minutes))
            {
                return false;
            }
            true
        };
        // The lowest relaxation level that has a candidate landing in the window; failing that, the
        // best miss at the lowest level that has any.
        let mut chosen: Option<(Unit, u8)> = None;
        let mut miss: Option<(Unit, u8)> = None;
        for level in 0..=4u8 {
            // The rest of the pass first, then (in the order they last played, longest ago first) the
            // units already dealt this pass: a track that has not played within the window may play
            // again before the pass ends when that is what lands the boundary in the window.
            let cands: Vec<(&Unit, &TrackMeta)> = remaining
                .iter()
                .chain(off_pass.iter())
                .filter(|u| eligible(u, level))
                .filter_map(|u| match u {
                    Unit::Item(i) => cat.meta(*i).map(|m| (u, m)),
                    Unit::Album(_) => None,
                })
                .take(FIT_CANDIDATES)
                .collect();
            if cands.is_empty() {
                continue;
            }
            let metas: Vec<&TrackMeta> = cands.iter().map(|(_, m)| *m).collect();
            // Singles and pairs first; triples only when those do not fit.
            let mut best: Option<(f64, usize)> = None;
            for depth in [1usize, 2, 3, 4] {
                ctx.budget.set(FIT_BUDGET);
                best = None;
                for (i, (_, m)) in cands.iter().enumerate() {
                    let start_x = prev_start + ctx.adv(prev, m);
                    // The liner count counts the candidate when it is music of the counted kind.
                    let c = liner_count
                        + u32::from(
                            ctx.liner
                                .as_ref()
                                .is_some_and(|(_, _, kind, _)| m.kind == *kind),
                        );
                    let mut used = vec![m.item_id];
                    if let Some(v) = ctx.after(m, start_x, c, depth - 1, &mut used, &metas) {
                        if best.is_none_or(|(b, _)| v < b) {
                            best = Some((v, i));
                        }
                    }
                }
                if best.is_some_and(|(v, _)| v < 1e7) {
                    break;
                }
            }
            if let Some((v, i)) = best {
                if v < 1e7 {
                    chosen = Some((cands[i].0.clone(), level));
                    break;
                }
                if miss.is_none() {
                    miss = Some((cands[i].0.clone(), level));
                }
            }
        }
        let chosen = chosen.or(miss);
        let Some((unit, level)) = chosen else {
            self.st.decks.insert(key, deck);
            return None;
        };
        let on_pass = remaining.contains(&unit);
        let pick = if on_pass {
            deck.take_unit(&unit, level, k)
        } else {
            None
        };
        self.st.decks.insert(key.clone(), deck);
        let relaxed = level > 0;
        if relaxed {
            self.relaxed_picks += 1;
        }
        let Unit::Item(id) = unit else {
            return None;
        };
        let mut e = QEntry::new(self.take_id(), id, Origin::Rotation);
        e.playlist_id = Some(pl.id);
        if let Some(pick) = pick {
            e.deck = Some(DeckRef {
                key,
                gen: pick.gen,
                seq: pick.seq,
            });
        } else {
            e.flags.push("off_pass".into());
        }
        e.prog_after = self.snap();
        e.flags.push("fitted".into());
        if relaxed {
            e.flags.push("rule_relaxed".into());
        }
        Some(vec![e])
    }

    /// The timed part of dealing the next slot of the queue (see the module docs). `Gated`: nothing may
    /// be dealt (a timed stop or end falls before the slot ends).
    pub(crate) fn generate_timed(
        &mut self,
        queue: &[QEntry],
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> TimedGen {
        let Some(program) = self.st.program.clone() else {
            return TimedGen::Nothing;
        };
        let evs = self.events(now_ms);
        let tail = self.tail(queue, cat);
        let slot_est = match tail {
            Some((m, s)) => (s + timing.advance_ms(m, m, false)).max(now_ms),
            None => now_ms,
        };
        let active: Vec<&Event> = evs
            .iter()
            .filter(|e| !self.event_logged(e) && !self.event_placed(e, queue))
            .collect();
        if active
            .iter()
            .any(|e| e.kind.is_act() && e.target <= slot_est)
        {
            return TimedGen::Gated;
        }
        let silent_playing = self.st.on_air.is_none() && queue.is_empty();
        // A `manual` program holds scheduled starts off until it ends: the slot after its last entry is
        // the end.
        let program_over = self.program_playlist().is_some_and(|pl| {
            pl.mode == Mode::Sequential
                && program.seq_pos >= pl.entries.len()
                && program.fill.is_none()
        });
        let mut cands: Vec<&Event> = active
            .into_iter()
            .filter(|e| !e.kind.is_act())
            // A stopped station is not started by an ID or a liner.
            .filter(|e| !(matches!(e.kind, EvKind::Insert(_)) && (self.stopped || program.halted)))
            .filter(|e| {
                // A running `manual` program holds scheduled starts off until it ends (4.6).
                !(matches!(e.origin, EvOrigin::Start | EvOrigin::OneShot)
                    && program.manual
                    && !silent_playing
                    && !program_over
                    && e.origin == EvOrigin::Start)
            })
            .collect();
        cands.sort_by_key(|e| e.rank());
        let mut steer: Option<&Event> = None;
        for ev in &cands {
            let ev_meta = match &ev.kind {
                EvKind::Insert(s) | EvKind::Play(s) => self.peek_meta(s, cat),
                _ => tail.map(|(m, _)| m),
            };
            let s_ev = match (tail, ev_meta) {
                (Some((m, s)), Some(em)) => (s + timing.advance_ms(m, em, false)).max(now_ms),
                _ => now_ms,
            };
            if s_ev >= ev.lo {
                // A start held by a `manual` program fires when the program ends only if that is still
                // inside its window; else it is missed (4.6).
                if ev.origin == EvOrigin::Start
                    && program.manual
                    && s_ev > ev.hi
                    && !self.late_allowed(ev, s_ev)
                {
                    self.log_event(
                        ev,
                        "missed",
                        None,
                        "held by a manual program past its window",
                    );
                    continue;
                }
                // The event takes this slot.
                let hard_at = ev.mode.hard_fallback().then_some(ev.hi);
                if let Some(v) =
                    self.place_event(ev, queue, s_ev, hard_at, false, now_ms, cat, timing)
                {
                    return TimedGen::Entries(v);
                }
                continue;
            }
            if steer.is_none() && ev.mode.fitted() && ev.lo - s_ev < self.cfg.horizon_ms {
                steer = Some(ev);
            }
        }
        if let Some(ev) = steer {
            // A liner that is due takes the slot first: the fit was computed with it in the chain.
            if !self.due_inserts(queue, slot_est, cat).is_empty() {
                return TimedGen::Nothing;
            }
            if let Some(v) = self.fit_pick(ev, queue, now_ms, cat, timing) {
                return TimedGen::Entries(v);
            }
        }
        TimedGen::Nothing
    }

    // ---- hard fallback and late events ---------------------------------------------------------------------------

    /// Make sure every event that must act soon has an entry at the head of the queue: a hard fallback
    /// within [`PREP_MS`], an event whose window has passed (late policy), and an event reached while the
    /// station is silent (it acts at `T`). Returns true when the queue changed.
    pub fn ensure_events(&mut self, now_ms: i64, cat: &Catalog, timing: &dyn Timing) -> bool {
        let Some(program) = self.st.program.clone() else {
            // No program: a station block or a scheduled start still acts.
            return self.ensure_events_inner(now_ms, cat, timing, false);
        };
        let _ = program;
        self.ensure_events_inner(now_ms, cat, timing, true)
    }

    fn ensure_events_inner(
        &mut self,
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
        _has_program: bool,
    ) -> bool {
        self.st.log.trim(now_ms);
        let evs = self.events(now_ms);
        let mut changed = false;
        // An event the plan put after its hard time with other entries in the way (a long item that
        // overshoots the window, then a liner, then the ID) cannot cut in when its time comes: it is
        // taken out of the queue and placed again, in front of what starts after the hard time.
        let blocked: Vec<&Event> = evs
            .iter()
            .filter(|e| {
                !e.kind.is_act()
                    && !self.event_logged(e)
                    && e.mode.hard_fallback()
                    && e.hi - now_ms <= PREP_MS
                    && self.event_placed(e, &self.st.queue)
                    && self.st.queue.iter().enumerate().any(|(i, q)| {
                        q.timed.as_ref().is_some_and(|t| {
                            t.block_id == e.block_id
                                && t.target_ms == e.target
                                && t.run_id == e.run_id
                        }) && self.st.queue[..i]
                            .iter()
                            .any(|b| b.planned_start_ms.is_some_and(|s| s > e.hi + 500))
                    })
            })
            .collect();
        let mut todo: Vec<&Event> = evs
            .iter()
            .filter(|e| !e.kind.is_act() && !self.event_logged(e))
            .filter(|e| !self.event_placed(e, &self.st.queue))
            .chain(blocked)
            .collect();
        todo.sort_by_key(|e| e.rank());
        for ev in todo {
            if !self.event_valid(ev) {
                continue;
            }
            if matches!(ev.kind, EvKind::Insert(_))
                && (self.stopped || self.st.program.as_ref().is_some_and(|p| p.halted))
            {
                if now_ms > ev.hi {
                    self.log_event(ev, "skipped", None, "the station is stopped");
                    changed = true;
                }
                continue;
            }
            let queue_empty = self.st.queue.is_empty();
            let silent = self.st.on_air.is_none() && queue_empty;
            let program_manual = self
                .st
                .program
                .as_ref()
                .is_some_and(|p| p.manual && !silent);
            if ev.origin == EvOrigin::Start && program_manual {
                // Held until the program ends; whether it may still fire then is decided as it is late.
                if now_ms > ev.hi && !self.late_allowed(ev, now_ms) {
                    self.log_event(
                        ev,
                        "missed",
                        None,
                        "held by a manual program past its window",
                    );
                    changed = true;
                }
                continue;
            }
            if now_ms > ev.hi {
                // Past its window.
                if matches!(ev.mode, BoundaryMode::Soft | BoundaryMode::HardWait)
                    && !silent
                    && (self.late_allowed(ev, now_ms)
                        || ev.mode == BoundaryMode::Soft && ev.origin == EvOrigin::Start)
                {
                    // It waits for the next boundary (never a cut); the dealing places it.
                    continue;
                }
                if self.late_allowed(ev, now_ms) {
                    changed |= self.force_place(ev, now_ms, true, now_ms, cat, timing);
                } else {
                    self.log_event(
                        ev,
                        "missed",
                        None,
                        "past its window and its late policy says skip",
                    );
                    changed = true;
                }
                continue;
            }
            let hard = ev.mode.hard_fallback();
            if silent && ev.target - now_ms <= PREP_MS {
                // Nothing is sounding: a boundary block acts at T exactly, whatever its mode.
                changed |= self.force_place(ev, ev.target.max(now_ms), false, now_ms, cat, timing);
            } else if hard && ev.hi - now_ms <= PREP_MS {
                changed |= self.force_place(ev, ev.hi.max(now_ms), false, now_ms, cat, timing);
            }
        }
        changed
    }

    /// Put the event's entry in the queue after what starts before `hard_at`, replacing its soft
    /// placement, and cut the old program's soft entries after it when it switches.
    pub(crate) fn force_place(
        &mut self,
        ev: &Event,
        hard_at: i64,
        late: bool,
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> bool {
        // Drop the entry the planner had put further down for this event (it is after the window).
        let mut q = std::mem::take(&mut self.st.queue);
        q.retain(|e| {
            !e.timed.as_ref().is_some_and(|t| {
                t.block_id == ev.block_id && t.target_ms == ev.target && t.run_id == ev.run_id
            })
        });
        let pos = q
            .iter()
            .take_while(|e| e.committed || e.planned_start_ms.is_some_and(|s| s < hard_at - 1))
            .count();
        let tail_q: Vec<QEntry> = q[..pos].to_vec();
        let rest: Vec<QEntry> = q[pos..].to_vec();
        self.st.queue = tail_q.clone();
        let slot = hard_at;
        let placed = self.place_event(ev, &tail_q, slot, Some(hard_at), late, now_ms, cat, timing);
        let Some(mut entries) = placed else {
            self.st.queue = q;
            return true;
        };
        let switch = matches!(ev.kind, EvKind::Switch { .. });
        if let Some(first) = entries.first_mut() {
            first.pinned = true;
            first.committed = false;
            if let Some(t) = first.timed.as_mut() {
                t.hard_at_ms = Some(hard_at);
                t.late = late;
            }
        }
        let mut out = tail_q;
        out.extend(entries);
        if switch {
            // The soft entries of the old program are gone; pinned ones stay.
            out.extend(rest.into_iter().filter(|e| e.pinned));
        } else {
            out.extend(rest);
        }
        self.recompute_links(&mut out, cat);
        self.st.queue = out;
        self.times_sig = None;
        self.ev_cache = EventCache::default();
        let mut q = std::mem::take(&mut self.st.queue);
        self.set_times(&mut q, now_ms, cat, timing);
        self.st.queue = q;
        self.lock(now_ms);
        // What follows was dealt before this entry existed (an ID resets the liner counter, a new
        // program deals its own items): the soft entries after it are dealt again.
        if !self.replanning {
            self.replanning = true;
            self.regenerate(now_ms, cat, timing, false);
            self.replanning = false;
        }
        true
    }

    // ---- acts at an exact time ------------------------------------------------------------------------------------

    /// The timed `stop`, `fade_stop` and `end` blocks (4.4). A stop is **armed** `ahead_ms` before its time
    /// (the station sets its fade up on the mixer on an exact frame, 4.9) and **settles** at its time: the
    /// block is logged `fired`, the cursor passes what lay between, the soft plan is cleared and nothing
    /// more is dealt until a start. Until then the plan goes on (items that start before the stop still
    /// play). `stop how=boundary` and `end` have no mixer set-up: they act when their time is reached.
    pub fn due_acts(
        &mut self,
        now_ms: i64,
        ahead_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> Vec<TimedAct> {
        let evs = self.events(now_ms);
        let mut out = Vec::new();
        let mut todo: Vec<&Event> = evs
            .iter()
            .filter(|e| {
                let ahead = if matches!(e.kind, EvKind::Side(_)) {
                    SIDE_AHEAD_MS
                } else {
                    ahead_ms.max(0)
                };
                e.kind.is_act() && !self.event_logged(e) && e.target - now_ms <= ahead
            })
            .collect();
        todo.sort_by_key(|e| (e.target, e.rank()));
        for ev in todo {
            if !self.event_valid(ev) {
                continue;
            }
            let key = (ev.block_id, ev.target);
            let armed = self.armed_acts.contains(&key);
            let late = now_ms - ev.target > 2000;
            if late && !self.late_allowed(ev, now_ms) {
                self.armed_acts.retain(|k| *k != key);
                self.log_event(
                    ev,
                    "missed",
                    None,
                    "its time passed and its late policy says skip",
                );
                if ev.flow {
                    self.passed_up_to(ev);
                }
                self.ev_cache = EventCache::default();
                continue;
            }
            match &ev.kind {
                EvKind::Side(a) => {
                    // Handed over up to `SIDE_AHEAD_MS` early; the worker sends it at the target.
                    self.settle_act(ev, now_ms.max(ev.target), late, cat, timing);
                    out.push(TimedAct {
                        block_id: ev.block_id,
                        name: ev.name.clone(),
                        target_ms: ev.target,
                        kind: ActKind::Side(a.clone()),
                    });
                }
                EvKind::Gain { gain_db, ramp_ms } => {
                    if !armed {
                        self.armed_acts.push(key);
                        out.push(TimedAct {
                            block_id: ev.block_id,
                            name: ev.name.clone(),
                            target_ms: ev.target,
                            kind: ActKind::Gain {
                                gain_db: *gain_db,
                                ramp_ms: *ramp_ms,
                            },
                        });
                    }
                    if ev.target <= now_ms {
                        self.settle_act(ev, now_ms, late, cat, timing);
                    }
                }
                EvKind::Mute { on, ramp_ms } => {
                    if !armed {
                        self.armed_acts.push(key);
                        out.push(TimedAct {
                            block_id: ev.block_id,
                            name: ev.name.clone(),
                            target_ms: ev.target,
                            kind: ActKind::Mute {
                                on: *on,
                                ramp_ms: *ramp_ms,
                            },
                        });
                    }
                    if ev.target <= now_ms {
                        self.settle_act(ev, now_ms, late, cat, timing);
                    }
                }
                EvKind::FadeStop { ms } => {
                    if !armed {
                        self.armed_acts.push(key);
                        out.push(TimedAct {
                            block_id: ev.block_id,
                            name: ev.name.clone(),
                            target_ms: ev.target,
                            kind: ActKind::FadeStop { fade_ms: *ms },
                        });
                    }
                    if ev.target <= now_ms {
                        self.settle_act(ev, now_ms, late, cat, timing);
                    }
                }
                EvKind::StopNow { fade_ms } => {
                    if !armed {
                        self.armed_acts.push(key);
                        out.push(TimedAct {
                            block_id: ev.block_id,
                            name: ev.name.clone(),
                            target_ms: ev.target,
                            kind: ActKind::Stop { fade_ms: *fade_ms },
                        });
                    }
                    if ev.target <= now_ms {
                        self.settle_act(ev, now_ms, late, cat, timing);
                    }
                }
                EvKind::StopBoundary {
                    fade_ms,
                    hard_after_late,
                } => {
                    if ev.target <= now_ms {
                        self.settle_act(ev, now_ms, late, cat, timing);
                        out.push(TimedAct {
                            block_id: ev.block_id,
                            name: ev.name.clone(),
                            target_ms: ev.target,
                            kind: ActKind::Hold,
                        });
                        if *hard_after_late {
                            // The hard stop 30 s later of `boundary`.
                            out.push(TimedAct {
                                block_id: ev.block_id,
                                name: ev.name.clone(),
                                target_ms: ev.target + 30_000,
                                kind: ActKind::HardStop { fade_ms: *fade_ms },
                            });
                        }
                    }
                }
                EvKind::End if ev.target <= now_ms => {
                    self.settle_act(ev, now_ms, late, cat, timing);
                    out.push(TimedAct {
                        block_id: ev.block_id,
                        name: ev.name.clone(),
                        target_ms: ev.target,
                        kind: ActKind::EndProgram,
                    });
                }
                _ => {}
            }
        }
        out.extend(self.due_seq_acts(now_ms, ahead_ms, cat));
        out
    }

    /// What a timed stop or end does to the program when its time is reached.
    pub(crate) fn settle_act(
        &mut self,
        ev: &Event,
        now_ms: i64,
        late: bool,
        cat: &Catalog,
        timing: &dyn Timing,
    ) {
        self.armed_acts.retain(|k| *k != (ev.block_id, ev.target));
        let how = if late { "late_run" } else { "fired" };
        self.log_event(
            ev,
            how,
            Some(ev.target.max(now_ms.min(ev.target + 2000))),
            "",
        );
        if ev.flow {
            self.passed_up_to(ev);
        }
        match &ev.kind {
            EvKind::StopNow { .. } | EvKind::StopBoundary { .. } | EvKind::FadeStop { .. } => {
                self.halt_program()
            }
            EvKind::End => self.end_program(now_ms.max(ev.target), cat, timing),
            _ => {}
        }
        self.ev_cache = EventCache::default();
    }

    /// The cursor passes the in-sequence blocks between it and a timed flow block that fires (4.4).
    pub(crate) fn passed_up_to(&mut self, ev: &Event) {
        if ev.origin == EvOrigin::Program
            && self
                .st
                .program
                .as_ref()
                .is_some_and(|p| ev.depth < p.stack.len())
        {
            // The parent's block wins: what is nested in it ends (4.4).
            self.unwind_to(ev.depth);
        }
        let (Some(idx), Some(pr)) = (ev.entry_idx, self.st.program.clone()) else {
            return;
        };
        let Some(pl) = self.prog.playlists.get(&pr.playlist_id).cloned() else {
            return;
        };
        let from = pr.fill.map_or(pr.seq_pos.min(idx), |f| f + 1).min(idx);
        let mut rows = Vec::new();
        for (i, e) in pl.entries.iter().enumerate().take(idx).skip(from.min(idx)) {
            let Entry::Block(b) = e else { continue };
            if !b.enabled || b.timed.is_some() && i != from {
                // Timed blocks keep their own times.
                if b.timed.is_some() {
                    continue;
                }
            }
            if matches!(b.kind, BlockKind::Start(_) | BlockKind::InsertEvery(_)) {
                continue;
            }
            rows.push(LogRow {
                run_id: Some(pr.run_id),
                block_id: b.id,
                name: b.name.clone(),
                kind: b.kind.type_name().to_string(),
                target_ms: ev.target,
                planned_ms: ev.target,
                fired_ms: Some(ev.target),
                how: "passed".into(),
                deviation_ms: None,
                detail: format!("passed over by `{}`", ev.name),
            });
        }
        for r in rows {
            self.st.log.push(r);
        }
        if let Some(p) = self.st.program.as_mut() {
            p.seq_pos = idx + 1;
            p.floor = idx + 1;
            p.fill = None;
            p.hold = None;
            p.wait_since = None;
            p.released = false;
            p.epoch += 1;
        }
    }

    /// A timed stop ended the audio: the soft plan goes and nothing more is dealt until a start.
    fn halt_program(&mut self) {
        if let Some(p) = self.st.program.as_mut() {
            p.halted = true;
        }
        self.st.queue.retain(|e| e.pinned);
    }

    /// The program ends: the station goes to its scheduled program or its default playlist (4.4).
    fn end_program(&mut self, now_ms: i64, cat: &Catalog, timing: &dyn Timing) {
        let next = self
            .st
            .scheduled_program
            .or(self.cfg.default_playlist)
            .or(self.st.program.as_ref().map(|p| p.playlist_id));
        self.st.queue.retain(|e| e.pinned);
        if let Some(n) = next {
            self.start_program_with(n, now_ms, "ended", None);
        }
        let mut q = std::mem::take(&mut self.st.queue);
        self.set_times(&mut q, now_ms, cat, timing);
        self.st.queue = q;
    }

    /// The next wall time after `now_ms` at which the scheduler has something to do other than deal:
    /// a hard fallback to prepare, an act, a silent start. The virtual-clock driver steps to it.
    pub fn next_wake_ms(&mut self, now_ms: i64, cat: &Catalog) -> Option<i64> {
        let evs = self.events(now_ms);
        let silent = self.st.on_air.is_none() && self.st.queue.is_empty();
        let mut best: Option<i64> = None;
        for e in evs.iter().filter(|e| !self.event_logged(e)) {
            let t = if e.kind.is_act() {
                Some(e.target)
            } else if self.event_placed(e, &self.st.queue) && !e.mode.hard_fallback() {
                None
            } else if e.mode.hard_fallback() {
                Some(e.hi - PREP_MS)
            } else if silent {
                Some(e.target - PREP_MS)
            } else if e.hi > now_ms {
                // A soft event whose window passes unplaced is looked at once, at its end.
                Some(e.hi + 1)
            } else {
                None
            };
            if let Some(t) = t {
                let t = t.max(now_ms + 1);
                best = Some(best.map_or(t, |b| b.min(t)));
            }
        }
        for t in [
            self.cursor_wake_ms(now_ms),
            self.seq_act_wake_ms(now_ms, crate::runtime::ACT_AHEAD_MS, cat),
        ]
        .into_iter()
        .flatten()
        {
            best = Some(best.map_or(t, |b| b.min(t)));
        }
        best
    }

    /// A block's side effect could not be sent (4.9): one more row, `failed`, one millisecond after the
    /// target (the block log keeps one row per block and target).
    pub fn log_failure(
        &mut self,
        block_id: i64,
        name: &str,
        kind: &str,
        target_ms: i64,
        detail: &str,
    ) {
        let run_id = self.st.program.as_ref().map(|p| p.run_id);
        let row = LogRow {
            run_id,
            block_id,
            name: name.into(),
            kind: kind.into(),
            target_ms: target_ms + 1,
            planned_ms: target_ms,
            fired_ms: None,
            how: "failed".into(),
            deviation_ms: None,
            detail: detail.into(),
        };
        self.st.log.push(row);
    }

    /// Station start, or an operator's `resume`: the program deals again.
    pub fn unhalt(&mut self) {
        if let Some(p) = self.st.program.as_mut() {
            p.halted = false;
        }
    }

    // ---- what happened -------------------------------------------------------------------------------------------------

    /// An entry that carries a timed tag began to sound.
    pub(crate) fn on_timed_started(&mut self, e: &QEntry, start_ms: i64) {
        let Some(t) = &e.timed else { return };
        let hard = e.flags.iter().any(|f| f == "hard_time");
        let how = if t.late || (t.mode.hard_fallback() && start_ms > t.hi_ms + 2000) {
            "late_run"
        } else if hard {
            "hard"
        } else {
            "soft"
        };
        let row = LogRow {
            run_id: t.run_id,
            block_id: t.block_id,
            name: t.name.clone(),
            kind: t.kind.clone(),
            target_ms: t.target_ms,
            planned_ms: t.target_ms,
            fired_ms: Some(start_ms),
            how: how.into(),
            deviation_ms: Some(start_ms - t.target_ms),
            detail: String::new(),
        };
        self.st.log.push(row);
        if t.kind == "start" || t.kind == "oneshot" {
            self.st.scheduled_program =
                e.playlist_id
                    .or(self.st.program.as_ref().map(|p| p.playlist_id));
        }
        if t.kind == "oneshot" {
            self.st.oneshots.retain(|o| -o.id != t.block_id);
        }
        if let (Some(idx), Some(pr)) = (t.entry_idx, self.st.program.as_mut()) {
            if t.run_id == Some(pr.run_id) && t.depth.unwrap_or(0) == pr.stack.len() {
                pr.floor = pr.floor.max(idx + 1);
            }
        }
        self.st
            .jump_fire
            .retain(|(b, tg)| !(*b == t.block_id && *tg == t.target_ms));
        self.ev_cache = EventCache::default();
    }

    /// The wall clock stepped from `old_ms` to `new_ms` (1.5). Forward: what was skipped within five
    /// minutes of the old time fires once, now; the rest is logged `missed`. Backward: nothing refires
    /// (the block log). The plan's predicted times are recomputed.
    pub fn on_clock_step(&mut self, old_ms: i64, new_ms: i64) {
        self.times_sig = None;
        self.ev_cache = EventCache::default();
        // The audio did not move, so every wall time the scheduler recorded moves with the clock.
        let d = new_ms - old_ms;
        if d != 0 {
            if let Some(s) = self.st.on_air_start_ms.as_mut() {
                *s += d;
            }
            for r in &mut self.st.history {
                r.start_ms += d;
            }
            for e in &mut self.st.queue {
                if let Some(s) = e.planned_start_ms.as_mut() {
                    *s += d;
                }
                if let Some(s) = e.planned_segue_ms.as_mut() {
                    *s += d;
                }
                if let Some(t) = e.timed.as_mut() {
                    // The target is a wall time and stays; the hard time of an entry too.
                    let _ = t;
                }
            }
            for c in self.st.counters.values_mut() {
                if let Some(l) = c.last_fired_ms.as_mut() {
                    *l += d;
                }
            }
        }
        if new_ms <= old_ms {
            return;
        }
        let evs = self.events(new_ms);
        let skipped: Vec<Event> = evs
            .iter()
            .filter(|e| e.target > old_ms && e.target <= new_ms && !self.event_logged(e))
            .cloned()
            .collect();
        for ev in skipped {
            if ev.target <= old_ms + JUMP_FIRE_MS {
                self.st.jump_fire.push((ev.block_id, ev.target));
            } else {
                self.log_event(&ev, "missed", None, "skipped by a clock step");
            }
        }
        self.ev_cache = EventCache::default();
    }

    // ---- one-shot changes -----------------------------------------------------------------------------------------------

    /// `POST /schedule/changes` (7.2). Returns the id.
    pub fn add_oneshot(
        &mut self,
        playlist_id: i64,
        at_ms: i64,
        how: StartHow,
        mode: BoundaryMode,
        early_s: i64,
        late_s: i64,
    ) -> Result<i64, String> {
        if !self.prog.playlists.contains_key(&playlist_id) {
            return Err(format!("no playlist {playlist_id}"));
        }
        let id = self.st.next_oneshot_id.max(1);
        self.st.next_oneshot_id = id + 1;
        self.st.oneshots.push(OneShot {
            id,
            playlist_id,
            at_ms,
            how,
            mode,
            early_s,
            late_s,
        });
        self.ev_cache = EventCache::default();
        Ok(id)
    }

    pub fn cancel_oneshot(&mut self, id: i64) -> bool {
        let n = self.st.oneshots.len();
        self.st.oneshots.retain(|o| o.id != id);
        self.ev_cache = EventCache::default();
        n != self.st.oneshots.len()
    }

    // ---- what is coming ---------------------------------------------------------------------------------------------------

    /// `GET /schedule/upcoming` (7.2): every unfired occurrence in the next `hours`, with the time the
    /// block is expected to act.
    pub fn upcoming(&self, now_ms: i64, hours: i64) -> Vec<Upcoming> {
        let upto = now_ms + hours * 3_600_000;
        let after = now_ms - 60_000;
        let evs = self.build_events(after, upto);
        let mut out = Vec::new();
        for ev in evs {
            if self.event_logged(&ev) {
                continue;
            }
            let planned = self
                .st
                .queue
                .iter()
                .chain(self.st.on_air.iter())
                .find(|e| {
                    e.timed.as_ref().is_some_and(|t| {
                        t.block_id == ev.block_id
                            && t.target_ms == ev.target
                            && t.run_id == ev.run_id
                    })
                })
                .and_then(|e| e.planned_start_ms.or(self.st.on_air_start_ms));
            let source = match &ev.kind {
                EvKind::Insert(s) | EvKind::Play(s) => format!("{s:?}"),
                EvKind::Switch { playlist_id, .. } => format!("playlist {playlist_id}"),
                _ => String::new(),
            };
            out.push(Upcoming {
                block_id: ev.block_id,
                name: ev.name.clone(),
                kind: ev.type_name.clone(),
                source,
                target_ms: ev.target,
                // A block that cuts in at the end of its window acts there at the latest.
                predicted_ms: match planned {
                    Some(p) if ev.mode.hard_fallback() => p.min(ev.hi),
                    Some(p) => p,
                    None => ev.target,
                },
                mode: ev.mode,
                lo_ms: ev.lo,
                hi_ms: ev.hi,
                planned: planned.is_some(),
                origin: match ev.origin {
                    EvOrigin::Program => "program",
                    EvOrigin::Station => "station",
                    EvOrigin::Start => "start",
                    EvOrigin::OneShot => "oneshot",
                },
            });
        }
        out.sort_by_key(|u| u.predicted_ms);
        out
    }

    /// The next occurrence of any `start` block (absolute or manual): what a station that runs no program
    /// shows as "expected", with its playlist and anchor.
    pub fn next_expected(&self, now_ms: i64) -> Option<(i64, i64, Anchor)> {
        let mut best: Option<(i64, i64, Anchor)> = None;
        for (pid, _, sb) in self.prog.scheduled_starts() {
            for spec in &sb.schedule {
                if let Some(t) = spec.first_after(&self.cfg.zone, None, now_ms, 8 * 86_400_000) {
                    if best.is_none_or(|(_, b, _)| t < b) {
                        best = Some((pid, t, sb.anchor));
                    }
                }
            }
        }
        best
    }

    /// Does the playlist have a `start` block expecting occurrences (for the display's countdown)?
    pub fn expected_start(&self, now_ms: i64, playlist_id: i64) -> Option<i64> {
        let pl = self.prog.playlists.get(&playlist_id)?;
        let (_, sb) = pl.start_block()?;
        sb.schedule
            .iter()
            .filter_map(|s: &TimeSpec| s.first_after(&self.cfg.zone, None, now_ms, 8 * 86_400_000))
            .min()
    }
}

/// The result of the timed part of dealing a slot.
pub(crate) enum TimedGen {
    Nothing,
    Gated,
    Entries(Vec<QEntry>),
}

#[allow(dead_code)]
fn until_is_open(u: &Until) -> bool {
    matches!(u, Until::Next | Until::Forever)
}
