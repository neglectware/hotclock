//! The station's programming: playlists as ordered entry lists of items, album blocks and control
//! blocks (architecture 4.1, 4.4, 4.5), and the station blocks. Built from `library::playlists` rows.
//! M1.5 ran items, album blocks and the standing `insert_every` block; M3 adds the timed blocks
//! (`insert`, `play`, `start_playlist`, `stop`, `fade_stop`, `end`), the `start` block's scheduled
//! occurrences and the validator's rules for them. The in-sequence side blocks (`osc_out`, `midi_out`,
//! `http_request`, `set_gain`, `set_mute`, `wait`, `loop`) are parsed as `Other` and run from M5.
//!
//! The timing of a timed block lives in its `params` (the library stores every block's own fields there):
//!
//! ```json
//! { "when": {"kind": "recurrence", "cron": "0 * * * *", "skip_dates": []},
//!   "mode": "soft_then_hard", "window_early_s": 60, "window_late_s": 30,
//!   "fill": {"category": "sweepers"}, "late_policy": "default", "late_window_s": 60,
//!   "source": {"category": "ids"} }
//! ```
//!
//! `when` is a [`common::timespec::TimeSpec`] (`relative`, `local`, `recurrence` or `date`).

use crate::side::{MidiKind, MidiMsg, OscArg, SideAction};
use common::timespec::{TimeSpec, Zone};
use library::playlists::{BlockRow, EntryKind, Mode, PlaylistRow};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Source {
    Category {
        id: Option<i64>,
        name: Option<String>,
    },
    Playlist(i64),
    Item(i64),
}

impl Source {
    pub fn from_json(v: &Value) -> Option<Source> {
        if let Some(id) = v.get("category_id").and_then(Value::as_i64) {
            return Some(Source::Category {
                id: Some(id),
                name: None,
            });
        }
        if let Some(n) = v.get("category").and_then(Value::as_str) {
            return Some(Source::Category {
                id: None,
                name: Some(n.to_string()),
            });
        }
        if let Some(id) = v.get("playlist_id").and_then(Value::as_i64) {
            return Some(Source::Playlist(id));
        }
        v.get("item_id").and_then(Value::as_i64).map(Source::Item)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InsertEvery {
    pub n: u32,
    pub source: Source,
    pub count_kind: String,
    pub min_gap_minutes: i64,
    pub resets_on: Vec<String>,
    pub inherit: bool,
}

/// What starts a block (4.4's table): the whole time a playlist runs, when the running order reaches it,
/// at a time, or (the `start` block) as the first entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    #[default]
    Standing,
    Sequence,
    Timed,
    First,
}

impl Trigger {
    pub fn parse(s: &str) -> Trigger {
        match s {
            "sequence" => Trigger::Sequence,
            "timed" => Trigger::Timed,
            "first" => Trigger::First,
            _ => Trigger::Standing,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Standing => "standing",
            Trigger::Sequence => "sequence",
            Trigger::Timed => "timed",
            Trigger::First => "first",
        }
    }
}

/// The modes of a timed block that acts at a boundary (4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryMode {
    /// A boundary inside the window, planned by best fit; a hard fallback at `T + late`.
    SoftThenHard,
    /// The first boundary at or after `T`, planned by best fit, never hard.
    Soft,
    /// Exactly at `T`: the hard-time transition of 2.5.
    Hard,
    /// The first boundary after `T`, with no planning (Rivendell's "wait").
    HardWait,
}

impl BoundaryMode {
    pub fn parse(s: &str) -> Option<BoundaryMode> {
        Some(match s {
            "soft_then_hard" => BoundaryMode::SoftThenHard,
            "soft" => BoundaryMode::Soft,
            "hard" => BoundaryMode::Hard,
            "hard_wait" => BoundaryMode::HardWait,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            BoundaryMode::SoftThenHard => "soft_then_hard",
            BoundaryMode::Soft => "soft",
            BoundaryMode::Hard => "hard",
            BoundaryMode::HardWait => "hard_wait",
        }
    }

    /// Does the block cut in at the end of its window?
    pub fn hard_fallback(self) -> bool {
        matches!(self, BoundaryMode::SoftThenHard | BoundaryMode::Hard)
    }

    /// Is the plan fitted so that a boundary falls inside the window?
    pub fn fitted(self) -> bool {
        matches!(self, BoundaryMode::SoftThenHard | BoundaryMode::Soft)
    }
}

/// What a missed timed block does (4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatePolicy {
    #[default]
    Default,
    Skip,
    RunNow,
    RunIfWithin,
}

impl LatePolicy {
    pub fn parse(s: &str) -> Option<LatePolicy> {
        Some(match s {
            "default" => LatePolicy::Default,
            "skip" => LatePolicy::Skip,
            "run_now" => LatePolicy::RunNow,
            "run_if_within" => LatePolicy::RunIfWithin,
            _ => return None,
        })
    }
}

/// When and how a timed block acts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Timed {
    pub spec: TimeSpec,
    pub mode: BoundaryMode,
    /// The window `[T - early, T + late]`, seconds.
    pub early_s: i64,
    pub late_s: i64,
    /// Sweepers and short bumpers that may close a gap before the window (4.6 step 3).
    pub fill: Option<Source>,
    pub late_policy: LatePolicy,
    pub late_window_s: Option<i64>,
}

impl Timed {
    pub fn early_ms(&self) -> i64 {
        self.early_s * 1000
    }

    pub fn late_ms(&self) -> i64 {
        self.late_s * 1000
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartHow {
    Now,
    Segue,
    End,
}

impl StartHow {
    pub fn parse(s: &str) -> Option<StartHow> {
        Some(match s {
            "now" => StartHow::Now,
            "segue" => StartHow::Segue,
            "end" => StartHow::End,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    Manual,
    Absolute,
}

/// The `start` block (4.5): how a playlist is started, and the occurrences it expects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartBlock {
    /// Recurrences and dates, with the block's skip dates applied to each.
    pub schedule: Vec<TimeSpec>,
    pub anchor: Anchor,
    pub how: StartHow,
    pub mode: BoundaryMode,
    pub early_s: i64,
    pub late_s: i64,
    pub arm_window_min: i64,
    /// `fallback_start`: auto-start a `manual` block this many seconds after its nominal time.
    pub fallback_grace_s: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Until {
    /// The cursor moves on at once; the source keeps playing until a later block takes over (4.5).
    Next,
    /// The cursor stays here; only a timed flow block (or an operator action) ends it.
    Forever,
    /// Until a time (relative to T0, or local time of day): the cursor stays until then.
    At(TimeSpec),
    Duration(i64),
    Count(u32),
}

/// How a `play` with an end time ends there (4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndMode {
    #[default]
    Boundary,
    Fade,
    /// Back-timing (4.9): a track ends at the target.
    Natural,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackMode {
    #[default]
    BestFit,
    /// The old `end_at`: the closing item's cue-out lands on the target to within one period.
    Exact,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BackTime {
    pub mode: BackMode,
    pub tolerance_s: f64,
}

pub const DEFAULT_TOLERANCE_S: f64 = 45.0;
/// The fade of the item before a back-timed closing item that is cut in at its hard time (4.9, `exact`).
pub const EXACT_FADE_S: f64 = 2.0;
pub const DEFAULT_FADE_S: f64 = 7.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayBlock {
    pub source: Source,
    pub until: Until,
    pub end: EndMode,
    pub fade_s: f64,
    pub back_time: Option<BackTime>,
}

impl PlayBlock {
    /// `until: next`, ending at a boundary: the default block.
    pub fn next(source: Source) -> PlayBlock {
        PlayBlock {
            source,
            until: Until::Next,
            end: EndMode::Boundary,
            fade_s: DEFAULT_FADE_S,
            back_time: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WaitUntil {
    At(TimeSpec),
    DurationS(i64),
    /// Until the operator sends Continue.
    Operator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopHow {
    Now,
    Boundary,
    AfterCurrent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BlockKind {
    InsertEvery(InsertEvery),
    /// One item into the queue at a boundary without moving the cursor: the top-of-hour ID.
    Insert {
        source: Source,
    },
    Start(StartBlock),
    Play(PlayBlock),
    StartPlaylist {
        playlist_id: i64,
        how: StartHow,
    },
    Stop {
        how: StopHow,
        fade_ms: i64,
    },
    FadeStop {
        seconds: f64,
    },
    End,
    Wait(WaitUntil),
    /// `to` is a 1-based position in the playlist; `None` is the first entry after `start`. `times: None`
    /// loops forever.
    Loop {
        to: Option<usize>,
        times: Option<u32>,
    },
    /// `osc_out`, `midi_out` and `http_request`.
    Side(SideAction),
    SetGain {
        gain_db: f64,
        ramp_ms: i64,
    },
    SetMute {
        on: bool,
        ramp_ms: i64,
    },
    /// A type this build does not know: stored, reported by the validator.
    Other(String),
}

impl BlockKind {
    pub fn type_name(&self) -> &str {
        match self {
            BlockKind::InsertEvery(_) => "insert_every",
            BlockKind::Insert { .. } => "insert",
            BlockKind::Start(_) => "start",
            BlockKind::Play(_) => "play",
            BlockKind::StartPlaylist { .. } => "start_playlist",
            BlockKind::Stop { .. } => "stop",
            BlockKind::FadeStop { .. } => "fade_stop",
            BlockKind::End => "end",
            BlockKind::Wait(_) => "wait",
            BlockKind::Loop { .. } => "loop",
            BlockKind::Side(a) => a.type_name(),
            BlockKind::SetGain { .. } => "set_gain",
            BlockKind::SetMute { .. } => "set_mute",
            BlockKind::Other(t) => t.split(' ').next().unwrap_or(t),
        }
    }

    /// Flow blocks move the program's cursor (4.4); the rest are side blocks.
    pub fn is_flow(&self) -> bool {
        matches!(
            self,
            BlockKind::Play(_)
                | BlockKind::StartPlaylist { .. }
                | BlockKind::Stop { .. }
                | BlockKind::FadeStop { .. }
                | BlockKind::End
                | BlockKind::Wait(_)
                | BlockKind::Loop { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub id: i64,
    pub name: String,
    pub priority: i64,
    pub enabled: bool,
    pub kind: BlockKind,
    #[serde(default)]
    pub trigger: Trigger,
    #[serde(default)]
    pub timed: Option<Timed>,
    /// What is wrong with the row's own fields (an unreadable time, an unknown mode): the validator
    /// reports it as an error and the scheduler never runs the block.
    #[serde(default)]
    pub problem: Option<String>,
}

/// The default window of a timed block of this type (4.6): the ID's is 60 s early and 30 s late; a
/// `play` or `start_playlist` at a time is 0 early and 30 s late.
fn default_window(kind: &str) -> (i64, i64) {
    match kind {
        "insert" => (60, 30),
        _ => (0, 30),
    }
}

fn parse_timed(kind: &str, p: &Value) -> Result<Timed, String> {
    let when = p
        .get("when")
        .ok_or_else(|| "a timed block needs `when`".to_string())?;
    let spec: TimeSpec =
        serde_json::from_value(when.clone()).map_err(|e| format!("`when` is not readable: {e}"))?;
    spec.check()?;
    let mode = match p.get("mode").and_then(Value::as_str) {
        None => BoundaryMode::SoftThenHard,
        Some(m) => BoundaryMode::parse(m).ok_or_else(|| format!("unknown mode `{m}`"))?,
    };
    let (de, dl) = default_window(kind);
    let early_s = p
        .get("window_early_s")
        .and_then(Value::as_i64)
        .unwrap_or(de);
    let late_s = p.get("window_late_s").and_then(Value::as_i64).unwrap_or(dl);
    if early_s < 0 || late_s < 0 {
        return Err("a window cannot be negative".into());
    }
    let late_policy = match p.get("late_policy").and_then(Value::as_str) {
        None => LatePolicy::Default,
        Some(l) => LatePolicy::parse(l).ok_or_else(|| format!("unknown late_policy `{l}`"))?,
    };
    Ok(Timed {
        spec,
        mode,
        early_s,
        late_s,
        fill: p.get("fill").and_then(Source::from_json),
        late_policy,
        late_window_s: p.get("late_window_s").and_then(Value::as_i64),
    })
}

fn parse_start(p: &Value) -> Result<StartBlock, String> {
    let skip: Vec<String> = p
        .get("skip_dates")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mut schedule = Vec::new();
    for s in p
        .get("schedule")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let mut spec = if let Some(c) = s.get("cron").and_then(Value::as_str) {
            TimeSpec::recurrence(c)?
        } else if let Some(d) = s.get("date").and_then(Value::as_str) {
            TimeSpec::date(d)?
        } else {
            return Err("a schedule entry has `cron` or `date`".into());
        };
        spec.skip_dates = skip.clone();
        spec.check()?;
        schedule.push(spec);
    }
    let anchor = match p.get("anchor").and_then(Value::as_str) {
        None | Some("manual") => Anchor::Manual,
        Some("absolute") => Anchor::Absolute,
        Some(a) => return Err(format!("unknown anchor `{a}`")),
    };
    let how = match p.get("how").and_then(Value::as_str) {
        None => StartHow::Segue,
        Some(h) => StartHow::parse(h).ok_or_else(|| format!("unknown how `{h}`"))?,
    };
    // The old daypart behavior: the first boundary after the time, never a cut (4.6).
    let mode = match p.get("mode").and_then(Value::as_str) {
        None => match how {
            StartHow::Now => BoundaryMode::Hard,
            _ => BoundaryMode::Soft,
        },
        Some(m) => BoundaryMode::parse(m).ok_or_else(|| format!("unknown mode `{m}`"))?,
    };
    Ok(StartBlock {
        schedule,
        anchor,
        how,
        mode,
        early_s: p.get("window_early_s").and_then(Value::as_i64).unwrap_or(0),
        late_s: p
            .get("window_late_s")
            .and_then(Value::as_i64)
            .unwrap_or(300),
        arm_window_min: p
            .get("arm_window_min")
            .and_then(Value::as_i64)
            .unwrap_or(60),
        fallback_grace_s: p
            .get("fallback_start")
            .filter(|f| f.get("enabled").and_then(Value::as_bool) == Some(true))
            .map(|f| f.get("grace_s").and_then(Value::as_i64).unwrap_or(0)),
    })
}

fn parse_spec(v: &Value) -> Result<TimeSpec, String> {
    let spec: TimeSpec =
        serde_json::from_value(v.clone()).map_err(|e| format!("a time is not readable: {e}"))?;
    spec.check()?;
    Ok(spec)
}

fn parse_play(source: Source, p: &Value) -> Result<PlayBlock, String> {
    let until = match p.get("until") {
        None | Some(Value::Null) => Until::Next,
        Some(Value::String(s)) if s == "next" => Until::Next,
        Some(Value::String(s)) if s == "forever" => Until::Forever,
        Some(v) if v.get("duration_s").is_some() => Until::Duration(
            v["duration_s"]
                .as_i64()
                .filter(|d| *d > 0)
                .ok_or("`duration_s` must be a positive number of seconds")?,
        ),
        Some(v) if v.get("count").is_some() => Until::Count(
            v["count"]
                .as_u64()
                .filter(|c| *c >= 1)
                .ok_or("`count` must be at least 1")? as u32,
        ),
        Some(v) if v.get("at").is_some() => Until::At(parse_spec(&v["at"])?),
        Some(_) => return Err("`until` is next, forever, or one of at, duration_s, count".into()),
    };
    let end = match p.get("end").and_then(Value::as_str) {
        None | Some("boundary") => EndMode::Boundary,
        Some("fade") => EndMode::Fade,
        Some("natural") => EndMode::Natural,
        Some(e) => return Err(format!("unknown end `{e}`")),
    };
    let back_time = match (end, p.get("back_time")) {
        (EndMode::Natural, bt) => {
            let mode = match bt.and_then(|b| b.get("mode")).and_then(Value::as_str) {
                None | Some("best_fit") => BackMode::BestFit,
                Some("exact") => BackMode::Exact,
                Some(m) => return Err(format!("unknown back_time mode `{m}`")),
            };
            let tolerance_s = bt
                .and_then(|b| b.get("tolerance_s"))
                .and_then(Value::as_f64)
                .unwrap_or(DEFAULT_TOLERANCE_S);
            if tolerance_s <= 0.0 {
                return Err("the back-timing tolerance must be above 0".into());
            }
            Some(BackTime { mode, tolerance_s })
        }
        _ => None,
    };
    let fade_s = p
        .get("fade_s")
        .and_then(Value::as_f64)
        .unwrap_or(DEFAULT_FADE_S);
    if !(0.0..=120.0).contains(&fade_s) {
        return Err("`fade_s` is 0 to 120 seconds".into());
    }
    Ok(PlayBlock {
        source,
        until,
        end,
        fade_s,
        back_time,
    })
}

fn parse_wait(p: &Value) -> Result<WaitUntil, String> {
    match p.get("until") {
        Some(Value::String(s)) if s == "operator" => Ok(WaitUntil::Operator),
        Some(v) if v.get("duration_s").is_some() => Ok(WaitUntil::DurationS(
            v["duration_s"]
                .as_i64()
                .filter(|d| *d >= 0)
                .ok_or("`duration_s` must be zero or more seconds")?,
        )),
        Some(v) if v.get("at").is_some() => Ok(WaitUntil::At(parse_spec(&v["at"])?)),
        _ => Err("a wait block needs `until`: operator, {duration_s}, or {at}".into()),
    }
}

fn parse_side(kind: &str, p: &Value) -> Result<SideAction, String> {
    match kind {
        "osc_out" => {
            let host = p
                .get("host")
                .and_then(Value::as_str)
                .filter(|h| !h.is_empty())
                .ok_or("an OSC block needs a host")?
                .to_string();
            let port = p
                .get("port")
                .and_then(Value::as_u64)
                .filter(|p| (1..=65535).contains(p))
                .ok_or("an OSC block needs a port from 1 to 65535")? as u16;
            let address = p
                .get("address")
                .and_then(Value::as_str)
                .filter(|a| a.starts_with('/'))
                .ok_or("an OSC address starts with /")?
                .to_string();
            let mut args = Vec::new();
            for a in p
                .get("args")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let t = a.get("type").and_then(Value::as_str).unwrap_or("");
                let v = &a["value"];
                args.push(match t {
                    "i" => OscArg::Int(
                        v.as_i64()
                            .filter(|i| i32::try_from(*i).is_ok())
                            .ok_or("an `i` argument is a whole number")?
                            as i32,
                    ),
                    "f" => OscArg::Float(v.as_f64().ok_or("an `f` argument is a number")? as f32),
                    "s" => OscArg::Str(v.as_str().ok_or("an `s` argument is text")?.to_string()),
                    _ => return Err("an OSC argument has type i, f or s".into()),
                });
            }
            Ok(SideAction::Osc {
                host,
                port,
                address,
                args,
            })
        }
        "midi_out" => {
            let port = p
                .get("port")
                .and_then(Value::as_str)
                .filter(|h| !h.is_empty())
                .ok_or("a MIDI block needs a port")?
                .to_string();
            let m = p.get("message").ok_or("a MIDI block needs a message")?;
            let kind = match m.get("type").and_then(Value::as_str) {
                Some("cc") => MidiKind::Cc,
                Some("note_on") => MidiKind::NoteOn,
                Some("note_off") => MidiKind::NoteOff,
                Some("pc") => MidiKind::Pc,
                Some("sysex") => MidiKind::Sysex,
                _ => return Err("a MIDI message is cc, note_on, note_off, pc or sysex".into()),
            };
            let num = |k: &str| m.get(k).and_then(Value::as_u64).unwrap_or(0);
            let msg = MidiMsg {
                kind,
                channel: num("channel").min(255) as u8,
                data1: num("data1").min(255) as u8,
                data2: num("data2").min(255) as u8,
                data: m
                    .get("data")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_u64)
                            .map(|b| b.min(255) as u8)
                            .collect()
                    })
                    .unwrap_or_default(),
            };
            crate::side::midi_bytes(&msg)?;
            Ok(SideAction::Midi { port, msg })
        }
        _ => {
            let method = p
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("POST")
                .to_ascii_uppercase();
            if !matches!(method.as_str(), "GET" | "POST" | "PUT" | "PATCH" | "DELETE") {
                return Err("a web request is GET, POST, PUT, PATCH or DELETE".into());
            }
            let url = p
                .get("url")
                .and_then(Value::as_str)
                .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
                .ok_or("a web request needs an http:// address")?
                .to_string();
            let headers = p
                .get("headers")
                .and_then(Value::as_object)
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            Ok(SideAction::Http {
                method,
                url,
                headers,
                body: p
                    .get("body")
                    .and_then(Value::as_str)
                    .filter(|b| !b.is_empty())
                    .map(str::to_string),
                timeout_s: p
                    .get("timeout_s")
                    .and_then(Value::as_u64)
                    .unwrap_or(5)
                    .clamp(1, 30),
            })
        }
    }
}

impl Block {
    /// A block built in code (tests, the CLI): no timing problem, trigger as given.
    pub fn new(
        id: i64,
        name: &str,
        kind: BlockKind,
        trigger: Trigger,
        timed: Option<Timed>,
    ) -> Block {
        Block {
            id,
            name: name.into(),
            priority: 100,
            enabled: true,
            kind,
            trigger,
            timed,
            problem: None,
        }
    }

    pub fn from_row(r: &BlockRow) -> Block {
        let p = &r.params;
        let trigger = Trigger::parse(&r.trigger);
        let mut problem: Option<String> = None;
        let mut timed = None;
        if trigger == Trigger::Timed {
            match parse_timed(&r.kind, p) {
                Ok(t) => timed = Some(t),
                Err(e) => problem = Some(e),
            }
        }
        let source = p.get("source").and_then(Source::from_json);
        let kind = match r.kind.as_str() {
            "insert_every" => match source {
                Some(source) => BlockKind::InsertEvery(InsertEvery {
                    n: p.get("n").and_then(Value::as_u64).unwrap_or(0) as u32,
                    source,
                    count_kind: p
                        .get("count_kind")
                        .and_then(Value::as_str)
                        .unwrap_or("music")
                        .to_string(),
                    min_gap_minutes: p
                        .get("min_gap_minutes")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    resets_on: p
                        .get("resets_on")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_else(|| vec!["id".to_string()]),
                    inherit: p.get("inherit").and_then(Value::as_bool).unwrap_or(true),
                }),
                None => BlockKind::Other("insert_every (no readable source)".into()),
            },
            "insert" => match source {
                Some(source) => BlockKind::Insert { source },
                None => {
                    problem.get_or_insert_with(|| "an insert block needs a source".into());
                    BlockKind::Other("insert (no readable source)".into())
                }
            },
            "start" => match parse_start(p) {
                Ok(s) => BlockKind::Start(s),
                Err(e) => {
                    problem.get_or_insert(e);
                    BlockKind::Other("start (unreadable)".into())
                }
            },
            "play" => match source {
                Some(source) => match parse_play(source, p) {
                    Ok(pb) => BlockKind::Play(pb),
                    Err(e) => {
                        problem.get_or_insert(e);
                        BlockKind::Other("play (unreadable)".into())
                    }
                },
                None => {
                    problem.get_or_insert_with(|| "a play block needs a source".into());
                    BlockKind::Other("play (no readable source)".into())
                }
            },
            "start_playlist" => match p.get("playlist_id").and_then(Value::as_i64) {
                Some(playlist_id) => BlockKind::StartPlaylist {
                    playlist_id,
                    how: p
                        .get("how")
                        .and_then(Value::as_str)
                        .and_then(StartHow::parse)
                        .unwrap_or(StartHow::Segue),
                },
                None => {
                    problem.get_or_insert_with(|| "start_playlist needs playlist_id".into());
                    BlockKind::Other("start_playlist (no playlist)".into())
                }
            },
            "stop" => BlockKind::Stop {
                how: match p.get("how").and_then(Value::as_str) {
                    Some("boundary") => StopHow::Boundary,
                    Some("after_current") => StopHow::AfterCurrent,
                    _ => StopHow::Now,
                },
                fade_ms: p.get("fade_ms").and_then(Value::as_i64).unwrap_or(2000),
            },
            "fade_stop" => BlockKind::FadeStop {
                seconds: p.get("seconds").and_then(Value::as_f64).unwrap_or(7.0),
            },
            "end" => BlockKind::End,
            "wait" => match parse_wait(p) {
                Ok(w) => BlockKind::Wait(w),
                Err(e) => {
                    problem.get_or_insert(e);
                    BlockKind::Other("wait (unreadable)".into())
                }
            },
            "loop" => BlockKind::Loop {
                to: p.get("to").and_then(Value::as_u64).map(|t| t as usize),
                times: match p.get("times") {
                    None | Some(Value::Null) => Some(1),
                    Some(Value::String(s)) if s == "forever" => None,
                    Some(v) => v.as_u64().map(|t| t as u32),
                },
            },
            "osc_out" | "midi_out" | "http_request" => match parse_side(&r.kind, p) {
                Ok(a) => BlockKind::Side(a),
                Err(e) => {
                    problem.get_or_insert(e);
                    BlockKind::Other(format!("{} (unreadable)", r.kind))
                }
            },
            "set_gain" => BlockKind::SetGain {
                gain_db: p.get("gain_db").and_then(Value::as_f64).unwrap_or(0.0),
                ramp_ms: p.get("ramp_ms").and_then(Value::as_i64).unwrap_or(0).max(0),
            },
            "set_mute" => BlockKind::SetMute {
                on: p.get("on").and_then(Value::as_bool).unwrap_or(true),
                ramp_ms: p.get("ramp_ms").and_then(Value::as_i64).unwrap_or(0).max(0),
            },
            other => {
                problem.get_or_insert_with(|| {
                    format!("`{other}` is not a block type this version knows")
                });
                BlockKind::Other(other.to_string())
            }
        };
        // Which triggers a type allows (4.5): `wait` and `loop` only run in sequence; only the insert rule is
        // standing; a timed block needs a type that can act at a time.
        if trigger == Trigger::Timed
            && matches!(
                kind,
                BlockKind::Wait(_)
                    | BlockKind::Loop { .. }
                    | BlockKind::InsertEvery(_)
                    | BlockKind::Start(_)
            )
        {
            problem.get_or_insert_with(|| {
                format!(
                    "a {} block runs in sequence, not at a time",
                    kind.type_name()
                )
            });
        }
        if trigger == Trigger::Standing
            && !matches!(
                kind,
                BlockKind::InsertEvery(_) | BlockKind::Start(_) | BlockKind::Other(_)
            )
        {
            problem.get_or_insert_with(|| {
                format!(
                    "a {} block is in sequence or at a time; only an insert rule is standing",
                    kind.type_name()
                )
            });
        }
        Block {
            id: r.id,
            name: r.name.clone(),
            priority: r.priority,
            enabled: r.enabled,
            kind,
            trigger,
            timed,
            problem,
        }
    }

    /// The item sources the block names (the station's track cache must hold their items).
    pub fn sources(&self) -> Vec<&Source> {
        let mut out = Vec::new();
        match &self.kind {
            BlockKind::InsertEvery(ie) => out.push(&ie.source),
            BlockKind::Insert { source } => out.push(source),
            BlockKind::Play(p) => out.push(&p.source),
            _ => {}
        }
        if let Some(f) = self.timed.as_ref().and_then(|t| t.fill.as_ref()) {
            out.push(f);
        }
        out
    }

    pub fn is_timed_flow(&self) -> bool {
        self.timed.is_some() && self.kind.is_flow()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum Entry {
    Item {
        item_id: i64,
        weight: f64,
    },
    Album {
        album_key: String,
        gapless: bool,
        weight: f64,
    },
    Block(Block),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Separation {
    pub title_minutes: i64,
    pub artist_minutes: i64,
    pub album_minutes: i64,
}

impl Separation {
    fn from_json(v: &Value) -> Separation {
        let get = |k: &str, d: i64| v.get(k).and_then(Value::as_i64).unwrap_or(d);
        Separation {
            title_minutes: get("sep_title_minutes", 60),
            artist_minutes: get("sep_artist_minutes", 0),
            album_minutes: get("sep_album_minutes", 0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub mode: Mode,
    pub no_repeat_minutes: i64,
    pub separation: Separation,
    pub station_blocks: bool,
    pub enabled: bool,
    pub entries: Vec<Entry>,
    /// The station this playlist belongs to (a program made for one station); none: any station.
    pub station_id: Option<String>,
}

impl Playlist {
    pub fn from_row(r: &PlaylistRow) -> Playlist {
        Playlist {
            id: r.id,
            name: r.name.clone(),
            mode: r.mode,
            no_repeat_minutes: r.no_repeat_minutes,
            separation: Separation::from_json(&r.separation),
            station_blocks: r.station_blocks,
            enabled: r.enabled,
            station_id: r.station_id.clone(),
            entries: r
                .entries
                .iter()
                .map(|e| match &e.kind {
                    EntryKind::Item { item_id } => Entry::Item {
                        item_id: *item_id,
                        weight: e.weight,
                    },
                    EntryKind::Album {
                        album_key,
                        continuity,
                    } => Entry::Album {
                        album_key: album_key.clone(),
                        gapless: continuity != "normal",
                        weight: e.weight,
                    },
                    EntryKind::Block(b) => Entry::Block(Block::from_row(b)),
                })
                .collect(),
        }
    }

    /// The item ids the playlist names directly.
    pub fn item_ids(&self) -> Vec<i64> {
        self.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Item { item_id, .. } => Some(*item_id),
                _ => None,
            })
            .collect()
    }

    pub fn blocks(&self) -> impl Iterator<Item = &Block> {
        self.entries.iter().filter_map(|e| match e {
            Entry::Block(b) => Some(b),
            _ => None,
        })
    }

    /// The `start` block, when the playlist has one.
    pub fn start_block(&self) -> Option<(&Block, &StartBlock)> {
        self.blocks().find_map(|b| match &b.kind {
            BlockKind::Start(s) => Some((b, s)),
            _ => None,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Programming {
    pub playlists: BTreeMap<i64, Playlist>,
    pub station_blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub playlist: Option<i64>,
    pub block: Option<i64>,
    pub error: bool,
    /// A stable name of the rule, for the editor and the tests.
    pub code: &'static str,
    pub message: String,
}

/// What the validator cannot see in the programming itself: the library's categories and items. A
/// check that needs it is skipped when the caller gives none.
pub trait Refs {
    fn category_exists(&self, id: Option<i64>, name: Option<&str>) -> bool;
    fn item_exists(&self, id: i64) -> bool;
    /// How many playable items a category holds, when known.
    fn category_playable(&self, _id: Option<i64>, _name: Option<&str>) -> Option<usize> {
        None
    }
    /// The effective length of the shortest playable item of a category, ms, when known.
    fn category_min_ms(&self, _id: Option<i64>, _name: Option<&str>) -> Option<i64> {
        None
    }
    /// The effective length of the shortest playable item of a playlist's own items, ms, when known.
    fn playlist_min_ms(&self, _playlist_id: i64) -> Option<i64> {
        None
    }
}

pub struct NoRefs;

impl Refs for NoRefs {
    fn category_exists(&self, _: Option<i64>, _: Option<&str>) -> bool {
        true
    }
    fn item_exists(&self, _: i64) -> bool {
        true
    }
}

/// Nesting deeper than this is refused (4.4).
pub const MAX_NESTING: usize = 4;

impl Programming {
    pub fn from_rows(playlists: &[PlaylistRow], station_blocks: &[BlockRow]) -> Programming {
        Programming {
            playlists: playlists
                .iter()
                .map(|r| (r.id, Playlist::from_row(r)))
                .collect(),
            station_blocks: station_blocks.iter().map(Block::from_row).collect(),
        }
    }

    pub fn by_name(&self, name: &str) -> Option<&Playlist> {
        self.playlists.values().find(|p| p.name == name)
    }

    /// Every `start` block of every enabled playlist: the weekly grid and the scheduled starts.
    pub fn scheduled_starts(&self) -> Vec<(i64, &Block, &StartBlock)> {
        self.playlists
            .values()
            .filter(|p| p.enabled)
            .filter_map(|p| p.start_block().map(|(b, s)| (p.id, b, s)))
            .filter(|(_, b, _)| b.enabled)
            .collect()
    }

    /// The checks of 4.4 that need nothing outside the programming.
    pub fn validate(&self) -> Vec<Problem> {
        self.validate_with(&NoRefs, None)
    }

    /// The validator of 4.4 for the block types M3 runs (and the cheap checks of the others).
    pub fn validate_with(&self, refs: &dyn Refs, zone: Option<&Zone>) -> Vec<Problem> {
        let mut out = Vec::new();
        for p in self.playlists.values() {
            self.validate_playlist(p, refs, zone, &mut out);
        }
        for b in &self.station_blocks {
            let mut push = |error: bool, code: &'static str, message: String| {
                out.push(Problem {
                    playlist: None,
                    block: Some(b.id),
                    error,
                    code,
                    message,
                })
            };
            if let Some(m) = &b.problem {
                push(true, "unreadable", m.clone());
            }
            if b.kind.is_flow() || matches!(b.kind, BlockKind::Start(_)) {
                push(
                    true,
                    "station_flow_block",
                    format!(
                        "a station block cannot be a {} block (flow blocks belong to a program)",
                        b.kind.type_name()
                    ),
                );
            }
            if let Some(t) = &b.timed {
                if t.spec.needs_t0() {
                    push(
                        true,
                        "station_relative_time",
                        "a station block has no T0; give it a recurrence or a date".into(),
                    );
                }
            }
            self.check_sources(b, refs, &mut |code, m| push(true, code, m));
            if let (BlockKind::InsertEvery(ie), true) = (&b.kind, true) {
                if ie.n < 1 {
                    push(
                        true,
                        "insert_every_n",
                        "insert_every needs n of at least 1".into(),
                    );
                }
            }
        }
        // Two playlists of one station with recurring `start` blocks at the same time.
        let starts = self.scheduled_starts();
        for (i, (pa, ba, sa)) in starts.iter().enumerate() {
            for (pb, bb, sb) in starts.iter().skip(i + 1) {
                if pa == pb {
                    continue;
                }
                let clash = sa.schedule.iter().any(|x| {
                    sb.schedule.iter().any(|y| match (&x.when, &y.when) {
                        (
                            common::timespec::When::Recurrence { cron: cx },
                            common::timespec::When::Recurrence { cron: cy },
                        ) => {
                            let (tx, ty) = (cx.times_of_day(), cy.times_of_day());
                            let share_time = tx.iter().any(|t| ty.contains(t));
                            let share_day = match (cx.weekdays(), cy.weekdays()) {
                                (Some(a), Some(b)) => a.iter().any(|d| b.contains(d)),
                                _ => true,
                            };
                            share_time && share_day
                        }
                        _ => false,
                    })
                });
                if clash {
                    out.push(Problem {
                        playlist: Some(*pa),
                        block: Some(ba.id),
                        error: true,
                        code: "start_clash",
                        message: format!(
                            "playlists {} and {} both start at the same time (blocks {} and {})",
                            pa, pb, ba.id, bb.id
                        ),
                    });
                }
            }
        }
        out
    }

    fn check_sources(&self, b: &Block, refs: &dyn Refs, bad: &mut dyn FnMut(&'static str, String)) {
        for s in b.sources() {
            match s {
                Source::Playlist(id) => {
                    if !self.playlists.contains_key(id) {
                        bad(
                            "source_missing",
                            format!("its source playlist {id} does not exist"),
                        );
                    }
                }
                Source::Category { id, name } => {
                    if !refs.category_exists(*id, name.as_deref()) {
                        bad(
                            "source_missing",
                            "its source category does not exist".into(),
                        );
                    }
                }
                Source::Item(i) => {
                    if !refs.item_exists(*i) {
                        bad(
                            "source_missing",
                            format!("its source item {i} does not exist"),
                        );
                    }
                }
            }
        }
        if let BlockKind::StartPlaylist { playlist_id, .. } = &b.kind {
            if !self.playlists.contains_key(playlist_id) {
                bad(
                    "source_missing",
                    format!("playlist {playlist_id} does not exist"),
                );
            }
        }
    }

    /// How deep a nest through `play` blocks goes from `id`, and whether it returns to a playlist on the
    /// way down (a cycle). `stack` holds the playlists above.
    fn nesting(&self, id: i64, stack: &mut Vec<i64>) -> (usize, bool) {
        if stack.contains(&id) {
            return (stack.len(), true);
        }
        let Some(p) = self.playlists.get(&id) else {
            return (stack.len(), false);
        };
        stack.push(id);
        let mut deepest = stack.len();
        let mut cycle = false;
        for b in p.blocks() {
            if let BlockKind::Play(PlayBlock {
                source: Source::Playlist(child),
                ..
            }) = &b.kind
            {
                let (d, c) = self.nesting(*child, stack);
                deepest = deepest.max(d);
                cycle |= c;
            }
        }
        stack.pop();
        (deepest, cycle)
    }

    /// The checks of 4.4 that belong to the M5 block types: `loop`, back-timing, `set_gain`.
    fn check_m5(
        &self,
        p: &Playlist,
        pos: usize,
        b: &Block,
        refs: &dyn Refs,
    ) -> Vec<(bool, &'static str, String)> {
        let mut out = Vec::new();
        let mut push =
            |error: bool, code: &'static str, message: String| out.push((error, code, message));
        match &b.kind {
            BlockKind::Loop { to, times } => {
                let first = p
                    .entries
                    .iter()
                    .position(|e| {
                        matches!(
                            e,
                            Entry::Block(Block {
                                kind: BlockKind::Start(_),
                                ..
                            })
                        )
                    })
                    .map_or(0, |i| i + 1);
                let target = to.map_or(first, |t| t.saturating_sub(1));
                if to.is_some_and(|t| t == 0) || times == &Some(0) {
                    push(
                        true,
                        "loop_target",
                        "a loop goes to a position from 1, and runs 1 or more times".into(),
                    );
                } else if target >= pos {
                    push(
                        true,
                        "loop_target",
                        "a loop goes back to an earlier entry of the list".into(),
                    );
                } else {
                    let takes_time = p.entries[target..pos].iter().any(|e| match e {
                        Entry::Item { .. } | Entry::Album { .. } => true,
                        Entry::Block(x) if !x.enabled => false,
                        Entry::Block(x) => match &x.kind {
                            BlockKind::Play(_) | BlockKind::Insert { .. } => true,
                            BlockKind::Wait(WaitUntil::DurationS(d)) => *d > 0,
                            BlockKind::Wait(_) => true,
                            _ => false,
                        },
                    });
                    if !takes_time {
                        push(
                            true,
                            "loop_no_time",
                            "the body of this loop takes no time (it has no play, item or wait), so it would repeat without end".into(),
                        );
                    }
                }
            }
            BlockKind::SetGain { gain_db, .. } if *gain_db > 0.0 => {
                push(
                    true,
                    "gain_range",
                    "the master gain sits after the safety limiter and cannot be raised above 0 dB; use the output trim to raise the level".into(),
                );
            }
            BlockKind::Play(pb) if pb.end == EndMode::Natural => {
                // The end time: an `until` time, else the next timed flow block's.
                let end_spec: Option<&TimeSpec> = match &pb.until {
                    Until::At(s) => Some(s),
                    _ => p.entries[pos + 1..].iter().find_map(|e| match e {
                        Entry::Block(x) if x.is_timed_flow() && x.enabled => {
                            x.timed.as_ref().map(|t| &t.spec)
                        }
                        _ => None,
                    }),
                };
                match end_spec {
                    None if !matches!(pb.until, Until::Duration(_)) => push(
                        true,
                        "back_time_no_end",
                        "back-timing needs an end time: an `until` time or duration, or a later timed flow block".into(),
                    ),
                    None => {}
                    Some(spec) => {
                        let start_rel = match &b.timed {
                            Some(t) => relative_ms(&t.spec),
                            None => {
                                // In sequence: zero when nothing before it takes time.
                                let idle = p.entries[..pos].iter().all(|e| match e {
                                    Entry::Block(x) => !matches!(
                                        x.kind,
                                        BlockKind::Play(_)
                                            | BlockKind::Wait(_)
                                            | BlockKind::Insert { .. }
                                    ),
                                    _ => false,
                                });
                                idle.then_some(0)
                            }
                        };
                        let shortest = match &pb.source {
                            Source::Category { id, name } => {
                                refs.category_min_ms(*id, name.as_deref())
                            }
                            Source::Playlist(id) => refs.playlist_min_ms(*id),
                            Source::Item(_) => None,
                        };
                        if let (Some(a), Some(z), Some(m)) =
                            (start_rel, relative_ms(spec), shortest)
                        {
                            if z - a < m {
                                push(
                                    false,
                                    "back_time_short",
                                    format!(
                                        "the back-timing window is {} s, shorter than the shortest item of the source ({} s)",
                                        (z - a) / 1000,
                                        m / 1000
                                    ),
                                );
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        out
    }

    /// A warning that needs a T0 (4.4): `block` is in the past at the start the dry run was asked about.
    pub fn past_at_start(playlist: i64, block: &Block, message: String) -> Problem {
        Problem {
            playlist: Some(playlist),
            block: Some(block.id),
            error: false,
            code: "past_at_start",
            message,
        }
    }

    fn validate_playlist(
        &self,
        p: &Playlist,
        refs: &dyn Refs,
        zone: Option<&Zone>,
        out: &mut Vec<Problem>,
    ) {
        let mut bad = |b: &Block, error: bool, code: &'static str, message: String| {
            out.push(Problem {
                playlist: Some(p.id),
                block: Some(b.id),
                error,
                code,
                message,
            })
        };
        // Timed flow blocks seen so far in list order: (block id, instant key) for same-time and
        // backward checks. Relative and same-day absolute times only; recurring ones are exempt.
        let mut seen_times: Vec<(i64, i64)> = Vec::new();
        let mut ended = false;
        for (pos, e) in p.entries.iter().enumerate() {
            let Entry::Block(b) = e else { continue };
            for (error, code, msg) in self.check_m5(p, pos, b, refs) {
                bad(b, error, code, msg);
            }
            if let Some(m) = &b.problem {
                let code = if m.starts_with("a negative relative offset") {
                    "negative_offset"
                } else {
                    "unreadable"
                };
                bad(b, true, code, m.clone());
            }
            if p.mode == Mode::Shuffle
                && b.trigger == Trigger::Sequence
                && !matches!(b.kind, BlockKind::InsertEvery(_) | BlockKind::Start(_))
            {
                bad(
                    b,
                    true,
                    "in_sequence_in_shuffle",
                    format!(
                        "a {} block in a shuffle playlist cannot be in sequence (in-sequence blocks need a sequential playlist)",
                        b.kind.type_name()
                    ),
                );
            }
            if ended && b.enabled {
                bad(
                    b,
                    false,
                    "never_fires",
                    "this block follows an `end` and can never fire".into(),
                );
            }
            if matches!(b.kind, BlockKind::End) && b.trigger != Trigger::Timed {
                ended = true;
            }
            self.check_sources(b, refs, &mut |code, m| bad(b, true, code, m));
            match &b.kind {
                BlockKind::InsertEvery(ie) => {
                    if ie.n < 1 {
                        bad(
                            b,
                            true,
                            "insert_every_n",
                            "insert_every needs n of at least 1".into(),
                        );
                    }
                    if let Source::Playlist(id) = ie.source {
                        if id == p.id {
                            bad(
                                b,
                                true,
                                "self_source",
                                "a playlist cannot be its own insert source".into(),
                            );
                        }
                    }
                }
                BlockKind::Play(pb) => {
                    // A source with fewer than 2 playable items cannot rotate.
                    let thin = match &pb.source {
                        Source::Playlist(id) => self.playlists.get(id).is_some_and(|c| {
                            c.entries
                                .iter()
                                .filter(|e| matches!(e, Entry::Item { .. } | Entry::Album { .. }))
                                .count()
                                < 2
                        }),
                        Source::Category { id, name } => refs
                            .category_playable(*id, name.as_deref())
                            .is_some_and(|n| n < 2),
                        Source::Item(_) => false,
                    };
                    if thin && !matches!(pb.source, Source::Item(_)) {
                        bad(
                            b,
                            false,
                            "thin_source",
                            "a play source with fewer than 2 playable items".into(),
                        );
                    }
                    if let Source::Playlist(child) = pb.source {
                        let mut stack = vec![p.id];
                        let (depth, cycle) = self.nesting(child, &mut stack);
                        if cycle {
                            bad(
                                b,
                                true,
                                "nesting_cycle",
                                "playing this playlist nests a playlist inside itself".into(),
                            );
                        } else if depth > MAX_NESTING {
                            bad(
                                b,
                                true,
                                "nesting_depth",
                                format!(
                                    "nesting is {depth} deep; at most {MAX_NESTING} are allowed"
                                ),
                            );
                        }
                    }
                }
                BlockKind::Start(s) if s.anchor == Anchor::Absolute && s.schedule.is_empty() => {
                    bad(
                        b,
                        true,
                        "start_no_schedule",
                        "an absolute start block needs a schedule".into(),
                    );
                }
                _ => {}
            }
            let Some(t) = &b.timed else { continue };
            if let common::timespec::When::Relative { offset_ms } = &t.spec.when {
                if *offset_ms < 0 {
                    bad(
                        b,
                        true,
                        "negative_offset",
                        "a negative relative offset".into(),
                    );
                }
            }
            if let Some(z) = zone {
                if let common::timespec::When::Date { local } = &t.spec.when {
                    if let Ok(dt) = local.parse::<common::timespec::CivilDateTime>() {
                        if z.in_gap(dt) {
                            bad(
                                b,
                                false,
                                "dst_nonexistent",
                                format!("{local} does not exist in {} (a clock change); it fires at the first valid instant", z.name()),
                            );
                        }
                    }
                }
                if let common::timespec::When::Recurrence { cron } = &t.spec.when {
                    if !cron.hour_any() {
                        // A named hour that falls in a spring-forward gap on some day of the year.
                        let times = cron.times_of_day();
                        if times.iter().any(|(h, _)| (2..=3).contains(h)) {
                            bad(
                                b,
                                false,
                                "dst_nonexistent",
                                "a time between 02:00 and 03:30 does not exist on the spring clock change; it fires at the first valid instant".into(),
                            );
                        }
                    }
                }
            }
            if b.kind.is_flow() && b.enabled {
                // Comparable instants: relative offsets, and same-day times of day.
                let key = match &t.spec.when {
                    common::timespec::When::Relative { offset_ms } => Some(*offset_ms),
                    common::timespec::When::Local { at, day_offset } => {
                        common::timespec::parse_time_of_day(at)
                            .ok()
                            .map(|ms| day_offset * 86_400_000 + ms + 1_000_000_000_000)
                    }
                    _ => None,
                };
                if let Some(k) = key {
                    if let Some((_, _)) = seen_times.iter().find(|(_, o)| *o == k) {
                        bad(
                            b,
                            true,
                            "same_time",
                            "two timed flow blocks at the same time in one playlist".into(),
                        );
                    } else if seen_times.iter().any(|(_, o)| {
                        (*o >= 1_000_000_000_000) == (k >= 1_000_000_000_000) && *o > k
                    }) {
                        bad(
                            b,
                            true,
                            "times_backward",
                            "timed flow blocks whose times run backward in list order".into(),
                        );
                    }
                    seen_times.push((b.id, k));
                }
            }
        }
        // A silent gap: a timed flow block reached early after a `stop` or `fade_stop`.
        let mut prev_stop: Option<(&Block, i64)> = None;
        for e in &p.entries {
            let Entry::Block(b) = e else { continue };
            match &b.kind {
                BlockKind::Stop { .. } | BlockKind::FadeStop { .. } if b.timed.is_some() => {
                    let end = match (&b.timed, &b.kind) {
                        (Some(t), BlockKind::FadeStop { seconds }) => {
                            relative_ms(&t.spec).map(|o| o + (seconds * 1000.0) as i64)
                        }
                        (Some(t), BlockKind::Stop { fade_ms, .. }) => {
                            relative_ms(&t.spec).map(|o| o + fade_ms)
                        }
                        _ => None,
                    };
                    prev_stop = end.map(|e| (b, e));
                }
                _ if b.is_timed_flow() => {
                    if let (Some((_, end)), Some(at)) = (
                        prev_stop,
                        b.timed.as_ref().and_then(|t| relative_ms(&t.spec)),
                    ) {
                        if at > end {
                            let gap = (at - end) / 1000;
                            out.push(Problem {
                                playlist: Some(p.id),
                                block: Some(b.id),
                                error: false,
                                code: "silent_gap",
                                message: format!(
                                    "the station is silent for {gap} s before this block (after a stop or fade_stop)"
                                ),
                            });
                        }
                    }
                    prev_stop = None;
                }
                _ => {}
            }
        }
    }
}

fn relative_ms(spec: &TimeSpec) -> Option<i64> {
    match &spec.when {
        common::timespec::When::Relative { offset_ms } => Some(*offset_ms),
        _ => None,
    }
}

/// Every block of a program's playlist and the station, with its owner: the scheduler's list of what
/// can act.
pub fn all_block_ids(p: &Programming) -> BTreeSet<i64> {
    let mut ids: BTreeSet<i64> = p.station_blocks.iter().map(|b| b.id).collect();
    for pl in p.playlists.values() {
        ids.extend(pl.blocks().map(|b| b.id));
    }
    ids
}
