//! The running station (milestone M1.5): the scheduler, the conductor and `state.db` joined. It is
//! driven from outside by [`Station::step`] with the mixer's frame position and the wall time at which
//! that frame is audible, so the same code runs under the real rt-out thread, under the paced file
//! output of the restart tests and under the offline renderer (`offline`).
//!
//! What it does, in order, every step:
//!
//! 1. collect the mixer's events: a voice that started makes its queue entry the one on air (the
//!    scheduler learns it, the as-run row is written);
//! 2. poll the library for changes (every 2 s) and copy what changed into the caches;
//! 3. keep the plan deep (`Scheduler::fill`) and decide whether the next boundary is due: when its first
//!    command frame is within the commit-ahead the conductor loads the item and sends the stops;
//! 4. write the checkpoint every 2 s.
//!
//! Every operator action (`op`) first takes back a boundary that was committed for an entry which is
//! going away (`Conductor::uncommit_last`), so an action always acts on what is sounding.

use crate::catalog::{Catalog, TrackMeta};
use crate::conductor::{Conductor, ConductorConfig, VoiceEvent};
use crate::failure::Gate;
use crate::item::{run_gain, Item, RunGain};
use crate::plan::{plan, s_frames, PlacedView, PlanInput, TransitionKind, MIN_LEAD_S};
use crate::programming::Programming;
use crate::sched::{At, OpEffect, QEntry, SchedConfig, Scheduler, SwitchHow, Timing};
use crate::state::{self, AsRunRow, Checkpoint, Loaded, Msg, StateWriter};
use crate::sync::LibrarySync;
use crate::timed::TimedAct;
use common::transition::{resolve, Chain, PresetBook, PresetPatch};
use engine::mixer::{EndReason, Mixer, VoiceId};
use engine::rt::Command;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// How long before its first frame a boundary is committed (the design's commit-ahead, 1.5).
pub const COMMIT_AHEAD_S: f64 = 10.0;
/// How long before its time an audio block (a timed stop) is set up on the mixer (4.9: 5 s ahead).
pub const ACT_AHEAD_MS: i64 = 5_000;
/// A first item that waits for its time is placed this long before it.
pub const START_AHEAD_MS: i64 = 2_000;
/// The resume fade (1.7).
pub const RESUME_FADE_MS: f64 = 150.0;
/// A checkpoint older than this is not resumed (1.7).
pub const RESUME_MAX_AGE_MS: i64 = 120_000;
/// At least this much of an item must remain before its segue to resume it in place (1.7).
pub const RESUME_MIN_REMAINING_MS: i64 = 20_000;

#[derive(Debug, Clone)]
pub struct StationOptions {
    pub name: String,
    pub rate: u32,
    pub preset: String,
    pub target_lufs: f64,
    pub max_boost_db: f64,
    /// The output trim from the toml, dB (2.4); a value saved in `state.db` wins at start.
    pub output_trim_db: f64,
    pub peak_allowance_db: f64,
    /// The automatic fade at the end of an item flagged `truncated_fade`: on or off, and its length, seconds.
    pub truncated_fade: bool,
    pub truncated_fade_s: f64,
    pub lock_ahead_s: u32,
    /// The playlist run when nothing else is (by name).
    pub default_playlist: Option<String>,
    /// The mixer's limiter lookahead, frames (`Mixer::limiter_latency_frames`).
    pub lookahead_frames: u64,
    /// Keep the whole sidecar (tests); a station that runs for months compacts it.
    pub record_sidecar: bool,
    pub seed: u64,
    pub decoder_threads: usize,
    pub checkpoint_every_ms: i64,
    pub sync_every_ms: i64,
    pub media_dir: PathBuf,
    /// The emergency playlist, by name (1.5): used for safe mode, silence escalation, a starved queue.
    pub emergency_playlist: Option<String>,
    /// `stopped_is_normal`: an empty queue is never starvation (no alert, no emergency playlist).
    pub stopped_is_normal: bool,
    /// How much of it the RAM loop holds, seconds (300).
    pub ram_loop_seconds: u32,
    /// The program-silence window, seconds (15); the escalation steps are one window apart.
    pub silence_s: f64,
    /// Seconds on air and healthy after which the `restarted` alert clears itself; 0 keeps it until acknowledged.
    pub restart_alert_clear_s: f64,
    /// The silence threshold, dBFS RMS (-50).
    pub silence_dbfs: f64,
    /// Where the nightly `state.db` snapshot goes (5.5, `/var/lib/hotclock/backup`); `None` writes none.
    pub backup_dir: Option<PathBuf>,
    /// Categories (or kinds) the loudness windows leave out while they are audible (6.1 `gate_categories`).
    pub loudness_gate: Vec<String>,
    /// IANA name of the zone timed blocks are written in (1.6).
    pub time_zone: String,
}

impl StationOptions {
    /// The automatic fade of a `truncated_fade` item, seconds; 0 when it is off.
    pub fn truncated_fade_seconds(&self) -> f64 {
        if self.truncated_fade {
            self.truncated_fade_s
        } else {
            0.0
        }
    }

    pub fn new(name: &str, media_dir: PathBuf) -> Self {
        Self {
            name: name.into(),
            rate: 48_000,
            preset: "plexamp".into(),
            target_lufs: -18.0,
            max_boost_db: 12.0,
            output_trim_db: 0.0,
            peak_allowance_db: 6.0,
            truncated_fade: true,
            truncated_fade_s: crate::plan::TRUNCATED_FADE_S,
            lock_ahead_s: 20,
            default_playlist: None,
            lookahead_frames: 96,
            record_sidecar: false,
            seed: 0x5EED,
            decoder_threads: 3,
            checkpoint_every_ms: 2000,
            sync_every_ms: 2000,
            media_dir,
            emergency_playlist: None,
            stopped_is_normal: false,
            ram_loop_seconds: 300,
            silence_s: 15.0,
            restart_alert_clear_s: 600.0,
            silence_dbfs: -50.0,
            backup_dir: None,
            loudness_gate: vec!["liner".into(), "id".into(), "promo".into()],
            time_zone: "UTC".into(),
        }
    }
}

/// Where the station is in its transport (4.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Playing,
    /// `hold`: the item on air plays to its end, then silence until `resume`.
    Held,
    /// `stop`: silent until `resume` or `next`.
    Stopped,
}

impl Transport {
    pub fn name(self) -> &'static str {
        match self {
            Transport::Playing => "playing",
            Transport::Held => "held",
            Transport::Stopped => "stopped",
        }
    }
}

/// An operator action (4.7).
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Next,
    FadeNext(Option<f64>),
    Cut,
    Stop {
        fade_s: Option<f64>,
    },
    Hold,
    Resume,
    Assist(bool),
    Reshuffle,
    Insert {
        item_id: i64,
        at: At,
    },
    Remove {
        entry_id: i64,
    },
    Move {
        entry_id: i64,
        to: usize,
    },
    /// Set (`Some`: `cut`, or a JSON `PresetPatch`) or clear (`None`) the per-entry transition of one
    /// queue entry (2.6, resolution step 4).
    SetTransition {
        entry_id: i64,
        transition: Option<String>,
    },
    PlayAlbum {
        album_key: String,
        gapless: bool,
        at: At,
    },
    Switch {
        playlist_id: i64,
        how: SwitchHow,
    },
    /// `POST /end-at` (7.2): play a playlist so that a track ends at a time.
    EndAt {
        playlist_id: i64,
        at_ms: i64,
        then: crate::program::EndThen,
    },
    /// Start a playlist as the program with a T0 (4.7, 7.2 `program/start`).
    StartProgram {
        playlist_id: i64,
        t0: crate::program::T0Req,
        how: SwitchHow,
        /// Who asked, for `t0_source` (5.3): `deck`, `ui` or `api`; `auto` is the fallback start.
        source: &'static str,
    },
    FireCart {
        item_id: i64,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum OpError {
    /// A load is on its way to the mixer: try again after the next block.
    Busy,
    Refused(String),
}

impl From<String> for OpError {
    fn from(s: String) -> Self {
        OpError::Refused(s)
    }
}

impl From<&str> for OpError {
    fn from(s: &str) -> Self {
        OpError::Refused(s.into())
    }
}

/// What is on air, for the API's `now`.
#[derive(Debug, Clone, PartialEq)]
pub struct NowInfo {
    pub entry_id: i64,
    pub item_id: i64,
    pub title: String,
    pub artist: String,
    pub category: String,
    pub position_ms: i64,
}

/// What `start` did.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StartInfo {
    /// The item resumed in place and the file position it resumed from.
    pub resumed: Option<(i64, i64)>,
    /// A checkpoint was found but the next item was started instead.
    pub next_instead: bool,
    /// The station was stopped, or held with its item over, when the process went down: it comes back
    /// the same way and plays nothing until `resume` (or `next`).
    pub silent: bool,
}

/// The transition planner as the scheduler's timing model, over the station's catalog.
pub struct RuntimeTiming<'a> {
    pub cat: &'a Catalog,
    pub presets: &'a PresetBook,
    pub preset: &'a str,
    pub rate: u32,
}

impl Timing for RuntimeTiming<'_> {
    fn advance_ms(&self, a: &TrackMeta, b: &TrackMeta, continuity: bool) -> i64 {
        let (Some(ta), Some(tb)) = (self.cat.get(a.item_id), self.cat.get(b.item_id)) else {
            return a.effective_ms;
        };
        let (Ok(ia), Ok(ib)) = (ta.item(), tb.item()) else {
            return a.effective_ms;
        };
        fn chain<'a>(preset: &'a str, i: &'a Item) -> Chain<'a> {
            Chain {
                station_preset: preset,
                category_preset: i.category_preset.as_deref(),
                category_patch: Some(&i.category_patch),
                item: Some(&i.transition),
                entry: None,
            }
        }
        let (Ok(pa), Ok(pb)) = (
            resolve(self.presets, &chain(self.preset, &ia)),
            resolve(self.presets, &chain(self.preset, &ib)),
        ) else {
            return a.effective_ms;
        };
        let (bin, bout, fe) = crate::plan::bounds_of(&ia, &pa);
        let view = PlacedView {
            item: &ia,
            start_frame: 0,
            bound_in_ms: bin,
            bound_out_ms: bout,
            bound_out_is_file_end: fe,
            frames: None,
        };
        let p = plan(&PlanInput {
            rate: self.rate,
            now: 0,
            a: &view,
            b: &ib,
            pa: &pa,
            pb: &pb,
            kind: if continuity {
                TransitionKind::Continuity
            } else {
                TransitionKind::Auto
            },
            truncated_fade_s: crate::plan::TRUNCATED_FADE_S,
        });
        (p.s_b as f64 * 1000.0 / f64::from(self.rate)).round() as i64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// An item of the chain the scheduler queued.
    Chain,
    /// An item of the emergency deck or the RAM loop (1.5): outside the scheduler.
    Alt,
    /// The item that was on air when the process went down, resumed in place (1.7).
    Resumed,
    /// Fired over the programme (`fire cart`).
    Cart,
}

pub(crate) struct Tracked {
    pub(crate) entry: QEntry,
    pub(crate) kind: Kind,
    pub(crate) as_run: Option<i64>,
    /// Why the stop we sent (if any) ends this voice, for the as-run row.
    pub(crate) end_hint: &'static str,
    /// Where the resumed voice started from, ms.
    pub(crate) resumed_from_ms: Option<i64>,
}

pub struct Station {
    pub opts: StationOptions,
    pub cond: Conductor,
    pub sched: Scheduler,
    pub cat: Catalog,
    pub presets: PresetBook,
    /// The station config's default transition preset (what `opts.preset` was before a runtime override).
    pub config_preset: String,
    pub(crate) state: Option<StateWriter>,
    pub(crate) sync: Option<LibrarySync>,
    pub(crate) voices: HashMap<(u8, u32), Tracked>,
    /// The queue entry whose voice is loaded and has not started.
    pub(crate) pending: Option<i64>,
    pub(crate) on_air_voice: Option<VoiceId>,
    pub transport: Transport,
    pub assist: bool,
    /// Assist mode (or a hold): the on-air item's own end has been armed and the station waits.
    pub(crate) finished_last: bool,
    pub(crate) run_gains: HashMap<i64, Option<RunGain>>,
    pub(crate) now_frame: u64,
    pub(crate) wall_ms: i64,
    pub(crate) last_checkpoint_wall: i64,
    pub(crate) last_sync_wall: i64,
    pub(crate) dirty: bool,
    pub(crate) next_as_run: i64,
    /// A load was sent since the last block was rendered (offline callers wait one block before an
    /// undo; on the rt thread loads are serviced before commands and this is never needed).
    pub load_in_flight: bool,
    /// The recent as-run rows (in memory, at most [`AS_RUN_KEEP`]; `state.db` holds all of them, and the
    /// as-run API reads it from there). A 24/7 station used to keep every row it had ever written.
    pub as_run: Vec<AsRunRow>,
    /// Rows written since the process started, trimmed or not.
    pub as_run_total: u64,
    /// The library could not be read at the last poll.
    pub library_unavailable: bool,
    pub start_info: StartInfo,
    pub(crate) default_playlist_id: Option<i64>,
    pub(crate) loaded_checkpoint: Option<Checkpoint>,
    /// The loudness controller's gain the checkpoint carried (6.1), read before the start consumes the
    /// checkpoint: where the controller resumes, with no re-convergence (1.7).
    pub loudness_restored_db: Option<f64>,
    /// The single-writer lock on `state.db`, held for the station's life.
    pub(crate) _lock: Option<std::fs::File>,
    /// Failure handling (1.5): alerts, pre-flight, the emergency deck, the RAM loop, silence.
    pub fail: crate::failure::FailState,
    /// A timed stop whose fade is set up on the mixer: every voice placed until it is reached gets the
    /// same stop, and at its frame the station is `Stopped`.
    pub(crate) armed_stop: Option<ArmedStop>,
    /// How far past `wall_ms` (milliseconds, below 1) the wall time of `now_frame` really is: the offline
    /// renderer, whose clock is exact, sets it so that a timed block lands on its frame; a real station
    /// leaves it 0 (its wall clock is read in milliseconds).
    pub wall_frac_ms: f64,
    /// The blocks already past when the last program was started, and what their late policy did.
    pub last_start: Option<serde_json::Value>,
    /// Where side effects (OSC, MIDI, web requests) go: a worker thread in production, a recorder in tests.
    pub side: Box<dyn crate::side::SideSink>,
    pub(crate) side_seq: u64,
    /// The master gain (2.4): what `set_gain`, `set_mute` and `fade_stop` have set.
    pub(crate) master: crate::acts::MasterState,
    /// A program start took the station (4.7 `program/start`): its first item starts on its own at the first
    /// frame after whatever was still sounding has faded out, never as a segue from it.
    pub(crate) program_start_at: Option<u64>,
}

/// See [`Station::armed_stop`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct ArmedStop {
    pub at: u64,
    pub fade_s: f64,
}

/// Most rows [`Station::as_run`] holds before it drops the oldest finished ones (down to half).
pub const AS_RUN_KEEP: usize = 1024;

/// The zone named in the options; UTC (with a warning) when the tz database does not know it.
pub(crate) fn zone_of(name: &str) -> common::timespec::Zone {
    common::timespec::Zone::named(name).unwrap_or_else(|e| {
        tracing::warn!("{e}; timed blocks use UTC");
        common::timespec::Zone::utc()
    })
}

pub(crate) fn parse_patch(s: &Option<String>) -> Option<PresetPatch> {
    let t = s.as_deref()?;
    if t == "cut" {
        return None;
    }
    serde_json::from_str(t).ok()
}

impl Station {
    /// A per-entry transition text must be `cut` or a JSON `PresetPatch` that names a preset the station
    /// has (the patch, if any, an object).
    pub(crate) fn check_entry_transition(&self, text: &str) -> Result<(), String> {
        if text == "cut" {
            return Ok(());
        }
        let pp: PresetPatch = serde_json::from_str(text)
            .map_err(|e| format!("the transition must be `cut` or a preset and/or a patch: {e}"))?;
        if let Some(name) = &pp.preset {
            self.presets
                .get(name)
                .map_err(|_| format!("no transition preset `{name}`"))?;
        }
        if pp.patch.as_ref().is_some_and(|p| !p.is_object()) {
            return Err("the patch must be a JSON object".into());
        }
        Ok(())
    }
}

pub(crate) fn lead_frames(rate: u32) -> u64 {
    s_frames(MIN_LEAD_S + 0.05, rate).max(1) as u64
}

impl Station {
    /// Set the output trim, dB (2.4): the mixer ramps to it without a click, and the value is kept in
    /// `state.db` so a restart comes back to it. Returns the value in force (clamped to the range).
    pub fn set_output_trim(&mut self, db: f64) -> f64 {
        let db = db.clamp(
            common::config::OUTPUT_TRIM_MIN_DB,
            common::config::OUTPUT_TRIM_MAX_DB,
        );
        self.opts.output_trim_db = db;
        self.cond.set_output_trim_db(db as f32);
        if let Some(w) = &self.state {
            w.send(Msg::Meta {
                key: "output_trim_db".into(),
                value: db.to_string(),
            });
        }
        db
    }

    /// Append an as-run row and keep the in-memory list bounded: past [`AS_RUN_KEEP`] rows the oldest
    /// *finished* rows go, down to half (a row still on air stays). What readers need of it is only the
    /// tail: the API's events read the last 64, `on_air_info` and the end of a voice look up a row by id
    /// (a row that has been dropped is looked up in vain, harmlessly), and the durable copy is in
    /// `state.db`.
    pub(crate) fn push_as_run(&mut self, row: AsRunRow) {
        self.as_run.push(row);
        self.as_run_total += 1;
        if self.as_run.len() > AS_RUN_KEEP {
            let excess = self.as_run.len() - AS_RUN_KEEP / 2;
            let mut dropped = 0;
            self.as_run.retain(|r| {
                if dropped < excess && r.ended_ms.is_some() {
                    dropped += 1;
                    false
                } else {
                    true
                }
            });
        }
    }

    /// Build a station from what a start reads: `state.db` (if given), and the library.
    pub fn open(
        opts: StationOptions,
        state_path: Option<PathBuf>,
        library_db: Option<PathBuf>,
        wall_ms: i64,
    ) -> Result<(Station, Mixer), String> {
        let lock = match &state_path {
            Some(p) => Some(state::lock_station(p)?),
            None => None,
        };
        let loaded: Loaded = match &state_path {
            Some(p) => state::load(p)?,
            None => Loaded::default(),
        };
        let writer = match &state_path {
            Some(p) => Some(StateWriter::start(p)?),
            None => None,
        };
        if let (Some(w), Some(dir)) = (&writer, &opts.backup_dir) {
            // What the last night's snapshot said, for `/health` until tonight's.
            if let Ok(mut g) = w.status.snapshot.lock() {
                *g = library::backup::read_status(dir, &format!("station-{}", opts.name));
            }
        }
        let mut cat = Catalog::new(&opts.media_dir);
        cat.restore_forced(
            &loaded
                .quarantine
                .iter()
                .map(|q| (q.item_id, q.sha256.clone(), q.reason.clone(), q.pushed))
                .collect::<Vec<_>>(),
        );
        cat.apply(loaded.tracks.clone());
        let mut prog = loaded.programming.clone().unwrap_or_default();
        let mut sync = library_db.clone().map(|p| LibrarySync::new(p, &opts.name));
        let mut unavailable = false;
        if let Some(s) = sync.as_mut() {
            let out = s.poll(true, &mut cat, &prog);
            unavailable = out.unavailable;
            if let Some(p) = out.programming {
                if let Some(w) = &writer {
                    w.send(Msg::Programming {
                        key: "programming".into(),
                        revision: 0,
                        body: serde_json::to_string(&*p).unwrap_or_default(),
                    });
                }
                prog = (*p).clone();
            }
            if let Some(w) = &writer {
                if !out.changed_rows.is_empty() {
                    w.send(Msg::Tracks {
                        upsert: out.changed_rows,
                        delete: vec![],
                    });
                }
            }
        }
        let default_playlist_id = opts
            .default_playlist
            .as_deref()
            .and_then(|n| prog.by_name(n))
            .map(|p| p.id);
        let sched_cfg = SchedConfig {
            lock_ahead_ms: i64::from(opts.lock_ahead_s) * 1000,
            default_playlist: default_playlist_id,
            zone: zone_of(&opts.time_zone),
            ..SchedConfig::default()
        };
        let prog = Arc::new(prog);
        let sched = match loaded.sched {
            Some(st) => Scheduler::restore(sched_cfg, prog.clone(), st),
            None => Scheduler::new(sched_cfg, prog.clone(), opts.seed),
        };
        // A default preset set at runtime (kept in `meta`) wins over the station config.
        let config_preset = opts.preset.clone();
        let mut opts = opts;
        let base_ok = loaded
            .station_preset_base
            .as_deref()
            .is_none_or(|b| b == config_preset);
        if let Some(p) = loaded.station_preset.as_deref().filter(|_| base_ok) {
            if PresetBook::builtin().get(p).is_ok() {
                opts.preset = p.to_string();
            } else {
                tracing::warn!("state.db names the transition preset `{p}`, which does not exist: using `{config_preset}`");
            }
        }
        let mut cfg = ConductorConfig::new(opts.rate, &opts.preset);
        cfg.target_lufs = opts.target_lufs;
        cfg.max_boost_db = opts.max_boost_db;
        cfg.peak_allowance_db = opts.peak_allowance_db;
        cfg.truncated_fade_s = opts.truncated_fade_seconds();
        cfg.decoder_threads = opts.decoder_threads;
        cfg.loudness_gate = opts.loudness_gate.clone();
        let presets = cfg.presets.clone();
        let (cond, mixer) = Conductor::new(cfg);
        cond.set_silence_threshold(opts.silence_dbfs as f32);
        // The trim the operator saved wins over the toml's (2.4).
        if let Some(db) = loaded.output_trim_db {
            opts.output_trim_db = db.clamp(
                common::config::OUTPUT_TRIM_MIN_DB,
                common::config::OUTPUT_TRIM_MAX_DB,
            );
        }
        cond.set_output_trim_db(opts.output_trim_db as f32);
        opts.lookahead_frames = mixer.limiter_latency_frames() as u64;
        let mut fail = crate::failure::FailState::new(
            loaded.next_alert_id.max(1),
            crate::preflight::MediaFaults::new(),
        );
        fail.state_dir = state_path
            .as_ref()
            .and_then(|p| p.parent().map(std::path::Path::to_path_buf));
        for (item_id, kind, _sha, count) in &loaded.item_errors {
            fail.errors.insert((*item_id, kind.clone()), *count);
        }
        fail.restart_gap_from = loaded.checkpoint.as_ref().map(|c| c.at_wall_ms);
        fail.safe_mode = loaded
            .checkpoint
            .as_ref()
            .is_some_and(|c| c.state == "safe");
        let repaired = loaded.repaired_from.clone();
        let assist = loaded
            .checkpoint
            .as_ref()
            .is_some_and(|c| c.mode == "assist");
        Ok((
            Station {
                opts,
                cond,
                sched,
                cat,
                presets,
                config_preset,
                state: writer,
                sync,
                voices: HashMap::new(),
                pending: None,
                on_air_voice: None,
                transport: Transport::Playing,
                assist,
                finished_last: false,
                run_gains: HashMap::new(),
                now_frame: 0,
                wall_ms,
                last_checkpoint_wall: i64::MIN,
                last_sync_wall: wall_ms,
                dirty: true,
                next_as_run: loaded.next_as_run_id.max(1),
                load_in_flight: false,
                as_run: Vec::new(),
                as_run_total: 0,
                library_unavailable: unavailable,
                start_info: StartInfo::default(),
                default_playlist_id,
                loudness_restored_db: loaded.checkpoint.as_ref().and_then(|c| c.loudness_gain_db),
                loaded_checkpoint: loaded.checkpoint,
                _lock: lock,
                fail,
                armed_stop: None,
                program_start_at: None,
                wall_frac_ms: 0.0,
                last_start: None,
                side: crate::acts::default_side_sink(),
                side_seq: 0,
                master: Default::default(),
            },
            mixer,
        ))
        .map(|(mut st, mixer)| {
            if let Some(p) = repaired {
                st.fail.alerts.raise(
                    crate::alerts::code::STATE_DB_CORRUPT,
                    "",
                    crate::alerts::Severity::Critical,
                    format!(
                        "state.db was corrupt; moved to {} and started fresh",
                        p.display()
                    ),
                    None,
                    wall_ms,
                );
            }
            for (id, why) in st.cat.take_ended() {
                tracing::info!(item_id = id, "quarantine ended: {why}");
                st.forget_item_failures(id);
            }
            for id in st.cat.take_seen() {
                if let Some(w) = &st.state {
                    w.send(Msg::QuarantineSeen { item_id: id });
                }
            }
            (st, mixer)
        })
    }

    /// A station over an in-memory catalog and programming, with no databases (tests).
    pub fn in_memory(
        opts: StationOptions,
        cat: Catalog,
        prog: Programming,
        wall_ms: i64,
    ) -> (Station, Mixer) {
        let default_playlist_id = opts
            .default_playlist
            .as_deref()
            .and_then(|n| prog.by_name(n))
            .map(|p| p.id);
        let sched_cfg = SchedConfig {
            lock_ahead_ms: i64::from(opts.lock_ahead_s) * 1000,
            default_playlist: default_playlist_id,
            zone: zone_of(&opts.time_zone),
            ..SchedConfig::default()
        };
        let sched = Scheduler::new(sched_cfg, Arc::new(prog), opts.seed);
        let mut cfg = ConductorConfig::new(opts.rate, &opts.preset);
        cfg.target_lufs = opts.target_lufs;
        cfg.max_boost_db = opts.max_boost_db;
        cfg.peak_allowance_db = opts.peak_allowance_db;
        cfg.truncated_fade_s = opts.truncated_fade_seconds();
        cfg.decoder_threads = opts.decoder_threads;
        cfg.loudness_gate = opts.loudness_gate.clone();
        let presets = cfg.presets.clone();
        let (cond, mixer) = Conductor::new(cfg);
        cond.set_silence_threshold(opts.silence_dbfs as f32);
        let mut opts = opts;
        cond.set_output_trim_db(opts.output_trim_db as f32);
        opts.lookahead_frames = mixer.limiter_latency_frames() as u64;
        let config_preset = opts.preset.clone();
        (
            Station {
                opts,
                cond,
                sched,
                cat,
                config_preset,
                presets,
                state: None,
                sync: None,
                voices: HashMap::new(),
                pending: None,
                on_air_voice: None,
                transport: Transport::Playing,
                assist: false,
                finished_last: false,
                run_gains: HashMap::new(),
                now_frame: 0,
                wall_ms,
                last_checkpoint_wall: i64::MIN,
                last_sync_wall: wall_ms,
                dirty: true,
                next_as_run: 1,
                load_in_flight: false,
                as_run: Vec::new(),
                as_run_total: 0,
                library_unavailable: false,
                start_info: StartInfo::default(),
                default_playlist_id,
                loaded_checkpoint: None,
                loudness_restored_db: None,
                _lock: None,
                fail: crate::failure::FailState::new(1, crate::preflight::MediaFaults::new()),
                armed_stop: None,
                program_start_at: None,
                wall_frac_ms: 0.0,
                last_start: None,
                side: crate::acts::default_side_sink(),
                side_seq: 0,
                master: Default::default(),
            },
            mixer,
        )
    }

    /// Change the station's default transition preset (2.6, resolution step 1) at runtime and keep it in
    /// `state.db` `meta`, where it wins over the config at the next start. Boundaries already committed
    /// keep the plan they were placed with; the next boundary that has not been committed is planned
    /// with the new preset (the planner reads it afresh on every tick until the commit).
    pub fn set_station_preset(&mut self, name: &str) -> Result<(), String> {
        self.presets.get(name)?;
        self.opts.preset = name.to_string();
        self.cond.cfg.station_preset = name.to_string();
        if let Some(w) = &self.state {
            w.send(Msg::Meta {
                key: crate::state::META_TRANSITION_PRESET.into(),
                value: name.into(),
            });
            w.send(Msg::Meta {
                key: crate::state::META_TRANSITION_PRESET_BASE.into(),
                value: self.config_preset.clone(),
            });
        }
        self.dirty = true;
        Ok(())
    }

    /// The `state.db` writer (tests flush it before a simulated restart).
    pub fn state_writer(&self) -> Option<&StateWriter> {
        self.state.as_ref()
    }

    /// Apply settings that take effect without a restart (the Settings page): `transition_preset` (from the
    /// next boundary that is not committed), `truncated_fade` and `truncated_fade_s` (likewise), `target_lufs`,
    /// `max_boost_db` and `peak_allowance_db` (from the
    /// next item prepared; the item on air and the one already committed keep their gain), `silence_dbfs`,
    /// `silence_s`, `lock_ahead_s` and `output_trim_db` (at once, the trim with its short ramp and kept in `meta`). Every value is checked first and nothing changes if one is
    /// wrong. Returns the keys applied.
    pub fn apply_live(
        &mut self,
        vals: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<String>, String> {
        fn num(k: &str, v: &serde_json::Value, lo: f64, hi: f64) -> Result<f64, String> {
            match v.as_f64() {
                Some(x) if (lo..=hi).contains(&x) => Ok(x),
                _ => Err(format!("`{k}` must be a number from {lo} to {hi}")),
            }
        }
        for (k, v) in vals {
            match k.as_str() {
                "transition_preset" => {
                    let n = v.as_str().ok_or("`transition_preset` must be a name")?;
                    self.presets.get(n)?;
                }
                "target_lufs" => {
                    num(k, v, -40.0, -5.0)?;
                }
                "max_boost_db" => {
                    num(k, v, 0.0, 30.0)?;
                }
                "peak_allowance_db" => {
                    num(k, v, 0.0, 12.0)?;
                }
                "truncated_fade" => {
                    v.as_bool()
                        .ok_or("`truncated_fade` must be true or false")?;
                }
                "truncated_fade_s" => {
                    num(k, v, 0.5, 10.0)?;
                }
                "silence_dbfs" => {
                    num(k, v, -100.0, -20.0)?;
                }
                "silence_s" => {
                    num(k, v, 1.0, 600.0)?;
                }
                "restart_alert_clear_s" => {
                    num(k, v, 0.0, 86_400.0)?;
                }
                "lock_ahead_s" => {
                    num(k, v, 5.0, 120.0)?;
                }
                "output_trim_db" => {
                    num(
                        k,
                        v,
                        common::config::OUTPUT_TRIM_MIN_DB,
                        common::config::OUTPUT_TRIM_MAX_DB,
                    )?;
                }
                other => {
                    return Err(format!(
                        "`{other}` cannot be changed while the station runs"
                    ))
                }
            }
        }
        let mut done = vec![];
        for (k, v) in vals {
            match k.as_str() {
                "transition_preset" => {
                    self.set_station_preset(v.as_str().unwrap_or_default())?;
                }
                "target_lufs" => {
                    let x = v.as_f64().unwrap_or(-18.0);
                    self.opts.target_lufs = x;
                    self.cond.cfg.target_lufs = x;
                }
                "max_boost_db" => {
                    let x = v.as_f64().unwrap_or(12.0);
                    self.opts.max_boost_db = x;
                    self.cond.cfg.max_boost_db = x;
                }
                "peak_allowance_db" => {
                    let x = v.as_f64().unwrap_or(6.0);
                    self.opts.peak_allowance_db = x;
                    self.cond.cfg.peak_allowance_db = x;
                }
                "truncated_fade" => {
                    self.opts.truncated_fade = v.as_bool().unwrap_or(true);
                    self.cond.cfg.truncated_fade_s = self.opts.truncated_fade_seconds();
                }
                "truncated_fade_s" => {
                    self.opts.truncated_fade_s =
                        v.as_f64().unwrap_or(crate::plan::TRUNCATED_FADE_S);
                    self.cond.cfg.truncated_fade_s = self.opts.truncated_fade_seconds();
                }
                "silence_dbfs" => {
                    let x = v.as_f64().unwrap_or(-50.0);
                    self.opts.silence_dbfs = x;
                    self.cond.set_silence_threshold(x as f32);
                }
                "silence_s" => self.opts.silence_s = v.as_f64().unwrap_or(15.0),
                "restart_alert_clear_s" => {
                    self.opts.restart_alert_clear_s = v.as_f64().unwrap_or(600.0)
                }
                "output_trim_db" => {
                    self.set_output_trim(v.as_f64().unwrap_or(0.0));
                }
                "lock_ahead_s" => {
                    let x = v.as_f64().unwrap_or(20.0) as u32;
                    self.opts.lock_ahead_s = x;
                    self.sched.cfg.lock_ahead_ms = i64::from(x) * 1000;
                }
                _ => {}
            }
            done.push(k.clone());
        }
        self.dirty = true;
        Ok(done)
    }

    // ---- small helpers ---------------------------------------------------------------------------

    pub(crate) fn rate(&self) -> u32 {
        self.opts.rate
    }

    fn wall_of(&self, frame: u64) -> i64 {
        self.wall_ms + ((frame as i64 - self.now_frame as i64) * 1000) / i64::from(self.rate())
    }

    pub(crate) fn item_of(&self, item_id: i64) -> Result<Arc<Item>, String> {
        self.cat
            .get(item_id)
            .ok_or_else(|| format!("item {item_id} is not in the station's catalog"))?
            .item()
    }

    /// One gain for the members of a gapless run (3.7).
    pub(crate) fn run_gain_for(&mut self, entry: &QEntry) -> Option<RunGain> {
        let rid = entry.run_id?;
        if let Some(g) = self.run_gains.get(&rid) {
            return *g;
        }
        let g = self.sched.st.runs.get(&rid).and_then(|r| {
            if r.continuity != "gapless" {
                return None;
            }
            let items: Vec<Arc<Item>> = r
                .members
                .iter()
                .filter_map(|m| self.item_of(*m).ok())
                .collect();
            if items.is_empty() {
                return None;
            }
            let members: Vec<(&Item, f64)> = items
                .iter()
                .map(|i| {
                    (
                        i.as_ref(),
                        ((i.markers.cue_out - i.markers.cue_in).max(1)) as f64 / 1000.0,
                    )
                })
                .collect();
            Some(run_gain(&members))
        });
        self.run_gains.insert(rid, g);
        g
    }

    fn flush_sched(&mut self) {
        if self.dirty {
            if let Some(w) = &self.state {
                w.send(Msg::Sched(Box::new(self.sched.snapshot())));
            }
            self.dirty = false;
        }
    }

    /// Wait until `state.db` holds everything sent so far.
    pub fn flush(&mut self) {
        self.flush_alerts();
        self.flush_sched();
        if let Some(w) = &self.state {
            w.flush();
        }
    }

    pub fn now(&self) -> Option<NowInfo> {
        let e = self.now_entry()?;
        let m = self.cat.meta(e.item_id)?;
        Some(NowInfo {
            entry_id: e.id,
            item_id: e.item_id,
            title: m.title.clone(),
            artist: m.artist.clone(),
            category: m.category.clone(),
            position_ms: self.position_ms().unwrap_or(0),
        })
    }

    /// The file position of the item on air, ms, at the last step.
    pub fn position_ms(&self) -> Option<i64> {
        let id = self.on_air_voice?;
        let p = self.cond.placed.iter().find(|p| p.id == id)?;
        if self.now_frame < p.start_frame {
            return None;
        }
        Some(
            p.bound_in_ms
                + ((self.now_frame - p.start_frame) as f64 * 1000.0 / f64::from(self.rate()))
                    as i64,
        )
    }

    // ---- start and resume (1.7) ---------------------------------------------------------------------

    /// Begin playing: resume what was on air if the checkpoint says so, else start the next item.
    pub fn start(
        &mut self,
        now_frame: u64,
        wall_ms: i64,
        send: &mut dyn FnMut(Command),
    ) -> Result<StartInfo, String> {
        self.now_frame = now_frame;
        self.wall_ms = wall_ms;
        self.fail.armed_frame = now_frame;
        if self.fail.safe_mode {
            self.enter_safe_mode();
        }
        self.start_ram_loader();
        let _ = self.cat.take_playable_changed();
        if self.sched.st.program.is_none() {
            let pid = self
                .default_playlist_id
                .or_else(|| {
                    self.sched
                        .prog
                        .playlists
                        .values()
                        .find(|p| p.enabled)
                        .map(|p| p.id)
                })
                .ok_or("there is no playlist to run")?;
            self.sched.start_program_with(pid, wall_ms, "start", None);
        }
        // Nothing is committed to a mixer that was just built.
        for e in &mut self.sched.st.queue {
            e.committed = false;
        }
        let ckpt = self.loaded_checkpoint.take();
        if let Some(w) = &self.state {
            w.send(Msg::CloseOpenAsRun {
                ended_ms: ckpt.as_ref().map_or(wall_ms, |c| c.at_wall_ms),
            });
        }
        let mut info = StartInfo::default();
        let lead = lead_frames(self.rate());
        let lead_ms = (lead as i64 * 1000) / i64::from(self.rate());
        let restored = self.sched.st.on_air.clone();
        // The transport comes back as it was (1.7: the checkpoint carries it): a station stopped by
        // its operator stays stopped after a crash or a reboot, and a hold still ends the item on air.
        let saved = ckpt.as_ref().map(|c| c.state.clone());
        let stopped = saved.as_deref() == Some("stopped");
        let held = saved.as_deref() == Some("held");
        if stopped {
            self.transport = Transport::Stopped;
            self.finished_last = true;
        }
        if let (Some(c), Some(entry)) = (&ckpt, &restored) {
            let fresh = wall_ms - c.at_wall_ms < RESUME_MAX_AGE_MS;
            let same = c.item_id == Some(entry.item_id);
            if fresh && same && !self.fail.safe_mode {
                if let (Ok(item), Some(pos)) = (self.item_of(entry.item_id), c.position_ms) {
                    let position = pos + (wall_ms + lead_ms - c.at_wall_ms);
                    let remaining = item.markers.segue - position;
                    if remaining >= RESUME_MIN_REMAINING_MS && (c.state == "playing" || held) {
                        let entry = entry.clone();
                        let run = self.run_gain_for(&entry);
                        self.cond.place_resumed(
                            item.clone(),
                            now_frame + lead,
                            position,
                            RESUME_FADE_MS,
                            run,
                            send,
                        )?;
                        let id = self.cond.placed.last().map(|p| p.id).ok_or("no voice")?;
                        // The scheduler's clock for this item: where its bound in would have started.
                        let params = self.cond.resolve_chain(&item, None)?;
                        let (bin, _, _) = crate::plan::bounds_of(&item, &params);
                        self.sched.st.on_air_start_ms = Some(wall_ms + lead_ms - (position - bin));
                        self.voices.insert(
                            (id.slot, id.gen),
                            Tracked {
                                entry,
                                kind: Kind::Resumed,
                                as_run: None,
                                end_hint: "end",
                                resumed_from_ms: Some(position),
                            },
                        );
                        self.load_in_flight = true;
                        info.resumed = Some((c.item_id.unwrap_or(0), position));
                        if held {
                            // Still held: the resumed item plays to its end and nothing follows.
                            self.cond.finish_last(send);
                            self.finished_last = true;
                            self.transport = Transport::Held;
                        }
                    }
                }
            }
            if info.resumed.is_none() {
                info.next_instead = true;
            }
        }
        if info.resumed.is_none() {
            // The interrupted item is over: what plays now is the next entry of the saved queue.
            if restored.is_some() {
                self.sched.on_air_ended(&self.cat);
            }
            let t = RuntimeTiming {
                cat: &self.cat,
                presets: &self.presets,
                preset: &self.opts.preset,
                rate: self.opts.rate,
            };
            self.sched.fill(wall_ms, &self.cat, &t);
            if stopped || held {
                self.transport = if held {
                    Transport::Held
                } else {
                    Transport::Stopped
                };
                self.finished_last = true;
                info.silent = true;
            } else if let Some(h) = self.peek_next() {
                // Nothing playable at start (a media disk that did not mount) is not a reason to exit: the
                // station comes up, the silence detector reports the dead air, and each step tries again.
                self.try_start_head(h, send);
            }
        }
        self.dirty = true;
        self.flush_sched();
        self.flush_alerts();
        self.start_info = info.clone();
        Ok(info)
    }

    /// [`Station::start_head`] where nothing is waiting for an answer (the station's own step): a failure is
    /// logged (at most every 10 s) and tried again at the next step; the silence detector reports the dead
    /// air if it lasts.
    pub(crate) fn try_start_head(&mut self, head: QEntry, send: &mut dyn FnMut(Command)) {
        if let Err(e) = self.start_head(head, send) {
            let now = self.wall_ms;
            if now - self.fail.start_fail_logged_ms >= 10_000 {
                tracing::warn!("nothing could be started: {e}");
                self.fail.start_fail_logged_ms = now;
            }
        }
    }

    /// Start `head` now (after silence: the very first item, after a stop, after an item ended with
    /// nothing committed). The item's file is checked here, on this thread (1.5): the first sound must
    /// not wait for a worker, and a bad file is dropped and the next entry tried.
    pub(crate) fn start_head(
        &mut self,
        head: QEntry,
        send: &mut dyn FnMut(Command),
    ) -> Result<(), String> {
        let mut head = head;
        let mut tries = 0;
        let item = loop {
            // A bad file, or an entry whose item cannot be built (gone from the catalog, a RAM item that is
            // not there), is dropped and the next one tried: an entry that can never start must not hold
            // the station silent step after step.
            if head.id < -1 || self.check_now(head.item_id) {
                match self.entry_item(&head) {
                    Ok(i) => break i,
                    Err(e) => tracing::warn!("dropping queue entry {}: {e}", head.id),
                }
            }
            if head.id >= -1 {
                self.sched.st.queue.retain(|e| e.id != head.id);
            } else {
                self.fail.alt_entry = None;
            }
            self.dirty = true;
            tries += 1;
            head = match self.peek_next() {
                Some(h) if tries < 8 => h,
                _ => return Err("no item in the queue can be played".into()),
            };
        };
        let mut patch = parse_patch(&head.transition);
        let item = self.prepare_item(&mut head, item, &mut patch);
        let run = self.run_gain_for(&head);
        let mut start = self.now_frame + lead_frames(self.rate());
        if let Some(nb) = self.program_start_at.take() {
            start = start.max(nb);
        }
        if let Some(at) = head.timed.as_ref().and_then(|t| t.hard_at_ms) {
            if at > self.wall_ms {
                start = start.max(self.frame_of_wall(at));
            }
        }
        self.master_restore(start, send);
        self.cond.place_first(item, start, run)?;
        self.reapply_armed_stop(send);
        let id = self.cond.placed.last().map(|p| p.id).ok_or("no voice")?;
        let alt = head.id < -1;
        self.voices.insert(
            (id.slot, id.gen),
            Tracked {
                entry: head.clone(),
                kind: if alt { Kind::Alt } else { Kind::Chain },
                as_run: None,
                end_hint: "end",
                resumed_from_ms: None,
            },
        );
        self.pending = Some(head.id);
        if !alt {
            if let Some(h) = self.sched.st.queue.first_mut() {
                if h.id == head.id {
                    h.committed = true;
                }
            }
        }
        self.transport = Transport::Playing;
        self.finished_last = false;
        self.load_in_flight = true;
        self.dirty = true;
        Ok(())
    }

    // ---- the step ----------------------------------------------------------------------------------------

    /// Advance the station to `now_frame` (the mixer's position), `wall_ms` being the wall time at which
    /// that frame is audible. Call every 20 ms or so; commands go to `send`.
    pub fn step(
        &mut self,
        now_frame: u64,
        wall_ms: i64,
        send: &mut dyn FnMut(Command),
    ) -> Result<(), String> {
        self.now_frame = now_frame;
        self.wall_ms = wall_ms;
        self.cond.tick();
        // The loudness windows leave out whatever plays while the transport is not `playing` (6.1).
        self.cond.loudness_link().playing.store(
            self.transport == Transport::Playing,
            std::sync::atomic::Ordering::Relaxed,
        );
        self.handle_events();
        self.side_poll();
        self.maybe_sync(send);
        {
            let t = RuntimeTiming {
                cat: &self.cat,
                presets: &self.presets,
                preset: &self.opts.preset,
                rate: self.opts.rate,
            };
            let head_before = self.sched.head().map(|h| (h.id, h.committed));
            let added = self.sched.fill(wall_ms, &self.cat, &t);
            if added > 0 || head_before != self.sched.head().map(|h| (h.id, h.committed)) {
                self.dirty = true;
            }
        }
        self.failure_step(send);
        self.sched.stopped = self.transport != Transport::Playing;
        self.fallback_step(send);
        self.timed_step(send);
        self.boundary(send)?;
        if wall_ms.saturating_sub(self.last_checkpoint_wall) >= self.opts.checkpoint_every_ms {
            self.checkpoint();
        }
        self.flush_sched();
        if !self.opts.record_sidecar {
            self.cond.compact(8);
        }
        Ok(())
    }

    fn checkpoint(&mut self) {
        self.last_checkpoint_wall = self.wall_ms;
        let on_air = self.now_entry().cloned();
        let c = Checkpoint {
            at_wall_ms: self.wall_ms,
            item_id: on_air.as_ref().map(|e| e.item_id),
            queue_entry_id: on_air.as_ref().map(|e| e.id),
            position_ms: on_air.as_ref().and(self.position_ms()),
            state: if self.fail.safe_mode {
                "safe".into()
            } else {
                self.transport.name().into()
            },
            mode: if self.assist { "assist" } else { "auto" }.into(),
            active_playlist_id: self.sched.st.program.as_ref().map(|p| p.playlist_id),
            loudness_gain_db: {
                let link = self.cond.loudness_link();
                link.enabled
                    .load(std::sync::atomic::Ordering::Relaxed)
                    .then(|| link.gain_db())
            },
        };
        if let Some(w) = &self.state {
            w.send(Msg::Checkpoint(c));
        }
    }

    fn handle_events(&mut self) {
        let events = std::mem::take(&mut self.cond.events);
        for ev in events {
            match ev {
                VoiceEvent::Started { id, frame } => {
                    let Some(tr) = self.voices.get_mut(&(id.slot, id.gen)) else {
                        continue;
                    };
                    let started_ms = self.wall_ms
                        + ((frame as i64 - self.now_frame as i64) * 1000)
                            / i64::from(self.opts.rate);
                    let entry = tr.entry.clone();
                    let kind = tr.kind;
                    let from = tr.resumed_from_ms;
                    let mut flags = entry.flags.clone();
                    match kind {
                        Kind::Chain => {
                            if self
                                .sched
                                .on_started(entry.id, started_ms, &self.cat)
                                .is_none()
                            {
                                // The entry left the queue after the boundary was committed: adopt it
                                // as what is on air, so the scheduler and the audio agree (and the
                                // item it follows goes into the history, the counters move).
                                self.sched.adopt(entry.clone(), started_ms, &self.cat);
                            }
                            if self.pending == Some(entry.id) {
                                self.pending = None;
                            }
                            self.fail.on_air_is_alt = false;
                            self.fail.alt_on_air = None;
                        }
                        Kind::Alt => {
                            // Outside the scheduler: the emergency deck and the RAM loop.
                            if self.pending == Some(entry.id) {
                                self.pending = None;
                            }
                            if self
                                .fail
                                .alt_entry
                                .as_ref()
                                .is_some_and(|(_, e)| e.id == entry.id)
                            {
                                self.fail.alt_entry = None;
                            }
                            self.fail.alt_on_air = Some(entry.clone());
                            self.fail.on_air_is_alt = true;
                        }
                        Kind::Resumed => {
                            flags.push("resumed".into());
                            if let Some(f) = from {
                                flags.push(format!("resumed_from_ms={f}"));
                            }
                        }
                        Kind::Cart => flags.push("cart".into()),
                    }
                    if kind != Kind::Cart {
                        self.on_air_voice = Some(id);
                    }
                    // Where in its file this play began, so a recording of the output can be lined up with
                    // the source afterwards (a quiet passage of the music is not a dropout).
                    // And the gain it plays at: the level under which the output is silent depends on it (a
                    // passage at -45 dBFS in a file boosted by 12 dB is heard at -33).
                    if let Some(p) = self.cond.placed.iter().find(|p| p.id == id) {
                        flags.push(format!("bound_in_ms={}", p.bound_in_ms));
                        if let Some(r) = self.cond.sidecar.voices.get(p.record) {
                            flags.push(format!("gain_db={:.1}", r.gain_db));
                        }
                    }
                    let meta = self.cat.meta(entry.item_id).cloned();
                    let row = AsRunRow {
                        id: self.next_as_run,
                        item_id: Some(entry.item_id),
                        title: meta.as_ref().map(|m| m.title.clone()).unwrap_or_default(),
                        artist: meta.as_ref().map(|m| m.artist.clone()).unwrap_or_default(),
                        category: meta
                            .as_ref()
                            .map(|m| m.category.clone())
                            .unwrap_or_default(),
                        origin: if kind == Kind::Cart {
                            "cart".into()
                        } else {
                            entry.origin.as_str().into()
                        },
                        block_id: entry.block_id,
                        run_id: entry.run_id,
                        playlist_id: entry.playlist_id,
                        started_ms,
                        planned_ms: entry.planned_start_ms,
                        flags,
                        ended_ms: None,
                        played_ms: None,
                        end_reason: None,
                    };
                    self.next_as_run += 1;
                    if let Some(tr) = self.voices.get_mut(&(id.slot, id.gen)) {
                        tr.as_run = Some(row.id);
                    }
                    self.fail
                        .jump_at_start
                        .insert(row.id, self.fail.jump_total_ms);
                    if let Some(w) = &self.state {
                        w.send(Msg::AsRunStart(row.clone()));
                    }
                    self.push_as_run(row);
                    self.dirty = true;
                    if kind != Kind::Cart {
                        // After the item's own row, so the rows of an item keep their places (1.7 step 4).
                        self.restart_row(started_ms);
                        self.checkpoint();
                    }
                }
                VoiceEvent::Starved { id, frame } => self.on_starved(id, frame),
                VoiceEvent::Ended {
                    id,
                    frame,
                    played,
                    reason,
                } => {
                    let Some(tr) = self.voices.remove(&(id.slot, id.gen)) else {
                        continue;
                    };
                    if reason == EndReason::Cancelled {
                        if self.pending == Some(tr.entry.id) {
                            self.pending = None;
                        }
                        continue;
                    }
                    if reason == EndReason::Failed && tr.as_run.is_none() {
                        // Never sounded (its file would not open or decode): drop the entry so the
                        // queue moves on, take the boundary back so nothing goes quiet, and quarantine
                        // the item (1.5).
                        self.sched.st.queue.retain(|e| e.id != tr.entry.id);
                        if self
                            .fail
                            .alt_entry
                            .as_ref()
                            .is_some_and(|(_, e)| e.id == tr.entry.id)
                        {
                            self.fail.alt_entry = None;
                        }
                        if self.pending == Some(tr.entry.id) {
                            self.pending = None;
                        }
                        self.fail.load_failed.push((id, tr.entry.item_id));
                        self.dirty = true;
                        continue;
                    }
                    if reason == EndReason::Failed && played == 0 {
                        // It started and produced nothing (the decoder had failed): the item is bad.
                        self.fail.load_failed.push((id, tr.entry.item_id));
                    }
                    let ended_ms = self.wall_of(frame);
                    let played_ms = (played as f64 * 1000.0 / f64::from(self.rate())) as i64;
                    let why = match reason {
                        EndReason::Eof => "end",
                        EndReason::Failed => "error",
                        _ => tr.end_hint,
                    };
                    if let Some(rid) = tr.as_run {
                        // A wall-clock jump inside the row leaves its two stamps on different clocks.
                        let jumped = self.fail.jump_total_ms
                            - self
                                .fail
                                .jump_at_start
                                .remove(&rid)
                                .unwrap_or(self.fail.jump_total_ms);
                        let flag = (jumped != 0).then(|| format!("clock_jump_ms={jumped}"));
                        if let Some(r) = self.as_run.iter_mut().find(|r| r.id == rid) {
                            if let Some(f) = &flag {
                                r.flags.push(f.clone());
                            }
                            r.ended_ms = Some(ended_ms);
                            r.played_ms = Some(played_ms);
                            r.end_reason = Some(why.into());
                        }
                        if let Some(w) = &self.state {
                            w.send(Msg::AsRunEnd {
                                id: rid,
                                ended_ms,
                                played_ms,
                                reason: why.into(),
                                flag,
                            });
                        }
                    }
                    if self.on_air_voice == Some(id) {
                        self.on_air_voice = None;
                    }
                }
            }
        }
    }

    fn maybe_sync(&mut self, send: &mut dyn FnMut(Command)) {
        let Some(sync) = self.sync.as_mut() else {
            return;
        };
        if self.wall_ms - self.last_sync_wall < self.opts.sync_every_ms {
            return;
        }
        self.last_sync_wall = self.wall_ms;
        let out = sync.poll(false, &mut self.cat, &self.sched.prog);
        self.library_unavailable = out.unavailable;
        if out.unavailable {
            self.fail.alerts.raise(
                crate::alerts::code::LIBRARY_DB_UNAVAILABLE,
                "",
                crate::alerts::Severity::Critical,
                "library.db cannot be read; playing from the track cache",
                None,
                self.wall_ms,
            );
        } else {
            self.fail.alerts.clear(
                crate::alerts::code::LIBRARY_DB_UNAVAILABLE,
                "",
                self.wall_ms,
            );
        }
        for (id, why) in self.cat.take_ended() {
            tracing::info!(item_id = id, "quarantine ended: {why}");
            self.forget_item_failures(id);
        }
        for id in self.cat.take_seen() {
            if let Some(w) = &self.state {
                w.send(Msg::QuarantineSeen { item_id: id });
            }
        }
        let replan = self.cat.take_playable_changed();
        if out.is_empty() {
            return;
        }
        if replan {
            // An item became playable or stopped being (a quarantine released, an item disabled): what
            // is dealt but not yet locked is dealt again.
            let t = RuntimeTiming {
                cat: &self.cat,
                presets: &self.presets,
                preset: &self.opts.preset,
                rate: self.opts.rate,
            };
            self.sched.regenerate(self.wall_ms, &self.cat, &t, false);
            self.reconcile_pending(send);
            self.dirty = true;
        }
        if !out.changed_rows.is_empty() {
            if let Some(w) = &self.state {
                w.send(Msg::Tracks {
                    upsert: out.changed_rows,
                    delete: vec![],
                });
            }
            self.run_gains.clear();
        }
        if let Some(p) = out.programming {
            if let Some(w) = &self.state {
                w.send(Msg::Programming {
                    key: "programming".into(),
                    revision: 0,
                    body: serde_json::to_string(&*p).unwrap_or_default(),
                });
            }
            self.default_playlist_id = self
                .opts
                .default_playlist
                .as_deref()
                .and_then(|n| p.by_name(n))
                .map(|pl| pl.id);
            self.sched.cfg.default_playlist = self.default_playlist_id;
            self.sched.set_programming(p);
            let t = RuntimeTiming {
                cat: &self.cat,
                presets: &self.presets,
                preset: &self.opts.preset,
                rate: self.opts.rate,
            };
            self.sched.regenerate(self.wall_ms, &self.cat, &t, false);
            self.reconcile_pending(send);
        }
        self.dirty = true;
    }

    /// If the entry the conductor committed is not the head any more, take the boundary back.
    fn reconcile_pending(&mut self, send: &mut dyn FnMut(Command)) {
        let Some(id) = self.pending else { return };
        if id < -1 || self.sched.head().map(|h| h.id) == Some(id) {
            return;
        }
        self.undo_pending(send);
    }

    /// The committed item starts too soon to take its boundary back safely: within the mixer's
    /// lookahead plus the minimum lead. Undoing it later than that would either reach the mixer after
    /// the item had started (the station's frame is up to a period old on rt-out) or leave a
    /// re-planned boundary unable to start its item as early as the one it replaced (a gap when A ends
    /// at its file end). An action in this window waits for the item to start (`Busy`), or, for
    /// `next`, `fade_next` and `cut`, lets the transition already under way stand.
    pub(crate) fn pending_imminent(&self) -> bool {
        self.pending.is_some()
            && self
                .cond
                .placed
                .last()
                .is_some_and(|p| !p.ended && p.start_frame <= self.undo_horizon())
    }

    fn undo_horizon(&self) -> u64 {
        self.now_frame + self.opts.lookahead_frames + lead_frames(self.rate())
    }

    pub(crate) fn undo_pending(&mut self, send: &mut dyn FnMut(Command)) {
        if self.pending.is_none() || self.pending_imminent() {
            return;
        }
        let voice = self.cond.placed.last().map(|p| p.id);
        if self.cond.uncommit_any(self.undo_horizon(), send).is_some() {
            if let Some(v) = voice {
                self.voices.remove(&(v.slot, v.gen));
            }
            self.pending = None;
        }
    }

    // ---- timed blocks (4.6, 4.9) ------------------------------------------------------------------------------

    /// `POST /schedule/changes` (7.2): a one-shot start of a playlist.
    pub fn add_schedule_change(
        &mut self,
        playlist_id: i64,
        at_ms: i64,
        how: crate::programming::StartHow,
        mode: crate::programming::BoundaryMode,
        early_s: i64,
        late_s: i64,
    ) -> Result<i64, String> {
        let id = self
            .sched
            .add_oneshot(playlist_id, at_ms, how, mode, early_s, late_s)?;
        self.dirty = true;
        Ok(id)
    }

    /// `DELETE /schedule/changes/{id}`.
    pub fn cancel_schedule_change(&mut self, id: i64) -> bool {
        let r = self.sched.cancel_oneshot(id);
        self.dirty = true;
        r
    }

    /// The output frame at which the wall time `ms` is audible (the mapping of 1.6, from this step).
    pub(crate) fn frame_of_wall(&self, ms: i64) -> u64 {
        let d = ((ms - self.wall_ms) as f64 - self.wall_frac_ms) * f64::from(self.rate()) / 1000.0;
        (self.now_frame as i64 + d.round() as i64).max(0) as u64
    }

    /// Timed stops, fade stops and ends: set up on exact frames a few seconds ahead, and a stop that has
    /// been reached stops the station.
    fn timed_step(&mut self, send: &mut dyn FnMut(Command)) {
        if let Some(a) = self.armed_stop {
            if self.now_frame >= a.at {
                self.armed_stop = None;
                self.transport = Transport::Stopped;
                self.finished_last = true;
                for t in self.voices.values_mut() {
                    t.end_hint = "stop";
                }
                // Nothing sounds any more: the scheduler's "on air" is over (a block that starts audio
                // now acts at its time, 4.6).
                self.sched.on_air_ended(&self.cat);
                self.dirty = true;
            }
        }
        let acts = {
            let t = RuntimeTiming {
                cat: &self.cat,
                presets: &self.presets,
                preset: &self.opts.preset,
                rate: self.opts.rate,
            };
            self.sched
                .due_acts(self.wall_ms, ACT_AHEAD_MS, &self.cat, &t)
        };
        for act in acts {
            self.apply_act(act, send);
        }
    }

    /// Carry out one act of the scheduler: a timed or in-sequence block's, or one fired by hand.
    pub(crate) fn apply_act(&mut self, act: TimedAct, send: &mut dyn FnMut(Command)) {
        use crate::timed::ActKind;
        self.dirty = true;
        match act.kind {
            ActKind::Stop { fade_ms } | ActKind::HardStop { fade_ms } => {
                self.undo_pending(send);
                let earliest = self.now_frame + s_frames(MIN_LEAD_S, self.rate()).max(0) as u64;
                let at = self.frame_of_wall(act.target_ms).max(earliest);
                let fade_s = fade_ms as f64 / 1000.0;
                self.cond.stop_all(at, fade_s, send);
                // The earlier stop of two stands.
                if self.armed_stop.is_none_or(|a| at < a.at) {
                    self.armed_stop = Some(ArmedStop { at, fade_s });
                }
                tracing::info!(block = %act.name, "timed stop armed {} ms ahead", act.target_ms - self.wall_ms);
            }
            ActKind::Hold => {
                self.undo_pending(send);
                self.cond.finish_last(send);
                self.transport = Transport::Held;
                self.finished_last = true;
            }
            ActKind::EndProgram => {
                self.reconcile_pending(send);
            }
            other => self.run_act(TimedAct { kind: other, ..act }, send),
        }
    }

    /// The program is replaced by `playlist_id` (a switch, or a program start with a T0 of its own).
    fn switch_program(
        &mut self,
        playlist_id: i64,
        how: SwitchHow,
        t0: Option<(i64, &'static str, Option<i64>)>,
        send: &mut dyn FnMut(Command),
    ) -> Result<OpEffect, OpError> {
        let wall_ms = self.wall_ms;
        let t = RuntimeTiming {
            cat: &self.cat,
            presets: &self.presets,
            preset: &self.opts.preset,
            rate: self.opts.rate,
        };
        let effect = match t0 {
            None => self
                .sched
                .switch(playlist_id, how, wall_ms, &self.cat, &t)?,
            Some((t0_ms, source, nominal)) => self.sched.switch_with(
                playlist_id,
                how,
                wall_ms,
                t0_ms,
                source,
                nominal,
                &self.cat,
                &t,
            )?,
        };
        if t0.is_some() {
            self.sched.consume_occurrences(playlist_id, wall_ms);
            // What the start dialog shows: the blocks already past for this T0 and what their policy did.
            let zone = self.sched.cfg.zone.clone();
            self.last_start = crate::preview::dry_run(
                &self.sched.prog,
                playlist_id,
                t0.map_or(wall_ms, |x| x.0),
                Some(wall_ms),
                &zone,
            )
            .map(|p| p.to_json(&zone)["past"].clone());
        }
        self.undo_pending(send);
        if t0.is_some() && how == SwitchHow::Now {
            self.take_station_for_program(send);
            return Ok(effect);
        }
        let silent = self.cond.live_voices() == 0;
        if silent || self.transport == Transport::Stopped {
            // Stopped: the new playlist's first item plays at once.
            if let Some(h) = self.peek_next() {
                self.start_head(h, send)?;
            }
        } else if how == SwitchHow::Now {
            if self.peek_next().is_none() {
                // The new program has nothing to play yet (it waits for the operator, or for a time): what is
                // sounding plays to its end and the program goes on from there.
                self.cond.finish_last(send);
                self.finished_last = true;
            } else {
                self.manual_transition(
                    TransitionKind::FadeNext {
                        fade_s: crate::plan::FADE_NEXT_DEFAULT_S,
                    },
                    "fade",
                    send,
                )?;
            }
        }
        Ok(effect)
    }

    /// A program start (`program/start`, from the Stream Deck or the web UI) always takes the station, whatever
    /// it was doing: stopped, held or after a stop, the audio starts again; anything sounding (a playlist,
    /// another program, a cart, a held item) fades out over the fade-next time; a stop whose fade is under way
    /// finishes first. The program's first item starts on its own right after (4.7).
    fn take_station_for_program(&mut self, send: &mut dyn FnMut(Command)) {
        let earliest = self.now_frame + s_frames(MIN_LEAD_S, self.rate()).max(0) as u64;
        let mut start_at = earliest;
        if let Some(a) = self.armed_stop.take() {
            // a stop (or the old program's fade out) is set up: it finishes, the program starts after it
            if a.at > self.now_frame {
                start_at = start_at.max(a.at);
            }
        }
        if self.cond.live_voices() > 0 {
            let fade = crate::plan::FADE_NEXT_DEFAULT_S;
            let end = earliest + s_frames(fade, self.rate()).max(1) as u64;
            if self.transport == Transport::Stopped {
                // a stop is fading: let it end where it was going to (no later than a fade-next from now)
                start_at = start_at.max(end.min(self.stop_end_frame().unwrap_or(end)));
            } else if start_at <= earliest {
                self.cond.stop_all(earliest, fade, send);
                start_at = start_at.max(end);
            }
            for t in self.voices.values_mut() {
                t.end_hint = "fade";
            }
        }
        self.transport = Transport::Playing;
        self.finished_last = false;
        self.sched.unhalt();
        self.assist_release();
        self.program_start_at = Some(start_at);
        self.dirty = true;
        if let Some(h) = self.peek_next() {
            let timed_later = h
                .timed
                .as_ref()
                .and_then(|t| t.hard_at_ms)
                .is_some_and(|at| at - self.wall_ms > START_AHEAD_MS);
            if !timed_later {
                self.try_start_head(h, send);
            }
        }
    }

    /// The last frame any voice still sounds to, from the stops set up on them.
    fn stop_end_frame(&self) -> Option<u64> {
        self.cond
            .placed
            .iter()
            .chain(self.cond.carts.iter())
            .filter(|p| !p.ended)
            .filter_map(|p| {
                self.cond.sidecar.voices[p.record]
                    .stop
                    .as_ref()
                    .map(|s| s.at + u64::from(s.frames))
            })
            .max()
    }

    /// While a timed stop is armed, whatever is placed after it was set up gets the same stop.
    pub(crate) fn reapply_armed_stop(&mut self, send: &mut dyn FnMut(Command)) {
        if let Some(a) = self.armed_stop {
            if a.at > self.now_frame {
                self.cond.stop_all(a.at, a.fade_s, send);
            }
        }
    }

    // ---- boundaries -----------------------------------------------------------------------------------------

    fn boundary(&mut self, send: &mut dyn FnMut(Command)) -> Result<(), String> {
        if self.transport != Transport::Playing {
            // A timed block that starts something acts at its time even when the station is stopped
            // (4.6: with nothing playing a boundary block acts at `T`).
            let due = self.transport == Transport::Stopped
                && self.armed_stop.is_none()
                && self.sched.head().is_some_and(|h| {
                    // An entry a timed block put here starts at its time; the first item dealt after an
                    // in-sequence stop starts the audio again (4.4).
                    h.timed
                        .as_ref()
                        .and_then(|t| t.hard_at_ms)
                        .is_some_and(|at| at - self.wall_ms <= START_AHEAD_MS)
                        || h.flags.iter().any(|f| f == "seq_start")
                });
            if !due {
                return Ok(());
            }
            self.transport = Transport::Playing;
            self.finished_last = false;
            self.sched.unhalt();
        }
        if self.apply_immediate(send)? {
            return Ok(());
        }
        if let Some(nb) = self.program_start_at {
            // A program start took the station: its first item starts on its own once it is dealt, after what
            // was sounding has faded out (never a segue from an item that is being stopped).
            if self.now_frame >= nb && self.cond.live_voices() == 0 {
                self.program_start_at = None;
            } else {
                if let Some(h) = self.peek_next() {
                    let wait = h
                        .timed
                        .as_ref()
                        .and_then(|t| t.hard_at_ms)
                        .is_some_and(|at| at - self.wall_ms > START_AHEAD_MS);
                    if !wait {
                        self.try_start_head(h, send);
                    }
                }
                return Ok(());
            }
        }
        if self.pending.is_some() {
            return Ok(());
        }
        let silent = self.cond.live_voices() == 0;
        if self.assist {
            // Assist (4.7): the item on air plays to its end and the station waits for `next`.
            if !silent && !self.finished_last {
                self.cond.finish_last(send);
                self.finished_last = true;
            }
            if silent && self.now_entry().is_some() {
                return Ok(());
            }
        }
        if silent {
            if let Some(h) = self.peek_next() {
                // An entry a timed block put here starts at its time.
                let wait = h
                    .timed
                    .as_ref()
                    .and_then(|t| t.hard_at_ms)
                    .is_some_and(|at| at - self.wall_ms > START_AHEAD_MS);
                if !wait {
                    self.try_start_head(h, send);
                }
            }
            return Ok(());
        }
        let Some(mut head) = self.peek_next() else {
            if !self.finished_last {
                self.cond.finish_last(send);
                self.finished_last = true;
            }
            return Ok(());
        };
        if self.assist {
            return Ok(());
        }
        if self.finished_last {
            // The queue had run out and the item on air was given its end; there is a next item now.
            self.release_finish(send);
        }
        let item = match self.entry_item(&head) {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!("dropping queue entry {}: {e}", head.id);
                if head.id >= -1 {
                    self.sched.st.queue.retain(|q| q.id != head.id);
                } else {
                    self.fail.alt_entry = None;
                }
                self.dirty = true;
                return Ok(());
            }
        };
        let mut patch = parse_patch(&head.transition);
        let item = self.prepare_item(&mut head, item, &mut patch);
        let kind = if head.continuity_from_prev {
            TransitionKind::Continuity
        } else if head.transition.as_deref() == Some("cut") {
            TransitionKind::Cut
        } else {
            TransitionKind::Auto
        };
        let needs_exact =
            head.continuity_from_prev || self.cond.last().is_some_and(|p| p.out_is_file_end);
        if needs_exact && !self.cond.length_known() {
            return Ok(());
        }
        let (mut p, mut pa, mut pb) =
            self.cond
                .plan_next(&item, kind, patch.as_ref(), self.now_frame)?;
        // A timed block's entry is cut in at its hard time when its natural boundary falls later (4.6).
        if let Some(h) = head.timed.as_ref().and_then(|t| t.hard_at_ms) {
            let at = self.frame_of_wall(h);
            if p.s_b > at + 1 {
                let earliest = self.now_frame + lead_frames(self.rate());
                let (p2, pa2, pb2) = self.cond.plan_next(
                    &item,
                    TransitionKind::HardTime {
                        at_frame: at.max(earliest),
                        // A back-timed closing item cuts the one before it over 2 s (4.9).
                        fade_s: head
                            .flags
                            .iter()
                            .any(|f| f == "bt_exact")
                            .then_some(crate::programming::EXACT_FADE_S),
                    },
                    patch.as_ref(),
                    self.now_frame,
                )?;
                (p, pa, pb) = (p2, pa2, pb2);
                head.flags.push("hard_time".into());
                if let Some(q) = self.sched.st.queue.first_mut() {
                    if q.id == head.id {
                        q.flags.push("hard_time".into());
                    }
                }
            }
        }
        let ahead = s_frames(
            COMMIT_AHEAD_S.min(f64::from(self.opts.lock_ahead_s)),
            self.rate(),
        )
        .max(0) as u64;
        if p.first_command_frame() > self.now_frame + ahead {
            return Ok(());
        }
        // The entry's file must have been read a moment ago (1.5: pre-flight again at commit).
        match self.gate(&head) {
            Gate::Go => {}
            Gate::Wait | Gate::Dropped => return Ok(()),
        }
        self.commit(&head, item, &p, &pa, &pb, patch, "segue", send)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit(
        &mut self,
        head: &QEntry,
        item: Arc<Item>,
        p: &crate::plan::Plan,
        pa: &common::transition::TransitionParams,
        pb: &common::transition::TransitionParams,
        patch: Option<PresetPatch>,
        hint: &'static str,
        send: &mut dyn FnMut(Command),
    ) -> Result<(), String> {
        let run = self.run_gain_for(head);
        let a_voice = self.cond.placed.last().map(|p| p.id);
        self.cond.commit(item, p, pa, pb, patch, run, send)?;
        let id = self.cond.placed.last().map(|p| p.id).ok_or("no voice")?;
        if let Some(a) = a_voice {
            if let Some(t) = self.voices.get_mut(&(a.slot, a.gen)) {
                t.end_hint = hint;
            }
        }
        let alt = head.id < -1;
        self.voices.insert(
            (id.slot, id.gen),
            Tracked {
                entry: head.clone(),
                kind: if alt { Kind::Alt } else { Kind::Chain },
                as_run: None,
                end_hint: "end",
                resumed_from_ms: None,
            },
        );
        self.pending = Some(head.id);
        if !alt {
            if let Some(h) = self.sched.st.queue.first_mut() {
                if h.id == head.id {
                    h.committed = true;
                }
            }
        }
        self.finished_last = false;
        self.load_in_flight = true;
        self.dirty = true;
        self.reapply_armed_stop(send);
        Ok(())
    }

    // ---- operator actions (4.7) -------------------------------------------------------------------------------

    /// Do an operator action. `Err(Busy)` means a boundary was committed in the block just sent and
    /// cannot be taken back until it has been rendered: try again after the next block.
    pub fn op(
        &mut self,
        op: Op,
        now_frame: u64,
        wall_ms: i64,
        send: &mut dyn FnMut(Command),
    ) -> Result<OpEffect, OpError> {
        self.now_frame = now_frame;
        self.wall_ms = wall_ms;
        let touches_boundary = !matches!(op, Op::Assist(_) | Op::FireCart { .. });
        if touches_boundary && self.pending.is_some() && self.load_in_flight {
            return Err(OpError::Busy);
        }
        // The committed item is about to start (or has started and its event is not in yet): its
        // boundary cannot be taken back. Before this check an action here planned from that item
        // into itself (`next`: the committed item played twice) or acted on an entry that was
        // already sounding.
        if self.pending_imminent() {
            match op {
                Op::Next | Op::FadeNext(_) | Op::Cut => {
                    // The segue the operator asked for is the one under way.
                    return Ok(OpEffect {
                        new_next_item: self.sched.head().map(|h| h.item_id),
                        ..OpEffect::default()
                    });
                }
                Op::Stop { .. } | Op::Assist(_) | Op::FireCart { .. } => {}
                _ => return Err(OpError::Busy),
            }
        }
        let earliest = now_frame + s_frames(MIN_LEAD_S, self.rate()).max(0) as u64;
        let mut effect = OpEffect::default();
        match op {
            Op::Next | Op::FadeNext(_) | Op::Cut => {
                self.sched.unhalt();
                self.undo_pending(send);
                let Some(head) = self.peek_next() else {
                    return Err(OpError::Refused("nothing is queued".into()));
                };
                let silent = self.cond.live_voices() == 0;
                self.assist_release();
                if silent || self.transport == Transport::Stopped {
                    self.start_head(head, send)?;
                } else {
                    let kind = match op {
                        Op::Next => TransitionKind::SegueNow,
                        Op::FadeNext(f) => TransitionKind::FadeNext {
                            fade_s: f.unwrap_or(crate::plan::FADE_NEXT_DEFAULT_S),
                        },
                        _ => TransitionKind::Cut,
                    };
                    let hint = match op {
                        Op::Next => "next",
                        Op::FadeNext(_) => "fade",
                        _ => "cut",
                    };
                    self.manual_transition(kind, hint, send)?;
                }
            }
            Op::Stop { fade_s } => {
                self.undo_pending(send);
                self.cond
                    .stop_all(earliest, fade_s.unwrap_or(crate::plan::STOP_FADE_S), send);
                for t in self.voices.values_mut() {
                    t.end_hint = "stop";
                }
                self.transport = Transport::Stopped;
                self.finished_last = true;
            }
            Op::Hold => {
                self.undo_pending(send);
                self.cond.finish_last(send);
                self.transport = Transport::Held;
                self.finished_last = true;
            }
            Op::Resume => {
                self.sched.unhalt();
                if self.transport != Transport::Stopped {
                    self.release_finish(send);
                }
                self.transport = Transport::Playing;
                self.finished_last = false;
                self.assist_release();
            }
            Op::Assist(on) => {
                self.assist = on;
                if !on && self.transport == Transport::Playing {
                    // The end assist armed on the item on air goes, or the next segue could not
                    // overlap it (a stop only ever shortens a voice).
                    self.release_finish(send);
                }
                self.finished_last = false;
                self.dirty = true;
            }
            Op::Reshuffle => {
                let t = RuntimeTiming {
                    cat: &self.cat,
                    presets: &self.presets,
                    preset: &self.opts.preset,
                    rate: self.opts.rate,
                };
                effect = self.sched.reshuffle(wall_ms, &self.cat, &t)?;
                self.reconcile_pending(send);
            }
            Op::Insert { item_id, at } => {
                let t = RuntimeTiming {
                    cat: &self.cat,
                    presets: &self.presets,
                    preset: &self.opts.preset,
                    rate: self.opts.rate,
                };
                effect = self.sched.insert(item_id, at, wall_ms, &self.cat, &t)?;
                self.reconcile_pending(send);
            }
            Op::Remove { entry_id } => {
                let t = RuntimeTiming {
                    cat: &self.cat,
                    presets: &self.presets,
                    preset: &self.opts.preset,
                    rate: self.opts.rate,
                };
                effect = self.sched.remove(entry_id, wall_ms, &self.cat, &t)?;
                self.reconcile_pending(send);
            }
            Op::Move { entry_id, to } => {
                let t = RuntimeTiming {
                    cat: &self.cat,
                    presets: &self.presets,
                    preset: &self.opts.preset,
                    rate: self.opts.rate,
                };
                effect = self
                    .sched
                    .move_entry(entry_id, to, wall_ms, &self.cat, &t)?;
                self.reconcile_pending(send);
            }
            Op::SetTransition {
                entry_id,
                transition,
            } => {
                let t = RuntimeTiming {
                    cat: &self.cat,
                    presets: &self.presets,
                    preset: &self.opts.preset,
                    rate: self.opts.rate,
                };
                if let Some(text) = &transition {
                    self.check_entry_transition(text)
                        .map_err(OpError::Refused)?;
                }
                let is_head = self.sched.head().map(|h| h.id) == Some(entry_id);
                effect = self
                    .sched
                    .set_transition(entry_id, transition, wall_ms, &self.cat, &t)?;
                // The boundary into the next item may already be planned on the mixer: take it back so
                // that it is planned again with the new setting.
                if is_head {
                    self.undo_pending(send);
                }
                self.reconcile_pending(send);
            }
            Op::PlayAlbum {
                album_key,
                gapless,
                at,
            } => {
                let t = RuntimeTiming {
                    cat: &self.cat,
                    presets: &self.presets,
                    preset: &self.opts.preset,
                    rate: self.opts.rate,
                };
                effect = self
                    .sched
                    .play_album(&album_key, gapless, at, wall_ms, &self.cat, &t)?;
                self.reconcile_pending(send);
            }
            Op::Switch { playlist_id, how } => {
                effect = self.switch_program(playlist_id, how, None, send)?;
            }
            Op::EndAt {
                playlist_id,
                at_ms,
                then,
            } => {
                if at_ms <= wall_ms + 5_000 {
                    return Err(OpError::Refused(
                        "the end time must be in the future".into(),
                    ));
                }
                let id = self.sched.make_end_at(playlist_id, at_ms, then)?;
                effect =
                    self.switch_program(id, SwitchHow::Now, Some((wall_ms, "api", None)), send)?;
            }
            Op::StartProgram {
                playlist_id,
                t0,
                how,
                source,
            } => {
                // A program made for one station never runs on another (a deck key pointed at the wrong station
                // once started eve's show on the office stream).
                if let Some(pl) = self.sched.prog.playlists.get(&playlist_id) {
                    if let Some(owner) = pl.station_id.as_deref().filter(|o| *o != self.opts.name) {
                        return Err(OpError::Refused(format!(
                            "`{}` is a program of station {owner}; this is station {}",
                            pl.name, self.opts.name
                        )));
                    }
                }
                let mut r = self.sched.resolve_t0(playlist_id, t0, wall_ms)?;
                if r.1 != "nominal" {
                    r.1 = source;
                }
                effect = self.switch_program(playlist_id, how, Some(r), send)?;
            }
            Op::FireCart { item_id } => {
                let item = self.item_of(item_id)?;
                self.cond
                    .fire_cart(item.clone(), earliest, send)
                    .map_err(OpError::Refused)?;
                let id = self.cond.carts.last().map(|p| p.id).ok_or("no voice")?;
                let mut entry = QEntry::new(-1, item_id, crate::sched::Origin::Manual);
                entry.committed = true;
                entry.planned_start_ms = Some(wall_ms);
                self.voices.insert(
                    (id.slot, id.gen),
                    Tracked {
                        entry,
                        kind: Kind::Cart,
                        as_run: None,
                        end_hint: "end",
                        resumed_from_ms: None,
                    },
                );
                self.load_in_flight = true;
            }
        }
        self.dirty = true;
        self.flush_sched();
        effect.new_next_item = self.sched.head().map(|h| h.item_id);
        Ok(effect)
    }

    fn assist_release(&mut self) {
        self.finished_last = false;
    }

    /// Take back the end `finish_last` armed on the last item (a hold, assist, or a queue that ran
    /// out), so the boundary planned next can let it play past its cue-out: a stop only ever
    /// shortens a voice, so the armed end would otherwise cut the overlap the next plan asks for.
    fn release_finish(&mut self, send: &mut dyn FnMut(Command)) {
        if self.finished_last {
            if let Some(p) = self.cond.placed.last() {
                if !p.ended {
                    send(p.id.clear_stop());
                }
            }
        }
        self.finished_last = false;
    }

    /// `next`, `fade_next`, `cut`: plan the boundary from what is sounding into the head of the queue
    /// and commit it now.
    fn manual_transition(
        &mut self,
        kind: TransitionKind,
        hint: &'static str,
        send: &mut dyn FnMut(Command),
    ) -> Result<(), String> {
        let head = self
            .peek_next()
            .ok_or_else(|| "nothing is queued".to_string())?;
        self.manual_transition_with(head, kind, hint, send)
    }

    pub(crate) fn manual_transition_with(
        &mut self,
        head: QEntry,
        kind: TransitionKind,
        hint: &'static str,
        send: &mut dyn FnMut(Command),
    ) -> Result<(), String> {
        let mut head = head;
        let item = self.entry_item(&head)?;
        let mut patch = parse_patch(&head.transition);
        let item = self.prepare_item(&mut head, item, &mut patch);
        let (p, pa, pb) = self
            .cond
            .plan_next(&item, kind, patch.as_ref(), self.now_frame)?;
        self.commit(&head, item, &p, &pa, &pb, patch, hint, send)
    }
}
