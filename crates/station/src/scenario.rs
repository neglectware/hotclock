//! The offline scenario renderer behind `hotclock render --scenario` (M1.4, architecture 15): a
//! YAML file names the items, the queue, the station's preset and manual actions; the renderer
//! drives the real conductor and the real mixer (decoder pool, voices, envelopes, limiter) one
//! block at a time, waiting for the decoders, and returns the audio and the sidecar JSON.
//!
//! ```yaml
//! name: attack_alignment
//! station: { preset: sam, target_lufs: -18 }
//! items:
//!   a: { file: tone_fade.flac }
//!   b: { file: pickup.flac, overrides: { cue_in: 33980 } }
//! queue:
//!   - { item: a }
//!   - { item: b, transition: { patch: { lead_max_s: 1.0 } }, run: album1 }
//! actions:
//!   - { at_s: 12.0, do: next }
//! ```
//!
//! A boundary is committed 10 s before the first frame it needs a command at (B's start, or A's
//! stop when that is earlier). A manual action acts on what is sounding: a committed boundary
//! whose incoming item has not started yet is undone first (`Conductor::uncommit_last`), then the
//! action is planned from the item on air. `stop` fades every placed voice.

use crate::conductor::{Conductor, ConductorConfig};
use crate::item::{run_gain, Item, ItemBuild, RunGain};
use crate::plan::{s_frames, TransitionKind, MIN_LEAD_S};
use crate::sidecar::Sidecar;
use analysis::effective::Overrides;
use common::transition::PresetPatch;
use engine::mixer::MAX_BLOCK;
use engine::rt::Command;
use engine::source::Source;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long before B's first sample its boundary is committed.
pub const COMMIT_AHEAD_S: f64 = 10.0;

fn d_rate() -> u32 {
    48_000
}
fn d_preset() -> String {
    "plexamp".into()
}
fn d_target() -> f64 {
    -18.0
}
fn d_boost() -> f64 {
    12.0
}
fn d_allow() -> f64 {
    6.0
}
fn d_start() -> f64 {
    0.1
}
fn d_music() -> String {
    "music".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StationSpec {
    pub preset: String,
    pub target_lufs: f64,
    pub max_boost_db: f64,
    pub peak_allowance_db: f64,
    /// The automatic fade at the end of a `truncated_fade` item, seconds; 0 is off (the station's
    /// `truncated_fade` and `truncated_fade_s`).
    pub truncated_fade_s: f64,
    /// When the first item starts, seconds.
    pub start_s: f64,
    /// Extra or replacement presets (name to parameter JSON).
    pub presets: BTreeMap<String, Value>,
    /// A shipped processing-chain preset (`radio`, `gentle`) to run the render through (architecture 6); none is the
    /// chain bypassed.
    pub chain: Option<String>,
}

impl Default for StationSpec {
    fn default() -> Self {
        Self {
            preset: d_preset(),
            target_lufs: d_target(),
            max_boost_db: d_boost(),
            peak_allowance_db: d_allow(),
            truncated_fade_s: crate::plan::TRUNCATED_FADE_S,
            start_s: d_start(),
            presets: BTreeMap::new(),
            chain: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OverridesSpec {
    pub cue_in: Option<i64>,
    pub attack: Option<i64>,
    pub intro: Option<i64>,
    pub segue: Option<i64>,
    pub fade_out: Option<i64>,
    pub cue_out: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ItemSpec {
    pub file: String,
    pub category: String,
    pub overrides: OverridesSpec,
    /// Segue profile parameter overrides (3.3).
    pub params: Value,
    pub gain_trim_db: f64,
    pub album_l: Option<f64>,
    pub album_gain_db: Option<f64>,
    /// The item's own transition override.
    pub transition: PresetPatch,
    /// The category's preset (a layer between the station's and the item's).
    pub category_preset: Option<String>,
    /// Replaces the category's shipped patch.
    pub category_patch: Option<Value>,
    pub empty_ramps: bool,
}

impl Default for ItemSpec {
    fn default() -> Self {
        Self {
            file: String::new(),
            category: d_music(),
            overrides: OverridesSpec::default(),
            params: Value::Null,
            gain_trim_db: 0.0,
            album_l: None,
            album_gain_db: None,
            transition: PresetPatch::default(),
            category_preset: None,
            category_patch: None,
            empty_ramps: false,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EntrySpec {
    pub item: String,
    /// The per-queue-entry override for the boundary into this item.
    pub transition: Option<PresetPatch>,
    /// Consecutive entries with the same run id are an album run (3.7).
    pub run: Option<String>,
    /// `gapless` (default) or `normal`: with `normal` a run is an ordered list with ordinary segues.
    pub continuity: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActionSpec {
    pub at_s: f64,
    #[serde(rename = "do")]
    pub act: String,
    pub fade_s: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub dir: Option<PathBuf>,
    #[serde(default = "d_rate")]
    pub rate: u32,
    #[serde(default)]
    pub station: StationSpec,
    pub items: BTreeMap<String, ItemSpec>,
    pub queue: Vec<EntrySpec>,
    #[serde(default)]
    pub actions: Vec<ActionSpec>,
}

impl Scenario {
    pub fn from_yaml(text: &str) -> Result<Scenario, String> {
        serde_yaml_ng::from_str(text).map_err(|e| format!("scenario: {e}"))
    }

    pub fn from_file(path: &Path) -> Result<Scenario, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::from_yaml(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

pub struct Rendered {
    pub rate: u32,
    /// Interleaved stereo, `sidecar.render_frames` frames.
    pub samples: Vec<f32>,
    pub sidecar: Sidecar,
    /// For a cropped preview: the timeline frame of `samples[0]` (the sidecar's frames stay on the full timeline).
    pub crop_from: u64,
}

impl Rendered {
    pub fn left(&self) -> Vec<f32> {
        self.samples.chunks_exact(2).map(|f| f[0]).collect()
    }

    pub fn write_wav(&self, path: &Path) -> Result<(), String> {
        let mut w = engine::wav::WavWriter::create(path, self.rate, 2, engine::wav::WavFormat::F32)
            .map_err(|e| e.to_string())?;
        let bytes: Vec<u8> = self.samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        w.write_bytes(&bytes).map_err(|e| e.to_string())?;
        w.finish().map(|_| ()).map_err(|e| e.to_string())
    }
}

fn to_item(name: &str, spec: &ItemSpec, dir: &Path) -> Result<Arc<Item>, String> {
    let path = {
        let p = Path::new(&spec.file);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            dir.join(p)
        }
    };
    let mut item = Item::analyze(
        &path,
        &ItemBuild {
            name: name.to_string(),
            category: spec.category.clone(),
            overrides: Overrides {
                cue_in: spec.overrides.cue_in,
                attack: spec.overrides.attack,
                intro: spec.overrides.intro,
                segue: spec.overrides.segue,
                fade_out: spec.overrides.fade_out,
                cue_out: spec.overrides.cue_out,
            },
            params: spec.params.clone(),
            gain_trim_db: spec.gain_trim_db,
            album_l: spec.album_l,
            album_gain_db: spec.album_gain_db,
            transition: spec.transition.clone(),
            empty_ramps: spec.empty_ramps,
        },
    )
    .map_err(|e| format!("item `{name}` ({}): {e}", path.display()))?;
    item.category_preset = spec.category_preset.clone();
    if let Some(p) = &spec.category_patch {
        item.category_patch = p.clone();
    }
    Ok(Arc::new(item))
}

/// Render the scenario. `fixtures` overrides the scenario's own `dir` for relative file names.
pub fn render(sc: &Scenario, fixtures: Option<&Path>) -> Result<Rendered, String> {
    if sc.queue.is_empty() {
        return Err("the queue is empty".into());
    }
    let dir = fixtures
        .map(Path::to_path_buf)
        .or_else(|| sc.dir.clone())
        .unwrap_or_else(|| PathBuf::from("."));
    let mut items: BTreeMap<String, Arc<Item>> = BTreeMap::new();
    for (name, spec) in &sc.items {
        items.insert(name.clone(), to_item(name, spec, &dir)?);
    }
    render_items(sc, items)
}

/// Render the scenario's queue with items the caller built (the library's preview builds them from the
/// stored rows, exactly as a station does, so the preview hears what the station would play). `sc.items` is
/// not read.
pub fn render_items(sc: &Scenario, items: BTreeMap<String, Arc<Item>>) -> Result<Rendered, String> {
    if sc.queue.is_empty() {
        return Err("the queue is empty".into());
    }
    let rate = sc.rate;
    let entry_item = |i: usize| -> Result<Arc<Item>, String> {
        items
            .get(&sc.queue[i].item)
            .cloned()
            .ok_or_else(|| format!("queue entry {i}: no item `{}`", sc.queue[i].item))
    };
    let n = sc.queue.len();

    // Album runs (3.7): one gain for the members of a run.
    let mut run_of: Vec<Option<RunGain>> = vec![None; n];
    let mut joined: Vec<bool> = vec![false; n]; // entry i continues the run of entry i-1 gaplessly
    let mut i = 0;
    while i < n {
        let Some(rid) = sc.queue[i].run.clone() else {
            i += 1;
            continue;
        };
        let mut j = i;
        while j + 1 < n && sc.queue[j + 1].run.as_deref() == Some(rid.as_str()) {
            j += 1;
        }
        let members: Vec<(Arc<Item>, f64)> = (i..=j)
            .map(|k| {
                let it = entry_item(k)?;
                let d = (it.markers.cue_out - it.markers.cue_in).max(1) as f64 / 1000.0;
                Ok((it, d))
            })
            .collect::<Result<_, String>>()?;
        let refs: Vec<(&Item, f64)> = members.iter().map(|(i, d)| (i.as_ref(), *d)).collect();
        let rg = run_gain(&refs);
        let normal = sc.queue[i..=j]
            .iter()
            .any(|e| e.continuity.as_deref() == Some("normal"));
        for k in i..=j {
            if !normal {
                run_of[k] = Some(rg);
            }
            if k > i && !normal {
                joined[k] = true;
            }
        }
        i = j + 1;
    }

    let mut cfg = ConductorConfig::new(rate, &sc.station.preset);
    cfg.target_lufs = sc.station.target_lufs;
    cfg.max_boost_db = sc.station.max_boost_db;
    cfg.peak_allowance_db = sc.station.peak_allowance_db;
    cfg.truncated_fade_s = sc.station.truncated_fade_s;
    for (name, params) in &sc.station.presets {
        cfg.presets.insert(name, params.clone());
    }
    let (mut cond, mut mixer) = Conductor::new(cfg);
    cond.sidecar.scenario = sc.name.clone();
    if let Some(name) = &sc.station.chain {
        let c = common::chain::preset_config(name)
            .ok_or_else(|| format!("no chain preset `{name}`"))?;
        cond.chain_control().apply(&c)?;
    }

    let start = s_frames(sc.station.start_s, rate).max(0) as u64;
    cond.place_first(entry_item(0)?, start, run_of[0])?;
    let mut next_idx = 1usize;
    let mut finished_placing = false;
    // A `stop` or `hold` has been done: later actions are ignored. (Not the same as
    // `finished_placing`, which is also set when the queue has simply run out: a `stop` after the
    // last boundary was committed used to be dropped.)
    let mut halted = false;
    let mut actions: Vec<&ActionSpec> = sc.actions.iter().collect();
    actions.sort_by(|a, b| a.at_s.total_cmp(&b.at_s));
    let mut ai = 0usize;
    let lookahead = mixer.limiter_latency_frames() as u64;

    let mut samples: Vec<f32> = Vec::new();
    let mut block = vec![0.0f32; MAX_BLOCK * 2];
    let mut t: u64 = 0;
    let mut cached: Option<(usize, u64)> = None; // (entry index, first frame the plan needs a command at)
    let mut waiting_since = Instant::now();
    // A voice was loaded since the last block: its load reaches the mixer only when a block is
    // rendered, and a cancel sent before that would be ignored as stale.
    let mut load_in_flight = true;
    let commit_ahead = s_frames(COMMIT_AHEAD_S, rate).max(0) as u64;
    let earliest = s_frames(MIN_LEAD_S, rate).max(0) as u64;

    loop {
        cond.tick();

        // ---- manual actions that are due ----
        while let Some(act) = actions.get(ai) {
            let due = match act.act.as_str() {
                // Planned three seconds ahead, so A's fade can start 1.5 s before the hard time.
                "hard_time" => s_frames(act.at_s - 3.0, rate).max(0) as u64,
                _ => s_frames(act.at_s, rate).max(0) as u64,
            };
            if due > t {
                break;
            }
            // A manual action acts on what is sounding: a boundary committed ahead whose incoming
            // item has not started is undone first (review M1.4; before, "next" planned from the
            // item not yet started, cancelling it, while the item on air kept playing).
            let now_mixed = t + lookahead;
            let unstarted = !halted
                && cond.placed.len() > 1
                && cond
                    .last()
                    .is_some_and(|p| p.start_frame > now_mixed && !p.ended);
            if unstarted && load_in_flight {
                break; // after the next block, when the load has reached the mixer
            }
            ai += 1;
            if halted {
                continue;
            }
            let mut send = |c: Command| mixer.command(&c.kind);
            while cond.uncommit_last(now_mixed, &mut send).is_some() {
                next_idx -= 1;
                cached = None;
                finished_placing = false;
            }
            match act.act.as_str() {
                "stop" => {
                    cond.stop_all(
                        t + earliest,
                        act.fade_s.unwrap_or(crate::plan::STOP_FADE_S),
                        &mut send,
                    );
                    finished_placing = true;
                    halted = true;
                }
                "hold" => {
                    cond.finish_last(&mut send);
                    finished_placing = true;
                    halted = true;
                }
                "next" | "fade_next" | "cut" | "hard_time" => {
                    if next_idx >= n {
                        return Err(format!(
                            "action `{}` at {} s: nothing is queued",
                            act.act, act.at_s
                        ));
                    }
                    let kind = match act.act.as_str() {
                        "next" => TransitionKind::SegueNow,
                        "fade_next" => TransitionKind::FadeNext {
                            fade_s: act.fade_s.unwrap_or(crate::plan::FADE_NEXT_DEFAULT_S),
                        },
                        "cut" => TransitionKind::Cut,
                        _ => TransitionKind::HardTime {
                            at_frame: s_frames(act.at_s, rate).max(0) as u64,
                            fade_s: None,
                        },
                    };
                    let b = entry_item(next_idx)?;
                    let entry = sc.queue[next_idx].transition.clone();
                    let (p, pa, pb) = cond.plan_next(&b, kind, entry.as_ref(), t)?;
                    cond.commit(b, &p, &pa, &pb, entry, run_of[next_idx], &mut send)?;
                    load_in_flight = true;
                    next_idx += 1;
                    cached = None;
                }
                other => return Err(format!("unknown action `{other}`")),
            }
        }

        // ---- the automatic boundary ----
        if !finished_placing && next_idx < n {
            let b = entry_item(next_idx)?;
            let entry = sc.queue[next_idx].transition.clone();
            let kind = if joined[next_idx] {
                TransitionKind::Continuity
            } else {
                TransitionKind::Auto
            };
            let needs_exact = joined[next_idx] || cond.last().is_some_and(|p| p.out_is_file_end);
            if !needs_exact || cond.length_known() {
                let first = match cached {
                    Some((i, s)) if i == next_idx => s,
                    _ => {
                        let (p, _, _) = cond.plan_next(&b, kind.clone(), entry.as_ref(), t)?;
                        cached = Some((next_idx, p.first_command_frame()));
                        p.first_command_frame()
                    }
                };
                if first <= t + commit_ahead {
                    let (p, pa, pb) = cond.plan_next(&b, kind, entry.as_ref(), t)?;
                    let mut send = |c: Command| mixer.command(&c.kind);
                    cond.commit(b, &p, &pa, &pb, entry, run_of[next_idx], &mut send)?;
                    load_in_flight = true;
                    next_idx += 1;
                    cached = None;
                }
            }
        }
        if !finished_placing && next_idx >= n && cached.is_none() {
            // Everything is placed; the last item plays to its bound end.
            let mut send = |c: Command| mixer.command(&c.kind);
            let all_manual_done = actions[ai..]
                .iter()
                .all(|a| a.act == "stop" || a.act == "hold");
            if all_manual_done || actions.len() == ai {
                cond.finish_last(&mut send);
                finished_placing = true;
            }
        }
        if finished_placing && cond.live_voices() == 0 {
            break;
        }
        if t > 4 * 3600 * u64::from(rate) {
            return Err("the render passed four hours".into());
        }

        // ---- wait for the decoders, then render a block ----
        if cond.not_ready(t + MAX_BLOCK as u64 + lookahead, 2 * MAX_BLOCK as u64) {
            if waiting_since.elapsed() > Duration::from_secs(60) {
                return Err("timed out waiting for the decoder".into());
            }
            std::thread::sleep(Duration::from_micros(200));
            continue;
        }
        waiting_since = Instant::now();
        mixer.render(&mut block);
        load_in_flight = false;
        samples.extend_from_slice(&block);
        t += MAX_BLOCK as u64;
    }

    cond.finish(cond.stats(), t);
    // A voice's end is seen when the mixer has mixed it into the bus, which runs `lookahead` frames (the
    // limiters' delay) ahead of the output: render on until the output has reached the last end.
    let last_end = cond
        .sidecar
        .voices
        .iter()
        .filter_map(|v| v.ended_frame)
        .max()
        .unwrap_or(t);
    while t < last_end {
        mixer.render(&mut block);
        samples.extend_from_slice(&block);
        t += MAX_BLOCK as u64;
    }
    let end = last_end.min(t);
    samples.truncate(end as usize * 2);
    cond.sidecar.render_frames = end;
    Ok(Rendered {
        rate,
        samples,
        sidecar: cond.sidecar.clone(),
        crop_from: 0,
    })
}

/// Render the window of a two-item scenario around its boundary: what the preview endpoint
/// (`GET /api/v1/preview/transition`, M1.7) calls. `pre_s` before B's first sample and
/// `post_s` after A's end (2.6, "Preview and audition"). It is a slice of the scenario render, so
/// the audio is sample-identical to what the scenario renderer produces for the same pair.
pub fn preview_pair(
    sc: &Scenario,
    fixtures: Option<&Path>,
    pre_s: f64,
    post_s: f64,
) -> Result<Rendered, String> {
    slice_pair(render(sc, fixtures)?, pre_s, post_s)
}

/// `preview_pair` with items built by the caller (see `render_items`).
pub fn preview_pair_items(
    sc: &Scenario,
    items: BTreeMap<String, Arc<Item>>,
    pre_s: f64,
    post_s: f64,
) -> Result<Rendered, String> {
    slice_pair(render_items(sc, items)?, pre_s, post_s)
}

fn slice_pair(r: Rendered, pre_s: f64, post_s: f64) -> Result<Rendered, String> {
    let Some(b) = r.sidecar.boundaries.first() else {
        return Ok(r);
    };
    let rate = r.rate;
    let from = b.s_b.saturating_sub(s_frames(pre_s, rate).max(0) as u64);
    let to = ((b.f_end_a.max(b.s_b as i64) + s_frames(post_s, rate)).max(0) as u64)
        .min(r.sidecar.render_frames);
    let samples = r.samples[from as usize * 2..(to.max(from) as usize) * 2].to_vec();
    Ok(Rendered {
        rate,
        samples,
        sidecar: r.sidecar,
        crop_from: from,
    })
}
