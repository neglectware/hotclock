//! The scheduler (architecture 4.2 to 4.7): the program, the queue and the rotation decks. It is a
//! plain state machine with no clock and no audio: every call takes the wall time in milliseconds,
//! a [`Catalog`] and a [`Timing`], so the virtual-clock tests and the 30-day simulation drive exactly
//! the code the station runs.
//!
//! ```text
//! [ on air ] [ committed next ] [ soft entries ... ]   (pinned entries sit wherever they were put)
//! ```
//!
//! * **On air** and **committed** (within `lock_ahead` of its start) entries are never changed by the
//!   scheduler; the operator can (`remove`, `insert`, `switch`), and the result says when the head of
//!   the queue changed so the conductor can take back a boundary it had already committed
//!   (`Conductor::uncommit_last`).
//! * **Soft** entries are the plan and are regenerated whenever something changes: the decks are
//!   rewound to the last retained pick and dealt again. **Pinned** entries keep their index.
//! * **Runs** (3.7): an album block, `play album`, or a `sequential` walk over adjacent album tracks;
//!   a shuffle never forms one. `continuity_from_prev` is set only where the entry is in a gapless
//!   run and adjacent to the entry before it, and recomputed after every change.

use crate::catalog::{Catalog, TrackMeta};
use crate::deck::{rotation_with, Deck, Pick, Rotation, Rules, Unit, WINDOW_SHARE_OF_PASS};
use crate::programming::{Block, BlockKind, Entry, InsertEvery, Playlist, Programming, Source};
use crate::rng::Rng;
use crate::timed::{BlockLog, EventCache, OneShot, TimedTag};
use common::timespec::Zone;
use library::playlists::Mode;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

pub const DEFAULT_MEAN_MS: f64 = 240_000.0;
/// Slack added to the pick-time window for the error of the predicted start times (the slot's start is
/// predicted before its item is known, and the overlap depends on the item: up to `overlap_max_s`).
pub const PREDICTION_SLACK_MS: i64 = 90_000;

#[derive(Debug, Clone)]
pub struct SchedConfig {
    /// An entry within this many ms of its start is committed (`lock_ahead_s`, 1.5).
    pub lock_ahead_ms: i64,
    /// The plan is kept at least this many items and this many ms deep (4.2).
    pub plan_items: usize,
    pub plan_ms: i64,
    /// Plays older than this are forgotten by the no-repeat and separation checks.
    pub history_ms: i64,
    pub default_playlist: Option<i64>,
    /// The share of one pass the no-repeat window may take (`deck::WINDOW_SHARE_OF_PASS`).
    pub window_share: f64,
    /// The station's time zone: timed blocks are written in local time (1.6).
    pub zone: Zone,
    /// A timed block enters the planning horizon this long before its window (4.6).
    pub horizon_ms: i64,
}

impl Default for SchedConfig {
    fn default() -> Self {
        Self {
            lock_ahead_ms: 20_000,
            plan_items: 5,
            plan_ms: 30 * 60_000,
            history_ms: 24 * 3_600_000,
            default_playlist: None,
            window_share: WINDOW_SHARE_OF_PASS,
            zone: Zone::utc(),
            horizon_ms: crate::timed::HORIZON_MS,
        }
    }
}

/// How the start of the next item follows the start of this one (start to start), ms. The station
/// answers with the transition planner; tests with [`EffectiveTiming`].
pub trait Timing {
    fn advance_ms(&self, a: &TrackMeta, b: &TrackMeta, continuity: bool) -> i64;
}

/// Each item takes its effective length (`cue_in` to `segue`): no overlap.
pub struct EffectiveTiming;

impl Timing for EffectiveTiming {
    fn advance_ms(&self, a: &TrackMeta, _b: &TrackMeta, _continuity: bool) -> i64 {
        a.effective_ms
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Rotation,
    Insert,
    Manual,
    Fill,
    Emergency,
    /// Dealt by a flow block (`play`, a scheduled start): the first item of the source.
    Block,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Rotation => "rotation",
            Origin::Insert => "insert",
            Origin::Manual => "manual",
            Origin::Fill => "fill",
            Origin::Emergency => "emergency",
            Origin::Block => "block",
        }
    }
}

/// Where a rotation pick came from, so the deck can be rewound to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeckRef {
    pub key: String,
    pub gen: u64,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QEntry {
    pub id: i64,
    pub item_id: i64,
    pub origin: Origin,
    pub block_id: Option<i64>,
    pub playlist_id: Option<i64>,
    pub pinned: bool,
    pub committed: bool,
    /// A per-entry transition for this boundary only: a JSON `PresetPatch`, or `cut`.
    pub transition: Option<String>,
    pub run_id: Option<i64>,
    pub continuity_from_prev: bool,
    pub deck: Option<DeckRef>,
    pub planned_start_ms: Option<i64>,
    pub planned_segue_ms: Option<i64>,
    pub flags: Vec<String>,
    /// Set when a timed block put this entry here (the ID, a scheduled start, a timed `play`).
    #[serde(default)]
    pub timed: Option<TimedTag>,
    /// The program as it was before this entry changed it (a scheduled start or `start_playlist` planned
    /// into the queue): a regeneration that drops the entry restores it.
    #[serde(default)]
    pub prog_before: Option<ProgramRun>,
    /// The program's cursor after this entry was dealt: a regeneration that keeps this entry puts the
    /// cursor back to it (unless an act or an operator action moved it since: `ProgramRun::epoch`).
    #[serde(default)]
    pub prog_after: Option<Box<ProgramRun>>,
}

impl QEntry {
    pub(crate) fn new(id: i64, item_id: i64, origin: Origin) -> Self {
        Self {
            id,
            item_id,
            origin,
            block_id: None,
            playlist_id: None,
            pinned: false,
            committed: false,
            transition: None,
            run_id: None,
            continuity_from_prev: false,
            deck: None,
            planned_start_ms: None,
            planned_segue_ms: None,
            flags: vec![],
            timed: None,
            prog_before: None,
            prog_after: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunInfo {
    pub members: Vec<i64>,
    /// `gapless` or `normal` (3.7).
    pub continuity: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Counter {
    pub count: u32,
    pub last_fired_ms: Option<i64>,
}

/// Where a `play` block with an end condition stands (4.5).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Hold {
    /// The cursor reached the block at this wall time (predicted, ms).
    pub since_ms: i64,
    /// Items dealt from the source so far (`until: count`).
    pub count: u32,
}

/// One level of the cursor: a playlist being walked (4.4). The program holds the innermost level in its own
/// fields and the levels above it (a `play` of a playlist nests it) in [`ProgramRun::stack`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cursor {
    pub playlist_id: i64,
    pub t0_ms: i64,
    /// The next entry of the playlist to deal.
    pub seq_pos: usize,
    /// The cursor never goes back before this entry (the last timed flow block that fired).
    pub floor: usize,
    /// The playlist entry (a `play` block) whose source is being dealt until a block takes over.
    pub fill: Option<usize>,
    /// Jumps made so far by each `loop` block (by position).
    pub loops: BTreeMap<usize, u32>,
    /// When the cursor reached the `wait` it stands at (predicted wall time).
    pub wait_since: Option<i64>,
    /// The operator pressed Continue for the wait it stands at.
    pub released: bool,
    pub hold: Option<Hold>,
    /// Nothing is dealt before this wall time (after an in-sequence `stop` or `fade_stop`).
    pub hold_until_ms: Option<i64>,
}

/// The program (4.1): the playlist being run at the top level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgramRun {
    /// The playlist the cursor is in now (the innermost one when playlists are nested).
    pub playlist_id: i64,
    pub t0_ms: i64,
    pub run_id: i64,
    /// `sequential`: the next entry of the playlist to deal.
    pub seq_pos: usize,
    /// The cursor never goes back before this entry (the last timed flow block that fired).
    #[serde(default)]
    pub floor: usize,
    /// The playlist entry (a `play` block) whose source is being dealt until a timed flow block preempts it.
    #[serde(default)]
    pub fill: Option<usize>,
    /// Started by hand and anchored `manual`: scheduled starts wait until it ends (4.6).
    #[serde(default)]
    pub manual: bool,
    /// A timed `stop` ended the program's audio: nothing is dealt until the station is started again.
    #[serde(default)]
    pub halted: bool,
    #[serde(default)]
    pub loops: BTreeMap<usize, u32>,
    #[serde(default)]
    pub wait_since: Option<i64>,
    #[serde(default)]
    pub released: bool,
    #[serde(default)]
    pub hold: Option<Hold>,
    #[serde(default)]
    pub hold_until_ms: Option<i64>,
    /// An in-sequence stop ended the audio: the next item dealt restarts it.
    #[serde(default)]
    pub silenced: bool,
    /// The levels above the current one (outermost first).
    #[serde(default)]
    pub stack: Vec<Cursor>,
    /// Where T0 came from (5.3 `t0_source`): `deck`, `ui` or `api` (a program start by hand), `auto` (the fallback
    /// start of a manual program, the one start that is automatic), `nominal`, `block`, `schedule`, `switch` (a
    /// playlist switch), `ended` (the program before it ended), `abort`, `start` (the station started).
    #[serde(default)]
    pub t0_source: String,
    /// The scheduled occurrence this run belongs to, if any.
    #[serde(default)]
    pub nominal_t0_ms: Option<i64>,
    /// Counts every change of the cursor that is not a plain deal (an act, an operator action): a snapshot
    /// taken before it must not be used to rewind the cursor.
    #[serde(default)]
    pub epoch: u64,
    /// The program was ended by hand (`abort`) or by its last entry; what runs now is the next program.
    #[serde(default)]
    pub ended: bool,
    /// The back-timing plan of the `play` block that is filling, for the status (4.9).
    #[serde(default)]
    pub bt: Option<BackStatus>,
}

/// `back_time` of `GET /program/state` and the `program` event (4.9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackStatus {
    pub block_id: i64,
    pub name: String,
    pub target_ms: i64,
    pub planned_end_ms: i64,
    /// `planning`, `on_target`, `within_tolerance` or `fallback`.
    pub state: String,
    pub mode: String,
    /// The closing item is dealt: nothing more will be.
    pub closed: bool,
}

impl BackStatus {
    pub fn delta_s(&self) -> f64 {
        (self.planned_end_ms - self.target_ms) as f64 / 1000.0
    }
}

impl ProgramRun {
    pub fn cursor(&self) -> Cursor {
        Cursor {
            playlist_id: self.playlist_id,
            t0_ms: self.t0_ms,
            seq_pos: self.seq_pos,
            floor: self.floor,
            fill: self.fill,
            loops: self.loops.clone(),
            wait_since: self.wait_since,
            released: self.released,
            hold: self.hold.clone(),
            hold_until_ms: self.hold_until_ms,
        }
    }

    pub fn load_cursor(&mut self, c: Cursor) {
        self.playlist_id = c.playlist_id;
        self.t0_ms = c.t0_ms;
        self.seq_pos = c.seq_pos;
        self.floor = c.floor;
        self.fill = c.fill;
        self.loops = c.loops;
        self.wait_since = c.wait_since;
        self.released = c.released;
        self.hold = c.hold;
        self.hold_until_ms = c.hold_until_ms;
    }

    /// The playlist the program was started with (the outermost level).
    pub fn root_playlist(&self) -> i64 {
        self.stack
            .first()
            .map_or(self.playlist_id, |c| c.playlist_id)
    }

    /// A fresh cursor into `playlist_id`, starting after the playlist's `start` block.
    pub fn fresh_cursor(playlist_id: i64, t0_ms: i64, first: usize) -> Cursor {
        Cursor {
            playlist_id,
            t0_ms,
            seq_pos: first,
            floor: first,
            fill: None,
            loops: BTreeMap::new(),
            wait_since: None,
            released: false,
            hold: None,
            hold_until_ms: None,
        }
    }
}

/// What an in-sequence block that acts at a boundary asks the station to do (4.4, 4.9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SeqKind {
    Side(crate::side::SideAction),
    Gain {
        gain_db: f64,
        ramp_ms: i64,
    },
    Mute {
        on: bool,
        ramp_ms: i64,
    },
    FadeStop {
        ms: i64,
    },
    /// `stop how=now`: fade everything over `fade_ms` and stop.
    Stop {
        fade_ms: i64,
    },
    /// `stop how=after_current` or `boundary`: the item on air plays to its end and nothing follows.
    Hold {
        hard_after_30s: bool,
        fade_ms: i64,
    },
}

/// An act of an in-sequence block: it happens at the boundary where the previous step ended, which is the
/// planned start of the entry dealt after it (or the planned end of the entry dealt before it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeqAct {
    pub block_id: i64,
    pub run_id: i64,
    pub name: String,
    pub kind: SeqKind,
    /// The entry dealt before it (None: nothing was playing).
    pub after_entry: Option<i64>,
    /// The entry dealt after it, once there is one.
    pub before_entry: Option<i64>,
    /// The time to act at when neither entry is known (and the fixed time of a timed end).
    pub at_ms: i64,
    /// `at_ms` is exact (a fade at a given time), not a boundary.
    pub fixed: bool,
    /// Which pass of a loop made it (so each pass logs its own row).
    pub visit: u32,
}

/// A played item, as the no-repeat and separation checks need it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayRec {
    pub item_id: i64,
    pub start_ms: i64,
    pub title_key: String,
    pub artist: String,
    pub album_key: String,
}

impl PlayRec {
    fn of(m: &TrackMeta, start_ms: i64) -> Self {
        Self {
            item_id: m.item_id,
            start_ms,
            title_key: m.title_key(),
            artist: crate::catalog::norm(&m.artist),
            album_key: m.album_key.clone(),
        }
    }
}

/// Everything the scheduler owns that must survive a restart (`state.db`, 5.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedState {
    pub program: Option<ProgramRun>,
    pub on_air: Option<QEntry>,
    pub on_air_start_ms: Option<i64>,
    pub queue: Vec<QEntry>,
    pub decks: BTreeMap<String, Deck>,
    pub counters: BTreeMap<i64, Counter>,
    pub runs: BTreeMap<i64, RunInfo>,
    pub rng: Rng,
    pub next_entry_id: i64,
    pub next_run_id: i64,
    pub history: Vec<PlayRec>,
    /// The block log (5.3): one row per block and target.
    #[serde(default)]
    pub log: BlockLog,
    /// One-shot scheduled changes (7.2).
    #[serde(default)]
    pub oneshots: Vec<OneShot>,
    #[serde(default)]
    pub next_oneshot_id: i64,
    /// The playlist whose recurring start block fired most recently: what the station returns to when a
    /// program ends (4.4).
    #[serde(default)]
    pub scheduled_program: Option<i64>,
    /// Timed blocks a clock step over their time told to fire once, now: `(block id, target)`.
    #[serde(default)]
    pub jump_fire: Vec<(i64, i64)>,
    /// The acts of in-sequence blocks that are waiting for their boundary.
    #[serde(default)]
    pub seq_acts: Vec<SeqAct>,
    /// The state of `manual` start blocks' occurrences (5.3 `start_arm`).
    #[serde(default)]
    pub arms: Vec<crate::program::ArmRec>,
    /// Blocks the operator fired by hand and that have not acted yet: `(block id, wall ms asked)`.
    #[serde(default)]
    pub hand_fire: Vec<(i64, i64)>,
    /// The back-timing plan as the status shows it: the program's own `bt` rewinds with the cursor, this copy
    /// stays until the end time has passed, even when the plan is dealt and the program has moved on.
    #[serde(default)]
    pub bt_display: Option<BackStatus>,
}

/// What an operation did to the head of the queue.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpEffect {
    /// The entry that plays next is not the one it was: a boundary already committed to the mixer must
    /// be taken back.
    pub head_changed: bool,
    /// The queue entry the operation created, if any.
    pub entry_id: Option<i64>,
    pub new_next_item: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    Next,
    Index(usize),
    End,
}

/// `switch playlist` (4.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchHow {
    Now,
    Segue,
    End,
}

/// The one-boundary transition override of a `switch how=end`: no overlap, after the outgoing cue-out.
pub const END_TRANSITION: &str = r#"{"preset":"none"}"#;

#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    pub playlist: String,
    pub units: usize,
    /// The mean length of a unit (an album block counts its tracks together), minutes.
    pub mean_minutes: f64,
    pub window_items: usize,
    pub k: usize,
    /// The requested window was cut to fit the playlist.
    pub capped: bool,
    /// The window the rotation keeps (the effective window), minutes.
    pub achievable_minutes: f64,
    pub requested_minutes: f64,
    /// What the seam alone guarantees, `(K + 1) * m`, minutes.
    pub seam_minutes: f64,
    /// The playlist's setting as the user made it, minutes (never changed by the cap).
    pub no_repeat_configured: i64,
    /// The window in use, minutes: the setting, or the computed maximum when the playlist is too
    /// short for it (`deck::WINDOW_SHARE_OF_PASS`).
    pub no_repeat_effective: f64,
    /// Why the two differ, e.g. `capped: 18 tracks, 129 min total`; `None` when they do not.
    pub no_repeat_reason: Option<String>,
}

impl Info {
    /// The playlist form's sentence (4.3).
    pub fn message(&self) -> Option<String> {
        self.capped.then(|| {
            format!(
                "this playlist can keep a track about {:.0} minutes apart; the window asks for {:.0}",
                self.achievable_minutes, self.requested_minutes
            )
        })
    }
}

pub struct Scheduler {
    pub cfg: SchedConfig,
    pub prog: Arc<Programming>,
    pub st: SchedState,
    /// How many picks needed a relaxed rule (for the tests and `/metrics`).
    pub relaxed_picks: u64,
    /// What the planned times were last computed from: recomputed only when it changes.
    pub(crate) times_sig: Option<u64>,
    /// The effective window last logged per playlist (requested, effective, K), so a change is logged
    /// once.
    logged_windows: BTreeMap<i64, (i64, i64, usize)>,
    /// The timed events of the next while (see `timed.rs`).
    pub(crate) ev_cache: EventCache,
    /// The station is stopped or held (set by the station each step): a station block that would start
    /// the audio waits.
    pub stopped: bool,
    pub(crate) replanning: bool,
    /// How many program ends the cursor has followed inside one dealing (a playlist with nothing playable
    /// would restart itself without end).
    pub(crate) end_depth: u8,
    /// Playlists made by `end-at` (7.2): they live in memory only and are merged into every programming
    /// the library sends.
    pub(crate) synthetic: BTreeMap<i64, Playlist>,
    /// Timed stops that were set up on the mixer and have not reached their time `(block id, target)`.
    pub(crate) armed_acts: Vec<(i64, i64)>,
}

pub(crate) struct Chain {
    /// Plays in order of start: history, on air, then the queue so far.
    pub(crate) recs: Vec<PlayRec>,
}

impl Scheduler {
    pub fn new(cfg: SchedConfig, prog: Arc<Programming>, seed: u64) -> Self {
        Self {
            cfg,
            prog,
            st: SchedState {
                program: None,
                on_air: None,
                on_air_start_ms: None,
                queue: Vec::new(),
                decks: BTreeMap::new(),
                counters: BTreeMap::new(),
                runs: BTreeMap::new(),
                rng: Rng::new(seed),
                next_entry_id: 1,
                next_run_id: 1,
                history: Vec::new(),
                log: BlockLog::default(),
                oneshots: Vec::new(),
                next_oneshot_id: 1,
                scheduled_program: None,
                jump_fire: Vec::new(),
                seq_acts: Vec::new(),
                arms: Vec::new(),
                hand_fire: Vec::new(),
                bt_display: None,
            },
            relaxed_picks: 0,
            times_sig: None,
            logged_windows: BTreeMap::new(),
            ev_cache: EventCache::default(),
            stopped: false,
            replanning: false,
            end_depth: 0,
            synthetic: BTreeMap::new(),
            armed_acts: Vec::new(),
        }
    }

    pub fn restore(cfg: SchedConfig, prog: Arc<Programming>, st: SchedState) -> Self {
        Self {
            cfg,
            prog,
            st,
            relaxed_picks: 0,
            times_sig: None,
            logged_windows: BTreeMap::new(),
            ev_cache: EventCache::default(),
            stopped: false,
            replanning: false,
            end_depth: 0,
            synthetic: BTreeMap::new(),
            armed_acts: Vec::new(),
        }
    }

    /// New programming (the library changed). Nothing on air or committed is touched; the soft plan is
    /// regenerated by the caller (`regenerate`).
    pub fn set_programming(&mut self, prog: Arc<Programming>) {
        let prog = if self.synthetic.is_empty() {
            prog
        } else {
            let mut p = (*prog).clone();
            for (id, pl) in &self.synthetic {
                p.playlists.insert(*id, pl.clone());
            }
            Arc::new(p)
        };
        self.prog = prog;
        self.ev_cache = EventCache::default();
    }

    pub fn program_playlist(&self) -> Option<&Playlist> {
        let id = self.st.program.as_ref()?.playlist_id;
        self.prog.playlists.get(&id)
    }

    pub fn head(&self) -> Option<&QEntry> {
        self.st.queue.first()
    }

    pub fn on_air(&self) -> Option<&QEntry> {
        self.st.on_air.as_ref()
    }

    pub(crate) fn take_id(&mut self) -> i64 {
        let id = self.st.next_entry_id;
        self.st.next_entry_id += 1;
        id
    }

    // ---- the seam figures -----------------------------------------------------------------------

    pub(crate) fn playable_units(&self, pl: &Playlist, cat: &Catalog) -> Vec<(Unit, f64)> {
        let mut out = Vec::new();
        for e in &pl.entries {
            match e {
                Entry::Item { item_id, weight } => {
                    if cat.meta(*item_id).is_some_and(|m| m.playable) {
                        out.push((Unit::Item(*item_id), *weight));
                    }
                }
                Entry::Album {
                    album_key, weight, ..
                } => {
                    if !cat.album_tracks(album_key).is_empty() {
                        out.push((Unit::Album(album_key.clone()), *weight));
                    }
                }
                Entry::Block(_) => {}
            }
        }
        out
    }

    pub(crate) fn unit_first_item(cat: &Catalog, u: &Unit) -> Option<i64> {
        match u {
            Unit::Item(i) => Some(*i),
            Unit::Album(k) => cat.album_tracks(k).first().map(|t| t.meta.item_id),
        }
    }

    /// The effective length of a unit, ms: an item's `cue_in` to `segue`, an album block's tracks
    /// together (it plays as one run and counts as one play, 4.3).
    fn unit_ms(cat: &Catalog, u: &Unit) -> Option<i64> {
        match u {
            Unit::Item(i) => cat.meta(*i).map(|m| m.effective_ms),
            Unit::Album(k) => {
                let t = cat.album_tracks(k);
                (!t.is_empty()).then(|| t.iter().map(|t| t.meta.effective_ms).sum())
            }
        }
    }

    /// The rotation figures of a pool of units (4.3): `m` is the mean unit length, and one pass
    /// lasts `n * m`.
    pub(crate) fn rotation_of(
        &self,
        pool: &[(Unit, f64)],
        no_repeat_minutes: i64,
        cat: &Catalog,
    ) -> Rotation {
        let lens: Vec<i64> = pool
            .iter()
            .filter(|(_, w)| *w > 0.0)
            .filter_map(|(u, _)| Self::unit_ms(cat, u))
            .collect();
        let mean = if lens.is_empty() {
            DEFAULT_MEAN_MS
        } else {
            lens.iter().sum::<i64>() as f64 / lens.len() as f64
        };
        let n = pool.iter().filter(|(_, w)| *w > 0.0).count();
        rotation_with(n, mean, no_repeat_minutes, self.cfg.window_share)
    }

    /// The seam `K` and the figures for the playlist form (4.3).
    pub fn rotation_info(&self, pl: &Playlist, cat: &Catalog) -> Info {
        let pool = self.playable_units(pl, cat);
        let r = self.rotation_of(&pool, pl.no_repeat_minutes, cat);
        Info {
            playlist: pl.name.clone(),
            units: r.n,
            mean_minutes: r.mean_ms / 60_000.0,
            window_items: r.window_items,
            k: r.k,
            capped: r.capped,
            achievable_minutes: r.achievable_minutes,
            requested_minutes: r.requested_minutes,
            seam_minutes: r.seam_minutes,
            no_repeat_configured: pl.no_repeat_minutes,
            no_repeat_effective: r.effective_minutes,
            no_repeat_reason: r.reason(),
        }
    }

    // ---- history ----------------------------------------------------------------------------------

    pub(crate) fn chain(&self, queue: &[QEntry], cat: &Catalog) -> Chain {
        let mut recs: Vec<PlayRec> = self.st.history.clone();
        if let (Some(e), Some(s)) = (&self.st.on_air, self.st.on_air_start_ms) {
            if let Some(m) = cat.meta(e.item_id) {
                recs.push(PlayRec::of(m, s));
            }
        }
        for e in queue {
            if let (Some(m), Some(s)) = (cat.meta(e.item_id), e.planned_start_ms) {
                recs.push(PlayRec::of(m, s));
            }
        }
        Chain { recs }
    }

    // ---- counters ---------------------------------------------------------------------------------

    /// Apply one entry to an insert counter: a played item of the counted kind counts, a fire of the
    /// block resets, and an item of a `resets_on` kind (an ID) resets.
    fn apply_count(
        c: &mut Counter,
        ie: &InsertEvery,
        block_id: i64,
        e: &QEntry,
        m: &TrackMeta,
        start_ms: i64,
    ) {
        if e.origin == Origin::Insert && e.block_id == Some(block_id) {
            c.count = 0;
            c.last_fired_ms = Some(start_ms);
        } else if ie.resets_on.contains(&m.kind) {
            c.count = 0;
        } else if m.kind == ie.count_kind {
            c.count += 1;
        }
    }

    /// Every standing `insert_every` block that applies: the program playlist's, and the station's.
    pub(crate) fn insert_blocks(&self) -> Vec<(&Block, &InsertEvery)> {
        let mut out = Vec::new();
        let levels = self.levels();
        let last = levels.len().saturating_sub(1);
        for (depth, (pid, _)) in levels.iter().enumerate() {
            let Some(pl) = self.prog.playlists.get(pid) else {
                continue;
            };
            for b in pl.blocks() {
                if let (true, BlockKind::InsertEvery(ie)) = (b.enabled, &b.kind) {
                    // A parent's standing block keeps counting under a nested playlist unless it says not to.
                    if depth == last || ie.inherit {
                        out.push((b, ie));
                    }
                }
            }
        }
        if let Some(root) = levels
            .first()
            .and_then(|(id, _)| self.prog.playlists.get(id))
        {
            if root.station_blocks {
                for b in &self.prog.station_blocks {
                    if let (true, BlockKind::InsertEvery(ie)) = (b.enabled, &b.kind) {
                        out.push((b, ie));
                    }
                }
            }
        }
        // `n` below 1 is a validation error (4.4); such a block would be due at every boundary.
        out.retain(|(_, ie)| ie.n >= 1);
        out
    }

    /// The counter of a block as it stands after `queue`: the persisted count (which includes the item
    /// on air), then each queued entry applied in order.
    pub(crate) fn planned_counter(
        &self,
        block: &Block,
        ie: &InsertEvery,
        queue: &[QEntry],
        cat: &Catalog,
    ) -> Counter {
        let mut c = self.st.counters.get(&block.id).cloned().unwrap_or_default();
        for e in queue {
            if let Some(m) = cat.meta(e.item_id) {
                Self::apply_count(&mut c, ie, block.id, e, m, e.planned_start_ms.unwrap_or(0));
            }
        }
        c
    }

    // ---- picking ------------------------------------------------------------------------------------

    /// The playable pool and seam `K` of a source, and a deck key.
    #[allow(clippy::type_complexity)]
    pub(crate) fn source_pool(
        &self,
        src: &Source,
        cat: &Catalog,
    ) -> Option<(String, Vec<(Unit, f64)>, Option<i64>)> {
        match src {
            Source::Category { id, name } => {
                let cid =
                    id.or_else(|| name.as_deref().and_then(|n| cat.category_id_by_name(n)))?;
                let pool = cat
                    .category_items(cid)
                    .into_iter()
                    .map(|i| (Unit::Item(i), 1.0))
                    .collect();
                Some((format!("cat:{cid}"), pool, None))
            }
            Source::Playlist(pid) => {
                let pl = self.prog.playlists.get(pid)?;
                Some((
                    format!("pl:{pid}"),
                    self.playable_units(pl, cat),
                    Some(*pid),
                ))
            }
            Source::Item(i) => Some((
                format!("item:{i}"),
                cat.meta(*i)
                    .filter(|m| m.playable)
                    .map(|_| vec![(Unit::Item(*i), 1.0)])
                    .unwrap_or_default(),
                None,
            )),
        }
    }

    /// Deal one unit from the deck `key` over `pool`, with the rules of playlist `pl` (`None`: no
    /// rules, the deck alone).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn deal(
        &mut self,
        key: &str,
        pool: &[(Unit, f64)],
        pl: Option<&Playlist>,
        chain: &Chain,
        slot_start_ms: i64,
        cat: &Catalog,
    ) -> Option<(Pick, bool)> {
        let (k, eff_ms, sep) = match pl {
            Some(pl) => {
                // The window the rotation keeps: the requested one, cut to a share of one pass
                // when the playlist is too short for it (4.3; `deck::WINDOW_SHARE_OF_PASS`).
                let r = self.rotation_of(pool, pl.no_repeat_minutes, cat);
                let eff = (r.effective_minutes * 60_000.0).round() as i64;
                let seen = (
                    pl.no_repeat_minutes,
                    r.effective_minutes.round() as i64,
                    r.k,
                );
                if self.logged_windows.get(&pl.id) != Some(&seen) {
                    self.logged_windows.insert(pl.id, seen);
                    tracing::info!(
                        playlist = %pl.name,
                        no_repeat_configured = pl.no_repeat_minutes,
                        no_repeat_effective = format!("{:.1}", r.effective_minutes),
                        reason = r.reason().unwrap_or_else(|| "as set".into()),
                        units = r.n,
                        mean_minutes = format!("{:.2}", r.mean_ms / 60_000.0),
                        k = r.k,
                        "rotation: no-repeat window"
                    );
                }
                // The predicted start of a slot differs from the real one by the odd second
                // (overlaps, decoder lengths): a margin keeps a play from landing just inside.
                let eff = if eff > 0 {
                    eff + PREDICTION_SLACK_MS
                } else {
                    eff
                };
                (r.k, eff, pl.separation.clone())
            }
            None => (0, 0, Default::default()),
        };
        let horizon = slot_start_ms
            - eff_ms
                .max(sep.title_minutes * 60_000)
                .max(sep.artist_minutes * 60_000)
                .max(sep.album_minutes * 60_000);
        let recent: Vec<&PlayRec> = chain
            .recs
            .iter()
            .filter(|r| r.start_ms > horizon - 1 && r.start_ms <= slot_start_ms)
            .collect();
        let item_of = |u: &Unit| Self::unit_first_item(cat, u);
        let eligible = |u: &Unit, level: u8| -> bool {
            let Some(first) = item_of(u) else {
                return false;
            };
            let Some(m) = cat.meta(first) else {
                return false;
            };
            let within =
                |r: &&PlayRec, min: i64| min > 0 && slot_start_ms - r.start_ms < min * 60_000;
            if level < 4 {
                let w = eff_ms;
                if w > 0
                    && recent
                        .iter()
                        .any(|r| r.item_id == first && slot_start_ms - r.start_ms < w)
                {
                    return false;
                }
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
        let last_played = |u: &Unit| -> i64 {
            let Some(first) = item_of(u) else {
                return i64::MIN;
            };
            chain
                .recs
                .iter()
                .rev()
                .find(|r| r.item_id == first)
                .map_or(i64::MIN, |r| r.start_ms)
        };
        let rules = Rules {
            eligible: &eligible,
            last_played: &last_played,
        };
        let mut deck = self.st.decks.remove(key).unwrap_or_default();
        let pick = deck.pick(pool, k, &rules, &mut self.st.rng);
        self.st.decks.insert(key.to_string(), deck);
        let pick = pick?;
        // `rule_relaxed`: a rule had to give (the effective window, or a separation rule), or nothing
        // passed and the unit that played longest ago was taken (4.3). A playlist whose requested
        // window was cut to fit is not "relaxed" on every play: the form says it was cut.
        let relaxed = pick.relaxed > 0 || pick.fallback;
        Some((pick, relaxed))
    }

    pub(crate) fn new_run(&mut self, members: Vec<i64>, continuity: &str) -> i64 {
        let id = self.st.next_run_id;
        self.st.next_run_id += 1;
        self.st.runs.insert(
            id,
            RunInfo {
                members,
                continuity: continuity.into(),
            },
        );
        id
    }

    /// The entries of an album unit: its playable tracks in disc and track order as one run (3.7).
    pub(crate) fn expand_album(
        &mut self,
        album_key: &str,
        gapless: bool,
        origin: Origin,
        cat: &Catalog,
    ) -> Vec<QEntry> {
        let tracks = cat.album_tracks(album_key);
        if tracks.is_empty() {
            return vec![];
        }
        let members: Vec<i64> = tracks.iter().map(|t| t.meta.item_id).collect();
        let run = self.new_run(members, if gapless { "gapless" } else { "normal" });
        tracks
            .iter()
            .map(|t| {
                let mut e = QEntry::new(self.take_id(), t.meta.item_id, origin);
                e.run_id = Some(run);
                e
            })
            .collect()
    }

    /// A signature of everything the planned times depend on, other than the wall clock.
    fn times_signature(&self, queue: &[QEntry], cat: &Catalog) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.st.on_air.as_ref().map(|e| e.id).hash(&mut h);
        self.st.on_air_start_ms.hash(&mut h);
        cat.generation.hash(&mut h);
        for e in queue {
            (e.id, e.continuity_from_prev).hash(&mut h);
        }
        h.finish()
    }

    /// Recompute the planned times only if the queue, what is on air or the catalog changed, or the
    /// head's planned start has fallen behind the clock.
    fn refresh_times(
        &mut self,
        queue: &mut [QEntry],
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) {
        let sig = self.times_signature(queue, cat);
        let stale = queue
            .first()
            .and_then(|e| e.planned_start_ms)
            .is_some_and(|s| s < now_ms);
        if self.times_sig == Some(sig)
            && !stale
            && queue.iter().all(|e| e.planned_start_ms.is_some())
        {
            return;
        }
        self.set_times(queue, now_ms, cat, timing);
        self.times_sig = Some(self.times_signature(queue, cat));
    }

    pub(crate) fn set_times(
        &self,
        queue: &mut [QEntry],
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) {
        let prev: Option<(&TrackMeta, i64)> = match (&self.st.on_air, self.st.on_air_start_ms) {
            (Some(e), Some(s)) => cat.meta(e.item_id).map(|m| (m, s)),
            _ => None,
        };
        let mut starts: Vec<Option<i64>> = Vec::with_capacity(queue.len());
        let mut last_meta: Option<(&TrackMeta, i64)> = prev;
        for e in queue.iter() {
            let Some(m) = cat.meta(e.item_id) else {
                starts.push(None);
                continue;
            };
            let start = match last_meta {
                Some((pm, ps)) => {
                    (ps + timing.advance_ms(pm, m, e.continuity_from_prev)).max(now_ms)
                }
                None => now_ms,
            };
            starts.push(Some(start));
            last_meta = Some((m, start));
        }
        for (i, e) in queue.iter_mut().enumerate() {
            e.planned_start_ms = starts[i];
        }
        for i in 0..queue.len() {
            let seg = starts.get(i + 1).copied().flatten().or_else(|| {
                let m = cat.meta(queue[i].item_id)?;
                Some(starts[i]? + m.effective_ms)
            });
            queue[i].planned_segue_ms = seg;
        }
    }

    /// Link runs (3.7): `continuity_from_prev` is true only where the entry is in a gapless run and
    /// adjacent to the entry before it. `prev` is the entry before `queue[0]` (the one on air).
    pub(crate) fn recompute_links(&self, queue: &mut [QEntry], cat: &Catalog) {
        let mut prev: Option<QEntry> = self.st.on_air.clone();
        for e in queue.iter_mut() {
            let gapless = e
                .run_id
                .and_then(|r| self.st.runs.get(&r))
                .is_some_and(|r| r.continuity == "gapless");
            e.continuity_from_prev = match (&prev, gapless) {
                (Some(p), true) if p.run_id == e.run_id && e.run_id.is_some() => {
                    match (cat.meta(p.item_id), cat.meta(e.item_id)) {
                        (Some(a), Some(b)) => cat.adjacent(a, b),
                        _ => false,
                    }
                }
                _ => false,
            };
            prev = Some(e.clone());
        }
    }

    /// The `insert_every` blocks that are due at the slot after `queue`, highest priority first (4.5): at
    /// most two non-music items in a row, a counter that has reached `n`, and the minimum gap.
    pub(crate) fn due_inserts(
        &self,
        queue: &[QEntry],
        slot_start: i64,
        cat: &Catalog,
    ) -> Vec<(i64, usize, Block, InsertEvery)> {
        let trailing_inserts = queue
            .iter()
            .rev()
            .chain(self.st.on_air.iter())
            .take_while(|e| e.origin == Origin::Insert)
            .count();
        let mut due: Vec<(i64, usize, Block, InsertEvery)> = Vec::new();
        if trailing_inserts >= 2 {
            return due;
        }
        for (i, (b, ie)) in self.insert_blocks().into_iter().enumerate() {
            let c = self.planned_counter(b, ie, queue, cat);
            if c.count < ie.n {
                continue;
            }
            let last_fire = queue
                .iter()
                .rev()
                .find(|e| e.origin == Origin::Insert && e.block_id == Some(b.id))
                .and_then(|e| e.planned_start_ms)
                .or(c.last_fired_ms);
            if ie.min_gap_minutes > 0
                && last_fire.is_some_and(|f| slot_start - f < ie.min_gap_minutes * 60_000)
            {
                continue;
            }
            due.push((b.priority, i, b.clone(), ie.clone()));
        }
        due.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        due
    }

    /// Produce the next entries after `queue` (one, or the tracks of an album), or `None` when there is
    /// nothing playable. This is where `insert_every` is decided (4.5) and where a rotation deck deals.
    fn generate(
        &mut self,
        queue: &[QEntry],
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> Option<Vec<QEntry>> {
        use crate::timed::TimedGen;
        match self.generate_timed(queue, now_ms, cat, timing) {
            TimedGen::Entries(v) => return Some(v),
            TimedGen::Gated => return None,
            TimedGen::Nothing => {}
        }
        if self.st.program.as_ref().is_some_and(|p| p.halted) {
            return None;
        }
        self.generate_normal(queue, now_ms, cat, timing, false)
    }

    /// The slot after `queue` by the program's own rules: an `insert_every` that is due, else the
    /// rotation or the cursor. `skip_inserts`: the first item of a new program goes first.
    pub(crate) fn generate_normal(
        &mut self,
        queue: &[QEntry],
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
        skip_inserts: bool,
    ) -> Option<Vec<QEntry>> {
        let program = self.st.program.clone()?;
        let pl = self.prog.playlists.get(&program.playlist_id)?.clone();
        // The planned start of the slot being filled.
        let slot_start = {
            let last = queue
                .last()
                .map(|e| (e.item_id, e.planned_start_ms))
                .or_else(|| {
                    self.st
                        .on_air
                        .as_ref()
                        .map(|e| (e.item_id, self.st.on_air_start_ms))
                });
            match last {
                Some((id, Some(start))) => {
                    let a = cat.meta(id);
                    // The next item's own identity is unknown yet; assume no continuity.
                    a.map_or(now_ms, |a| {
                        (start + timing.advance_ms(a, a, false)).max(now_ms)
                    })
                }
                _ => now_ms,
            }
        };
        let chain = self.chain(queue, cat);

        // ---- an insert that is due goes first (4.5) ----
        if !skip_inserts {
            let due = self.due_inserts(queue, slot_start, cat);
            for (_, _, b, ie) in due {
                let Some((key, pool, _)) = self.source_pool(&ie.source, cat) else {
                    continue;
                };
                if pool.is_empty() {
                    continue;
                }
                if let Some((pick, _)) = self.deal(&key, &pool, None, &chain, slot_start, cat) {
                    if let Unit::Item(id) = pick.unit {
                        let mut e = QEntry::new(self.take_id(), id, Origin::Insert);
                        e.block_id = Some(b.id);
                        e.playlist_id = Some(pl.id);
                        e.deck = Some(DeckRef {
                            key: key.clone(),
                            gen: pick.gen,
                            seq: pick.seq,
                        });
                        return Some(vec![e]);
                    }
                }
            }
        }

        // ---- the program itself ----
        match pl.mode {
            Mode::Shuffle => {
                let pool = self.playable_units(&pl, cat);
                if pool.is_empty() {
                    return None;
                }
                let key = format!("pl:{}", pl.id);
                let (pick, relaxed) = self.deal(&key, &pool, Some(&pl), &chain, slot_start, cat)?;
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
                    Unit::Album(k) => {
                        let gapless = pl.entries.iter().any(|e| {
                            matches!(e, Entry::Album { album_key, gapless: true, .. } if album_key == k)
                        });
                        self.expand_album(k, gapless, Origin::Rotation, cat)
                    }
                };
                for e in &mut out {
                    e.playlist_id = Some(pl.id);
                    e.deck = Some(dref.clone());
                    if relaxed {
                        e.flags.push("rule_relaxed".into());
                    }
                }
                Some(out)
            }
            Mode::Sequential => {
                self.generate_sequential(&pl, cat, queue, slot_start, now_ms, timing)
            }
        }
    }

    /// Adjacent album tracks meeting in a `sequential` walk form a run (3.7). `q` is the queue so far
    /// (the entry before `new` is its last, or the one on air); a run started by the entry before is
    /// recorded on it too.
    fn link_sequential(&mut self, q: &mut [QEntry], new: &mut [QEntry], cat: &Catalog) {
        for j in 0..new.len() {
            // The entry before new[j].
            let before: Option<QEntry> = if j > 0 {
                Some(new[j - 1].clone())
            } else {
                q.last().cloned().or_else(|| self.st.on_air.clone())
            };
            let Some(before) = before else { continue };
            let (Some(a), Some(b)) = (cat.meta(before.item_id), cat.meta(new[j].item_id)) else {
                continue;
            };
            if !(new[j].origin == Origin::Rotation
                && before.origin == Origin::Rotation
                && new[j].run_id.is_none()
                && cat.adjacent(a, b))
            {
                continue;
            }
            let rid = match before.run_id {
                Some(r) => r,
                None => {
                    let r = self.new_run(vec![before.item_id], "gapless");
                    if j > 0 {
                        new[j - 1].run_id = Some(r);
                    } else if let Some(last) = q.last_mut() {
                        last.run_id = Some(r);
                    } else if let Some(oa) = self.st.on_air.as_mut() {
                        oa.run_id = Some(r);
                    }
                    r
                }
            };
            if let Some(r) = self.st.runs.get_mut(&rid) {
                r.members.push(new[j].item_id);
            }
            new[j].run_id = Some(rid);
        }
    }

    // ---- filling ---------------------------------------------------------------------------------------

    fn depth_reached(&self, queue: &[QEntry], now_ms: i64) -> bool {
        let highest_pinned = queue.iter().rposition(|e| e.pinned).map_or(0, |i| i + 1);
        if queue.len() < self.cfg.plan_items.max(highest_pinned) {
            return false;
        }
        let end = queue
            .last()
            .and_then(|e| e.planned_segue_ms)
            .unwrap_or(now_ms);
        end - now_ms >= self.cfg.plan_ms
    }

    /// Keep the soft plan `plan_items` and `plan_ms` deep. Returns how many entries were added.
    pub fn fill(&mut self, now_ms: i64, cat: &Catalog, timing: &dyn Timing) -> usize {
        self.ensure_events(now_ms, cat, timing);
        let mut q = std::mem::take(&mut self.st.queue);
        self.refresh_times(&mut q, now_ms, cat, timing);
        // An entry a timed block planned does not start before its window opens. After a restart, a
        // clock step or any change that moved what plays before it, the plan is dealt again.
        let early = q.iter().any(|e| {
            !e.committed
                && !e.pinned
                && e.timed.as_ref().is_some_and(|t| {
                    !t.late && e.planned_start_ms.is_some_and(|s| s + 1000 < t.lo_ms)
                })
        });
        if early && !self.replanning {
            self.st.queue = q;
            self.replanning = true;
            self.regenerate(now_ms, cat, timing, false);
            self.replanning = false;
            return 0;
        }
        let mut added = 0;
        while !self.depth_reached(&q, now_ms) && added < 400 {
            let Some(mut more) = self.generate(&q, now_ms, cat, timing) else {
                break;
            };
            if self
                .program_playlist()
                .is_some_and(|p| p.mode == Mode::Sequential)
            {
                self.link_sequential(&mut q, &mut more, cat);
            }
            added += more.len();
            q.extend(more);
            self.recompute_links(&mut q, cat);
            self.set_times(&mut q, now_ms, cat, timing);
        }
        self.st.queue = q;
        self.lock(now_ms);
        self.prune_runs();
        added
    }

    /// The head becomes committed when it is within `lock_ahead` of its start (4.2).
    pub fn lock(&mut self, now_ms: i64) {
        if let Some(h) = self.st.queue.first_mut() {
            if h.planned_start_ms
                .is_some_and(|s| s - now_ms <= self.cfg.lock_ahead_ms)
            {
                h.committed = true;
            }
        }
    }

    fn prune_runs(&mut self) {
        let live: std::collections::BTreeSet<i64> = self
            .st
            .queue
            .iter()
            .chain(self.st.on_air.iter())
            .filter_map(|e| e.run_id)
            .collect();
        self.st.runs.retain(|id, _| live.contains(id));
    }

    // ---- program ----------------------------------------------------------------------------------------

    /// Begin running `playlist_id` as the program from `t0_ms`. The queue is left alone: use
    /// [`Scheduler::switch`] for an operator's change.
    pub fn start_program(&mut self, playlist_id: i64, t0_ms: i64) {
        self.start_program_with(playlist_id, t0_ms, "auto", None);
    }

    /// As [`Scheduler::start_program`], saying where T0 came from (5.3 `t0_source`) and which scheduled
    /// occurrence the run belongs to.
    pub fn start_program_with(
        &mut self,
        playlist_id: i64,
        t0_ms: i64,
        t0_source: &str,
        nominal_t0_ms: Option<i64>,
    ) {
        let run_id = self.st.next_run_id;
        self.st.next_run_id += 1;
        let manual = self
            .prog
            .playlists
            .get(&playlist_id)
            .and_then(|p| p.start_block())
            .is_some_and(|(_, s)| s.anchor == crate::programming::Anchor::Manual);
        self.st.program = Some(ProgramRun {
            playlist_id,
            t0_ms,
            run_id,
            seq_pos: 0,
            floor: 0,
            fill: None,
            manual,
            halted: false,
            loops: BTreeMap::new(),
            wait_since: None,
            released: false,
            hold: None,
            hold_until_ms: None,
            silenced: false,
            stack: Vec::new(),
            t0_source: t0_source.into(),
            nominal_t0_ms,
            epoch: 0,
            ended: false,
            bt: None,
        });
        self.ev_cache = EventCache::default();
    }

    // ---- what starts ------------------------------------------------------------------------------------

    /// `entry_id` began to sound at `start_ms`: it is now on air, the entry that was on air goes into
    /// the history, and the counters move. Entries ahead of it in the queue were passed over. Returns the
    /// entry that is now on air.
    pub fn on_started(&mut self, entry_id: i64, start_ms: i64, cat: &Catalog) -> Option<QEntry> {
        let pos = self.st.queue.iter().position(|e| e.id == entry_id)?;
        self.st.queue.drain(..pos);
        let e = self.st.queue.remove(0);
        Some(self.begin(e, start_ms, cat))
    }

    /// An entry that is no longer in the queue began to sound (its boundary was committed and could not
    /// be taken back before an operator removed it or the plan was regenerated): it goes on air with the
    /// same bookkeeping as [`Scheduler::on_started`], so the history, the no-repeat check and the insert
    /// counters still see it and the item it replaced.
    pub fn adopt(&mut self, e: QEntry, start_ms: i64, cat: &Catalog) -> QEntry {
        self.st.queue.retain(|q| q.id != e.id);
        self.begin(e, start_ms, cat)
    }

    fn begin(&mut self, mut e: QEntry, start_ms: i64, cat: &Catalog) -> QEntry {
        e.committed = true;
        if let (Some(prev), Some(s)) = (self.st.on_air.take(), self.st.on_air_start_ms) {
            if let Some(m) = cat.meta(prev.item_id) {
                self.st.history.push(PlayRec::of(m, s));
            }
        }
        let horizon = start_ms - self.cfg.history_ms;
        self.st.history.retain(|r| r.start_ms >= horizon);
        if let Some(m) = cat.meta(e.item_id) {
            let blocks: Vec<(i64, InsertEvery)> = self
                .insert_blocks()
                .into_iter()
                .map(|(b, ie)| (b.id, ie.clone()))
                .collect();
            for (bid, ie) in blocks {
                let c = self.st.counters.entry(bid).or_default();
                Self::apply_count(c, &ie, bid, &e, m, start_ms);
            }
        }
        if let Some(r) = &e.deck {
            if let Some(d) = self.st.decks.get_mut(&r.key) {
                d.hard_seq = d.hard_seq.max(Some(r.seq));
            }
        }
        self.st.on_air = Some(e.clone());
        self.st.on_air_start_ms = Some(start_ms);
        self.on_timed_started(&e, start_ms);
        e
    }

    /// Note that the entry on air ended (the station went quiet, or the queue ran out).
    pub fn on_air_ended(&mut self, cat: &Catalog) {
        if let (Some(prev), Some(s)) = (self.st.on_air.take(), self.st.on_air_start_ms.take()) {
            if let Some(m) = cat.meta(prev.item_id) {
                self.st.history.push(PlayRec::of(m, s));
            }
        }
    }

    // ---- regeneration -------------------------------------------------------------------------------------

    /// Regenerate the soft plan: the decks go back to the last retained pick and deal again. Retained
    /// are the entry on air, committed entries, pinned entries, and the rest of a run whose earlier
    /// member is retained. `reshuffle` also redraws what is left of the current pass, with the items
    /// played within the window moved to the end (4.3).
    pub fn regenerate(&mut self, now_ms: i64, cat: &Catalog, timing: &dyn Timing, reshuffle: bool) {
        let q = std::mem::take(&mut self.st.queue);
        let mut retained: Vec<bool> = q.iter().map(|e| e.pinned || e.committed).collect();
        // The rest of a run stays with its retained members.
        let mut runs_kept: std::collections::BTreeSet<i64> =
            self.st.on_air.iter().filter_map(|e| e.run_id).collect();
        for (i, e) in q.iter().enumerate() {
            if retained[i] {
                if let Some(r) = e.run_id {
                    runs_kept.insert(r);
                }
            }
        }
        for (i, e) in q.iter().enumerate() {
            if !retained[i] && e.run_id.is_some_and(|r| runs_kept.contains(&r)) {
                // Only entries directly continuing a kept run: the run's later members.
                retained[i] = true;
            }
        }
        // A scheduled start or `start_playlist` planned into a soft entry that is being dealt again: the
        // program is what it was before it.
        if let Some(before) = q
            .iter()
            .zip(retained.iter())
            .filter(|(_, r)| !**r)
            .find_map(|(e, _)| e.prog_before.clone())
        {
            self.st.program = Some(before);
            self.ev_cache = crate::timed::EventCache::default();
        }
        // Rewind every deck to its last retained pick.
        let mut last_seq: BTreeMap<String, u64> = BTreeMap::new();
        for e in self.st.on_air.iter().chain(
            q.iter()
                .zip(retained.iter())
                .filter(|(_, r)| **r)
                .map(|(e, _)| e),
        ) {
            if let Some(d) = &e.deck {
                let s = last_seq.entry(d.key.clone()).or_insert(d.seq);
                *s = (*s).max(d.seq);
            }
        }
        let keys: Vec<String> = self.st.decks.keys().cloned().collect();
        for k in keys {
            if let Some(d) = self.st.decks.get_mut(&k) {
                let target = last_seq.get(&k).copied().max(d.hard_seq);
                d.rewind_to(target);
            }
        }
        // The cursor goes back the same way: to what it was after the last entry that stays (unless an act
        // or an operator action moved it since: then it stays where it is).
        if let Some(p) = self.st.program.as_mut() {
            let best = self
                .st
                .on_air
                .iter()
                .chain(
                    q.iter()
                        .zip(retained.iter())
                        .filter(|(_, r)| **r)
                        .map(|(e, _)| e),
                )
                .rfind(|e| {
                    e.prog_after
                        .as_ref()
                        .is_some_and(|s| s.run_id == p.run_id && s.epoch == p.epoch)
                });
            if let Some(snap) = best.and_then(|e| e.prog_after.as_ref()) {
                let mut c = snap.cursor();
                c.floor = c.floor.max(p.floor);
                p.load_cursor(c);
                p.stack = snap.stack.clone();
                p.silenced = snap.silenced;
            } else if p.stack.is_empty()
                && self
                    .prog
                    .playlists
                    .get(&p.playlist_id)
                    .is_some_and(|pl| pl.mode == Mode::Sequential)
                && self.st.on_air.is_none()
            {
                p.seq_pos = p.floor;
                if p.floor == 0 {
                    p.fill = None;
                    p.hold = None;
                    p.wait_since = None;
                    p.loops.clear();
                }
            }
        }
        // In-sequence acts whose entries are not kept go with them.
        {
            let kept: std::collections::BTreeSet<i64> = self
                .st
                .on_air
                .iter()
                .map(|e| e.id)
                .chain(
                    q.iter()
                        .zip(retained.iter())
                        .filter(|(_, r)| **r)
                        .map(|(e, _)| e.id),
                )
                .collect();
            self.st.seq_acts.retain(|a| {
                a.after_entry.is_none_or(|i| kept.contains(&i))
                    && a.before_entry.is_none_or(|i| kept.contains(&i))
            });
        }
        if reshuffle {
            if let Some(pl) = self.program_playlist().cloned() {
                if pl.mode == Mode::Shuffle {
                    let mut chain = self.chain(&[], cat);
                    chain.recs.retain(|r| r.start_ms <= now_ms + 3_600_000);
                    let pool = self.playable_units(&pl, cat);
                    let window = (self
                        .rotation_of(&pool, pl.no_repeat_minutes, cat)
                        .effective_minutes
                        * 60_000.0)
                        .round() as i64;
                    let recent_ids: std::collections::HashSet<i64> = chain
                        .recs
                        .iter()
                        .filter(|r| window > 0 && now_ms - r.start_ms < window)
                        .map(|r| r.item_id)
                        .collect();
                    let key = format!("pl:{}", pl.id);
                    if let Some(deck) = self.st.decks.get_mut(&key) {
                        let recent = |u: &Unit| match u {
                            Unit::Item(i) => recent_ids.contains(i),
                            Unit::Album(k) => cat
                                .album_tracks(k)
                                .first()
                                .is_some_and(|t| recent_ids.contains(&t.meta.item_id)),
                        };
                        deck.reshuffle_rest(&mut self.st.rng, &recent);
                    }
                }
            }
        }
        // Build the new queue: retained entries keep their place, soft ones are dealt again.
        let mut out: Vec<QEntry> = Vec::with_capacity(q.len());
        for (i, e) in q.into_iter().enumerate() {
            if retained[i] {
                out.push(e);
                continue;
            }
            self.set_times(&mut out, now_ms, cat, timing);
            if let Some(more) = self.generate(&out, now_ms, cat, timing) {
                out.extend(more);
            }
        }
        self.recompute_links(&mut out, cat);
        self.st.queue = out;
        self.fill(now_ms, cat, timing);
    }

    pub(crate) fn head_id(&self) -> Option<i64> {
        self.st.queue.first().map(|e| e.id)
    }

    fn effect(&self, before: Option<i64>, entry_id: Option<i64>) -> OpEffect {
        let after = self.head_id();
        OpEffect {
            head_changed: before != after,
            entry_id,
            new_next_item: self.st.queue.first().map(|e| e.item_id),
        }
    }

    /// Reshuffle (4.3, 4.7): refused for a `sequential` playlist.
    pub fn reshuffle(
        &mut self,
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> Result<OpEffect, String> {
        match self.program_playlist() {
            None => return Err("nothing is running".into()),
            Some(p) if p.mode == Mode::Sequential => {
                return Err(
                    "a sequential playlist plays in its own order and cannot be reshuffled".into(),
                )
            }
            _ => {}
        }
        let before = self.head_id();
        self.regenerate(now_ms, cat, timing, true);
        Ok(self.effect(before, None))
    }

    // ---- manual operations (4.7) -----------------------------------------------------------------------------

    fn index_of(&self, at: At) -> usize {
        match at {
            At::Next => 0,
            At::Index(i) => i.min(self.st.queue.len()),
            At::End => self.st.queue.len(),
        }
    }

    /// Insert an item as a pinned entry.
    pub fn insert(
        &mut self,
        item_id: i64,
        at: At,
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> Result<OpEffect, String> {
        if !cat.meta(item_id).is_some_and(|m| m.playable) {
            return Err(format!("item {item_id} is not playable"));
        }
        let before = self.head_id();
        let mut e = QEntry::new(self.take_id(), item_id, Origin::Manual);
        e.pinned = true;
        let id = e.id;
        let idx = self.index_of(at);
        // The entry it displaces from the head is no longer next: it is not committed any more.
        if idx == 0 {
            if let Some(h) = self.st.queue.first_mut() {
                h.committed = false;
            }
        }
        self.st.queue.insert(idx, e);
        self.after_edit(now_ms, cat, timing);
        Ok(self.effect(before, Some(id)))
    }

    /// `play album`: the album's tracks in disc and track order as one run of pinned entries.
    pub fn play_album(
        &mut self,
        album_key: &str,
        gapless: bool,
        at: At,
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> Result<OpEffect, String> {
        let mut entries = self.expand_album(album_key, gapless, Origin::Manual, cat);
        if entries.is_empty() {
            return Err(format!(
                "the library holds no playable tracks of `{album_key}`"
            ));
        }
        let before = self.head_id();
        for e in &mut entries {
            e.pinned = true;
        }
        let first = entries[0].id;
        let idx = self.index_of(at);
        if idx == 0 {
            if let Some(h) = self.st.queue.first_mut() {
                h.committed = false;
            }
        }
        for (k, e) in entries.into_iter().enumerate() {
            self.st.queue.insert(idx + k, e);
        }
        self.after_edit(now_ms, cat, timing);
        Ok(self.effect(before, Some(first)))
    }

    pub fn remove(
        &mut self,
        entry_id: i64,
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> Result<OpEffect, String> {
        let pos = self
            .st
            .queue
            .iter()
            .position(|e| e.id == entry_id)
            .ok_or_else(|| format!("no queue entry {entry_id}"))?;
        let before = self.head_id();
        let removed = self.st.queue.remove(pos);
        // A removed rotation pick stays dealt: it is gone, not to be dealt again this pass.
        let _ = removed;
        if pos == 0 {
            if let Some(h) = self.st.queue.first_mut() {
                h.committed = false;
            }
        }
        self.after_edit(now_ms, cat, timing);
        Ok(self.effect(before, None))
    }

    /// Set or clear the per-entry transition of one queue entry (2.6, resolution step 4): `cut`, or a JSON
    /// `PresetPatch`, for the boundary into that entry only. The caller validates the text.
    pub fn set_transition(
        &mut self,
        entry_id: i64,
        transition: Option<String>,
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> Result<OpEffect, String> {
        let pos = self
            .st
            .queue
            .iter()
            .position(|e| e.id == entry_id)
            .ok_or_else(|| format!("no queue entry {entry_id}"))?;
        let before = self.head_id();
        let e = &mut self.st.queue[pos];
        e.transition = transition;
        if pos == 0 {
            e.committed = false;
        }
        self.after_edit(now_ms, cat, timing);
        Ok(self.effect(before, Some(entry_id)))
    }

    /// Move an entry to `to`; a moved entry is pinned there.
    pub fn move_entry(
        &mut self,
        entry_id: i64,
        to: usize,
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> Result<OpEffect, String> {
        let pos = self
            .st
            .queue
            .iter()
            .position(|e| e.id == entry_id)
            .ok_or_else(|| format!("no queue entry {entry_id}"))?;
        let before = self.head_id();
        let mut e = self.st.queue.remove(pos);
        e.pinned = true;
        e.committed = false;
        let to = to.min(self.st.queue.len());
        if to == 0 || pos == 0 {
            if let Some(h) = self.st.queue.first_mut() {
                h.committed = false;
            }
        }
        self.st.queue.insert(to, e);
        self.after_edit(now_ms, cat, timing);
        Ok(self.effect(before, Some(entry_id)))
    }

    /// After an operator edit: links and times are recomputed and the plan refilled. Soft entries are
    /// kept as they are (they were dealt already); only a reshuffle or a programming change redeals.
    fn after_edit(&mut self, now_ms: i64, cat: &Catalog, timing: &dyn Timing) {
        let mut q = std::mem::take(&mut self.st.queue);
        self.recompute_links(&mut q, cat);
        self.set_times(&mut q, now_ms, cat, timing);
        self.st.queue = q;
        self.lock(now_ms);
        self.fill(now_ms, cat, timing);
    }

    /// `switch playlist` (4.7): the program is replaced. On air stays; everything not pinned is dealt
    /// again from the new playlist's deck.
    pub fn switch(
        &mut self,
        playlist_id: i64,
        how: SwitchHow,
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> Result<OpEffect, String> {
        self.switch_with(
            playlist_id,
            how,
            now_ms,
            now_ms,
            "switch",
            None,
            cat,
            timing,
        )
    }

    /// As [`Scheduler::switch`], with the program's T0 (and where it came from) given: a program start (4.7).
    #[allow(clippy::too_many_arguments)]
    pub fn switch_with(
        &mut self,
        playlist_id: i64,
        how: SwitchHow,
        now_ms: i64,
        t0_ms: i64,
        t0_source: &str,
        nominal_t0_ms: Option<i64>,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> Result<OpEffect, String> {
        let pl = self
            .prog
            .playlists
            .get(&playlist_id)
            .ok_or_else(|| format!("no playlist {playlist_id}"))?;
        if !pl.enabled {
            return Err(format!("playlist `{}` is disabled", pl.name));
        }
        if self.playable_units(pl, cat).is_empty() && pl.mode == Mode::Shuffle {
            return Err(format!("playlist `{}` has nothing playable", pl.name));
        }
        let before = self.head_id();
        self.start_program_with(playlist_id, t0_ms, t0_source, nominal_t0_ms);
        // Everything soft and committed goes; pinned entries stay.
        let old = std::mem::take(&mut self.st.queue);
        self.st.queue = old.into_iter().filter(|e| e.pinned).collect();
        // The new playlist's deck resumes after what was really dealt (on air).
        // Every deck goes back to what has really started: the soft picks it dealt are gone.
        for d in self.st.decks.values_mut() {
            let hard = d.hard_seq;
            d.rewind_to(hard);
        }
        // `now`: the new program's first item takes over at once, so it is planned from the present, not from
        // the end of the item that is sounding (which a transition is about to replace).
        let held = (how == SwitchHow::Now)
            .then(|| (self.st.on_air.take(), self.st.on_air_start_ms.take()));
        let mut q = std::mem::take(&mut self.st.queue);
        self.set_times(&mut q, now_ms, cat, timing);
        self.st.queue = q;
        self.fill(now_ms, cat, timing);
        if let Some((e, s)) = held {
            self.st.on_air = e;
            self.st.on_air_start_ms = s;
            self.times_sig = None;
        }
        if how == SwitchHow::End {
            // The first pick of the new playlist: after the outgoing cue-out, no overlap.
            if let Some(h) = self.st.queue.first_mut() {
                h.transition = Some(END_TRANSITION.into());
            }
        }
        Ok(self.effect(before, None))
    }

    // ---- persistence ---------------------------------------------------------------------------------------

    pub fn snapshot(&self) -> SchedState {
        self.st.clone()
    }
}

/// The wall clock of a virtual run: helper for the tests and the simulator.
pub type History = VecDeque<PlayRec>;
