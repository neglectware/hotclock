//! An item as the executor sees it: the effective markers (milliseconds from the first audible
//! sample), loudness, the stored envelopes and Plex-style ramps, and the item's own transition
//! override. The library holds the same facts in `items`, `analysis`, `item_effective` and
//! `item_overrides`; this is the in-memory form the planner reads. `Item::analyze` builds one from
//! a file (what the scenario renderer and the tests use); the station will build them from the
//! library in M1.5.

use analysis::analyze::{analyze_file, window_loudness_exact, AnalysisError};
use analysis::effective::{effective_markers, EffectiveInput, Overrides};
use analysis::env::Envelopes;
use analysis::markers::{FileFacts, Flag, LongIntroPolicy, ProfileParams};
use analysis::quiet::QuietProfile;
use analysis::ramp::{parse_ramp, window_ramps, Ramp};
use common::transition::{Bounds, PresetPatch, BUILTIN_CATEGORIES};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Effective markers in milliseconds from the start of the file (3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Markers {
    pub cue_in: i64,
    pub attack: i64,
    pub intro: Option<i64>,
    pub segue: i64,
    pub fade_out: Option<i64>,
    pub cue_out: i64,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub path: PathBuf,
    pub category: String,
    /// The category's kind (`music`, `liner`, `id`, `promo`, ...).
    pub kind: String,
    pub category_preset: Option<String>,
    pub category_patch: Value,
    pub gain_offset_db: f64,
    pub markers: Markers,
    pub flags: BTreeSet<Flag>,
    pub file_ms: i64,
    pub manual_cue_in: bool,
    pub manual_cue_out: bool,
    pub manual_segue: bool,
    /// The loudness thresholds and the gain use: `L_int`, or `L_W` when a cue is overridden.
    pub l_used: f64,
    pub true_peak: f64,
    pub gain_trim_db: f64,
    /// The loudness of the item's album, LUFS, when it has one (3.7).
    pub album_l: Option<f64>,
    /// An album gain given outright, dB (Plex's `albumGain` when replaying its logs).
    pub album_gain_db: Option<f64>,
    pub env: Arc<Envelopes>,
    /// The ramps over the whole file, as stored at import.
    pub file_ramps: (Ramp, Ramp),
    /// The item's own transition override (`item_overrides.params.transition`).
    pub transition: PresetPatch,
    /// The longest quiet stretch inside the cue window per threshold (1.5, program silence): `None` for an
    /// item analysed before the library measured it.
    pub quiet: Option<Arc<QuietProfile>>,
}

/// What `Item::analyze` needs beyond the file.
#[derive(Debug, Clone, Default)]
pub struct ItemBuild {
    pub name: String,
    pub category: String,
    pub overrides: Overrides,
    /// Segue profile parameter overrides (3.3), e.g. `{"segue_lu": -18}`.
    pub params: Value,
    pub gain_trim_db: f64,
    pub album_l: Option<f64>,
    pub album_gain_db: Option<f64>,
    pub transition: PresetPatch,
    /// Replace the stored ramps with empty ones (a test of the butt-joint case).
    pub empty_ramps: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ItemError {
    #[error("{0}")]
    Analysis(#[from] AnalysisError),
    #[error("unknown category `{0}`")]
    Category(String),
}

impl Item {
    pub fn analyze(path: &Path, b: &ItemBuild) -> Result<Item, ItemError> {
        let cat = BUILTIN_CATEGORIES
            .iter()
            .find(|c| c.0 == b.category)
            .ok_or_else(|| ItemError::Category(b.category.clone()))?;
        let (_, kind, profile_name, offset, patch) = *cat;
        let mut profile = ProfileParams::builtin(profile_name).unwrap_or_else(ProfileParams::music);
        profile.name = profile_name.to_string();
        if !b.params.is_null() {
            profile.apply_json(&b.params);
        }
        let fa = analyze_file(path, &profile)?;
        let facts = FileFacts {
            file_ms: fa.file_ms,
            l_int: fa.l_int,
            lra: fa.lra,
        };
        let run = |l_w_exact: Option<(i64, i64, f64)>| {
            effective_markers(&EffectiveInput {
                facts,
                env: &fa.env,
                profile: &profile,
                ovr: b.overrides,
                policy: LongIntroPolicy::Play,
                base_flags: &fa.base_flags,
                l_w_exact,
            })
        };
        let mut e = run(None);
        let cue_overridden = b.overrides.cue_in.is_some() || b.overrides.cue_out.is_some();
        if cue_overridden {
            // The library does this in a background job (3.3); here it is one more decode.
            if let Ok(l) = window_loudness_exact(path, e.window.0, e.window.1) {
                e = run(Some((e.window.0, e.window.1, l)));
            }
        }
        let mut ramps = (parse_ramp(&fa.ramp_start), parse_ramp(&fa.ramp_end));
        if b.empty_ramps {
            ramps = (vec![], vec![]);
        }
        Ok(Item {
            name: if b.name.is_empty() {
                path.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            } else {
                b.name.clone()
            },
            path: path.to_path_buf(),
            category: b.category.clone(),
            kind: kind.to_string(),
            category_preset: None,
            category_patch: serde_json::from_str(patch).unwrap_or(Value::Null),
            gain_offset_db: offset,
            markers: Markers {
                cue_in: e.cue_in,
                attack: e.attack,
                intro: e.intro,
                segue: e.segue,
                fade_out: e.fade_out,
                cue_out: e.cue_out,
            },
            flags: e.flags.clone(),
            file_ms: fa.file_ms,
            manual_cue_in: e.cue_in_manual,
            manual_cue_out: b.overrides.cue_out.is_some()
                && !e.flags.contains(&Flag::CueOverrideInvalid),
            manual_segue: b.overrides.segue == Some(e.segue),
            l_used: e.l_used,
            true_peak: fa.true_peak,
            gain_trim_db: b.gain_trim_db,
            album_l: b.album_l,
            album_gain_db: b.album_gain_db,
            quiet: Some(Arc::new(QuietProfile::measure(
                &fa.env.e50,
                e.cue_in,
                e.cue_out,
                fa.file_ms,
            ))),
            env: Arc::new(fa.env.clone()),
            file_ramps: ramps,
            transition: b.transition.clone(),
        })
    }

    /// An item from the row the library (or the station's track cache) holds: the effective markers,
    /// loudness inputs, stored ramps and `K10` blocks, and the transition layers, with no analysis of
    /// the file. `path` is the stored file.
    pub fn from_row(row: &library::tracks::TrackRow, path: PathBuf) -> Result<Item, String> {
        let flags: BTreeSet<Flag> = row
            .flags
            .iter()
            .filter_map(|n| Flag::from_name(n))
            .collect();
        let k10 = if row.k10.is_empty() {
            Vec::new()
        } else {
            analysis::env::dequantize(&analysis::env::unpack(&row.k10)?)
        };
        let k200 = if row.k200.is_empty() {
            Vec::new()
        } else {
            analysis::env::dequantize(&analysis::env::unpack(&row.k200)?)
        };
        let category_patch: Value =
            serde_json::from_str(&row.category_patch).unwrap_or(Value::Null);
        let transition: PresetPatch =
            serde_json::from_str(&row.item_transition).unwrap_or_default();
        Ok(Item {
            name: row.title.clone(),
            path,
            category: row.category.clone(),
            kind: row.kind.clone(),
            category_preset: row.category_preset.clone(),
            category_patch,
            gain_offset_db: row.gain_offset_db,
            markers: Markers {
                cue_in: row.cue_in,
                attack: row.attack,
                intro: row.intro,
                segue: row.segue,
                fade_out: row.fade_out,
                cue_out: row.cue_out,
            },
            flags,
            file_ms: row.file_ms,
            manual_cue_in: row.manual_cue_in,
            manual_cue_out: row.manual_cue_out,
            manual_segue: row.manual_segue,
            l_used: row.l_used,
            true_peak: row.true_peak,
            gain_trim_db: row.gain_trim_db,
            album_l: None,
            album_gain_db: None,
            env: Arc::new(analysis::env::Envelopes {
                k10,
                k200,
                ..Default::default()
            }),
            file_ramps: (parse_ramp(&row.ramp_start), parse_ramp(&row.ramp_end)),
            transition,
            quiet: QuietProfile::from_json(&row.quiet_profile).map(Arc::new),
        })
    }

    /// The item's `(start, end)` ramps for the given bounds (2.6): over the whole file, as stored;
    /// or, when the bounds are the cue window or a cue is overridden, recomputed inside the
    /// effective window from the stored `K10` blocks (3.3, the "as if only W" rule).
    pub fn ramps(&self, bounds: Bounds) -> (Ramp, Ramp) {
        let manual = self.manual_cue_in || self.manual_cue_out;
        if bounds == Bounds::File && !manual {
            return self.file_ramps.clone();
        }
        if self.file_ramps.0.is_empty() && self.file_ramps.1.is_empty() {
            return self.file_ramps.clone();
        }
        window_ramps(
            &self.env,
            self.l_used,
            self.markers.cue_in,
            self.markers.cue_out,
        )
    }

    /// The item's `(start, end)` ramps over the window that actually plays, `[in_ms, out_ms]`
    /// (`plan::bounds_of`, or where a resumed voice started): the ones stored at import when that
    /// is the whole file, otherwise recomputed inside the window from the stored `K10` blocks.
    /// This, not the chain's `bounds`, is what a boundary must read: an item that came in under
    /// one preset and leaves under another was placed with the bounds of the first (M1.4 review).
    pub fn ramps_in_window(&self, in_ms: i64, out_ms: i64) -> (Ramp, Ramp) {
        if (in_ms <= 0 && out_ms >= self.file_ms)
            || (self.file_ramps.0.is_empty() && self.file_ramps.1.is_empty())
        {
            return self.file_ramps.clone();
        }
        window_ramps(&self.env, self.l_used, in_ms, out_ms)
    }

    /// True for a voice item (liner, id, promo): the voice-content rule of 2.6 applies.
    pub fn is_voice(&self) -> bool {
        common::transition::is_voice_kind(&self.kind)
    }
}

/// Loudness of an album run (3.7): what every member's gain is computed from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunGain {
    pub l: f64,
    pub true_peak: f64,
}

/// The duration-weighted power mean of the members' `L_W` (3.7) and their highest true peak.
pub fn run_gain(members: &[(&Item, f64)]) -> RunGain {
    let total: f64 = members.iter().map(|m| m.1).sum::<f64>().max(1e-9);
    let p: f64 = members
        .iter()
        .map(|(i, d)| d * 10f64.powf(i.l_used / 10.0))
        .sum::<f64>()
        / total;
    RunGain {
        l: 10.0 * p.log10(),
        true_peak: members
            .iter()
            .map(|(i, _)| i.true_peak)
            .fold(f64::NEG_INFINITY, f64::max),
    }
}

/// Normalization (3.6): `target - L + offsets`, limited to the maximum boost and to
/// `peak_allowance` dB into the -1 dBFS ceiling. `run` replaces the item's own loudness and peak
/// inside an album run.
pub fn gain_db(
    item: &Item,
    target_lufs: f64,
    max_boost_db: f64,
    peak_allowance_db: f64,
    run: Option<RunGain>,
) -> f64 {
    let (l, tp) = match run {
        Some(r) => (r.l, r.true_peak),
        None => (item.l_used, item.true_peak),
    };
    let g = target_lufs - l + item.gain_offset_db + item.gain_trim_db;
    let g = g.min(max_boost_db);
    g.min((-1.0 - tp) + peak_allowance_db)
}
