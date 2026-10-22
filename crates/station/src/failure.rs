//! Failure handling (milestone M1.6, architecture 1.5): what the station does when something goes wrong.
//! The rule (owner, 2026-09-29): try everything to keep playing what it is supposed to play; barring a
//! hardware failure, work around problems with individual tracks. Every per-item problem has the same
//! four-part answer: **skip** it without dead air, **substitute** through the normal rules, **quarantine
//! and flag** it when the fault is the file's, and **never stop the station**.
//!
//! The pieces, all driven from [`Station::step`]:
//!
//! * pre-flight of queued items (on a worker thread) and again at commit; missing, truncated and
//!   mismatched files are dropped from the queue and quarantined;
//! * the early-end watch: a voice whose decoder ended before it should (a fatal decode error, a file cut
//!   off after import) gets the emergency transition of 1.5 (the next item 50 ms before the audio runs out);
//! * program-silence detection with its escalation: skip at the window, the emergency playlist at twice
//!   the window, the RAM loop at three times;
//! * the alternative sources of programming (emergency deck, RAM loop) and when the station uses them
//!   (safe mode, silence, a failing media store, a depleted or starved playlist);
//! * the state of `state.db` (buffered writes, `db_unavailable`, `disk_full`, corruption), free disk space,
//!   clock jumps, and device loss.
//!
//! Alerts (`alerts`) record all of it; the station's health is the worst severity among the active ones.

use crate::alerts::{code, Alert, Alerts, HealthStatus, Severity};
use crate::conductor::Conductor;
use crate::item::Item;
use crate::plan::{ms_frames, s_frames, TransitionKind};
use crate::preflight::{self, MediaFaults, Preflight, Verdict};
use crate::programming::Entry;
use crate::ram::{self, RamLoop, RamSlot};
use crate::runtime::{parse_patch, Kind, Station, Tracked, Transport};
use crate::sched::{Origin, QEntry};
use crate::state::{LibOp, Msg, FAIL_CORRUPT, FAIL_FULL, FAIL_NONE};
use common::transition::PresetPatch;
use engine::rt::Command;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

/// How long the emergency playlist (or the RAM loop) holds the station after a silence escalation or a
/// media-store failure before normal programming is tried again at a boundary (ms).
pub const ALT_HOLD_MS: i64 = 5 * 60_000;
/// Items that must fail within [`STORE_WINDOW_MS`] for the media store to be judged failing (1.5).
pub const STORE_FAIL_ITEMS: usize = 3;
pub const STORE_WINDOW_MS: i64 = 10 * 60_000;
/// The store is re-tested every this long while it is judged failing (1.5: every 60 s).
pub const STORE_PROBE_MS: i64 = 60_000;
/// While all is well the first queue entries are re-read this often, so a store that goes bad is noticed
/// within seconds and not at the next boundary.
pub const STORE_WATCH_MS: i64 = 2_000;
/// A decoder that ends this much earlier than the item should is an early end (1.5: 2 s).
pub const EARLY_END_S: f64 = 2.0;
/// `db_unavailable` turns critical after this long (1.5).
pub const DB_CRITICAL_MS: i64 = 60_000;
/// A database alert clears once writes have worked for this long.
pub const DB_CLEAR_AFTER_MS: i64 = 5_000;
/// A device outage shorter than this resumes the same position; a longer one moves on (1.5).
pub const DEVICE_RESUME_MAX_MS: u32 = 30_000;
/// Free space under this raises `disk_low` (1.5).
pub const DISK_LOW_BYTES: u64 = 5 * 1024 * 1024 * 1024;
pub const DISK_CHECK_MS: i64 = 60_000;
/// Silence for longer than this (a clock stepping) is a clock jump (1.5: 2 s).
pub const CLOCK_JUMP_MS: i64 = 2_000;
/// The silence window of an item is at least its own longest quiet stretch plus this (1.5, derived from
/// the analysis instead of a hand-set `silence_ok`).
pub const QUIET_MARGIN_MS: i64 = 5_000;
/// The file-level threshold the quiet profile is read at is the detector's threshold minus the item's gain,
/// plus this: the profile's measure (the louder channel over 50 ms) can read a passage up to 3 dB louder
/// than the mixer's (the mean of both channels over one block) does.
pub const QUIET_SLACK_DB: f64 = 3.0;
/// A voice whose decoder has left it starved this long is given up on: the next item plays (1.5, the
/// emergency transition of a voice whose ring ran dry).
pub const STARVE_GIVE_UP_MS: i64 = 2_000;
/// A quarantine after this many silent skips of one item, and after this many fatal decode errors.
pub const SILENT_QUARANTINE_AT: i64 = 2;
pub const DECODE_QUARANTINE_AT: i64 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AltSource {
    Emergency,
    Ram,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AltWhy {
    /// Safe mode: until an operator acknowledges it.
    Safe,
    /// A silence escalation.
    Silence,
    /// The media store is failing.
    Store,
    /// The emergency playlist could not be read.
    EmergencyFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AltMode {
    pub source: AltSource,
    pub why: AltWhy,
    pub since_ms: i64,
    pub hold_until_ms: i64,
}

/// A change of programme the station makes at once (not at the next boundary).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Immediate {
    Cut,
    /// A cut whose as-run reason is `device` (a sound-card outage of 30 s or more).
    Device,
    Fade(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailKind {
    Missing,
    Truncated,
    Mismatch,
    TruncatedOnAir,
    Decode,
    Silent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    Go,
    Wait,
    Dropped,
}

pub struct FailState {
    pub alerts: Alerts,
    pub preflight: Preflight,
    pub safe_mode: bool,
    pub alt: Option<AltMode>,
    /// The next entry from the emergency deck or the RAM loop, until it has been placed.
    pub(crate) alt_entry: Option<(AltSource, QEntry)>,
    /// The emergency playlist's playable items, scanned when the station starts using it.
    pub(crate) alt_ids: Option<Vec<i64>>,
    pub(crate) alt_cursor: usize,
    pub(crate) ram_cursor: usize,
    pub(crate) next_alt_id: i64,
    pub(crate) alt_on_air: Option<QEntry>,
    pub(crate) on_air_is_alt: bool,
    /// Why the station plays from the emergency deck without being told to: `depleted` or `starved`.
    pub transient: Option<&'static str>,
    pub ram: Option<Arc<RamLoop>>,
    pub(crate) ram_slot: Option<Arc<RamSlot>>,
    pub ram_error: Option<String>,
    pub(crate) armed_frame: u64,
    pub(crate) silence_stage: u8,
    /// The silent run (frames) when the last stage fired: the next needs another window after it.
    pub(crate) stage_run: u64,
    pub(crate) audible_since_ms: Option<i64>,
    pub(crate) immediate: Option<Immediate>,
    /// The entry the immediate change is about (a silent item, one interrupted by a device outage): if
    /// something else is on air by the time it can be applied, the change is dropped (an operator's
    /// `next` or the boundary already moved on, and the new item must not be cut).
    pub(crate) immediate_target: Option<i64>,
    /// The on-air voice whose ring ran dry, the frame it first did and what it had consumed then.
    pub(crate) starving: Option<((u8, u32), u64, u64)>,
    /// Voices that have starved (their audio is late): a successor placed later must not overlap them.
    pub(crate) late: HashSet<(u8, u32)>,
    /// Failures of an item's current file by kind (`silent`, `decode`, ...).
    pub(crate) errors: HashMap<(i64, String), i64>,
    pub(crate) store_failures: VecDeque<(i64, i64)>,
    pub store_failing: bool,
    pub(crate) store_probe_ms: i64,
    pub(crate) probe_ids: Vec<i64>,
    pub(crate) store_watch_ms: i64,
    pub(crate) drained: HashSet<(u8, u32)>,
    pub(crate) xruns: VecDeque<i64>,
    pub(crate) to_drop: Vec<i64>,
    /// Voices that failed before they sounded, and their items: handled in the next step.
    pub(crate) load_failed: Vec<(engine::mixer::VoiceId, i64)>,
    pub(crate) db_failing_since: Option<i64>,
    pub(crate) db_ok_since: Option<i64>,
    pub(crate) disk_checked_ms: i64,
    pub disk_free_bytes: Option<u64>,
    pub(crate) last_wall_ms: i64,
    /// The local day a snapshot was last asked for (5.5).
    pub(crate) snapshot_day: Option<i64>,
    pub(crate) last_frame: u64,
    pub device_lost_at: Option<i64>,
    /// The move to the next item after a long outage was prepared while the card was still away.
    pub(crate) device_armed: bool,
    /// The checkpoint's wall time when the process started: the as-run `restart` row runs from here to
    /// the first sound.
    pub(crate) restart_gap_from: Option<i64>,
    /// The audio frame from which the station has been on air and healthy since `restarted` was raised
    /// (audio time, so a step of the wall clock cannot clear the alert early). `None` while it is not.
    pub(crate) restart_healthy_from_frame: Option<u64>,
    /// The sum of the wall-clock jumps detected so far, ms (forward positive).
    pub(crate) jump_total_ms: i64,
    /// For each as-run row still open: `jump_total_ms` when it started, so its end can tell whether a jump
    /// fell inside it.
    pub(crate) jump_at_start: std::collections::HashMap<i64, i64>,
    /// `state.db` is corrupt at runtime: restart into the start path at the next item boundary.
    pub wants_restart: bool,
    /// Where `disk_free_bytes` is measured (the station's state directory).
    pub(crate) state_dir: Option<std::path::PathBuf>,
    /// When "nothing could be started" was last logged (it is retried every step).
    pub(crate) start_fail_logged_ms: i64,
}

impl FailState {
    pub fn new(next_alert_id: i64, faults: Arc<MediaFaults>) -> Self {
        Self {
            alerts: Alerts::new(next_alert_id),
            preflight: Preflight::new(faults),
            safe_mode: false,
            alt: None,
            alt_entry: None,
            alt_ids: None,
            alt_cursor: 0,
            ram_cursor: 0,
            next_alt_id: -1000,
            alt_on_air: None,
            on_air_is_alt: false,
            transient: None,
            ram: None,
            ram_slot: None,
            ram_error: None,
            armed_frame: 0,
            silence_stage: 0,
            stage_run: 0,
            audible_since_ms: None,
            immediate: None,
            immediate_target: None,
            starving: None,
            late: HashSet::new(),
            errors: HashMap::new(),
            store_failures: VecDeque::new(),
            store_failing: false,
            store_probe_ms: 0,
            probe_ids: Vec::new(),
            store_watch_ms: 0,
            drained: HashSet::new(),
            xruns: VecDeque::new(),
            to_drop: Vec::new(),
            load_failed: Vec::new(),
            db_failing_since: None,
            db_ok_since: None,
            disk_checked_ms: i64::MIN,
            disk_free_bytes: None,
            last_wall_ms: i64::MIN,
            snapshot_day: None,
            last_frame: 0,
            device_lost_at: None,
            device_armed: false,
            restart_gap_from: None,
            restart_healthy_from_frame: None,
            jump_total_ms: 0,
            jump_at_start: std::collections::HashMap::new(),
            wants_restart: false,
            state_dir: None,
            start_fail_logged_ms: i64::MIN / 2,
        }
    }
}

/// What `/health` and the display need from the failure side (12).
#[derive(Debug, Clone)]
pub struct Health {
    pub status: HealthStatus,
    pub alerts: Vec<Alert>,
    /// `normal`, `emergency`, `ram_loop` or `safe`.
    pub mode: &'static str,
    /// Why the station is not in normal programming, if it is not.
    pub mode_reason: Option<&'static str>,
    pub silence_s: f64,
    pub out_rms_dbfs: f32,
    pub queue_depth: usize,
    pub db_ok: bool,
    pub db_backlog: usize,
    pub quarantined: usize,
    pub disk_free_bytes: Option<u64>,
    pub ram_loop_seconds: Option<f64>,
    /// The last nightly snapshot of `state.db` and its check (5.5).
    pub snapshot: Option<library::backup::SnapshotInfo>,
}

/// What to make of a voice that starved `elapsed` frames ago and has consumed `progress` frames since:
/// `None` to wait (under `give_up` frames), `Some(true)` when it is stuck (dry, or trickling at under half
/// speed), `Some(false)` when it has caught up.
pub(crate) fn starve_verdict(elapsed: u64, progress: u64, give_up: u64) -> Option<bool> {
    if elapsed < give_up {
        return None;
    }
    Some(progress * 2 < elapsed)
}

fn markers_ok(i: &Item) -> bool {
    let m = &i.markers;
    m.cue_in >= 0
        && m.cue_in < m.segue
        && m.segue <= m.cue_out
        && m.cue_out - m.cue_in >= analysis::markers::MIN_WINDOW_MS
        && m.cue_out <= i.file_ms + 100
}

impl Station {
    // ---- reporting ---------------------------------------------------------------------------------

    pub fn alerts(&self) -> &Alerts {
        &self.fail.alerts
    }

    pub fn media_faults(&self) -> Arc<MediaFaults> {
        self.fail.preflight.faults.clone()
    }

    pub fn health(&self) -> Health {
        let st = self.cond.stats();
        let (mode, reason) = match (self.fail.alt, self.fail.transient) {
            (Some(a), _) if a.why == AltWhy::Safe => ("safe", Some("safe_mode")),
            (Some(a), _) => (
                match a.source {
                    AltSource::Emergency => "emergency",
                    AltSource::Ram => "ram_loop",
                },
                Some(match a.why {
                    AltWhy::Silence => "silence",
                    AltWhy::Store => "media_store_failing",
                    _ => "emergency_failed",
                }),
            ),
            (None, Some(t)) => ("emergency", Some(t)),
            (None, None) => ("normal", None),
        };
        let (db_ok, backlog) = match &self.state {
            Some(w) => (
                !w.status.failing.load(Relaxed),
                w.status.backlog.load(Relaxed),
            ),
            None => (true, 0),
        };
        Health {
            status: self.fail.alerts.status(),
            alerts: self.fail.alerts.active().into_iter().cloned().collect(),
            mode,
            mode_reason: reason,
            silence_s: st.silent_run_frames as f64 / f64::from(self.opts.rate),
            out_rms_dbfs: st.last_block_rms_dbfs,
            queue_depth: self.sched.st.queue.len(),
            db_ok,
            db_backlog: backlog,
            quarantined: self
                .cat
                .ids()
                .iter()
                .filter(|i| {
                    self.cat.meta(**i).is_some_and(|m| !m.playable)
                        && self.cat.get(**i).is_some_and(|e| e.row.quarantined)
                })
                .count(),
            disk_free_bytes: self.fail.disk_free_bytes,
            ram_loop_seconds: self.fail.ram.as_ref().map(|r| r.seconds(self.opts.rate)),
            snapshot: self
                .state
                .as_ref()
                .and_then(|w| w.status.snapshot.lock().ok().and_then(|g| g.clone())),
        }
    }

    /// The entry that is on air: from the emergency deck or the RAM loop, or the scheduler's.
    pub(crate) fn now_entry(&self) -> Option<&QEntry> {
        if self.fail.on_air_is_alt {
            self.fail.alt_on_air.as_ref()
        } else {
            self.sched.on_air()
        }
    }

    pub(crate) fn flush_alerts(&mut self) {
        for ev in self.fail.alerts.take_events() {
            match ev.change {
                crate::alerts::AlertChange::Raised => tracing::warn!(
                    code = %ev.alert.code, key = %ev.alert.key, severity = ev.alert.severity.name(),
                    "alert raised: {}", ev.alert.message
                ),
                crate::alerts::AlertChange::Escalated => tracing::warn!(
                    code = %ev.alert.code, key = %ev.alert.key, severity = ev.alert.severity.name(),
                    "alert escalated"
                ),
                crate::alerts::AlertChange::Cleared => {
                    tracing::info!(code = %ev.alert.code, key = %ev.alert.key, "alert cleared")
                }
            }
            if let Some(w) = &self.state {
                w.send(Msg::Alert {
                    change: ev.change,
                    alert: ev.alert,
                    at_ms: ev.at_ms,
                });
            }
        }
    }

    // ---- the step ------------------------------------------------------------------------------------

    /// Everything the failure side does in one step, before the boundary is decided.
    pub(crate) fn failure_step(&mut self, send: &mut dyn FnMut(Command)) {
        self.poll_ram();
        // An outage that has lasted 30 s will end with the next item, not the same position (1.5): the
        // move is prepared now, on the paused timeline, so nothing of the old item is heard on return.
        if let Some(t) = self.fail.device_lost_at {
            if !self.fail.device_armed && self.wall_ms - t >= i64::from(DEVICE_RESUME_MAX_MS) {
                self.fail.device_armed = true;
                self.set_immediate(Immediate::Device, true);
            }
        }
        self.handle_unstarted_failures(send);
        self.handle_load_failures(send);
        self.drop_quarantined_from_queue(send);
        self.poll_preflight();
        self.request_preflight();
        self.watch_voices(send);
        self.starve_step(send);
        self.silence_step();
        self.store_step();
        self.depletion_step();
        if self.fail.alerts.is_active(code::XRUN_BURST) {
            let now = self.wall_ms;
            while self.fail.xruns.front().is_some_and(|t| now - t > 600_000) {
                self.fail.xruns.pop_front();
            }
            if self.fail.xruns.len() <= 5 {
                self.fail.alerts.clear(code::XRUN_BURST, "", now);
            }
        }
        self.db_step();
        self.disk_step();
        self.clock_step();
        self.restart_alert_step();
        self.snapshot_step();
        self.clear_voice_alerts();
        self.flush_alerts();
    }

    /// `decode_slow` is about a voice, not the file: it clears when no voice of that item is playing any
    /// more (the history keeps it). (`decode_glitch` stays: a bad packet is a property of the file, and one
    /// standing info alert per such file is what the digest should show, not one per play.)
    fn clear_voice_alerts(&mut self) {
        let stale: Vec<(String, String)> = self
            .fail
            .alerts
            .active()
            .into_iter()
            .filter(|a| a.code == code::DECODE_SLOW)
            .filter(|a| {
                !self.cond.placed.iter().any(|p| {
                    !p.ended
                        && self
                            .voices
                            .get(&(p.id.slot, p.id.gen))
                            .is_some_and(|t| Some(t.entry.item_id) == a.item_id)
                })
            })
            .map(|a| (a.code.clone(), a.key.clone()))
            .collect();
        let now = self.wall_ms;
        for (c, k) in stale {
            self.fail.alerts.clear(&c, &k, now);
        }
    }

    // ---- pre-flight (1.5) ----------------------------------------------------------------------------

    fn media_path(&self, item_id: i64) -> Option<(std::path::PathBuf, u64)> {
        let e = self.cat.get(item_id)?;
        Some((e.row.path(self.cat.media()), e.row.file_bytes.max(0) as u64))
    }

    /// Ask for a check of the first queue entries: new ones at once, and the first three again every
    /// [`STORE_WATCH_MS`] (a store that goes bad is noticed in seconds).
    fn request_preflight(&mut self) {
        let now = self.wall_ms;
        let ids: Vec<i64> = self
            .sched
            .st
            .queue
            .iter()
            .take(6)
            .map(|e| e.item_id)
            .collect();
        let recheck = now - self.fail.store_watch_ms >= STORE_WATCH_MS;
        if recheck {
            self.fail.store_watch_ms = now;
        }
        for (i, id) in ids.into_iter().enumerate() {
            let due = match self.fail.preflight.results.get(&id) {
                None => true,
                Some((_, at)) => recheck && i < 3 && now - at >= STORE_WATCH_MS,
            };
            if due {
                if let Some((p, n)) = self.media_path(id) {
                    self.fail.preflight.request(id, p, n, now);
                }
            }
        }
    }

    fn poll_preflight(&mut self) {
        let now = self.wall_ms;
        for (id, v) in self.fail.preflight.poll(now) {
            self.handle_verdict(id, &v);
        }
    }

    /// Act on a pre-flight verdict. Returns true when the item is bad.
    fn handle_verdict(&mut self, id: i64, v: &Verdict) -> bool {
        if !self.cat.meta(id).is_some_and(|m| m.playable) {
            return !v.is_ok();
        }
        match v {
            Verdict::Ok => false,
            Verdict::Missing(d) => {
                self.item_failed(id, FailKind::Missing, d);
                true
            }
            Verdict::Truncated { actual, expected } => {
                self.item_failed(
                    id,
                    FailKind::Truncated,
                    &format!("the file is {actual} bytes, the library says {expected}"),
                );
                true
            }
            Verdict::Mismatch { actual, expected } => {
                self.item_failed(
                    id,
                    FailKind::Mismatch,
                    &format!("the file is {actual} bytes, the library says {expected}"),
                );
                true
            }
            Verdict::Unreadable(d) => {
                self.store_failure(id, d);
                true
            }
        }
    }

    /// The gate at commit: the entry's pre-flight must be fresh and good.
    pub(crate) fn gate(&mut self, head: &QEntry) -> Gate {
        if head.id < -1 {
            return Gate::Go;
        }
        let now = self.wall_ms;
        match self.fail.preflight.fresh(head.item_id, now).cloned() {
            Some(Verdict::Ok) => Gate::Go,
            Some(v) => {
                self.handle_verdict(head.item_id, &v);
                self.sched.st.queue.retain(|e| e.id != head.id);
                self.dirty = true;
                Gate::Dropped
            }
            None => {
                if let Some((p, n)) = self.media_path(head.item_id) {
                    self.fail.preflight.request(head.item_id, p, n, now);
                }
                Gate::Wait
            }
        }
    }

    /// A check on the calling thread, for the moments nothing can wait (the first item after silence,
    /// the emergency deck).
    pub(crate) fn check_now(&mut self, item_id: i64) -> bool {
        let Some((p, n)) = self.media_path(item_id) else {
            return false;
        };
        let v = preflight::check(&p, n, &self.fail.preflight.faults);
        let now = self.wall_ms;
        self.fail
            .preflight
            .results
            .insert(item_id, (v.clone(), now));
        !self.handle_verdict(item_id, &v)
    }

    // ---- items that fail -----------------------------------------------------------------------------

    /// Record a failure of one item and do what 1.5 says: an alert, the error count, quarantine at the
    /// threshold. Never stops anything.
    pub fn item_failed(&mut self, item_id: i64, kind: FailKind, detail: &str) {
        let now = self.wall_ms;
        if self.cat.get(item_id).is_some_and(|e| e.row.quarantined) {
            // Its file is judged already (a queued copy's pre-flight found the cut, and now the copy on air
            // runs into it): one fault of one file is one error, and the quarantine stands.
            tracing::info!(item_id, ?kind, "already quarantined: {detail}");
            return;
        }
        let title = self
            .cat
            .meta(item_id)
            .map(|m| m.title.clone())
            .unwrap_or_default();
        let (alert, reason, threshold) = match kind {
            FailKind::Missing => (code::ITEM_MISSING, "missing", 1),
            FailKind::Truncated | FailKind::TruncatedOnAir => {
                (code::ITEM_TRUNCATED, "truncated", 1)
            }
            FailKind::Mismatch => (code::ITEM_TRUNCATED, "hash_mismatch", 1),
            FailKind::Decode => (code::ITEM_DECODE_FAILED, "decode", DECODE_QUARANTINE_AT),
            FailKind::Silent => (code::ITEM_SILENT, "silent", SILENT_QUARANTINE_AT),
        };
        self.fail.alerts.raise(
            alert,
            &item_id.to_string(),
            Severity::Warning,
            format!("`{title}` (item {item_id}): {detail}"),
            Some(item_id),
            now,
        );
        let sha = self
            .cat
            .get(item_id)
            .map(|e| e.row.sha256.clone())
            .unwrap_or_default();
        let n = {
            let c = self
                .fail
                .errors
                .entry((item_id, reason.to_string()))
                .or_insert(0);
            *c += 1;
            *c
        };
        if let Some(w) = &self.state {
            w.send(Msg::ItemErrors {
                item_id,
                kind: reason.into(),
                sha256: sha.clone(),
                count: n,
                at_ms: now,
            });
            w.send(Msg::Library(LibOp::AddErrors {
                item_id,
                n: 1,
                sha256: sha,
            }));
        }
        if n >= threshold {
            self.quarantine_item(item_id, reason);
        }
    }

    /// Quarantine an item on the station's authority (1.5): never picked again, dropped from the queue
    /// at the next step, `item_quarantined` raised once, written to the library as soon as it can be.
    pub fn quarantine_item(&mut self, item_id: i64, reason: &str) {
        let now = self.wall_ms;
        if !self.cat.meta(item_id).is_some_and(|m| m.playable) {
            return;
        }
        if !self.cat.force_quarantine(item_id, reason) {
            return;
        }
        let sha = self
            .cat
            .get(item_id)
            .map(|e| e.row.sha256.clone())
            .unwrap_or_default();
        let title = self
            .cat
            .meta(item_id)
            .map(|m| m.title.clone())
            .unwrap_or_default();
        if let Some(w) = &self.state {
            w.send(Msg::Quarantine {
                item_id,
                reason: reason.into(),
                sha256: sha.clone(),
                at_ms: now,
            });
            w.send(Msg::Library(LibOp::Quarantine {
                item_id,
                reason: reason.into(),
                sha256: sha,
            }));
        }
        self.fail.alerts.raise(
            code::ITEM_QUARANTINED,
            &item_id.to_string(),
            Severity::Warning,
            format!("`{title}` (item {item_id}) is quarantined: {reason}"),
            Some(item_id),
            now,
        );
        self.fail.to_drop.push(item_id);
        self.dirty = true;
    }

    /// The failures counted against an item's current file, by kind (`silent`, `decode`, ...).
    pub fn item_errors(&self, item_id: i64) -> Vec<(String, i64)> {
        let mut v: Vec<(String, i64)> = self
            .fail
            .errors
            .iter()
            .filter(|((i, _), _)| *i == item_id)
            .map(|((_, k), n)| (k.clone(), *n))
            .collect();
        v.sort();
        v
    }

    /// The operator's "Release from quarantine" (the library service does the same to `library.db`).
    pub fn release_item(&mut self, item_id: i64) {
        let now = self.wall_ms;
        let was = self.cat.unforce(item_id);
        self.forget_item_failures(item_id);
        if let Some(w) = &self.state {
            w.send(Msg::Library(LibOp::Release { item_id }));
        }
        if was {
            tracing::info!(item_id, "released from quarantine");
        }
        let _ = now;
    }

    /// A quarantine ended (released in the library, or the file was replaced): forget what was counted.
    pub(crate) fn forget_item_failures(&mut self, item_id: i64) {
        let now = self.wall_ms;
        self.fail.errors.retain(|(i, _), _| *i != item_id);
        self.fail.preflight.forget(item_id);
        if let Some(w) = &self.state {
            w.send(Msg::QuarantineDrop { item_id });
            w.send(Msg::ItemErrorsClear { item_id });
        }
        let key = item_id.to_string();
        for c in [
            code::ITEM_QUARANTINED,
            code::ITEM_MISSING,
            code::ITEM_TRUNCATED,
            code::ITEM_SILENT,
            code::ITEM_DECODE_FAILED,
        ] {
            self.fail.alerts.clear(c, &key, now);
        }
    }

    /// Take quarantined items out of the queue (and take back a boundary committed for one, when it is
    /// not too late), so replacements are picked through the normal rules.
    fn drop_quarantined_from_queue(&mut self, send: &mut dyn FnMut(Command)) {
        if self.fail.to_drop.is_empty() {
            return;
        }
        let ids = std::mem::take(&mut self.fail.to_drop);
        let mut retry = Vec::new();
        for item_id in ids {
            let entries: Vec<(i64, bool)> = self
                .sched
                .st
                .queue
                .iter()
                .filter(|e| e.item_id == item_id)
                .map(|e| (e.id, self.pending == Some(e.id)))
                .collect();
            for (entry_id, is_pending) in entries {
                if is_pending {
                    self.undo_pending(send);
                    if self.pending == Some(entry_id) {
                        // Too late to take back: it starts (or fails to open) and the queue moves on.
                        retry.push(item_id);
                        continue;
                    }
                }
                let t = crate::runtime::RuntimeTiming {
                    cat: &self.cat,
                    presets: &self.presets,
                    preset: &self.opts.preset,
                    rate: self.opts.rate,
                };
                let _ = self.sched.remove(entry_id, self.wall_ms, &self.cat, &t);
            }
        }
        self.fail.to_drop = retry;
        self.dirty = true;
    }

    /// The committed item's decoder failed before it produced a sample and the item has not started: cancel
    /// it in the mixer, take the boundary back so the item before it keeps its own end, drop the entry, and
    /// quarantine the item. Nothing is heard (the boundary is committed ~10 s ahead; the decoder fails at once).
    fn handle_unstarted_failures(&mut self, send: &mut dyn FnMut(Command)) {
        let Some(last) = self.cond.placed.last() else {
            return;
        };
        if last.ended
            || last.start_frame <= self.now_frame
            || !(last.status.failed.load(Relaxed) && last.status.total() == Some(0))
        {
            return;
        }
        let key = (last.id.slot, last.id.gen);
        let entry = self.voices.get(&key).map(|t| t.entry.clone());
        if self.cond.uncommit_any(self.now_frame, send).is_none() {
            return;
        }
        self.voices.remove(&key);
        let Some(e) = entry else { return };
        if self.pending == Some(e.id) {
            self.pending = None;
        }
        self.sched.st.queue.retain(|q| q.id != e.id);
        if self
            .fail
            .alt_entry
            .as_ref()
            .is_some_and(|(_, a)| a.id == e.id)
        {
            self.fail.alt_entry = None;
        }
        self.dirty = true;
        self.judge_unopened(e.item_id);
    }

    /// A committed item's decoder failed before it produced a sample. Whose fault? The file is read again
    /// (pre-flight): gone, cut or changed is the item's (as at pre-flight); unreadable is the store's (a disk
    /// or share going away must not quarantine the library item by item); readable but undecodable is the
    /// item's, quarantined at once (`decode`).
    fn judge_unopened(&mut self, item_id: i64) {
        let v = match self.media_path(item_id) {
            Some((p, n)) => preflight::check(&p, n, &self.fail.preflight.faults),
            None => return,
        };
        if v.is_ok() {
            self.item_failed(
                item_id,
                FailKind::Decode,
                "the file could not be opened or decoded",
            );
            self.quarantine_item(item_id, "decode");
        } else {
            self.handle_verdict(item_id, &v);
        }
    }

    /// A committed item's file would not open or decode: nothing was heard of it. The boundary is taken
    /// back (the item before it gets its own end again), the item is quarantined at once, and the next
    /// boundary picks a replacement.
    fn handle_load_failures(&mut self, send: &mut dyn FnMut(Command)) {
        if self.fail.load_failed.is_empty() {
            return;
        }
        for (id, item_id) in std::mem::take(&mut self.fail.load_failed) {
            self.cond.drop_failed(id, send);
            self.judge_unopened(item_id);
        }
    }

    // ---- the early-end watch: fatal decode errors and truncated files (1.5) ---------------------------

    fn watch_voices(&mut self, send: &mut dyn FnMut(Command)) {
        let rate = self.opts.rate;
        let horizon =
            self.now_frame + self.opts.lookahead_frames + crate::runtime::lead_frames(rate);
        let mut found: Option<(usize, u64, FailKind, String)> = None;
        for (i, p) in self.cond.placed.iter().enumerate() {
            if p.ended || p.start_frame > horizon {
                continue;
            }
            let key = (p.id.slot, p.id.gen);
            if self.fail.drained.contains(&key) {
                continue;
            }
            let Some(d) = Conductor::drain_frame(p) else {
                continue;
            };
            if p.status.total() == Some(0) {
                continue; // it never produced a sample: the load-failure path handles it
            }
            let from_frames = ms_frames(p.bound_in_ms, rate).max(0) as u64;
            let expected_end = if p.out_is_file_end {
                p.start_frame
                    + p.status
                        .expected()
                        .map_or(d.saturating_sub(p.start_frame), |e| {
                            e.saturating_sub(from_frames)
                        })
            } else {
                p.start_frame + ms_frames(p.bound_out_ms - p.bound_in_ms, rate).max(0) as u64
            };
            let slack = s_frames(EARLY_END_S, rate).max(0) as u64;
            let truncated = p.status.truncated.load(Relaxed);
            let failed = p.status.failed.load(Relaxed);
            // The decoder's own verdicts (`truncated`: short of a declared exact length; `failed`: a fatal
            // decode error) always count. The length rule counts only for a voice bounded by markers: for
            // one that plays to its file end the declared length of an MP3 or AAC is a hint, not a fact.
            let early_by_length = !p.out_is_file_end && d + slack < expected_end;
            if !(truncated || failed || early_by_length) {
                continue;
            }
            let err = p
                .status
                .error
                .lock()
                .ok()
                .and_then(|g| g.clone())
                .unwrap_or_default();
            let kind = if failed && !truncated {
                FailKind::Decode
            } else {
                FailKind::TruncatedOnAir
            };
            let detail = if err.is_empty() {
                format!(
                    "the file ended {:.1} s before it should",
                    expected_end.saturating_sub(d) as f64 / f64::from(rate)
                )
            } else {
                err
            };
            found = Some((i, d, kind, detail));
            break;
        }
        // Voices that have drained without trouble need no more looking at.
        let ok: Vec<(u8, u32)> = self
            .cond
            .placed
            .iter()
            .filter(|p| !p.ended && p.start_frame <= horizon)
            .filter_map(|p| Conductor::drain_frame(p).map(|_| (p.id.slot, p.id.gen)))
            .collect();
        // Recoverable decode errors (a bad packet, padded with zeros for its duration) are counted and
        // reported at `info` (1.5); more than 20 in one item is fatal, which the decoder itself decides.
        let glitchy: Vec<(u8, u32, u32)> = self
            .cond
            .placed
            .iter()
            .filter(|p| !p.ended && p.status.glitches.load(Relaxed) > 0)
            .map(|p| (p.id.slot, p.id.gen, p.status.glitches.load(Relaxed)))
            .collect();
        for (slot, gen, n) in glitchy {
            if let Some(item_id) = self.voices.get(&(slot, gen)).map(|t| t.entry.item_id) {
                let now = self.wall_ms;
                self.fail.alerts.raise(
                    code::DECODE_GLITCH,
                    &item_id.to_string(),
                    Severity::Info,
                    format!("item {item_id}: {n} packet(s) failed to decode and were replaced by silence"),
                    Some(item_id),
                    now,
                );
            }
        }
        for k in ok {
            if found.as_ref().is_none_or(|(i, ..)| {
                (self.cond.placed[*i].id.slot, self.cond.placed[*i].id.gen) != k
            }) {
                self.fail.drained.insert(k);
            }
        }
        let Some((idx, drain, kind, detail)) = found else {
            if self.fail.drained.len() > 64 {
                let live: HashSet<(u8, u32)> = self
                    .cond
                    .placed
                    .iter()
                    .map(|p| (p.id.slot, p.id.gen))
                    .collect();
                self.fail.drained.retain(|k| live.contains(k));
            }
            return;
        };
        let key = (self.cond.placed[idx].id.slot, self.cond.placed[idx].id.gen);
        self.fail.drained.insert(key);
        let Some(item_id) = self.voices.get(&key).map(|t| t.entry.item_id) else {
            return;
        };
        self.item_failed(item_id, kind, &detail);
        if let Some(t) = self.voices.get_mut(&key) {
            t.end_hint = "error";
        }
        self.emergency_transition(idx, drain, send);
    }

    /// The emergency transition (1.5): the next item starts 50 ms before A's audio runs out, and A gets a
    /// 30 ms fade. If a boundary out of A was committed already and would start too late, it is taken back
    /// first (when it is not too close to start).
    fn emergency_transition(&mut self, a_idx: usize, drain: u64, send: &mut dyn FnMut(Command)) {
        let last = self.cond.placed.len() - 1;
        if a_idx < last {
            let b_start = self.cond.placed[a_idx + 1].start_frame;
            if b_start <= drain + 2400 {
                return; // B starts before A's audio runs out anyway
            }
            if a_idx + 1 != last || self.pending.is_none() || self.pending_imminent() {
                return;
            }
            self.undo_pending(send);
            if self.pending.is_some() || a_idx + 1 != self.cond.placed.len() {
                return;
            }
        } else if self.pending.is_some() {
            return;
        }
        let Some(head) = self.peek_next() else {
            // Nothing follows: the item ends where its audio ends.
            return;
        };
        let Ok(item) = self.entry_item(&head) else {
            return;
        };
        let mut patch = parse_patch(&head.transition);
        let mut head = head;
        let item = self.prepare_item(&mut head, item, &mut patch);
        match self.cond.plan_next(
            &item,
            TransitionKind::Emergency { drain_frame: drain },
            patch.as_ref(),
            self.now_frame,
        ) {
            Ok((p, pa, pb)) => {
                if let Err(e) = self.commit(&head, item, &p, &pa, &pb, patch, "error", send) {
                    tracing::warn!("emergency transition failed: {e}");
                }
            }
            Err(e) => tracing::warn!("emergency transition cannot be planned: {e}"),
        }
    }

    // ---- starved voices (1.4, 1.5) ---------------------------------------------------------------------

    /// A voice's ring ran dry (the mixer's `Starved`): its decoder is 20 s behind, so the disk or the file is
    /// in trouble. The voice is zero-filled and plays the rest late; the station makes sure nothing overlaps
    /// its late tail, and gives up on it (the next item, now) if it does not recover.
    ///
    /// Public so a test can deliver the event: offline rendering waits for its decoders, so it never starves.
    pub fn on_starved(&mut self, id: engine::mixer::VoiceId, frame: u64) {
        let key = (id.slot, id.gen);
        self.fail.late.insert(key);
        let Some(item_id) = self.voices.get(&key).map(|t| t.entry.item_id) else {
            return;
        };
        let consumed = self
            .cond
            .placed
            .iter()
            .find(|p| p.id == id)
            .map_or(0, |p| p.status.consumed_frames.load(Relaxed));
        if self.fail.starving.is_none_or(|(k, ..)| k != key) {
            self.fail.starving = Some((key, frame, consumed));
        }
        let now = self.wall_ms;
        self.fail.alerts.raise(
            code::DECODE_SLOW,
            &item_id.to_string(),
            Severity::Warning,
            format!("item {item_id}: the decoder fell behind and the voice ran dry"),
            Some(item_id),
            now,
        );
    }

    fn starve_step(&mut self, send: &mut dyn FnMut(Command)) {
        // A late voice's successor must not sound over its tail (a continuity joint, or a plays-to-end tail).
        if !self.fail.late.is_empty() {
            let live: Vec<(engine::mixer::VoiceId, (u8, u32))> = self
                .cond
                .placed
                .iter()
                .filter(|p| !p.ended)
                .map(|p| (p.id, (p.id.slot, p.id.gen)))
                .collect();
            self.fail.late.retain(|k| live.iter().any(|(_, l)| l == k));
            for (id, k) in live {
                if self.fail.late.contains(&k) {
                    self.cond.guard_late(id, self.now_frame, send);
                }
            }
        }
        let Some((key, first, consumed0)) = self.fail.starving else {
            return;
        };
        let Some(p) = self
            .cond
            .placed
            .iter()
            .find(|p| (p.id.slot, p.id.gen) == key && !p.ended)
        else {
            self.fail.starving = None;
            return;
        };
        let elapsed = self.now_frame.saturating_sub(first);
        let progress = p
            .status
            .consumed_frames
            .load(Relaxed)
            .saturating_sub(consumed0);
        let give_up = s_frames(STARVE_GIVE_UP_MS as f64 / 1000.0, self.opts.rate).max(1) as u64;
        let Some(stuck) = starve_verdict(elapsed, progress, give_up) else {
            return;
        };
        let item_id = self.voices.get(&key).map(|t| t.entry.item_id);
        self.fail.starving = None;
        if !stuck {
            // It caught up (a moment's stall): the late tail is guarded, nothing else to do.
            if let Some(id) = item_id {
                let now = self.wall_ms;
                self.fail
                    .alerts
                    .clear(code::DECODE_SLOW, &id.to_string(), now);
            }
            return;
        }
        // Still dry, or trickling at under half speed: move on, as for a fatal decode error. Not the item's
        // fault as far as anyone can tell (a slow disk starves every voice), so it is not charged.
        let on_air = self.on_air_voice.map(|v| (v.slot, v.gen)) == Some(key);
        tracing::warn!(?item_id, on_air, "a voice stayed starved; moving on");
        if on_air {
            self.set_immediate(Immediate::Cut, true);
        }
    }

    // ---- program silence (1.5) ---------------------------------------------------------------------

    /// The silence window of the item on air, frames; `None` when the item is exempt (`silence_ok`).
    ///
    /// The profile's window (15 s; 60 s for an `ambient` category or inside a run), or the item's own longest
    /// quiet stretch plus [`QUIET_MARGIN_MS`] when that is longer: a piece that is quiet on purpose is judged
    /// on its own content, with nothing set by hand. The stretch is read from the analysis at the level the
    /// detector will see (its threshold minus the gain the station gives the item) over the part of the file
    /// the voice plays (the cue window, or the whole file for a `bounds = file` preset).
    pub(crate) fn silence_window(&self) -> Option<u64> {
        let rate = f64::from(self.opts.rate);
        let base = self.opts.silence_s.max(1.0);
        let frames = |ms: f64| (ms / 1000.0 * rate) as u64;
        let Some(e) = self.now_entry() else {
            return Some(frames(base * 1000.0));
        };
        let Some(t) = self.cat.get(e.item_id) else {
            return Some(frames(base * 1000.0));
        };
        if t.row.silence_ok {
            return None;
        }
        let ambient = t.row.kind == "ambient" || t.row.category == "ambient";
        let profile_s = if ambient || e.run_id.is_some() {
            60.0f64.max(base)
        } else {
            base
        };
        let own_ms = self.quiet_ms_of(e).map_or(0, |q| q + QUIET_MARGIN_MS);
        Some(frames((profile_s * 1000.0).max(own_ms as f64)))
    }

    /// The longest quiet stretch of the entry's item as the detector will hear it, ms (`None` without a
    /// profile: an item analysed before the library measured it).
    pub(crate) fn quiet_ms_of(&self, e: &QEntry) -> Option<i64> {
        let item = self.entry_item(e).ok()?;
        let q = item.quiet.as_ref()?;
        let run = e
            .run_id
            .and_then(|r| self.run_gains.get(&r).copied().flatten());
        let gain = crate::item::gain_db(
            &item,
            self.opts.target_lufs,
            self.opts.max_boost_db,
            self.opts.peak_allowance_db,
            run,
        );
        // The part of the file the voice plays: past the cue window when its bounds say so (and when there
        // is no voice to ask, the whole file: never less than the truth).
        let whole = self
            .on_air_voice
            .and_then(|id| self.cond.placed.iter().find(|p| p.id == id))
            .is_none_or(|p| {
                p.bound_in_ms < item.markers.cue_in || p.bound_out_ms > item.markers.cue_out
            });
        Some(q.longest_below(self.opts.silence_dbfs - gain + QUIET_SLACK_DB, whole))
    }

    /// The item whose voice is on air now (not one that has ended and left the station with nothing to
    /// play: that dead air is not the item's fault).
    fn sounding_item(&self) -> Option<i64> {
        self.on_air_start_frame()?;
        self.now_entry().map(|e| e.item_id)
    }

    /// The frame the on-air voice started at, when it has.
    fn on_air_start_frame(&self) -> Option<u64> {
        let id = self.on_air_voice?;
        let p = self.cond.placed.iter().find(|p| p.id == id)?;
        (p.start_frame <= self.now_frame).then_some(p.start_frame)
    }

    /// Ask for a change of programme now. `targeted`: it is about the entry on air (a silent item, an
    /// outage), and is dropped if another entry is on air before it can be applied.
    pub(crate) fn set_immediate(&mut self, im: Immediate, targeted: bool) {
        self.fail.immediate = Some(im);
        self.fail.immediate_target = if targeted {
            self.now_entry().map(|e| e.id)
        } else {
            None
        };
    }

    fn silence_step(&mut self) {
        let rate = u64::from(self.opts.rate);
        let st = self.cond.stats();
        // Playing and not waiting on purpose (assist, or a hold, whose item has had its end): the output
        // should sound. That includes a station with no voice at all (nothing could be started, the queue
        // ran dry): dead air is dead air, and it is exactly what the escalation is for.
        let expect_sound =
            self.transport == Transport::Playing && !(self.assist && self.finished_last);
        let window = self.silence_window();
        if !expect_sound || window.is_none() {
            self.fail.armed_frame = self.now_frame;
            self.fail.audible_since_ms = None;
            if self.fail.silence_stage > 0 && !expect_sound {
                self.fail.silence_stage = 0;
            }
            return;
        }
        let window = window.unwrap_or(u64::MAX);
        let gap = (self.opts.silence_s.max(1.0) * rate as f64) as u64;
        let stage = self.fail.silence_stage;
        let since = self.now_frame.saturating_sub(self.fail.armed_frame);
        let run = st.silent_run_frames.min(since);
        let now = self.wall_ms;
        if st.silent_run_frames == 0 {
            let s = *self.fail.audible_since_ms.get_or_insert(now);
            if now - s >= 2000
                && (self.fail.silence_stage > 0 || self.fail.alerts.is_active(code::SILENCE))
            {
                self.fail.silence_stage = 0;
                self.fail.alerts.clear(code::SILENCE, "", now);
            }
            return;
        }
        self.fail.audible_since_ms = None;
        // The skip judges the item on air by its own window, so only the part of the silence inside it
        // counts: a quiet tail of the item before it (heard under a crossfade) is not this item's. The
        // escalation (emergency playlist, RAM loop) keeps counting the whole silence.
        let run_item = self
            .on_air_start_frame()
            .map_or(run, |f| run.min(self.now_frame.saturating_sub(f)));
        if stage < 1 && run_item >= window {
            self.fail.silence_stage = 1;
            self.fail.stage_run = run;
            let on_air = self.sounding_item();
            self.fail.alerts.raise(
                code::SILENCE,
                "",
                Severity::Warning,
                format!(
                    "the output has been silent for {:.0} s; skipping",
                    run as f64 / rate as f64
                ),
                on_air,
                now,
            );
            if let Some(id) = on_air {
                self.item_failed(id, FailKind::Silent, "silent on air");
            }
            self.set_immediate(Immediate::Cut, true);
        }
        // Each further stage needs the silence to go on for another `gap` after the one before (1.5: "if
        // silence continues another 15 s"), so a quiet tail followed by a quiet opening, neither of them
        // long enough to skip, can never jump straight to the emergency playlist.
        if run < self.fail.stage_run {
            // The output sounded in between (too briefly to clear the alert): count from this silence.
            self.fail.stage_run = 0;
        }
        // The item now on air must also have had its own window: a quiet opening after a skip is judged
        // like any other.
        if stage == 1 && run >= self.fail.stage_run + gap && run_item >= window {
            self.fail.silence_stage = 2;
            self.fail.alerts.raise(
                code::SILENCE,
                "",
                Severity::Critical,
                "still silent: switching to the emergency playlist",
                None,
                now,
            );
            if let Some(id) = self.sounding_item() {
                self.item_failed(id, FailKind::Silent, "silent on air");
            }
            let source = match self.fail.alt {
                Some(a) if a.source == AltSource::Emergency && self.fail.ram.is_some() => {
                    AltSource::Ram
                }
                Some(a) => a.source,
                None => AltSource::Emergency,
            };
            self.fail.stage_run = run;
            self.set_alt(source, AltWhy::Silence);
            self.set_immediate(Immediate::Cut, true);
        }
        if stage == 2 && run >= self.fail.stage_run + gap && run_item >= window {
            self.fail.silence_stage = 3;
            self.fail.stage_run = run;
            if self.fail.ram.is_some() && self.fail.alt.is_none_or(|a| a.source != AltSource::Ram) {
                self.set_alt(AltSource::Ram, AltWhy::Silence);
                self.set_immediate(Immediate::Cut, true);
            }
        }
    }

    // ---- alternative programming: the emergency deck and the RAM loop ------------------------------------

    /// Start using the emergency playlist or the RAM loop for `why`.
    pub(crate) fn set_alt(&mut self, source: AltSource, why: AltWhy) {
        let now = self.wall_ms;
        let hold = if why == AltWhy::Safe {
            i64::MAX
        } else {
            now.saturating_add(ALT_HOLD_MS)
        };
        if source == AltSource::Ram && self.fail.ram.is_none() {
            // Nothing in memory to play (still loading, or nothing to load): stay on the emergency deck.
            if self.fail.alt.is_some() {
                return;
            }
            self.fail.alt = Some(AltMode {
                source: AltSource::Emergency,
                why,
                since_ms: now,
                hold_until_ms: hold,
            });
            self.fail.alt_entry = None;
            return;
        }
        let changed = self
            .fail
            .alt
            .is_none_or(|a| a.source != source || a.why != why);
        self.fail.alt = Some(AltMode {
            source,
            why,
            since_ms: now,
            hold_until_ms: hold,
        });
        if changed {
            self.fail.alt_entry = None;
            self.fail.alt_ids = None;
        }
        tracing::warn!("programme source: {source:?} ({why:?})");
    }

    fn end_alt(&mut self) {
        if let Some(a) = self.fail.alt.take() {
            tracing::info!("back to normal programming ({:?} over)", a.why);
            if a.why == AltWhy::EmergencyFailed {
                let now = self.wall_ms;
                self.fail.alerts.clear(code::EMERGENCY_FAILED, "", now);
            }
        }
        self.fail.alt_entry = None;
        self.fail.alt_ids = None;
    }

    /// Playable, non-quarantined items of the emergency playlist, in order.
    fn emergency_ids(&self) -> Vec<i64> {
        let Some(name) = self.opts.emergency_playlist.as_deref() else {
            return Vec::new();
        };
        let Some(pl) = self.sched.prog.by_name(name) else {
            return Vec::new();
        };
        let mut ids = Vec::new();
        for e in &pl.entries {
            match e {
                Entry::Item { item_id, .. } => {
                    if self.cat.meta(*item_id).is_some_and(|m| m.playable) {
                        ids.push(*item_id);
                    }
                }
                Entry::Album { album_key, .. } => {
                    for t in self.cat.album_tracks(album_key) {
                        ids.push(t.meta.item_id);
                    }
                }
                Entry::Block(_) => {}
            }
        }
        ids
    }

    fn alt_qentry(&mut self, item_id: i64, ram: bool) -> QEntry {
        let id = self.fail.next_alt_id;
        self.fail.next_alt_id -= 1;
        if self.fail.next_alt_id < -1_000_000_000 {
            self.fail.next_alt_id = -1000;
        }
        let mut e = QEntry::new(id, item_id, Origin::Emergency);
        e.committed = true;
        e.planned_start_ms = Some(self.wall_ms);
        e.flags = if ram { vec!["ram_loop".into()] } else { vec![] };
        e
    }

    /// The next entry of the emergency deck: the next playable item whose file reads. `None` when no item
    /// can be played (the caller goes to the RAM loop).
    fn emergency_pick(&mut self) -> Option<QEntry> {
        let mut ids = match self.fail.alt_ids.take() {
            Some(v) => v,
            None => self.emergency_ids(),
        };
        let mut tries = 0;
        let mut found = None;
        while !ids.is_empty() && tries < ids.len() + 1 {
            tries += 1;
            if self.fail.alt_cursor >= ids.len() {
                self.fail.alt_cursor = 0;
            }
            let id = ids[self.fail.alt_cursor];
            if !self.cat.meta(id).is_some_and(|m| m.playable) || !self.check_now(id) {
                ids.retain(|i| *i != id);
                continue;
            }
            self.fail.alt_cursor += 1;
            found = Some(id);
            break;
        }
        self.fail.alt_ids = Some(ids);
        found.map(|id| self.alt_qentry(id, false))
    }

    fn ram_pick(&mut self) -> Option<QEntry> {
        let n = self.fail.ram.as_ref()?.items.len();
        if n == 0 {
            return None;
        }
        let i = self.fail.ram_cursor % n;
        self.fail.ram_cursor = (i + 1) % n;
        let id = self.fail.ram.as_ref()?.items[i].source_item_id;
        Some(self.alt_qentry(id, true))
    }

    /// Is the active playlist short of playable items because of quarantine (1.5, `playlist_depleted`)?
    fn depleted(&self) -> bool {
        let Some(pl) = self.sched.program_playlist() else {
            return false;
        };
        let (mut total, mut playable) = (0usize, 0usize);
        for e in &pl.entries {
            match e {
                Entry::Item { item_id, .. } => {
                    total += 1;
                    if self.cat.meta(*item_id).is_some_and(|m| m.playable) {
                        playable += 1;
                    }
                }
                Entry::Album { album_key, .. } => {
                    total += 1;
                    if !self.cat.album_tracks(album_key).is_empty() {
                        playable += 1;
                    }
                }
                Entry::Block(_) => {}
            }
        }
        playable < 2 && total > playable
    }

    fn depletion_step(&mut self) {
        let now = self.wall_ms;
        // stopped on purpose, or a program's planned silence: a starvation alert from before is over
        if self.fail.alerts.is_active(code::QUEUE_STARVED) && self.silence_is_planned() {
            self.fail.alerts.clear(code::QUEUE_STARVED, "", now);
        }
        if self.depleted() {
            let name = self
                .sched
                .program_playlist()
                .map(|p| p.name.clone())
                .unwrap_or_default();
            self.fail.alerts.raise(
                code::PLAYLIST_DEPLETED,
                "",
                Severity::Warning,
                format!("`{name}` has fewer than 2 playable items; playing the emergency playlist"),
                None,
                now,
            );
        } else if self.fail.alerts.is_active(code::PLAYLIST_DEPLETED) {
            self.fail.alerts.clear(code::PLAYLIST_DEPLETED, "", now);
        }
    }

    /// An empty queue that is meant: the station is stopped or held on purpose (by the operator or a program's
    /// stop or fade out), a stop is on its way, a program start is about to deal its first item, the program waits
    /// for a time or the operator, or the station is one where stopped is normal. None of these is starvation:
    /// no `queue_starved` alert, no emergency playlist.
    pub(crate) fn silence_is_planned(&self) -> bool {
        self.opts.stopped_is_normal
            || self.transport != crate::runtime::Transport::Playing
            || self.armed_stop.is_some()
            || self.program_start_at.is_some()
            || self
                .sched
                .st
                .program
                .as_ref()
                .is_some_and(|p| p.halted || p.silenced || p.hold.is_some() || p.wait_since.is_some())
            // the program dealt up to a timed block of its own (a fade out at T0 + 14:40): the queue runs out
            // before it on purpose
            || (self.sched.st.program.is_some()
                && self
                    .sched
                    .upcoming(self.wall_ms, 6)
                    .iter()
                    .any(|u| u.origin == "program" && u.target_ms > self.wall_ms))
    }

    /// The entry the next boundary should play: from the scheduler, or from the emergency deck or the
    /// RAM loop when the station is not in normal programming.
    pub(crate) fn peek_next(&mut self) -> Option<QEntry> {
        let now = self.wall_ms;
        if let Some(a) = self.fail.alt {
            let over = match a.why {
                AltWhy::Silence | AltWhy::EmergencyFailed => now >= a.hold_until_ms,
                AltWhy::Store => !self.fail.store_failing,
                AltWhy::Safe => false,
            };
            if over {
                self.end_alt();
            }
        }
        if self.fail.safe_mode && self.fail.alt.is_none() {
            // A silence escalation or a failing store took over from safe mode for a while; when it is
            // over the station is still in safe mode (only an operator's `ack` ends it).
            self.set_alt(AltSource::Emergency, AltWhy::Safe);
        }
        let wanted = match self.fail.alt {
            Some(a) => Some(a.source),
            None => {
                if self.depleted() {
                    self.fail.transient = Some("playlist_depleted");
                    Some(AltSource::Emergency)
                } else if self.sched.head().is_none()
                    && self.opts.emergency_playlist.is_some()
                    && !self.silence_is_planned()
                {
                    self.fail.transient = Some("queue_starved");
                    Some(AltSource::Emergency)
                } else {
                    self.fail.transient = None;
                    None
                }
            }
        };
        if self.fail.alerts.is_active(code::QUEUE_STARVED)
            && (self.sched.head().is_some() || self.silence_is_planned())
        {
            self.fail.alerts.clear(code::QUEUE_STARVED, "", now);
        }
        let Some(mut source) = wanted else {
            return self.sched.head().cloned();
        };
        if self.fail.transient == Some("queue_starved") && self.fail.alt.is_none() {
            self.fail.alerts.raise(
                code::QUEUE_STARVED,
                "",
                Severity::Critical,
                "the queue is empty; playing the emergency playlist",
                None,
                now,
            );
        }
        if let Some((s, e)) = &self.fail.alt_entry {
            if *s == source {
                return Some(e.clone());
            }
        }
        self.fail.alt_entry = None;
        let mut entry = match source {
            AltSource::Emergency => self.emergency_pick(),
            AltSource::Ram => self.ram_pick(),
        };
        if entry.is_none() && source == AltSource::Emergency && self.fail.ram.is_some() {
            // The emergency playlist cannot be played: the RAM loop takes over at once (1.5).
            self.fail.alerts.raise(
                code::EMERGENCY_FAILED,
                "",
                Severity::Critical,
                "the emergency playlist cannot be read; playing the RAM loop",
                None,
                now,
            );
            self.set_alt(AltSource::Ram, AltWhy::EmergencyFailed);
            source = AltSource::Ram;
            entry = self.ram_pick();
        }
        let Some(e) = entry else {
            // Neither the emergency playlist nor a RAM loop can play: the scheduler's entry is better than
            // silence (a station in safe mode with no emergency playlist still plays its programming).
            return self.sched.head().cloned();
        };
        self.fail.alt_entry = Some((source, e.clone()));
        Some(e)
    }

    /// The item an entry plays: a disk item, or the RAM copy of one.
    pub(crate) fn entry_item(&self, e: &QEntry) -> Result<Arc<Item>, String> {
        if e.flags.iter().any(|f| f == "ram_loop") {
            return self
                .fail
                .ram
                .as_ref()
                .and_then(|r| r.items.iter().find(|i| i.source_item_id == e.item_id))
                .map(|i| i.item.clone())
                .ok_or_else(|| "the RAM loop item is gone".to_string());
        }
        self.item_of(e.item_id)
    }

    /// Markers that cannot be trusted (`cue_in < segue <= cue_out` fails, or they lie outside the file)
    /// play as the whole file for this play (1.5: never skipped for this reason).
    pub(crate) fn prepare_item(
        &mut self,
        head: &mut QEntry,
        item: Arc<Item>,
        patch: &mut Option<PresetPatch>,
    ) -> Arc<Item> {
        if markers_ok(&item) {
            return item;
        }
        let now = self.wall_ms;
        self.fail.alerts.raise(
            code::ITEM_MARKER_INVALID,
            &head.item_id.to_string(),
            Severity::Warning,
            format!(
                "`{}` has markers that make no sense ({}..{} ms, segue {}); playing the whole file",
                item.name, item.markers.cue_in, item.markers.cue_out, item.markers.segue
            ),
            Some(head.item_id),
            now,
        );
        if !head.flags.iter().any(|f| f == "marker_invalid") {
            head.flags.push("marker_invalid".into());
        }
        let mut it = (*item).clone();
        it.manual_cue_in = false;
        it.manual_cue_out = false;
        it.manual_segue = false;
        it.markers = crate::item::Markers {
            cue_in: 0,
            attack: 0,
            intro: None,
            segue: it.file_ms.max(1),
            fade_out: None,
            cue_out: it.file_ms.max(1),
        };
        let mut p = patch.take().unwrap_or_default();
        let mut obj = match p.patch.take() {
            Some(serde_json::Value::Object(o)) => o,
            _ => serde_json::Map::new(),
        };
        obj.insert("bounds".into(), serde_json::json!("file"));
        p.patch = Some(serde_json::Value::Object(obj));
        *patch = Some(p);
        Arc::new(it)
    }

    // ---- what changes the programme at once -----------------------------------------------------------------

    /// Take an immediate change of programme (silence, a failing store, safe mode, a long device outage)
    /// to the boundary logic. Returns true when it handled the step.
    pub(crate) fn apply_immediate(
        &mut self,
        send: &mut dyn FnMut(Command),
    ) -> Result<bool, String> {
        let Some(im) = self.fail.immediate else {
            return Ok(false);
        };
        if self.transport != Transport::Playing {
            self.fail.immediate = None;
            return Ok(false);
        }
        if let Some(t) = self.fail.immediate_target {
            if self.now_entry().map(|e| e.id) != Some(t) {
                // Another entry went on air first (an operator's `next`, or a boundary that was too close to
                // take back): the change was about the old one and must not cut the new one.
                self.fail.immediate = None;
                self.fail.immediate_target = None;
                return Ok(false);
            }
        }
        if self.pending.is_some() {
            if self.pending_imminent() {
                return Ok(false); // let it start; try again
            }
            self.undo_pending(send);
            if self.pending.is_some() {
                return Ok(false);
            }
        }
        let Some(head) = self.peek_next() else {
            self.fail.immediate = None;
            return Ok(false);
        };
        // A queued item is read again first (1.5: pre-flight at commit); the change waits a step for it.
        if head.id >= -1 {
            match self.gate(&head) {
                Gate::Go => {}
                Gate::Wait | Gate::Dropped => return Ok(true),
            }
        }
        self.fail.immediate = None;
        if im == Immediate::Device && self.fail.device_lost_at.is_some() {
            // Prepared while the card is still away (1.5: an outage of 30 s or more moves on): the timeline
            // stands still, so a planned cut would let the old item sound for the cut's lead after the card
            // returns. Instead everything placed ends a few milliseconds into the paused timeline (inside
            // the engine's resume fade-in, so nothing of it is heard) and the next item starts from silence.
            for t in self.voices.values_mut() {
                if t.kind != Kind::Cart {
                    t.end_hint = "device";
                }
            }
            let at = self.now_frame + 2 * self.opts.lookahead_frames.max(1);
            self.cond.stop_all(at, 0.003, send);
            self.try_start_head(head, send);
            return Ok(true);
        }
        if self.cond.live_voices() == 0 {
            self.try_start_head(head, send);
            return Ok(true);
        }
        let (kind, hint) = match im {
            Immediate::Cut => (TransitionKind::Cut, "cut"),
            Immediate::Device => (TransitionKind::Cut, "device"),
            Immediate::Fade(s) => (TransitionKind::FadeNext { fade_s: s }, "fade"),
        };
        self.manual_transition_with(head, kind, hint, send)?;
        Ok(true)
    }

    // ---- the media store (1.5) ------------------------------------------------------------------------------

    fn store_failure(&mut self, item_id: i64, detail: &str) {
        let now = self.wall_ms;
        if !self.fail.store_failures.iter().any(|(_, i)| *i == item_id) {
            tracing::warn!(item_id, "read failed: {detail}");
        }
        self.fail.store_failures.push_back((now, item_id));
    }

    fn store_step(&mut self) {
        let now = self.wall_ms;
        while self
            .fail
            .store_failures
            .front()
            .is_some_and(|(t, _)| now - t > STORE_WINDOW_MS)
        {
            self.fail.store_failures.pop_front();
        }
        if !self.fail.store_failing {
            let distinct: HashSet<i64> = self.fail.store_failures.iter().map(|(_, i)| *i).collect();
            if distinct.len() >= STORE_FAIL_ITEMS {
                self.fail.store_failing = true;
                self.fail.store_probe_ms = now + STORE_PROBE_MS;
                self.fail.alerts.raise(
                    code::MEDIA_STORE_FAILING,
                    "",
                    Severity::Critical,
                    format!(
                        "{} items could not be read; playing the RAM loop",
                        distinct.len()
                    ),
                    None,
                    now,
                );
                if self.fail.ram.is_some() {
                    self.set_alt(AltSource::Ram, AltWhy::Store);
                    self.set_immediate(Immediate::Fade(0.5), false);
                }
            }
            return;
        }
        // Failing: every 60 s read a few queued items again; if all read, the store is back.
        if !self.fail.probe_ids.is_empty() {
            let ids = self.fail.probe_ids.clone();
            let results: Vec<Option<bool>> = ids
                .iter()
                .map(|i| {
                    self.fail
                        .preflight
                        .results
                        .get(i)
                        .filter(|(_, at)| *at >= self.fail.store_probe_ms - STORE_PROBE_MS)
                        .map(|(v, _)| v.is_ok())
                })
                .collect();
            if results.iter().all(Option::is_some) {
                self.fail.probe_ids.clear();
                if results.iter().all(|r| *r == Some(true)) {
                    self.fail.store_failing = false;
                    self.fail.store_failures.clear();
                    self.fail.alerts.clear(code::MEDIA_STORE_FAILING, "", now);
                    tracing::info!("the media store reads again");
                }
            }
            return;
        }
        if now >= self.fail.store_probe_ms {
            let ids: Vec<i64> = self
                .cat
                .ids()
                .into_iter()
                .filter(|i| self.cat.meta(*i).is_some_and(|m| m.playable))
                .take(3)
                .collect();
            self.fail.store_probe_ms = now + STORE_PROBE_MS;
            for id in &ids {
                if let Some((p, n)) = self.media_path(*id) {
                    self.fail.preflight.results.remove(id);
                    self.fail.preflight.request(*id, p, n, now);
                }
            }
            self.fail.probe_ids = ids;
        }
    }

    // ---- the databases, the disk and the clock -----------------------------------------------------------------

    fn db_step(&mut self) {
        let now = self.wall_ms;
        let Some(w) = &self.state else { return };
        let failing = w.status.failing.load(Relaxed);
        let kind = w.status.failure_kind.load(Relaxed);
        if !failing || kind == FAIL_NONE {
            // Cleared only after writes have worked for a while: a database that fails every other write
            // (a lock contended on and off) must not raise and clear the alert on every retry, and its
            // 60 s to critical must not start over at each lucky write.
            let ok = *self.fail.db_ok_since.get_or_insert(now);
            if now - ok >= DB_CLEAR_AFTER_MS {
                self.fail.db_failing_since = None;
                for c in [code::DB_UNAVAILABLE, code::DISK_FULL] {
                    self.fail.alerts.clear(c, "", now);
                }
            }
            return;
        }
        self.fail.db_ok_since = None;
        let since = *self.fail.db_failing_since.get_or_insert(now);
        match kind {
            FAIL_FULL => {
                self.fail.alerts.raise(
                    code::DISK_FULL,
                    "",
                    Severity::Critical,
                    "state.db cannot be written: the disk is full; as-run rows are buffered",
                    None,
                    now,
                );
                self.fail.alerts.clear(code::DB_UNAVAILABLE, "", now);
            }
            FAIL_CORRUPT => {
                self.fail.alerts.raise(
                    code::STATE_DB_CORRUPT,
                    "",
                    Severity::Critical,
                    "state.db is corrupt; the station restarts at the next item to repair it",
                    None,
                    now,
                );
                self.fail.wants_restart = true;
            }
            _ => {
                self.fail.alerts.raise(
                    code::DB_UNAVAILABLE,
                    "",
                    Severity::Warning,
                    "state.db cannot be written (read-only or locked); writes are buffered",
                    None,
                    now,
                );
                if now - since >= DB_CRITICAL_MS {
                    self.fail.alerts.raise(
                        code::DB_UNAVAILABLE,
                        "",
                        Severity::Critical,
                        "",
                        None,
                        now,
                    );
                }
            }
        }
    }

    fn disk_step(&mut self) {
        let now = self.wall_ms;
        if now.saturating_sub(self.fail.disk_checked_ms) < DISK_CHECK_MS {
            return;
        }
        self.fail.disk_checked_ms = now;
        let Some(dir) = self.fail.state_dir.clone() else {
            return;
        };
        let Ok(vfs) = rustix::fs::statvfs(&dir) else {
            return;
        };
        let free = vfs.f_bavail.saturating_mul(vfs.f_frsize);
        self.fail.disk_free_bytes = Some(free);
        if free < DISK_LOW_BYTES {
            self.fail.alerts.raise(
                code::DISK_LOW,
                "",
                Severity::Warning,
                format!("{:.1} GB free on the state disk", free as f64 / 1e9),
                None,
                now,
            );
        } else {
            self.fail.alerts.clear(code::DISK_LOW, "", now);
        }
    }

    /// The nightly snapshot of `state.db` (5.5): once per local day, from 02:30, on the writer's thread. The
    /// first step after a start past 02:30 takes one too, so a box that was off at night is covered. A
    /// snapshot that fails its check raises `snapshot_failed`; the next good one clears it.
    fn snapshot_step(&mut self) {
        let now = self.wall_ms;
        let Some(dir) = self.opts.backup_dir.clone() else {
            return;
        };
        let Some(w) = &self.state else { return };
        let (day, minute) = common::localtime::local_day_minute(now);
        if self.fail.snapshot_day.is_none() {
            // A restart after tonight's snapshot does not take another (a crash loop would take one per start).
            let done_today = w
                .status
                .snapshot
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|i| i.date == common::localtime::ymd(day)))
                .unwrap_or(false);
            if done_today {
                self.fail.snapshot_day = Some(day);
            }
        }
        if self.fail.snapshot_day != Some(day) && minute >= library::backup::AT_MINUTE {
            self.fail.snapshot_day = Some(day);
            w.send(crate::state::Msg::Snapshot {
                dir,
                name: format!("station-{}", self.opts.name),
                date: common::localtime::ymd(day),
                now_ms: now,
            });
        }
        let last = w.status.snapshot.lock().ok().and_then(|g| g.clone());
        match last {
            Some(i) if !i.ok() => {
                self.fail.alerts.raise(
                    code::BACKUP_CHECK_FAILED,
                    "",
                    Severity::Warning,
                    format!("the nightly state.db snapshot failed: {}", i.check),
                    None,
                    now,
                );
            }
            Some(_) => {
                self.fail.alerts.clear(code::BACKUP_CHECK_FAILED, "", now);
            }
            None => {}
        }
    }

    /// A step of wall time that the audio clock did not take is a clock jump (1.5). Audio is unaffected
    /// (it runs on frames); the scheduler recomputes its predictions from the new wall time at once.
    fn clock_step(&mut self) {
        let now = self.wall_ms;
        let frame = self.now_frame;
        let (lw, lf) = (self.fail.last_wall_ms, self.fail.last_frame);
        self.fail.last_wall_ms = now;
        self.fail.last_frame = frame;
        if lw == i64::MIN || self.fail.device_lost_at.is_some() {
            return;
        }
        let d_wall = now - lw;
        let d_audio = (frame.saturating_sub(lf) as i64 * 1000) / i64::from(self.opts.rate);
        let jump = d_wall - d_audio;
        if jump.abs() > CLOCK_JUMP_MS {
            // The scheduler moves its recorded wall times with the clock, fires what a forward step
            // skipped within five minutes once, now, and refires nothing after a step back (the block
            // log, 1.5).
            self.sched.on_clock_step(now - jump, now);
            self.fail.jump_total_ms += jump;
            self.fail.alerts.raise(
                code::CLOCK_JUMP,
                "",
                Severity::Warning,
                format!(
                    "the wall clock moved {:+.1} s relative to the audio clock",
                    jump as f64 / 1000.0
                ),
                None,
                now,
            );
            self.dirty = true;
        } else if self.fail.alerts.is_active(code::CLOCK_JUMP) && jump.abs() < 100 {
            // A quiet moment after the jump: the alert stays for a minute of audio, then goes.
            if let Some(a) = self
                .fail
                .alerts
                .active()
                .into_iter()
                .find(|a| a.code == code::CLOCK_JUMP)
            {
                if now - a.raised_ms > 60_000 {
                    self.fail.alerts.clear(code::CLOCK_JUMP, "", now);
                }
            }
        }
    }

    /// `restarted` clears by itself once the station has been on air and healthy for `restart_alert_clear_s`
    /// (default 600 s; 0 keeps it until an operator acknowledges it). "Healthy" is: no critical alert, not in
    /// safe mode, the card present. The time is counted in audio frames and starts again from zero whenever
    /// the station is not healthy. The as-run `restart` row and the alert's history stay.
    fn restart_alert_step(&mut self) {
        if !self.fail.alerts.is_active(code::RESTARTED) {
            self.fail.restart_healthy_from_frame = None;
            return;
        }
        let healthy = !self.fail.safe_mode
            && self.fail.device_lost_at.is_none()
            && self.fail.alerts.status() != HealthStatus::Critical;
        if !healthy {
            self.fail.restart_healthy_from_frame = None;
            return;
        }
        let from = *self
            .fail
            .restart_healthy_from_frame
            .get_or_insert(self.now_frame);
        let limit_s = self.opts.restart_alert_clear_s;
        if limit_s <= 0.0 {
            return;
        }
        let healthy_s = self.now_frame.saturating_sub(from) as f64 / f64::from(self.opts.rate);
        if healthy_s >= limit_s {
            let now = self.wall_ms;
            self.fail.alerts.clear_code(code::RESTARTED, now);
            self.fail.restart_healthy_from_frame = None;
        }
    }

    // ---- the RAM loop ------------------------------------------------------------------------------------

    /// Start decoding the emergency playlist into memory (1.5): done once, in the background.
    pub(crate) fn start_ram_loader(&mut self) {
        if self.fail.ram_slot.is_some() || self.fail.ram.is_some() {
            return;
        }
        let ids = self.emergency_ids();
        let items: Vec<(i64, Arc<Item>)> = ids
            .into_iter()
            .filter_map(|id| self.item_of(id).ok().map(|i| (id, i)))
            .collect();
        if items.is_empty() {
            self.fail.ram_error = Some("the station has no emergency playlist to load".into());
            return;
        }
        self.fail.ram_slot = Some(ram::spawn_load(
            items,
            self.opts.rate,
            self.opts.ram_loop_seconds,
        ));
    }

    fn poll_ram(&mut self) {
        let Some(slot) = self.fail.ram_slot.clone() else {
            return;
        };
        match slot.take() {
            None => {}
            Some(Ok(r)) => {
                for i in &r.items {
                    self.cond.ram.insert(i.item.path.clone(), i.clip.clone());
                }
                tracing::info!(
                    "RAM loop ready: {} items, {:.0} s",
                    r.items.len(),
                    r.seconds(self.opts.rate)
                );
                self.fail.ram = Some(r);
                self.fail.ram_slot = None;
            }
            Some(Err(e)) => {
                tracing::warn!("the RAM loop could not be loaded: {e}");
                self.fail.ram_error = Some(e);
                self.fail.ram_slot = None;
            }
        }
    }

    // ---- device loss (1.5) ---------------------------------------------------------------------------------

    /// The sound card went away: the station clock pauses (the engine stands still), voices keep their
    /// positions. The process stays up and feeds its watchdog.
    pub fn on_device_lost(&mut self) {
        let now = self.wall_ms;
        self.fail.device_lost_at = Some(now);
        self.fail.alerts.raise(
            code::DEVICE_LOST,
            "",
            Severity::Critical,
            "the sound card is gone; retrying every 2 s",
            None,
            now,
        );
        self.flush_alerts();
    }

    /// The card is back after `outage_ms`: under 30 s the same position resumes (the engine already did
    /// that, with its fade-in); after longer, the next item plays as if the current one had ended.
    pub fn on_device_restored(&mut self, outage_ms: u32) {
        let now = self.wall_ms;
        self.fail.device_lost_at = None;
        self.fail.last_wall_ms = i64::MIN;
        self.fail.alerts.clear(code::DEVICE_LOST, "", now);
        if outage_ms >= DEVICE_RESUME_MAX_MS
            && self.transport == Transport::Playing
            && !self.fail.device_armed
        {
            self.set_immediate(Immediate::Device, true);
        }
        self.fail.device_armed = false;
        self.flush_alerts();
    }

    /// rt-out stopped advancing (or started again): `engine_stall` (1.5). The runner also stops feeding
    /// the systemd watchdog while it lasts.
    pub fn on_engine_stall(&mut self, stalled: bool) {
        let now = self.wall_ms;
        if stalled {
            self.fail.alerts.raise(
                code::ENGINE_STALL,
                "",
                Severity::Critical,
                "the audio thread has stopped advancing; the watchdog is no longer fed",
                None,
                now,
            );
        } else {
            self.fail.alerts.clear(code::ENGINE_STALL, "", now);
        }
        self.flush_alerts();
    }

    /// The loudness controller's two alerts (6.1): `loudness_at_limit` (a warning) once the gain has sat at a
    /// range limit for 10 minutes, `loudness_frozen` (info) once it has been frozen for 30 minutes while the
    /// station was playing. Both clear with the condition.
    pub fn on_loudness(&mut self, at_limit_s: f64, frozen_playing_s: f64) {
        let now = self.wall_ms;
        let a = &mut self.fail.alerts;
        if at_limit_s >= 600.0 {
            a.raise(
                code::LOUDNESS_AT_LIMIT,
                "",
                Severity::Warning,
                "the loudness controller's gain has been at its limit for 10 minutes: the program is far from the target",
                None,
                now,
            );
        } else if at_limit_s == 0.0 {
            a.clear(code::LOUDNESS_AT_LIMIT, "", now);
        }
        if frozen_playing_s >= 1800.0 {
            a.raise(
                code::LOUDNESS_FROZEN,
                "",
                Severity::Info,
                "the loudness controller has been frozen for 30 minutes while the station was playing (too little program to measure)",
                None,
                now,
            );
        } else if frozen_playing_s == 0.0 {
            a.clear(code::LOUDNESS_FROZEN, "", now);
        }
        self.flush_alerts();
    }

    /// The network stream's condition (1.3a): three alerts that raise and clear with it. `stream_no_listener`
    /// and `stream_encoder_lag` are warnings. `stream_encoder_failed` is a warning while a sound card also
    /// carries the program, and **critical on a stream-only station** (no card): there the stream is the only
    /// output, so a failed encoder means the station is off the air (owner, 2026-10-01).
    pub fn on_stream(&mut self, s: &StreamStatus) {
        let now = self.wall_ms;
        let set =
            |alerts: &mut crate::alerts::Alerts, c: &str, sev: Severity, on: bool, msg: String| {
                if on {
                    alerts.raise(c, "", sev, msg, None, now);
                } else {
                    alerts.clear(c, "", now);
                }
            };
        let a = &mut self.fail.alerts;
        set(
            a,
            code::STREAM_NO_LISTENER,
            Severity::Warning,
            s.no_listener,
            format!(
                "nobody has been listening to the stream for {:.0} s",
                s.no_listener_s
            ),
        );
        set(
            a,
            code::STREAM_ENCODER_LAG,
            Severity::Warning,
            s.encoder_lag,
            format!("{} ms of audio are queued for the stream encoder", s.lag_ms),
        );
        let (sev, msg) = if s.stream_only {
            (
                Severity::Critical,
                "the stream encoder has failed and the stream is this station's only output: the station is off the air",
            )
        } else {
            (
                Severity::Warning,
                "the stream encoder has failed; the program still plays on the sound card",
            )
        };
        set(
            a,
            code::STREAM_ENCODER_FAILED,
            sev,
            s.encoder_failed,
            msg.to_string(),
        );
        self.flush_alerts();
    }

    /// The card recovered from an underrun (`xrun_burst`, 1.5): more than 5 in 10 minutes is a warning.
    pub fn on_xrun(&mut self) {
        let now = self.wall_ms;
        self.fail.xruns.push_back(now);
        while self.fail.xruns.front().is_some_and(|t| now - t > 600_000) {
            self.fail.xruns.pop_front();
        }
        if self.fail.xruns.len() > 5 {
            self.fail.alerts.raise(
                code::XRUN_BURST,
                "",
                Severity::Warning,
                format!("{} underruns in the last 10 minutes", self.fail.xruns.len()),
                None,
                now,
            );
        }
        self.flush_alerts();
    }

    /// A test hook: make `state.db` writes fail (read-only, full) or work again.
    pub fn set_state_fault(&self, mode: crate::state::FaultMode) {
        if let Some(w) = &self.state {
            w.set_fault(mode);
        }
    }

    /// Wait until the writer has tried everything sent so far (tests).
    pub fn flush_writer(&self) {
        if let Some(w) = &self.state {
            w.flush();
        }
    }

    /// The writer's status (buffered writes, whether it is failing), for tests and `/metrics`.
    pub fn writer_status(&self) -> Option<Arc<crate::state::WriterStatus>> {
        self.state.as_ref().map(|w| w.status.clone())
    }

    // ---- acknowledging ------------------------------------------------------------------------------------------

    /// An operator acknowledges an alert (`safe_mode`, `state_db_corrupt`, ...): the alert clears, and for
    /// `safe_mode` the station returns to normal programming.
    pub fn ack(&mut self, alert_code: &str) -> bool {
        let now = self.wall_ms;
        if alert_code == code::SAFE_MODE && self.fail.safe_mode {
            self.fail.safe_mode = false;
            if self.fail.alt.is_some_and(|a| a.why == AltWhy::Safe) {
                self.end_alt();
            }
            self.set_immediate(Immediate::Cut, false);
        }
        let n = self.fail.alerts.clear_code(alert_code, now);
        if alert_code == code::STATE_DB_CORRUPT {
            self.fail.wants_restart = false;
        }
        self.flush_alerts();
        n > 0
    }

    /// Enter safe mode (a crash loop, 1.5): ignore the schedule and programming, play the emergency
    /// playlist (or the RAM loop) until an operator acknowledges it.
    pub fn enter_safe_mode(&mut self) {
        let now = self.wall_ms;
        self.fail.safe_mode = true;
        self.set_alt(AltSource::Emergency, AltWhy::Safe);
        self.fail.alerts.raise(
            code::SAFE_MODE,
            "",
            Severity::Critical,
            "the station restarted too often; safe mode: playing the emergency playlist until acknowledged",
            None,
            now,
        );
        self.flush_alerts();
    }

    /// The as-run row for the time the process was down (1.7 step 4).
    pub(crate) fn restart_row(&mut self, first_sound_ms: i64) {
        let Some(from) = self.fail.restart_gap_from.take() else {
            return;
        };
        let gap = (first_sound_ms - from).max(0);
        let row = crate::state::AsRunRow {
            id: self.next_as_run,
            item_id: None,
            title: "(restart)".into(),
            artist: String::new(),
            category: String::new(),
            origin: "restart".into(),
            block_id: None,
            run_id: None,
            playlist_id: None,
            started_ms: from,
            planned_ms: None,
            flags: vec![format!("dead_air_ms={gap}")],
            ended_ms: Some(first_sound_ms),
            played_ms: Some(gap),
            end_reason: Some("restart".into()),
        };
        self.next_as_run += 1;
        if let Some(w) = &self.state {
            w.send(Msg::AsRunStart(row.clone()));
            w.send(Msg::AsRunEnd {
                id: row.id,
                ended_ms: first_sound_ms,
                played_ms: gap,
                reason: "restart".into(),
                flag: None,
            });
        }
        self.push_as_run(row);
        // The gap is counted from the last checkpoint, which was written up to one checkpoint interval
        // before the process died (the soak measured 2.7 s against 3.0 s and 3.9 s said): it is an upper
        // bound, and the message says so.
        self.fail.alerts.clear_code(code::RESTARTED, first_sound_ms);
        self.fail.restart_healthy_from_frame = None;
        self.fail.alerts.raise(
            code::RESTARTED,
            "",
            Severity::Warning,
            restart_message(gap, self.opts.checkpoint_every_ms),
            None,
            first_sound_ms,
        );
    }

    /// Unused by the runtime itself; the tracked voice map is exposed to tests through this.
    #[doc(hidden)]
    pub fn tracked_count(&self) -> usize {
        self.voices
            .values()
            .filter(|t: &&Tracked| t.kind != Kind::Cart)
            .count()
    }
}

/// The text of the `restarted` alert. The figure is an upper bound: it runs from the last checkpoint, and the
/// process stopped some time (up to one checkpoint interval) after that.
pub(crate) fn restart_message(gap_ms: i64, checkpoint_every_ms: i64) -> String {
    format!(
        "the station restarted; no more than {:.1} s without sound (counted from the last checkpoint, so the real break was up to {:.1} s shorter)",
        gap_ms as f64 / 1000.0,
        checkpoint_every_ms.min(gap_ms).max(0) as f64 / 1000.0
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_restart_message_is_an_upper_bound_and_says_so() {
        let m = restart_message(3_004, 2_000);
        assert!(m.contains("no more than 3.0 s"), "{m}");
        assert!(m.contains("up to 2.0 s shorter"), "{m}");
        // A gap shorter than the interval cannot be shortened by more than itself.
        assert!(restart_message(900, 2_000).contains("up to 0.9 s shorter"));
    }

    #[test]
    fn a_starved_voice_is_given_up_on_only_when_it_stays_dry_or_trickles() {
        let g = 96_000; // 2 s
        assert_eq!(starve_verdict(48_000, 0, g), None, "too soon to judge");
        assert_eq!(starve_verdict(96_000, 0, g), Some(true), "still dry");
        assert_eq!(
            starve_verdict(96_000, 30_000, g),
            Some(true),
            "a trickle under half speed"
        );
        assert_eq!(
            starve_verdict(96_000, 60_000, g),
            Some(false),
            "it caught up"
        );
    }
}

/// What the runner tells the station about the stream each step (see [`Station::on_stream`]).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StreamStatus {
    pub no_listener: bool,
    pub no_listener_s: f64,
    pub encoder_lag: bool,
    pub lag_ms: u64,
    pub encoder_failed: bool,
    /// No sound card (`device = "null"` or a file): the stream is the station's only output.
    pub stream_only: bool,
}
