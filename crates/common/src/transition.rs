//! The transition model (architecture 2.6): every parameter of how one item hands over to the
//! next as typed data, the five shipped presets as JSON, and the resolution of the layers
//! (station preset, category, item, queue entry) into one parameter set.
//!
//! Keys a preset or a patch does not name keep the value of the layer below; keys nobody names
//! take the model's defaults below, which are the `sam` preset's values (so a custom preset that
//! switches `align.mode` to `attack_to_exit` gets a 2 s lead, not zero).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitMode {
    SegueMarker,
    LastCrossing,
    FixedBeforeEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignMode {
    AtCueIn,
    AttackToExit,
    FixedOverlap,
    RampOverlap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FadeCurve {
    None,
    Linear,
    Cosine,
    Log,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LongIntroPolicy {
    Play,
    SkipToAttack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bounds {
    Cue,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Exit {
    pub mode: ExitMode,
    pub threshold_lu: f64,
    pub seconds: f64,
}

impl Default for Exit {
    fn default() -> Self {
        Self {
            mode: ExitMode::SegueMarker,
            threshold_lu: -12.0,
            seconds: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutFade {
    pub curve: FadeCurve,
    pub seconds: f64,
}

impl Default for OutFade {
    fn default() -> Self {
        Self {
            curve: FadeCurve::None,
            seconds: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Align {
    pub mode: AlignMode,
    pub overlap_s: f64,
    /// `ramp_overlap`: the ramp level a ramp is read at is `threshold_db - g`.
    pub threshold_db: f64,
    pub overlap_max_s: f64,
    /// `ramp_overlap`: the loudness the item's gain `g` is measured against, LUFS. `g` is this
    /// minus the item's (album) loudness. It is Plex's own leveling target, not the station's
    /// `target_lufs`, because the -16 dB of `T = -16 - g` was calibrated against Plex's
    /// `albumGain`, which is `-16 - album loudness`.
    pub gain_reference_lufs: f64,
}

impl Default for Align {
    fn default() -> Self {
        Self {
            mode: AlignMode::AttackToExit,
            overlap_s: 0.0,
            threshold_db: -16.0,
            overlap_max_s: 15.0,
            gain_reference_lufs: -16.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InFade {
    pub curve: FadeCurve,
    pub seconds: f64,
    pub skip_if_cold_start: bool,
}

impl Default for InFade {
    fn default() -> Self {
        Self {
            curve: FadeCurve::None,
            seconds: 0.0,
            skip_if_cold_start: true,
        }
    }
}

/// One resolved parameter set (the table of 2.6).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransitionParams {
    pub exit: Exit,
    pub out_fade: OutFade,
    pub align: Align,
    pub in_fade: InFade,
    pub lead_max_s: f64,
    pub accept_lead_s: f64,
    pub tail_max_s: f64,
    pub tail_fade_s: f64,
    pub long_intro_policy: LongIntroPolicy,
    pub gap_s: f64,
    pub bounds: Bounds,
}

impl Default for TransitionParams {
    fn default() -> Self {
        Self {
            exit: Exit::default(),
            out_fade: OutFade::default(),
            align: Align::default(),
            in_fade: InFade::default(),
            lead_max_s: 2.0,
            accept_lead_s: 2.0,
            tail_max_s: 12.0,
            tail_fade_s: 2.0,
            long_intro_policy: LongIntroPolicy::Play,
            gap_s: 0.0,
            bounds: Bounds::Cue,
        }
    }
}

impl TransitionParams {
    /// From a fully or partly filled JSON object (missing keys take the defaults).
    pub fn from_json(v: &Value) -> Result<Self, String> {
        serde_json::from_value(v.clone()).map_err(|e| format!("transition parameters: {e}"))
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// `(name, params JSON, one-sentence description)` of the five shipped presets (2.6). The library
/// seeds `transition_presets` from this list.
pub const BUILTIN_PRESETS: &[(&str, &str, &str)] = &[
    (
        "basic",
        r#"{"exit":{"mode":"fixed_before_end","seconds":5},"out_fade":{"curve":"linear","seconds":5},"align":{"mode":"fixed_overlap","overlap_s":5},"in_fade":{"curve":"linear","seconds":5,"skip_if_cold_start":false},"lead_max_s":0,"accept_lead_s":0,"long_intro_policy":"play","gap_s":0,"bounds":"file"}"#,
        "Two songs overlap for a fixed 5 seconds with a straight-line fade down and up (mpd and Winamp style).",
    ),
    (
        "plexamp",
        r#"{"out_fade":{"curve":"none"},"align":{"mode":"ramp_overlap","threshold_db":-16,"overlap_max_s":15},"in_fade":{"curve":"none"},"long_intro_policy":"play","gap_s":0,"bounds":"file"}"#,
        "No fades: tracks overlap by the time their quiet ends and starts, so loud parts never meet.",
    ),
    (
        "sam",
        r#"{"exit":{"mode":"segue_marker"},"out_fade":{"curve":"none"},"align":{"mode":"attack_to_exit"},"in_fade":{"curve":"none"},"lead_max_s":2.0,"accept_lead_s":2.0,"tail_max_s":12,"tail_fade_s":2,"long_intro_policy":"play","gap_s":0,"bounds":"cue"}"#,
        "The next song's first loud moment lands where the last one falls below -12 LU; nothing is faded.",
    ),
    (
        "fm",
        r#"{"exit":{"mode":"last_crossing","threshold_lu":-8},"out_fade":{"curve":"none"},"align":{"mode":"attack_to_exit"},"in_fade":{"curve":"none"},"lead_max_s":3.0,"accept_lead_s":3.0,"tail_max_s":2,"tail_fade_s":0.5,"long_intro_policy":"skip_to_attack","gap_s":0,"bounds":"cue"}"#,
        "Aggressive FM automation: the next song hits 8 LU before the last ends, long quiet intros are skipped.",
    ),
    (
        "none",
        r#"{"exit":{"mode":"fixed_before_end","seconds":0},"out_fade":{"curve":"none"},"align":{"mode":"at_cue_in"},"in_fade":{"curve":"none"},"lead_max_s":0,"accept_lead_s":0,"tail_max_s":0,"long_intro_policy":"play","gap_s":0,"bounds":"cue"}"#,
        "One item ends and the next starts on the next frame: no overlap, no gap.",
    ),
];

/// `(name, kind, segue profile, gain offset dB, transition patch JSON)` of the seeded categories
/// (3.4, 3.6, 2.6). The library seeds `categories` from this list.
pub const BUILTIN_CATEGORIES: &[(&str, &str, &str, f64, &str)] = &[
    ("music", "music", "music", 0.0, "{}"),
    (
        "liner",
        "liner",
        "liner",
        -1.0,
        r#"{"accept_lead_s":0,"lead_max_s":0.5,"tail_max_s":3,"tail_fade_s":0.5}"#,
    ),
    (
        "id",
        "id",
        "liner",
        -1.0,
        r#"{"accept_lead_s":0,"lead_max_s":0.5,"tail_max_s":3,"tail_fade_s":0.5}"#,
    ),
    (
        "promo",
        "promo",
        "liner",
        0.0,
        r#"{"accept_lead_s":0,"lead_max_s":0.5,"tail_max_s":3,"tail_fade_s":0.5}"#,
    ),
    ("ad", "commercial", "liner", 0.0, "{}"),
    (
        "ambient",
        "music",
        "ambient",
        0.0,
        r#"{"lead_max_s":0,"tail_max_s":8,"tail_fade_s":4}"#,
    ),
    ("longform", "show", "longform", 0.0, r#"{"gap_s":0.5}"#),
];

/// Category kinds that carry spoken word: the voice-content rule of 2.6 applies to them.
pub fn is_voice_kind(kind: &str) -> bool {
    matches!(kind, "liner" | "id" | "promo")
}

/// Deep merge of two JSON objects: keys of `patch` win, objects merge, anything else replaces.
pub fn merge(base: &Value, patch: &Value) -> Value {
    match (base, patch) {
        // No patch at all (a missing JSON column) changes nothing.
        (b, Value::Null) => b.clone(),
        (Value::Object(b), Value::Object(p)) => {
            let mut out: Map<String, Value> = b.clone();
            for (k, v) in p {
                let merged = match out.get(k) {
                    Some(old) => merge(old, v),
                    None => v.clone(),
                };
                out.insert(k.clone(), merged);
            }
            Value::Object(out)
        }
        (_, p) => p.clone(),
    }
}

/// The keys a category patch applies as caps (the smaller value wins) rather than overrides (2.6).
const CAP_KEYS: [&str; 4] = ["lead_max_s", "accept_lead_s", "tail_max_s", "tail_fade_s"];

/// Apply a category patch: the lead and tail parameters as caps, everything else as overrides.
pub fn merge_category_patch(base: &Value, patch: &Value) -> Value {
    let (Value::Object(b), Value::Object(p)) = (base, patch) else {
        return merge(base, patch);
    };
    let mut out = b.clone();
    for (k, v) in p {
        if CAP_KEYS.contains(&k.as_str()) {
            let cap = v.as_f64();
            let cur = out.get(k).and_then(Value::as_f64);
            match (cap, cur) {
                (Some(c), Some(x)) => {
                    out.insert(k.clone(), Value::from(c.min(x)));
                }
                (Some(_), None) => {
                    // The layer below does not name it: the model's default is the value being capped.
                    let default = TransitionParams::default().to_json();
                    let x = default.get(k).and_then(Value::as_f64).unwrap_or(f64::MAX);
                    out.insert(k.clone(), Value::from(cap.unwrap_or(x).min(x)));
                }
                _ => {}
            }
        } else {
            let merged = match out.get(k) {
                Some(old) => merge(old, v),
                None => v.clone(),
            };
            out.insert(k.clone(), merged);
        }
    }
    Value::Object(out)
}

/// A preset name and/or a patch: what an item override or a queue-entry override carries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PresetPatch {
    pub preset: Option<String>,
    pub patch: Option<Value>,
}

/// The named presets available to a station (the library's `transition_presets` rows).
#[derive(Debug, Clone, Default)]
pub struct PresetBook {
    presets: BTreeMap<String, Value>,
}

impl PresetBook {
    /// The five shipped presets.
    pub fn builtin() -> Self {
        let mut b = Self::default();
        for (name, json, _) in BUILTIN_PRESETS {
            b.presets.insert(
                (*name).to_string(),
                serde_json::from_str(json).expect("builtin"),
            );
        }
        b
    }

    pub fn insert(&mut self, name: &str, params: Value) {
        self.presets.insert(name.to_string(), params);
    }

    pub fn get(&self, name: &str) -> Result<&Value, String> {
        self.presets
            .get(name)
            .ok_or_else(|| format!("unknown transition preset `{name}`"))
    }

    pub fn names(&self) -> Vec<&str> {
        self.presets.keys().map(String::as_str).collect()
    }
}

/// The layers of one resolution chain (an item's side of a boundary), lowest first.
#[derive(Debug, Clone, Default)]
pub struct Chain<'a> {
    /// The station's default preset name.
    pub station_preset: &'a str,
    /// The category's preset, if it names one, and its patch (caps for lead and tail).
    pub category_preset: Option<&'a str>,
    pub category_patch: Option<&'a Value>,
    /// The item's override.
    pub item: Option<&'a PresetPatch>,
    /// The per-queue-entry override, for one boundary only.
    pub entry: Option<&'a PresetPatch>,
}

fn layer(book: &PresetBook, cur: Value, pp: &PresetPatch) -> Result<Value, String> {
    let mut v = cur;
    if let Some(name) = &pp.preset {
        v = merge(&v, book.get(name)?);
    }
    if let Some(patch) = &pp.patch {
        v = merge(&v, patch);
    }
    Ok(v)
}

/// Resolve one chain: station preset, category preset then category patch, item override, entry
/// override, each later layer winning for exactly the parameters it names (2.6, "Resolution").
pub fn resolve(book: &PresetBook, c: &Chain) -> Result<TransitionParams, String> {
    let mut v = book.get(c.station_preset)?.clone();
    if let Some(name) = c.category_preset {
        v = merge(&v, book.get(name)?);
    }
    if let Some(patch) = c.category_patch {
        v = merge_category_patch(&v, patch);
    }
    if let Some(pp) = c.item {
        v = layer(book, v, pp)?;
    }
    if let Some(pp) = c.entry {
        v = layer(book, v, pp)?;
    }
    TransitionParams::from_json(&v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_builtin_parses_and_round_trips() {
        let book = PresetBook::builtin();
        for (name, _, _) in BUILTIN_PRESETS {
            let p = TransitionParams::from_json(book.get(name).unwrap()).unwrap();
            let again = TransitionParams::from_json(&p.to_json()).unwrap();
            assert_eq!(p, again, "{name}");
        }
        let plex = TransitionParams::from_json(book.get("plexamp").unwrap()).unwrap();
        assert_eq!(plex.align.mode, AlignMode::RampOverlap);
        assert_eq!(plex.align.threshold_db, -16.0);
        assert_eq!(plex.align.overlap_max_s, 15.0);
        assert_eq!(plex.bounds, Bounds::File);
        assert_eq!(plex.out_fade.curve, FadeCurve::None);
        let fm = TransitionParams::from_json(book.get("fm").unwrap()).unwrap();
        assert_eq!(fm.exit.mode, ExitMode::LastCrossing);
        assert_eq!(fm.exit.threshold_lu, -8.0);
        assert_eq!(fm.long_intro_policy, LongIntroPolicy::SkipToAttack);
    }

    #[test]
    fn later_layers_win_for_exactly_the_keys_they_name() {
        let book = PresetBook::builtin();
        let cat_patch = json!({"tail_max_s": 8, "gap_s": 0.5});
        let item = PresetPatch {
            preset: Some("fm".into()),
            patch: None,
        };
        let entry = PresetPatch {
            preset: None,
            patch: Some(json!({"lead_max_s": 0.25})),
        };
        let p = resolve(
            &book,
            &Chain {
                station_preset: "sam",
                category_preset: None,
                category_patch: Some(&cat_patch),
                item: Some(&item),
                entry: Some(&entry),
            },
        )
        .unwrap();
        // The station's sam, then the category's caps (tail 12 -> 8) and gap override, then the
        // item's fm (its tail_max_s 2 is below the category's 8 but a preset overrides), then the entry.
        assert_eq!(p.exit.mode, ExitMode::LastCrossing);
        assert_eq!(p.tail_max_s, 2.0);
        assert_eq!(p.gap_s, 0.0, "fm names gap_s = 0 and comes later");
        assert_eq!(p.lead_max_s, 0.25);
        assert_eq!(p.accept_lead_s, 3.0);
    }

    #[test]
    fn a_null_patch_changes_nothing() {
        let base = json!({"lead_max_s": 2.0});
        assert_eq!(merge(&base, &Value::Null), base);
        assert_eq!(merge_category_patch(&base, &Value::Null), base);
    }

    #[test]
    fn category_patch_caps_lead_and_tail() {
        let base = json!({"lead_max_s": 2.0, "tail_max_s": 12, "gap_s": 0});
        let out = merge_category_patch(
            &base,
            &json!({"lead_max_s": 0.5, "tail_max_s": 30, "gap_s": 0.5}),
        );
        assert_eq!(out["lead_max_s"], json!(0.5));
        assert_eq!(out["tail_max_s"], json!(12.0), "a cap never raises");
        assert_eq!(out["gap_s"], json!(0.5), "gap_s overrides");
    }

    #[test]
    fn unknown_preset_is_an_error_naming_it() {
        let book = PresetBook::builtin();
        let e = resolve(
            &book,
            &Chain {
                station_preset: "nope",
                ..Chain::default()
            },
        )
        .unwrap_err();
        assert!(e.contains("nope"), "{e}");
    }
}
