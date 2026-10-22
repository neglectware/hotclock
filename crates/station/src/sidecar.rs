//! The sidecar JSON of a render: every voice start with the file position and gain it plays at,
//! every ramp and stop sent for it, and the events the mixer reported, all in output frames. The
//! offline tests read frames from here (attack alignment "within 1 frame (sidecar)") and measure
//! the audio independently.

use serde::Serialize;

#[derive(Debug, Clone, Serialize, Default)]
pub struct StopRecord {
    pub at: u64,
    pub frames: u32,
    /// `declick`, `cosine`, `linear`, `db_linear`, `equal_power` or `cut`.
    pub curve: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RampRecord {
    pub start: u64,
    pub end: u64,
    pub from: f32,
    pub to: f32,
    pub curve: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct VoiceRecord {
    pub item: String,
    pub path: String,
    pub slot: u8,
    pub gen: u32,
    /// The frame the plan asked the first sample to sound at.
    pub start_frame: u64,
    /// The frame the mixer says it did (`Started` event).
    pub started_frame: Option<u64>,
    pub ended_frame: Option<u64>,
    pub end_reason: Option<String>,
    /// Frames the voice took from its ring.
    pub played: Option<u64>,
    /// The file position, ms, of the first sample.
    pub file_start_ms: i64,
    pub gain_db: f64,
    pub declick_in: bool,
    pub stop: Option<StopRecord>,
    pub envelope: Vec<RampRecord>,
    /// The exact frames the decoder produced (from `file_start_ms`), once known.
    pub total_frames: Option<u64>,
    /// The length the plan was made with, and its difference from `total_frames` when they differ.
    pub planned_frames: Option<u64>,
    pub length_mismatch_frames: Option<i64>,
    /// Marker positions on the timeline (frames): `attack`, `segue`, `cue_out`.
    pub attack_frame: u64,
    pub segue_frame: u64,
    pub cue_out_frame: u64,
    pub starved_frames_events: u32,
    pub late_frames: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct BoundaryRecord {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub preset_a: String,
    pub preset_b: String,
    pub s_b: u64,
    pub e_a: i64,
    pub f_end_a: i64,
    pub overlap_frames: i64,
    pub lead_frames: i64,
    pub clamped: bool,
    pub ramp: Option<serde_json::Value>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Sidecar {
    pub scenario: String,
    pub rate: u32,
    pub voices: Vec<VoiceRecord>,
    pub boundaries: Vec<BoundaryRecord>,
    pub render_frames: u64,
    pub starved_frames: u64,
    pub late_starts: u32,
    pub limiter_over_1db_frames: u64,
    pub notes: Vec<String>,
}

impl Sidecar {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}
