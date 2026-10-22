//! The station's network API, the part that lives next to the station (7.2, 7.4).
//!
//! The station is owned by one thread (the runner's loop). HTTP threads never touch it: they read a
//! **view** the loop publishes every 250 ms and at every change, and they send **commands** (operator
//! actions) through a channel that the loop drains every step and answers with a fresh state. WebSocket
//! events go out through a [`Hub`] with a bounded queue per client, and the meter frames come straight
//! from the meter thread into the same hub.

use crate::meters::{self, MeterShared, FRAME_LEN};
use crate::ws::Hub;
use engine::mixer::MixerStats;
use serde_json::{json, Value};
use station::alerts::HealthStatus;
use station::api_view::OnAirInfo;
use station::programming::{BoundaryMode, StartHow};
use station::runtime::{Op, OpError, Station, Transport};
use station::sched::{At, QEntry, SwitchHow};
use station::state::iso_ms;
use station::timed::Upcoming;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// How long a command may wait for the loop (a `Busy` boundary is retried until then).
/// Commands that may wait for the loop; more are answered `503 busy` at once.
pub const COMMAND_QUEUE: usize = 64;
/// Commands the loop executes in one step.
pub const COMMANDS_PER_STEP: usize = 4;
pub const COMMAND_TIMEOUT: Duration = Duration::from_millis(3500);
/// The view is stale after this long: the loop has stopped, and `/health` says so.
pub const VIEW_STALE: Duration = Duration::from_millis(3000);

#[derive(Debug, Clone)]
pub struct ApiOptions {
    pub name: String,
    pub display_name: String,
    /// The product's display name (`GET /health` field `product`).
    pub product: String,
    pub media_dir: PathBuf,
    pub library_db: PathBuf,
    pub state_db: Option<PathBuf>,
    pub device: String,
    pub version: String,
    /// The station's config file, where `PUT /api/v1/loudness` writes the `[loudness]` block (6.1).
    pub config_path: Option<PathBuf>,
    /// The `[loudness]` block as the station started with it.
    pub loudness: common::config::LoudnessConfig,
    /// The gain the controller starts at when no checkpoint carries one (the static estimate, 6.1), dB.
    pub loudness_static_db: f64,
}

/// What the runner knows about the sound output, for `/health` and `/metrics`.
#[derive(Debug, Clone, Default)]
pub struct AudioInfo {
    pub open: bool,
    pub rt_state: &'static str,
    pub format: String,
    pub rate: u32,
    pub period_frames: u32,
    pub periods: u32,
    pub frames_written: u64,
    pub xruns: u32,
    pub device_restored: u32,
    pub delay_frames: i64,
    /// Starts within the crash-loop window (1.5) and in total since boot, for `hotclock_restarts_total`.
    pub restarts: u64,
}

#[derive(Debug)]
pub enum ApiOp {
    Op(Op),
    Play {
        playlist_id: Option<i64>,
    },
    Mode {
        assist: bool,
    },
    Ack {
        alert_id: i64,
    },
    Reload,
    /// Release an item from quarantine on this station (the operator's `release`, as `hotclock ctl` does).
    Release {
        item_id: i64,
    },
    Album {
        item_id: Option<i64>,
        album_key: Option<String>,
        gapless: bool,
        at: At,
    },
    Rotation {
        playlist_id: Option<i64>,
    },
    /// The station's default transition preset (2.6, step 1), kept in `state.db` `meta`.
    TransitionPreset {
        /// `None` only reads.
        name: Option<String>,
    },
    /// Settings that apply without a restart (`Station::apply_live`).
    LiveSettings {
        values: serde_json::Map<String, Value>,
    },
    /// The output trim (2.4): `Some(db)` sets it, `None` only reads it.
    Trim {
        db: Option<f64>,
    },
    External {
        name: String,
        value: Value,
    },
    /// `GET /schedule/upcoming?hours=`.
    Upcoming {
        hours: i64,
    },
    /// `GET /program/state`.
    ProgramState,
    /// `POST /program/start`.
    ProgramStart {
        playlist_id: i64,
        t0: station::program::T0Req,
        how: SwitchHow,
        source: &'static str,
    },
    /// `POST /program/abort`.
    ProgramAbort {
        stop_audio: bool,
    },
    /// `POST /program/continue`.
    ProgramContinue,
    /// `POST /program/arm` and `disarm`.
    ProgramArm {
        playlist_id: i64,
        occurrence_ms: Option<i64>,
        armed: bool,
    },
    /// `POST /program/blocks/{id}/fire`.
    ProgramFire {
        block_id: i64,
    },
    /// `POST /program/blocks/next/skip`.
    ProgramSkip,
    /// `POST /end-at`.
    EndAt {
        playlist_id: i64,
        at_ms: i64,
        then: station::program::EndThen,
    },
    /// `POST /schedule/changes`.
    AddChange {
        playlist_id: i64,
        at_ms: i64,
        how: StartHow,
        mode: BoundaryMode,
        early_s: i64,
        late_s: i64,
    },
    /// `DELETE /schedule/changes/{id}`.
    CancelChange {
        id: i64,
    },
}

pub enum ApiReply {
    /// Done: the resulting state and an operation-specific extra.
    Done { state: Value, extra: Value },
    Refused {
        status: u16,
        code: &'static str,
        message: String,
    },
}

pub struct ApiCmd {
    pub op: ApiOp,
    pub reply: std::sync::mpsc::Sender<ApiReply>,
    pub deadline: Instant,
}

/// What the HTTP threads read.
#[derive(Default)]
pub struct View {
    pub updated: Option<Instant>,
    pub state: Value,
    pub now: Option<NowView>,
    pub queue: Value,
    pub alerts: Value,
    pub health: Value,
    pub health_status: u16,
    pub audio: AudioInfo,
    pub counters: Counters,
}

impl View {
    /// The loop has not republished for [`VIEW_STALE`] (or never has).
    pub fn is_stale(&self) -> bool {
        self.updated.is_none_or(|u| u.elapsed() > VIEW_STALE)
    }
}

#[derive(Clone)]
pub struct NowView {
    pub json: Value,
    pub started_ms: i64,
    pub segue_ms: Option<i64>,
    pub ends_ms: i64,
    pub next: Value,
}

/// Numbers for `/metrics`.
#[derive(Default, Clone)]
pub struct Counters {
    pub items_played: BTreeMap<String, u64>,
    pub item_errors: BTreeMap<String, u64>,
    pub silence_seconds: f64,
    pub limiter_active_seconds: f64,
    pub out_rms_dbfs: f64,
    pub queue_depth: usize,
    pub rule_relaxed: u64,
    /// Timed blocks by outcome (`soft`, `hard`, `late_run`, `missed`, ...), and how far from their time the
    /// ones that fired did: cumulative upper bounds in seconds (1, 5, 10, 30, 60, more), their sum and count.
    pub blocks_by_how: BTreeMap<String, u64>,
    pub block_deviation_buckets: [u64; 6],
    pub block_deviation_sum_s: f64,
    pub block_deviation_count: u64,
    pub block_last_deviation_s: Option<f64>,
    pub db_backlog: usize,
    pub tap_drops: u64,
    pub quarantined: usize,
    pub alerts_raised: BTreeMap<String, u64>,
    pub frames_written: u64,
    pub xruns: u64,
    pub device_reopens: u64,
    pub restarts: u64,
    pub disk_free_bytes: Option<u64>,
}

pub struct StationApi {
    pub opts: ApiOptions,
    pub hub: Arc<Hub>,
    pub meter: Arc<MeterShared>,
    pub view: RwLock<View>,
    tx: Mutex<SyncSender<ApiCmd>>,
    pub started: Instant,
    pub external: Mutex<VecDeque<(i64, String, Value)>>,
    /// The network stream mount (1.3a), set once by the runner when `[stream]` is enabled.
    pub stream: std::sync::OnceLock<Arc<stream::Mount>>,
    /// The settings the loudness API changes while the station plays (gate categories, peak allowance).
    pub loudness_live: std::sync::OnceLock<Arc<station::conductor::LoudnessLive>>,
    /// The loudness settings now in force (`PUT /api/v1/loudness` changes them; 6.1).
    pub loudness_cfg: Mutex<common::config::LoudnessConfig>,
    /// The processing chain's settings, presets and persistence (M6), set once by the runner.
    pub chain: std::sync::OnceLock<Arc<crate::chain_api::ChainApi>>,
}

impl StationApi {
    pub fn new(opts: ApiOptions) -> (Arc<StationApi>, Receiver<ApiCmd>) {
        // Bounded: a client that fires commands faster than the loop takes them gets `503 busy` instead of
        // growing a queue the loop would then have to work through.
        let (tx, rx) = std::sync::mpsc::sync_channel(COMMAND_QUEUE);
        let loudness_cfg = Mutex::new(opts.loudness.clone());
        let api = Arc::new(StationApi {
            loudness_cfg,
            opts,
            hub: Arc::new(Hub::default()),
            meter: Arc::new(MeterShared::default()),
            view: RwLock::new(View::default()),
            tx: Mutex::new(tx),
            started: Instant::now(),
            external: Mutex::new(VecDeque::new()),
            stream: std::sync::OnceLock::new(),
            loudness_live: std::sync::OnceLock::new(),
            chain: std::sync::OnceLock::new(),
        });
        (api, rx)
    }

    /// The envelope of 7.4 around `data` (already serialized).
    pub fn envelope(&self, typ: &str, seq: u64, data: &str) -> String {
        format!(
            "{{\"v\":1,\"type\":\"{typ}\",\"station\":{},\"seq\":{seq},\"at\":\"{}\",\"data\":{data}}}",
            json!(self.opts.name),
            iso_ms(wall_ms())
        )
    }

    pub fn emit(&self, typ: &str, data: &Value) {
        let topic = topic_of(typ);
        if !self.hub.wants(topic) {
            return;
        }
        let text = data.to_string();
        self.hub
            .send_with(topic, &|seq| self.envelope(typ, seq, &text));
    }

    /// A `snapshot` message for one client, carrying that client's current sequence number.
    pub fn snapshot_message(&self, client_seq: u64) -> String {
        let state = self
            .view
            .read()
            .map(|v| v.state.to_string())
            .unwrap_or_else(|_| "null".into());
        self.envelope("snapshot", client_seq, &state)
    }

    /// Send a command to the station loop and wait for its answer.
    /// The history of this station's config file, when it has a config file and a data directory.
    pub fn history(&self) -> Option<crate::config_history::ConfigHistory> {
        Some(crate::config_history::ConfigHistory::beside(
            self.opts.state_db.as_ref()?,
            self.opts.config_path.as_ref()?,
        ))
    }

    pub fn command(&self, op: ApiOp) -> ApiReply {
        let (rtx, rrx) = std::sync::mpsc::channel();
        let cmd = ApiCmd {
            op,
            reply: rtx,
            deadline: Instant::now() + COMMAND_TIMEOUT,
        };
        let sent = match self.tx.lock() {
            Ok(t) => t.try_send(cmd),
            Err(_) => {
                return ApiReply::Refused {
                    status: 503,
                    code: "unavailable",
                    message: "the station is not running".into(),
                }
            }
        };
        match sent {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return ApiReply::Refused {
                    status: 503,
                    code: "busy",
                    message: "too many commands waiting for the station; try again in a moment"
                        .into(),
                }
            }
            Err(TrySendError::Disconnected(_)) => {
                return ApiReply::Refused {
                    status: 503,
                    code: "unavailable",
                    message: "the station is not running".into(),
                }
            }
        }
        match rrx.recv_timeout(COMMAND_TIMEOUT + Duration::from_millis(1500)) {
            Ok(r) => r,
            Err(_) => ApiReply::Refused {
                status: 504,
                code: "timeout",
                message: "the station did not answer in time".into(),
            },
        }
    }

    pub fn state_json(&self) -> Value {
        self.view
            .read()
            .map(|v| v.state.clone())
            .unwrap_or(Value::Null)
    }

    pub fn is_stale(&self) -> bool {
        self.view.read().map(|v| v.is_stale()).unwrap_or(true)
    }
}

/// The subscription topic an event type belongs to (7.4).
pub fn topic_of(typ: &str) -> &'static str {
    match typ {
        "now" | "timing" => "now",
        "queue" => "queue",
        "state" => "state",
        "alert" | "alert_cleared" => "alerts",
        "asrun" => "asrun",
        "loudness" => "loudness",
        "chain" => "chain",
        "program" | "block" => "program",
        "library" => "library",
        _ => "state",
    }
}

pub const JSON_TOPICS: &str = "state,now,queue,alerts,asrun,program,loudness,chain";

pub fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

// ---- JSON of the station's state --------------------------------------------------------------------------------

fn art_url(st: &Station, item_id: i64, size: u32) -> Value {
    match st.cat.get(item_id) {
        Some(t) if t.row.art_sha.is_some() => {
            json!(format!("/api/v1/items/{item_id}/art?size={size}"))
        }
        _ => Value::Null,
    }
}

fn item_brief(st: &Station, item_id: i64) -> Value {
    match st.cat.get(item_id) {
        Some(t) => json!({
            "item_id": item_id, "title": t.meta.title, "artist": t.meta.artist, "album": t.row.album,
            "category": t.meta.category, "kind": t.meta.kind,
            "art_url": art_url(st, item_id, 144),
        }),
        None => {
            json!({"item_id": item_id, "title": format!("item {item_id}"), "artist": "", "album": "",
                       "category": "", "kind": "", "art_url": null})
        }
    }
}

/// The per-entry transition as the API shows it: `null` (none), `"cut"`, or `{preset, patch}`.
fn entry_transition_json(t: &Option<String>) -> Value {
    match t.as_deref() {
        None => Value::Null,
        Some("cut") => json!("cut"),
        Some(text) => serde_json::from_str(text).unwrap_or(Value::Null),
    }
}

fn queue_entry_json(st: &Station, e: &QEntry, now_ms: i64) -> Value {
    let mut v = item_brief(st, e.item_id);
    let o = v.as_object_mut().expect("object");
    o.insert("kind".into(), json!("track"));
    o.insert("entry_id".into(), json!(e.id));
    o.insert("origin".into(), json!(e.origin.as_str()));
    o.insert("pinned".into(), json!(e.pinned));
    o.insert("committed".into(), json!(e.committed));
    o.insert("run_id".into(), json!(e.run_id));
    o.insert("continuity_from_prev".into(), json!(e.continuity_from_prev));
    o.insert("transition".into(), entry_transition_json(&e.transition));
    o.insert("flags".into(), json!(e.flags));
    o.insert(
        "planned_start".into(),
        e.planned_start_ms.map_or(Value::Null, |m| json!(iso_ms(m))),
    );
    o.insert(
        "planned_segue".into(),
        e.planned_segue_ms.map_or(Value::Null, |m| json!(iso_ms(m))),
    );
    o.insert(
        "in_ms".into(),
        e.planned_start_ms
            .map_or(Value::Null, |m| json!(m - now_ms)),
    );
    v
}

pub fn queue_json(st: &Station, now_ms: i64) -> Value {
    // The tracks, with the upcoming blocks between them by planned time (4.9).
    let tracks: Vec<(Option<i64>, Value)> = st
        .sched
        .st
        .queue
        .iter()
        .map(|e| (e.planned_start_ms, queue_entry_json(st, e, now_ms)))
        .collect();
    let mut blocks = block_rows(st, now_ms).into_iter().peekable();
    let mut out: Vec<Value> = Vec::with_capacity(tracks.len() + 4);
    for (start, v) in tracks {
        if let Some(t) = start {
            while blocks.peek().is_some_and(|(bt, _)| *bt <= t) {
                out.push(blocks.next().expect("peeked").1);
            }
        }
        out.push(v);
    }
    out.extend(blocks.map(|b| b.1));
    Value::Array(out)
}

fn now_view(st: &Station, info: &OnAirInfo) -> NowView {
    let e = &info.entry;
    let mut v = item_brief(st, e.item_id);
    let row_segue = st.cat.get(e.item_id).map(|t| t.row.segue);
    let file_ms = st.cat.get(e.item_id).map_or(0, |t| t.row.file_ms);
    let head = st.sched.head();
    let segue_ms = head
        .and_then(|h| h.planned_start_ms)
        .or_else(|| row_segue.map(|s| info.started_ms + (s - info.bound_in_ms).max(0)));
    let ends_ms = info.started_ms + (info.bound_out_ms - info.bound_in_ms).max(0);
    let next = match head {
        Some(h) => {
            let mut n = item_brief(st, h.item_id);
            n.as_object_mut().expect("object").insert(
                "starts_at".into(),
                h.planned_start_ms.map_or(Value::Null, |m| json!(iso_ms(m))),
            );
            n
        }
        None => Value::Null,
    };
    let o = v.as_object_mut().expect("object");
    o.insert("entry_id".into(), json!(e.id));
    o.insert("started_at".into(), json!(iso_ms(info.started_ms)));
    o.insert("cue_in_ms".into(), json!(info.bound_in_ms));
    o.insert(
        "segue_at".into(),
        segue_ms.map_or(Value::Null, |m| json!(iso_ms(m))),
    );
    o.insert("ends_at".into(), json!(iso_ms(ends_ms)));
    o.insert("duration_ms".into(), json!(file_ms));
    o.insert("origin".into(), json!(e.origin.as_str()));
    o.insert("run_id".into(), json!(e.run_id));
    o.insert(
        "continuity_next".into(),
        json!(head.is_some_and(|h| h.continuity_from_prev)),
    );
    o.insert("emergency".into(), json!(info.alt));
    o.insert("item_gain_db".into(), json!(info.gain_db));
    NowView {
        json: v,
        started_ms: info.started_ms,
        segue_ms,
        ends_ms,
        next,
    }
}

pub fn upcoming_json(list: &[Upcoming], now_ms: i64) -> Value {
    Value::Array(
        list.iter()
            .map(|u| {
                json!({
                    "block_id": u.block_id, "name": u.name, "type": u.kind, "origin": u.origin,
                    "source": u.source, "mode": u.mode.as_str(),
                    "target": iso_ms(u.target_ms), "predicted_at": iso_ms(u.predicted_ms),
                    "window": {"from": iso_ms(u.lo_ms), "to": iso_ms(u.hi_ms)},
                    "in_ms": u.predicted_ms - now_ms, "planned": u.planned,
                    "predicted_deviation_ms": u.predicted_ms - u.target_ms,
                })
            })
            .collect(),
    )
}

/// The badge a block shows in a queue row (4.9).
pub fn badge_of(kind: &str) -> &'static str {
    match kind {
        "fade_stop" => "FADE",
        "stop" | "end" => "STOP",
        "play" | "start_playlist" | "start" => "PLAY",
        "osc_out" => "OSC",
        "midi_out" => "MIDI",
        "http_request" => "HTTP",
        "insert" | "insert_every" => "ID",
        "wait" => "WAIT",
        "loop" => "LOOP",
        "set_gain" => "GAIN",
        "set_mute" => "MUTE",
        _ => "BLOCK",
    }
}

fn back_time_json(st: &Station) -> Value {
    let now = wall_ms();
    match st
        .sched
        .st
        .bt_display
        .as_ref()
        .filter(|b| b.target_ms + 10_000 >= now)
    {
        Some(b) => json!({
            "block_id": b.block_id, "block": b.name, "target": iso_ms(b.target_ms),
            "planned_end": iso_ms(b.planned_end_ms), "delta_s": b.delta_s(),
            "state": b.state, "mode": b.mode, "closed": b.closed,
            "label": format!("END {:+.0} s", b.delta_s()),
        }),
        None => json!({"state": "off"}),
    }
}

/// The upcoming blocks as queue rows: timed blocks in the next hour, the acts of in-sequence blocks that
/// wait for their boundary, and the block the cursor stands at. `(planned ms, row)`.
pub fn block_rows(st: &Station, now_ms: i64) -> Vec<(i64, Value)> {
    let mut out: Vec<(i64, Value)> = Vec::new();
    for u in st.sched.upcoming(now_ms, 1) {
        if u.predicted_ms < now_ms - 2000 {
            continue;
        }
        out.push((
            u.predicted_ms,
            json!({
                "kind": "block", "block_id": u.block_id, "name": u.name, "type": u.kind, "badge": badge_of(&u.kind),
                "block_type": badge_of(&u.kind),
                "origin": u.origin, "trigger": "timed", "mode": u.mode.as_str(),
                "planned_start": iso_ms(u.predicted_ms), "in_ms": u.predicted_ms - now_ms,
            }),
        ));
    }
    for (a, t) in st.sched.pending_seq_acts(&st.cat) {
        let ty = match &a.kind {
            station::sched::SeqKind::Side(s) => s.type_name(),
            station::sched::SeqKind::Gain { .. } => "set_gain",
            station::sched::SeqKind::Mute { .. } => "set_mute",
            station::sched::SeqKind::FadeStop { .. } => "fade_stop",
            _ => "stop",
        };
        out.push((
            t,
            json!({
                "kind": "block", "block_id": a.block_id, "name": a.name, "type": ty, "badge": badge_of(ty),
                "block_type": badge_of(ty),
                "origin": "program", "trigger": "sequence",
                "planned_start": iso_ms(t), "in_ms": t - now_ms,
            }),
        ));
    }
    if let Some(c) = st.sched.cursor_block(&st.cat) {
        if c.type_name == "wait" {
            let at = c.until_ms;
            out.push((
                at.unwrap_or(now_ms),
                json!({
                    "kind": "block", "block_id": c.block_id, "name": c.name, "type": "wait", "badge": "WAIT", "block_type": "WAIT",
                    "origin": "program", "trigger": "sequence", "wait_kind": c.wait_kind,
                    "planned_start": at.map(iso_ms), "in_ms": at.map(|m| m - now_ms),
                }),
            ));
        }
    }
    // The back-timing plan is shown on the row of the block that ends the fill: `END +3 s` (4.9).
    if let Some(b) = st
        .sched
        .st
        .bt_display
        .as_ref()
        .filter(|b| b.target_ms + 10_000 >= now_ms)
    {
        for (t, v) in out.iter_mut() {
            let ends = matches!(
                v["type"].as_str(),
                Some("fade_stop" | "stop" | "end" | "play" | "start_playlist")
            );
            if ends && (*t - b.target_ms).abs() <= 1500 {
                v["end_label"] = json!(format!("END {:+.0} s", b.delta_s()));
            }
        }
    }
    out.sort_by_key(|x| x.0);
    out
}

/// `GET /program/state` (7.2): the program in progress, its cursor and the levels nested in it, every
/// block with its time and what became of it, the next block for countdown keys, the next expected start
/// and `back_time` (4.9).
pub fn program_state_json(st: &Station, now_ms: i64) -> Value {
    let zone = st.sched.cfg.zone.clone();
    // two days: a stop after midnight with a day offset is more than a day from a T0 just after midnight
    let upcoming = st.sched.upcoming(now_ms, 49);
    let prog = st.sched.prog.clone();
    let pr = st.sched.st.program.clone();
    let wait = st.sched.cursor_block(&st.cat);
    let program = pr.as_ref().map(|p| {
        let name_of = |id: i64| prog.playlists.get(&id).map(|pl| pl.name.clone());
        let root = p.root_playlist();
        let mut stack: Vec<Value> = p
            .stack
            .iter()
            .enumerate()
            .map(|(i, c)| {
                json!({"depth": i, "playlist_id": c.playlist_id, "playlist": name_of(c.playlist_id),
                    "position": c.seq_pos + 1, "t0": iso_ms(c.t0_ms)})
            })
            .collect();
        stack.push(json!({"depth": p.stack.len(), "playlist_id": p.playlist_id, "playlist": name_of(p.playlist_id),
            "position": p.seq_pos + 1, "t0": iso_ms(p.t0_ms)}));
        let state = if p.halted {
            "halted"
        } else if wait.as_ref().is_some_and(|w| w.wait_kind == Some("operator")) {
            "waiting"
        } else if st.transport.name() == "stopped" {
            "stopped"
        } else {
            "running"
        };
        json!({
            "run_id": p.run_id, "playlist_id": root, "playlist": name_of(root),
            "t0": iso_ms(p.t0_ms), "t0_ms": p.t0_ms,
            "t0_source": if p.t0_source.is_empty() { "auto" } else { p.t0_source.as_str() },
            "nominal_t0": p.nominal_t0_ms.map(iso_ms),
            "state": state, "manual": p.manual,
            "cursor": {"position": p.seq_pos + 1, "fill": p.fill.map(|f| f + 1), "depth": p.stack.len(),
                "playlist_id": p.playlist_id, "playlist": name_of(p.playlist_id)},
            "stack": stack,
            "waiting": wait.as_ref().filter(|w| w.type_name == "wait").map(|w| json!({
                "block_id": w.block_id, "name": w.name, "kind": w.wait_kind,
                "until": w.until_ms.map(iso_ms), "in_ms": w.until_ms.map(|u| u - now_ms),
            })),
        })
    });
    // Every block of the program (each level), with what is known of it.
    let mut blocks: Vec<Value> = Vec::new();
    if let Some(p) = &pr {
        let mut levels: Vec<(usize, i64, usize)> = p
            .stack
            .iter()
            .enumerate()
            .map(|(d, c)| (d, c.playlist_id, c.seq_pos))
            .collect();
        levels.push((p.stack.len(), p.playlist_id, p.seq_pos));
        for (depth, pid, pos) in levels {
            let Some(pl) = prog.playlists.get(&pid) else {
                continue;
            };
            for (i, e) in pl.entries.iter().enumerate() {
                let station::programming::Entry::Block(b) = e else {
                    continue;
                };
                if matches!(b.kind, station::programming::BlockKind::InsertEvery(_)) {
                    continue;
                }
                let up = upcoming.iter().find(|u| u.block_id == b.id);
                let last = st.sched.st.log.rows.iter().rev().find(|r| {
                    r.block_id == b.id && r.run_id == Some(p.run_id) && r.kind != "back_time"
                });
                let status = match (last, up) {
                    (Some(r), _) => r.how.clone(),
                    (None, Some(_)) => "pending".to_string(),
                    (None, None) if b.timed.is_none() && i < pos => "done".into(),
                    (None, None) if b.timed.is_none() && i == pos => "current".into(),
                    _ => "pending".into(),
                };
                blocks.push(json!({
                    "block_id": b.id, "name": b.name, "type": b.kind.type_name(), "badge": badge_of(b.kind.type_name()),
                    "trigger": b.trigger.as_str(), "position": i + 1, "depth": depth, "playlist_id": pid,
                    "status": status, "enabled": b.enabled,
                    "at": up.map(|u| iso_ms(u.predicted_ms)), "in_ms": up.map(|u| u.predicted_ms - now_ms),
                    "fired_at": last.and_then(|r| r.fired_ms).map(iso_ms),
                    "deviation_ms": last.and_then(|r| r.deviation_ms),
                    "current": b.timed.is_none() && i == pos && depth == p.stack.len(),
                }));
            }
        }
    }
    let next = upcoming
        .iter()
        .filter(|u| u.target_ms >= now_ms - 60_000)
        .min_by_key(|u| u.predicted_ms);
    let next_block = next.map(|u| {
        json!({"block_id": u.block_id, "name": u.name, "type": u.kind, "badge": badge_of(&u.kind),
            "at": iso_ms(u.predicted_ms), "in_ms": u.predicted_ms - now_ms})
    });
    let expected = st.sched.expected(now_ms).map(|e| {
        let name = prog.playlists.get(&e.playlist_id).map(|p| p.name.clone());
        json!({
            "playlist_id": e.playlist_id, "playlist": name, "block_id": e.block_id,
            "at": iso_ms(e.at_ms), "in_ms": e.at_ms - now_ms,
            "anchor": if e.manual { "manual" } else { "absolute" },
            "armed": e.armed, "fallback_grace_s": e.fallback_grace_s,
            "label": format!("expected {}", zone.local_string(e.at_ms).get(11..16).unwrap_or("")),
        })
    });
    let last_block = st
        .sched
        .st
        .log
        .rows
        .iter()
        .rev()
        .find(|r| r.fired_ms.is_some() && r.how != "passed" && r.kind != "back_time")
        .map(|r| json!({"name": r.name, "type": r.kind, "how": r.how, "target": iso_ms(r.target_ms), "fired_at": r.fired_ms.map(iso_ms), "deviation_ms": r.deviation_ms}));
    json!({
        "last_block": last_block,
        "running": program.is_some(),
        "program": program,
        "blocks": blocks,
        "upcoming": upcoming_json(&upcoming, now_ms),
        "next_block": next_block,
        "expected": expected,
        "back_time": back_time_json(st),
        "last_start": st.last_start.clone(),
        "time_zone": zone.name(),
        "at": iso_ms(now_ms),
    })
}

fn alerts_json(st: &Station) -> Value {
    Value::Array(
        st.alerts()
            .active()
            .into_iter()
            .map(|a| {
                json!({
                    "id": a.id, "code": a.code, "key": a.key, "severity": a.severity.name(),
                    "message": a.message, "raised_at": iso_ms(a.raised_ms), "item_id": a.item_id,
                })
            })
            .collect(),
    )
}

/// The rotation figures of a playlist (4.3): the two no-repeat values and why they differ.
pub fn rotation_json(st: &Station, playlist_id: i64) -> Option<Value> {
    let pl = st.sched.prog.playlists.get(&playlist_id)?;
    let i = st.sched.rotation_info(pl, &st.cat);
    Some(json!({
        "playlist_id": playlist_id, "playlist": i.playlist, "units": i.units,
        "mean_minutes": i.mean_minutes, "window_items": i.window_items, "seam_k": i.k,
        "capped": i.capped,
        "no_repeat_configured": i.no_repeat_configured,
        "no_repeat_effective": i.no_repeat_effective,
        "no_repeat_reason": i.no_repeat_reason,
        "message": i.message(),
        "requested_minutes": i.requested_minutes, "achievable_minutes": i.achievable_minutes,
        "seam_minutes": i.seam_minutes,
    }))
}

fn active_playlist_json(st: &Station) -> Value {
    let Some(p) = st.sched.st.program.as_ref() else {
        return Value::Null;
    };
    let mut v = json!({"id": p.playlist_id});
    if let Some(r) = rotation_json(st, p.playlist_id) {
        let o = v.as_object_mut().expect("object");
        o.insert("name".into(), r["playlist"].clone());
        o.insert("rotation".into(), r);
    }
    v
}

fn state_string(st: &Station) -> &'static str {
    if st.fail.safe_mode {
        "safe"
    } else {
        st.transport.name()
    }
}

/// The station's default transition preset: the current one, the config's, and the presets it can take.
pub fn transition_preset_json(st: &Station) -> Value {
    let available: Vec<Value> = common::transition::BUILTIN_PRESETS
        .iter()
        .filter(|(n, _, _)| st.presets.get(n).is_ok())
        .map(|(n, _, d)| json!({"name": n, "description": d}))
        .collect();
    json!({
        "preset": st.opts.preset,
        "config_preset": st.config_preset,
        "overridden": st.opts.preset != st.config_preset,
        "presets": available,
    })
}

/// What `GET /api/v1/state` and the `snapshot` event carry.
/// The carts playing now, for the `state` snapshot and event: `[{item_id, started_at}]` (7.4).
fn carts_json(st: &Station) -> Value {
    Value::Array(
        st.playing_carts()
            .into_iter()
            .map(|(item_id, started_ms)| json!({"item_id": item_id, "started_at": iso_ms(started_ms)}))
            .collect(),
    )
}

pub fn state_json(
    st: &Station,
    api: &StationApi,
    now: &Option<NowView>,
    playlist: &Value,
    audio: &AudioInfo,
    now_ms: i64,
    health_status: HealthStatus,
) -> Value {
    json!({
        "station": api.opts.name,
        "display_name": api.opts.display_name,
        "state": state_string(st),
        "mode": if st.assist { "assist" } else { "auto" },
        "output_trim_db": st.opts.output_trim_db,
        "carts": carts_json(st),
        "hold": st.transport == Transport::Held,
        "safe_mode": st.fail.safe_mode,
        "operating_mode": st.health().mode,
        "health": health_status.name(),
        "now": now.as_ref().map(|n| n.json.clone()),
        "next": now.as_ref().map(|n| n.next.clone()).unwrap_or(Value::Null),
        "queue": queue_json(st, now_ms),
        "alerts": alerts_json(st),
        "active_playlist": playlist,
        "transition_preset": st.opts.preset,
        "transition_preset_config": st.config_preset,
        "output": {
            "device": api.opts.device, "rate": audio.rate, "format": audio.format,
            "period_frames": audio.period_frames, "periods": audio.periods, "open": audio.open,
        },
        "at": iso_ms(now_ms),
    })
}

// ---- the driver: called by the runner every step -------------------------------------------------------------

struct Pending {
    cmd: ApiCmd,
}

/// Called with an alert code when one is acknowledged through the API.
pub type AckHook = Box<dyn FnMut(&str) + Send>;

pub struct Driver {
    pub api: Arc<StationApi>,
    rx: Receiver<ApiCmd>,
    pending: VecDeque<Pending>,
    last_publish: Option<Instant>,
    last_timing: Instant,
    last_loud: Instant,
    last_playlist: Instant,
    playlist_json: Value,
    last_queue: String,
    last_program: u64,
    last_program_check: Instant,
    last_blocks_check: Instant,
    blocks_sig: u64,
    last_state_event: String,
    seen_started: HashSet<i64>,
    announced_end: HashSet<i64>,
    known_alerts: BTreeMap<i64, String>,
    counters: Counters,
    xrun_samples: VecDeque<(Instant, u32)>,
    /// Where `ack safe_mode` clears the crash-loop counter.
    pub on_ack: Option<AckHook>,
    /// A pending `now` event held until the item is audible.
    hold_now: Option<(i64, Value)>,
    pub restarts: u64,
    /// The last publish found the view locked by a reader and is retried on the next step.
    publish_missed: bool,
    /// How often that happened (for tests and diagnosis).
    pub publishes_missed: u64,
}

impl Driver {
    pub fn new(api: Arc<StationApi>, rx: Receiver<ApiCmd>) -> Driver {
        Driver {
            api,
            rx,
            pending: VecDeque::new(),
            last_publish: None,
            last_timing: Instant::now(),
            last_loud: Instant::now(),
            last_playlist: Instant::now() - Duration::from_secs(60),
            playlist_json: Value::Null,
            last_queue: String::new(),
            last_program: 0,
            last_program_check: Instant::now() - Duration::from_secs(60),
            last_blocks_check: Instant::now() - Duration::from_secs(60),
            blocks_sig: 0,
            last_state_event: String::new(),
            seen_started: HashSet::new(),
            announced_end: HashSet::new(),
            known_alerts: BTreeMap::new(),
            counters: Counters::default(),
            xrun_samples: VecDeque::new(),
            on_ack: None,
            hold_now: None,
            restarts: 0,
            publish_missed: false,
            publishes_missed: 0,
        }
    }

    /// Xruns in the last hour, from samples taken once a minute.
    pub fn xruns_1h(&self, current: u32) -> u32 {
        self.xrun_samples
            .front()
            .map_or(0, |(_, x)| current.saturating_sub(*x))
    }

    /// One step of the API side of the loop.
    pub fn step(
        &mut self,
        st: &mut Station,
        frame: u64,
        wall: i64,
        audio: &AudioInfo,
        mixer: &MixerStats,
        send: &mut dyn FnMut(engine::rt::Command),
    ) {
        let mut changed = false;
        // New commands join the queue; the queue is worked in order, and a `Busy` one waits for the next step.
        // At most COMMANDS_PER_STEP are executed per step (each builds a full state for its reply), so a burst
        // of requests spreads over steps instead of lengthening one; the channel is bounded, so the queue is.
        while let Ok(cmd) = self.rx.try_recv() {
            self.pending.push_back(Pending { cmd });
        }
        let mut still = VecDeque::new();
        let mut done = 0;
        while let Some(p) = self.pending.pop_front() {
            if done >= COMMANDS_PER_STEP {
                if Instant::now() < p.cmd.deadline {
                    still.push_back(p);
                } else {
                    let _ = p.cmd.reply.send(ApiReply::Refused {
                        status: 503,
                        code: "busy",
                        message: "the station had too many commands to work through; try again"
                            .into(),
                    });
                }
                continue;
            }
            done += 1;
            match self.exec(st, &p.cmd.op, frame, wall, send) {
                Exec::Busy if Instant::now() < p.cmd.deadline => still.push_back(p),
                Exec::Busy => {
                    let _ = p.cmd.reply.send(ApiReply::Refused {
                        status: 409,
                        code: "busy",
                        message: "a boundary is being set up; try again in a moment".into(),
                    });
                }
                Exec::Reply(r) => {
                    changed = true;
                    // The active playlist may have changed: rebuild its rotation figures now.
                    self.playlist_json = Value::Null;
                    // The reply carries the state after the action, built now.
                    let r = match r {
                        ExecResult::Ok(extra) => {
                            let s = self.build_state(st, audio, wall, mixer);
                            ApiReply::Done { state: s, extra }
                        }
                        ExecResult::Refused(status, code, message) => ApiReply::Refused {
                            status,
                            code,
                            message,
                        },
                    };
                    let _ = p.cmd.reply.send(r);
                }
            }
        }
        self.pending = still;
        let due = self
            .last_publish
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(250));
        self.events(st, wall, changed);
        if due || changed || self.publish_missed {
            self.publish(st, audio, wall, mixer);
        }
        // `wall` above is the audible time of the frames just rendered (ahead of the clock by the device
        // delay); the event goes out when the item is audible on the real clock.
        self.release_now(wall_ms());
    }

    fn build_state(
        &mut self,
        st: &Station,
        audio: &AudioInfo,
        wall: i64,
        _mixer: &MixerStats,
    ) -> Value {
        if self.last_playlist.elapsed() >= Duration::from_secs(2) || self.playlist_json.is_null() {
            self.last_playlist = Instant::now();
            self.playlist_json = active_playlist_json(st);
        }
        let now = st.on_air_info().map(|i| now_view(st, &i));
        state_json(
            st,
            &self.api,
            &now,
            &self.playlist_json,
            audio,
            wall,
            st.alerts().status(),
        )
    }

    /// Rebuild the view the HTTP threads read.
    fn publish(&mut self, st: &Station, audio: &AudioInfo, wall: i64, mixer: &MixerStats) {
        if self.last_playlist.elapsed() >= Duration::from_secs(2) || self.playlist_json.is_null() {
            self.last_playlist = Instant::now();
            self.playlist_json = active_playlist_json(st);
        }
        let now = st.on_air_info().map(|i| now_view(st, &i));
        let health = st.health();
        let state = state_json(
            st,
            &self.api,
            &now,
            &self.playlist_json,
            audio,
            wall,
            health.status,
        );
        // One xrun sample a minute, kept for an hour.
        if self
            .xrun_samples
            .back()
            .is_none_or(|(t, _)| t.elapsed() >= Duration::from_secs(60))
        {
            self.xrun_samples.push_back((Instant::now(), audio.xruns));
            while self.xrun_samples.len() > 61 {
                self.xrun_samples.pop_front();
            }
        }
        let c = &mut self.counters;
        c.silence_seconds = mixer.silent_run_frames as f64 / f64::from(audio.rate.max(1));
        c.out_rms_dbfs = f64::from(mixer.last_block_rms_dbfs);
        c.limiter_active_seconds =
            mixer.limiter_active_frames as f64 / f64::from(audio.rate.max(1));
        c.queue_depth = health.queue_depth;
        c.rule_relaxed = st.sched.relaxed_picks;
        {
            // The block log of the last 72 hours (what the scheduler keeps): outcome counts and the
            // distribution of the deviation of the blocks that fired.
            let (mut by, mut b, mut sum, mut n) = (BTreeMap::new(), [0u64; 6], 0.0, 0u64);
            for r in st.sched.st.log.rows.iter().filter(|r| r.how != "passed") {
                *by.entry(r.how.clone()).or_insert(0) += 1;
                if let (Some(d), Some(_)) = (r.deviation_ms, r.fired_ms) {
                    let a = (d.abs() as f64) / 1000.0;
                    for (i, le) in [1.0, 5.0, 10.0, 30.0, 60.0].iter().enumerate() {
                        if a <= *le {
                            b[i] += 1;
                        }
                    }
                    b[5] += 1;
                    sum += a;
                    n += 1;
                }
            }
            c.blocks_by_how = by;
            c.block_deviation_buckets = b;
            c.block_deviation_sum_s = sum;
            c.block_deviation_count = n;
            c.block_last_deviation_s = st
                .sched
                .st
                .log
                .rows
                .iter()
                .rev()
                .find(|r| r.fired_ms.is_some() && r.how != "passed")
                .and_then(|r| r.deviation_ms)
                .map(|d| d as f64 / 1000.0);
        }
        c.db_backlog = health.db_backlog;
        c.tap_drops = mixer.tap_drops;
        c.quarantined = health.quarantined;
        c.frames_written = audio.frames_written;
        c.xruns = u64::from(audio.xruns);
        c.device_reopens = u64::from(audio.device_restored);
        c.restarts = audio.restarts;
        c.disk_free_bytes = health.disk_free_bytes;
        c.alerts_raised = st.alerts().raised_total.clone();
        c.item_errors = st
            .alerts()
            .raised_total
            .iter()
            .filter(|(k, _)| {
                matches!(
                    k.as_str(),
                    "item_truncated"
                        | "item_missing"
                        | "item_silent"
                        | "item_decode_failed"
                        | "item_marker_invalid"
                        | "analysis_failed"
                )
            })
            .map(|(k, v)| (k.trim_start_matches("item_").to_string(), *v))
            .collect();
        let health_json = crate::observe::health_json(
            st,
            &self.api,
            audio,
            &now,
            &health,
            self.xruns_1h(audio.xruns),
            &self.api.meter,
        );
        let status = health.status.http_status();
        let queue = state["queue"].clone();
        let alerts = state["alerts"].clone();
        // `try_write`: the loop never waits on a lock that HTTP threads hold. When a reader has it, the view is
        // published on the next step (20 ms later) instead.
        let counters = self.counters.clone();
        let stored = try_store_view(&self.api, |v| {
            v.updated = Some(Instant::now());
            v.state = state;
            v.now = now;
            v.queue = queue;
            v.alerts = alerts;
            v.health = health_json;
            v.health_status = status;
            v.audio = audio.clone();
            v.counters = counters;
        });
        self.publish_missed = !stored;
        if stored {
            self.last_publish = Some(Instant::now());
        } else {
            self.publishes_missed += 1;
        }
    }

    /// Events of 7.4 from what changed in the station since the last step.
    fn events(&mut self, st: &mut Station, wall: i64, _changed: bool) {
        let api = self.api.clone();
        // `now` and `asrun`: the as-run rows tell what started and what ended.
        let n = st.as_run.len();
        let from = n.saturating_sub(64);
        for r in &st.as_run[from..] {
            if r.origin != "cart" && !self.seen_started.contains(&r.id) {
                self.seen_started.insert(r.id);
                *self
                    .counters
                    .items_played
                    .entry(r.origin.clone())
                    .or_insert(0) += 1;
                // A row for the item on air: its `now` event goes out when it becomes audible.
                if let Some(info) = st.on_air_info() {
                    if info.as_run_id == Some(r.id) {
                        let nv = now_view(st, &info);
                        self.hold_now = Some((info.started_ms, nv.json));
                    }
                }
            } else if r.origin == "cart" && !self.seen_started.contains(&r.id) {
                self.seen_started.insert(r.id);
                *self.counters.items_played.entry("cart".into()).or_insert(0) += 1;
            }
            if let (Some(ended), false) = (r.ended_ms, self.announced_end.contains(&r.id)) {
                self.announced_end.insert(r.id);
                let data = json!({
                    "started_at": iso_ms(r.started_ms), "ended_at": iso_ms(ended), "title": r.title,
                    "artist": r.artist, "category": r.category, "origin": r.origin,
                    "end_reason": r.end_reason, "played_ms": r.played_ms,
                    "deviation_ms": r.planned_ms.map(|p| r.started_ms - p), "flags": r.flags,
                    "item_id": r.item_id,
                });
                api.emit("asrun", &data);
            }
        }
        if self.seen_started.len() > 4096 {
            let keep: HashSet<i64> = st.as_run[from..].iter().map(|r| r.id).collect();
            self.seen_started.retain(|i| keep.contains(i));
            self.announced_end.retain(|i| keep.contains(i));
        }
        // Alerts: raised, escalated, cleared.
        let active: BTreeMap<i64, (String, String, String, String)> = st
            .alerts()
            .active()
            .into_iter()
            .map(|a| {
                (
                    a.id,
                    (
                        a.code.clone(),
                        a.severity.name().to_string(),
                        a.message.clone(),
                        a.key.clone(),
                    ),
                )
            })
            .collect();
        for (id, (code, sev, msg, _)) in &active {
            if self.known_alerts.get(id) != Some(sev) {
                api.emit(
                    "alert",
                    &json!({"id": id, "code": code, "severity": sev, "message": msg}),
                );
                self.known_alerts.insert(*id, sev.clone());
            }
        }
        let gone: Vec<i64> = self
            .known_alerts
            .keys()
            .filter(|k| !active.contains_key(k))
            .copied()
            .collect();
        for id in gone {
            self.known_alerts.remove(&id);
            api.emit("alert_cleared", &json!({"id": id}));
        }
        // The queue, when its content changed: a hash of what identifies it, so the JSON is built only then.
        let sig = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for e in &st.sched.st.queue {
                (
                    e.id,
                    e.item_id,
                    e.planned_start_ms,
                    e.committed,
                    e.pinned,
                    e.continuity_from_prev,
                    &e.transition,
                )
                    .hash(&mut h);
            }
            // The blocks between the tracks: looked at twice a second, not at every step.
            if self.last_blocks_check.elapsed() >= Duration::from_millis(500) {
                self.last_blocks_check = Instant::now();
                let mut hb = std::collections::hash_map::DefaultHasher::new();
                for (t, v) in block_rows(st, wall) {
                    (v["block_id"].as_i64(), t / 1000).hash(&mut hb);
                }
                self.blocks_sig = hb.finish();
            }
            self.blocks_sig.hash(&mut h);
            h.finish().to_string()
        };
        if sig != self.last_queue {
            self.last_queue = sig;
            let entries = queue_json(st, wall);
            api.emit("queue", &json!({"entries": entries}));
        }
        // The program: its cursor, the waiting block, the back-timing plan and the next block (7.4).
        if self.last_program_check.elapsed() >= Duration::from_millis(500) {
            self.last_program_check = Instant::now();
            let ps = program_state_json(st, wall);
            let sig = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (
                    ps["program"]["run_id"].as_i64(),
                    ps["program"]["cursor"].to_string(),
                    ps["program"]["state"].as_str().map(str::to_string),
                    ps["program"]["waiting"]["block_id"].as_i64(),
                    ps["next_block"]["block_id"].as_i64(),
                    ps["next_block"]["at"].as_str().map(str::to_string),
                    ps["back_time"]["state"].as_str().map(str::to_string),
                    ps["back_time"]["planned_end"].as_str().map(str::to_string),
                    ps["blocks"].as_array().map(|b| {
                        b.iter()
                            .map(|x| x["status"].as_str().unwrap_or("").to_string())
                            .collect::<Vec<_>>()
                    }),
                    ps["expected"]["at"].as_str().map(str::to_string),
                    ps["expected"]["armed"].as_bool(),
                )
                    .hash(&mut h);
                h.finish()
            };
            if sig != self.last_program {
                self.last_program = sig;
                api.emit("program", &ps);
            }
        }
        // The transport and mode.
        let sj = json!({
            "state": state_string(st), "mode": if st.assist { "assist" } else { "auto" },
            "carts": carts_json(st),
            "active_playlist": st.sched.st.program.as_ref().map(|p| json!({"id": p.playlist_id,
                "name": st.sched.prog.playlists.get(&p.playlist_id).map(|pl| pl.name.clone())})),
            "hold": st.transport == Transport::Held, "safe_mode": st.fail.safe_mode,
            // the emergency playlist or the RAM loop: the page shows it, and must see it end
            "operating_mode": st.health().mode,
        });
        let key = sj.to_string();
        if key != self.last_state_event {
            self.last_state_event = key;
            api.emit("state", &sj);
        }
        // `timing` at 1 Hz.
        if self.last_timing.elapsed() >= Duration::from_secs(1) {
            self.last_timing = Instant::now();
            if let Some(i) = st.on_air_info() {
                let nv = now_view(st, &i);
                let elapsed = wall - i.started_ms;
                api.emit(
                    "timing",
                    &json!({
                        "entry_id": i.entry.id, "elapsed_ms": elapsed,
                        "remaining_to_segue_ms": nv.segue_ms.map(|s| s - wall),
                        "remaining_to_end_ms": nv.ends_ms - wall,
                        "next": if nv.next.is_null() { Value::Null } else {
                            json!({"title": nv.next["title"], "artist": nv.next["artist"], "starts_at": nv.next["starts_at"]})
                        },
                    }),
                );
            }
        }
        // `loudness` at 1 Hz (7.4): the readings are the meter's, the rest is the controller's.
        if self.last_loud.elapsed() >= Duration::from_secs(1) {
            self.last_loud = Instant::now();
            api.emit("loudness", &loudness_event(&api));
        }
    }

    /// Send a held `now` event once the item is audible (the audible time is a little ahead of the mixer's
    /// report, by the device delay).
    fn release_now(&mut self, wall: i64) {
        if let Some((at, _)) = &self.hold_now {
            if wall >= *at {
                if let Some((_, data)) = self.hold_now.take() {
                    self.api.emit("now", &data);
                }
            }
        }
    }

    fn exec(
        &mut self,
        st: &mut Station,
        op: &ApiOp,
        frame: u64,
        wall: i64,
        send: &mut dyn FnMut(engine::rt::Command),
    ) -> Exec {
        use ExecResult::{Ok as Done, Refused};
        let run = |st: &mut Station, o: Op, send: &mut dyn FnMut(engine::rt::Command)| match st
            .op(o, frame, wall, send)
        {
            Ok(_) => Exec::Reply(Done(Value::Null)),
            Err(OpError::Busy) => Exec::Busy,
            Err(OpError::Refused(m)) => Exec::Reply(Refused(409, "refused", m)),
        };
        match op {
            ApiOp::Op(o) => run(st, o.clone(), send),
            ApiOp::Play { playlist_id } => match playlist_id {
                Some(id) => run(
                    st,
                    Op::Switch {
                        playlist_id: *id,
                        how: SwitchHow::Now,
                    },
                    send,
                ),
                None => match st.transport {
                    Transport::Playing if st.cond.live_voices() > 0 => Exec::Reply(Refused(
                        409,
                        "refused",
                        "the station is already playing".into(),
                    )),
                    _ => run(st, Op::Resume, send),
                },
            },
            ApiOp::Mode { assist } => run(st, Op::Assist(*assist), send),
            ApiOp::Ack { alert_id } => {
                let code = st
                    .alerts()
                    .active()
                    .into_iter()
                    .find(|a| a.id == *alert_id)
                    .map(|a| a.code.clone());
                match code {
                    None => Exec::Reply(Refused(
                        404,
                        "not_found",
                        format!("no active alert {alert_id}"),
                    )),
                    Some(code) => {
                        if let Some(f) = self.on_ack.as_mut() {
                            f(&code);
                        }
                        st.ack_alert_id(*alert_id);
                        Exec::Reply(Done(json!({"acknowledged": code})))
                    }
                }
            }
            ApiOp::Release { item_id } => {
                st.release_item(*item_id);
                Exec::Reply(Done(json!({"released": item_id})))
            }
            ApiOp::Reload => {
                st.request_sync();
                Exec::Reply(Done(Value::Null))
            }
            ApiOp::Album {
                item_id,
                album_key,
                gapless,
                at,
            } => {
                let key = match (album_key, item_id) {
                    (Some(k), _) => k.clone(),
                    (None, Some(i)) => match st.cat.meta(*i) {
                        Some(m) if !m.album_key.is_empty() => m.album_key.clone(),
                        Some(_) => {
                            return Exec::Reply(Refused(
                                409,
                                "refused",
                                format!("item {i} has no album tag"),
                            ))
                        }
                        None => {
                            return Exec::Reply(Refused(404, "not_found", format!("no item {i}")))
                        }
                    },
                    _ => {
                        return Exec::Reply(Refused(
                            400,
                            "bad_request",
                            "give item_id or album_key".into(),
                        ))
                    }
                };
                let before: HashSet<i64> = st.sched.st.queue.iter().map(|e| e.id).collect();
                let r = st.op(
                    Op::PlayAlbum {
                        album_key: key.clone(),
                        gapless: *gapless,
                        at: *at,
                    },
                    frame,
                    wall,
                    send,
                );
                match r {
                    Err(OpError::Busy) => Exec::Busy,
                    Err(OpError::Refused(m)) => Exec::Reply(Refused(409, "refused", m)),
                    Ok(eff) => {
                        // The entries the album added, in queue order, with the continuity flags.
                        let entries: Vec<Value> = st
                            .sched
                            .st
                            .queue
                            .iter()
                            .filter(|e| !before.contains(&e.id) || eff.entry_id == Some(e.id))
                            .filter(|e| st.cat.meta(e.item_id).is_some_and(|m| m.album_key == key))
                            .map(|e| queue_entry_json(st, e, wall))
                            .collect();
                        Exec::Reply(Done(json!({
                            "album_key": key,
                            "continuity": if *gapless { "gapless" } else { "normal" },
                            "entries": entries,
                        })))
                    }
                }
            }
            ApiOp::Rotation { playlist_id } => {
                let id =
                    playlist_id.or_else(|| st.sched.st.program.as_ref().map(|p| p.playlist_id));
                match id.and_then(|i| rotation_json(st, i)) {
                    Some(v) => Exec::Reply(Done(v)),
                    None => Exec::Reply(Refused(404, "not_found", "no such playlist".into())),
                }
            }
            ApiOp::TransitionPreset { name } => match name {
                None => Exec::Reply(Done(transition_preset_json(st))),
                Some(n) => match st.set_station_preset(n) {
                    Ok(()) => Exec::Reply(Done(transition_preset_json(st))),
                    Err(e) => Exec::Reply(Refused(400, "bad_request", e)),
                },
            },
            ApiOp::LiveSettings { values } => match st.apply_live(values) {
                Ok(done) => {
                    let mut v = transition_preset_json(st);
                    v["applied"] = json!(done);
                    Exec::Reply(Done(v))
                }
                Err(e) => Exec::Reply(Refused(400, "bad_request", e)),
            },
            ApiOp::Upcoming { hours } => {
                let list = st.sched.upcoming(wall, (*hours).clamp(1, 24 * 14));
                Exec::Reply(Done(
                    json!({"events": upcoming_json(&list, wall), "time_zone": st.sched.cfg.zone.name()}),
                ))
            }
            ApiOp::ProgramState => Exec::Reply(Done(program_state_json(st, wall))),
            ApiOp::ProgramStart {
                playlist_id,
                t0,
                how,
                source,
            } => {
                let r = st.op(
                    Op::StartProgram {
                        playlist_id: *playlist_id,
                        t0: *t0,
                        how: *how,
                        source,
                    },
                    frame,
                    wall,
                    send,
                );
                match r {
                    Ok(_) => {
                        let state = program_state_json(st, wall);
                        Exec::Reply(Done(json!({
                            "started": true, "program": state["program"], "past": st.last_start.clone().unwrap_or(json!([])),
                            "expected": state["expected"],
                        })))
                    }
                    Err(OpError::Busy) => Exec::Busy,
                    Err(OpError::Refused(m)) => Exec::Reply(Refused(409, "refused", m)),
                }
            }
            ApiOp::EndAt {
                playlist_id,
                at_ms,
                then,
            } => {
                let r = st.op(
                    Op::EndAt {
                        playlist_id: *playlist_id,
                        at_ms: *at_ms,
                        then: *then,
                    },
                    frame,
                    wall,
                    send,
                );
                match r {
                    Ok(_) => Exec::Reply(Done(
                        json!({"ends_at": iso_ms(*at_ms), "back_time": back_time_json(st)}),
                    )),
                    Err(OpError::Busy) => Exec::Busy,
                    Err(OpError::Refused(m)) => Exec::Reply(Refused(409, "refused", m)),
                }
            }
            ApiOp::ProgramAbort { stop_audio } => match st.program_abort(*stop_audio, send) {
                Ok(()) => Exec::Reply(Done(json!({"aborted": true, "stopped_audio": stop_audio}))),
                Err(e) => Exec::Reply(Refused(409, "refused", e)),
            },
            ApiOp::ProgramContinue => match st.program_continue() {
                Ok(name) => Exec::Reply(Done(json!({"released": name}))),
                Err(e) => Exec::Reply(Refused(409, "refused", e)),
            },
            ApiOp::ProgramArm {
                playlist_id,
                occurrence_ms,
                armed,
            } => match st
                .sched
                .set_armed(*playlist_id, *occurrence_ms, *armed, wall)
            {
                Ok((block, occ)) => {
                    st.mark_dirty();
                    Exec::Reply(Done(
                        json!({"block_id": block, "occurrence": iso_ms(occ), "armed": armed}),
                    ))
                }
                Err(e) => Exec::Reply(Refused(409, "refused", e)),
            },
            ApiOp::ProgramFire { block_id } => match st.program_fire(*block_id, send) {
                Ok(()) => Exec::Reply(Done(json!({"fired": block_id}))),
                Err(e) => Exec::Reply(Refused(409, "refused", e)),
            },
            ApiOp::ProgramSkip => match st.program_skip() {
                Ok(name) => Exec::Reply(Done(json!({"skipped": name}))),
                Err(e) => Exec::Reply(Refused(409, "refused", e)),
            },
            ApiOp::AddChange {
                playlist_id,
                at_ms,
                how,
                mode,
                early_s,
                late_s,
            } => {
                if *at_ms <= wall {
                    return Exec::Reply(Refused(
                        422,
                        "invalid",
                        "`at` must be in the future".into(),
                    ));
                }
                match st.add_schedule_change(*playlist_id, *at_ms, *how, *mode, *early_s, *late_s) {
                    Ok(id) => Exec::Reply(Done(json!({"id": id, "at": iso_ms(*at_ms)}))),
                    Err(e) => Exec::Reply(Refused(404, "not_found", e)),
                }
            }
            ApiOp::CancelChange { id } => {
                if st.cancel_schedule_change(*id) {
                    Exec::Reply(Done(json!({"cancelled": id})))
                } else {
                    Exec::Reply(Refused(
                        404,
                        "not_found",
                        format!("no scheduled change {id}"),
                    ))
                }
            }
            ApiOp::Trim { db } => {
                let now = match db {
                    Some(db) => st.set_output_trim(*db),
                    None => st.opts.output_trim_db,
                };
                Exec::Reply(Done(json!({
                    "output_trim_db": now,
                    "min_db": common::config::OUTPUT_TRIM_MIN_DB,
                    "max_db": common::config::OUTPUT_TRIM_MAX_DB,
                })))
            }
            ApiOp::External { name, value } => {
                if let Ok(mut q) = self.api.external.lock() {
                    q.push_back((wall, name.clone(), value.clone()));
                    while q.len() > 100 {
                        q.pop_front();
                    }
                }
                tracing::info!(name = %name, "external event: {value}");
                // The one name the station acts on: a script's `continue` releases a wait for the operator.
                if name == "continue" {
                    return match st.program_continue() {
                        Ok(b) => Exec::Reply(Done(json!({"accepted": true, "released": b}))),
                        Err(e) => Exec::Reply(Done(
                            json!({"accepted": true, "released": null, "note": e}),
                        )),
                    };
                }
                Exec::Reply(Done(json!({"accepted": true})))
            }
        }
    }
}

enum Exec {
    Busy,
    Reply(ExecResult),
}

enum ExecResult {
    Ok(Value),
    Refused(u16, &'static str, String),
}

/// Replace the published view without ever waiting: `false` when an HTTP thread holds a read lock right now
/// (the caller retries on its next step). The station loop must not block on a lock other threads hold.
pub fn try_store_view(api: &StationApi, f: impl FnOnce(&mut View)) -> bool {
    let mut g = match api.view.try_write() {
        Ok(g) => g,
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return false,
    };
    f(&mut g);
    true
}

/// The raw meter frame for the `/chain/meters` endpoint: the latest frame the meter thread built. Frames are
/// built only while someone listens, so the endpoint asks for them to be built and waits a moment.
/// The `loudness` event and the readings half of `GET /api/v1/loudness` (7.2, 7.4): the meter's readings and
/// the controller's status as of the last 250 ms.
pub fn loudness_event(api: &StationApi) -> Value {
    let l = api
        .meter
        .loud
        .lock()
        .map(|l| *l)
        .unwrap_or([meters::NONE; 4]);
    let f = |v: i16| {
        if v == meters::NONE {
            Value::Null
        } else {
            json!(f64::from(v) / 100.0)
        }
    };
    let st = api.meter.ctl.status.lock().ok().and_then(|s| s.clone());
    let (target, gain, state) = match &st {
        Some(s) if s.enabled => (json!(s.target_lufs), s.gain_db, s.state.name()),
        _ => (Value::Null, 0.0, "off"),
    };
    json!({"target": target, "momentary": f(l[0]), "short_term": f(l[1]), "window": f(l[2]),
           "integrated": f(l[3]), "gain_db": gain, "state": state})
}

pub fn latest_frame(api: &StationApi) -> Option<[u8; FRAME_LEN]> {
    api.meter.latest.lock().ok().and_then(|l| *l)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api() -> (Arc<StationApi>, Receiver<ApiCmd>) {
        StationApi::new(ApiOptions {
            name: "t".into(),
            display_name: "T".into(),
            product: "P".into(),
            media_dir: PathBuf::new(),
            library_db: PathBuf::new(),
            state_db: None,
            device: "null".into(),
            version: "0".into(),
            config_path: None,
            loudness: Default::default(),
            loudness_static_db: 0.0,
        })
    }

    #[test]
    fn publishing_never_waits_for_a_reader() {
        let (api, _rx) = api();
        let a2 = api.clone();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let reader = std::thread::spawn(move || {
            let _g = a2.view.read().unwrap();
            held_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
        });
        held_rx.recv().unwrap();
        let t = Instant::now();
        assert!(!try_store_view(&api, |v| v.health_status = 299));
        assert!(t.elapsed() < Duration::from_millis(50), "it did not wait");
        release_tx.send(()).unwrap();
        reader.join().unwrap();
        assert!(try_store_view(&api, |v| v.health_status = 299));
        assert_eq!(api.view.read().unwrap().health_status, 299);
    }

    #[test]
    fn a_full_command_queue_answers_busy_at_once() {
        let (api, rx) = api();
        // Nobody drains `rx`: COMMAND_QUEUE callers wait, the next is refused without waiting.
        let waiters: Vec<_> = (0..COMMAND_QUEUE)
            .map(|_| {
                let a = api.clone();
                std::thread::spawn(move || a.command(ApiOp::Reload))
            })
            .collect();
        std::thread::sleep(Duration::from_millis(300));
        let t = Instant::now();
        loop {
            // Wait until the queue has filled.
            let r = api.command(ApiOp::Reload);
            if let ApiReply::Refused {
                status: 503,
                code: "busy",
                ..
            } = r
            {
                break;
            }
            assert!(t.elapsed() < Duration::from_secs(20), "never refused");
        }
        drop(rx);
        for w in waiters {
            assert!(matches!(w.join().unwrap(), ApiReply::Refused { .. }));
        }
    }
}
