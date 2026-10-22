//! Marker detection (architecture 3.1, 3.2, 3.4, 3.5): pure functions over the stored
//! envelopes, so a marker can be recomputed in milliseconds without decoding.
//!
//! Everything here works **inside a window** `[c_in, c_out]` (ms): the recompute rule of 3.3.
//! With no cue override the window is the auto one found by [`auto_cues`], and the result equals
//! a plain analysis. Nothing outside the window is ever read.

use crate::blocks::gate;
use crate::env::Envelopes;
use serde_json::Value;
use std::collections::BTreeSet;

/// Shortest allowed window, `cue_out - cue_in`.
pub const MIN_WINDOW_MS: i64 = 250;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Flag {
    NoAudio,
    ColdStart,
    ColdEnd,
    LongTail,
    SoftStart,
    LongIntro,
    LongIntroReview,
    OverrideOutsideWindow,
    GainPending,
    GaplessUnknown,
    AmbientSuspect,
    SegueSuspect,
    Clipped,
    Short,
    /// A pair of cue overrides that left no valid window was ignored (3.3).
    CueOverrideInvalid,
    /// The file ends in the middle of its own fade-out, still above -70 dBFS (`tail.rs`).
    TruncatedFade,
}

impl Flag {
    pub fn name(self) -> &'static str {
        match self {
            Flag::NoAudio => "no_audio",
            Flag::ColdStart => "cold_start",
            Flag::ColdEnd => "cold_end",
            Flag::LongTail => "long_tail",
            Flag::SoftStart => "soft_start",
            Flag::LongIntro => "long_intro",
            Flag::LongIntroReview => "long_intro_review",
            Flag::OverrideOutsideWindow => "override_outside_window",
            Flag::GainPending => "gain_pending",
            Flag::GaplessUnknown => "gapless_unknown",
            Flag::AmbientSuspect => "ambient_suspect",
            Flag::SegueSuspect => "segue_suspect",
            Flag::Clipped => "clipped",
            Flag::Short => "short",
            Flag::CueOverrideInvalid => "cue_override_invalid",
            Flag::TruncatedFade => "truncated_fade",
        }
    }

    pub fn from_name(s: &str) -> Option<Flag> {
        ALL_FLAGS.iter().copied().find(|f| f.name() == s)
    }
}

pub const ALL_FLAGS: [Flag; 16] = [
    Flag::NoAudio,
    Flag::ColdStart,
    Flag::ColdEnd,
    Flag::LongTail,
    Flag::SoftStart,
    Flag::LongIntro,
    Flag::LongIntroReview,
    Flag::OverrideOutsideWindow,
    Flag::GainPending,
    Flag::GaplessUnknown,
    Flag::AmbientSuspect,
    Flag::SegueSuspect,
    Flag::Clipped,
    Flag::Short,
    Flag::CueOverrideInvalid,
    Flag::TruncatedFade,
];

pub fn flags_to_json(f: &BTreeSet<Flag>) -> String {
    let names: Vec<&str> = f.iter().map(|f| f.name()).collect();
    serde_json::to_string(&names).unwrap_or_else(|_| "[]".into())
}

pub fn flags_from_json(s: &str) -> BTreeSet<Flag> {
    serde_json::from_str::<Vec<String>>(s)
        .unwrap_or_default()
        .iter()
        .filter_map(|n| Flag::from_name(n))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LongIntroPolicy {
    Play,
    SkipToAttack,
}

impl LongIntroPolicy {
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "play" => Some(Self::Play),
            "skip_to_attack" => Some(Self::SkipToAttack),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Play => "play",
            Self::SkipToAttack => "skip_to_attack",
        }
    }
}

/// A segue profile after the item's parameter overrides (3.3, 3.4). The transition-side rows
/// of the 3.4 table (`lead_max_s`, tails, policy) belong to the transition model (2.6), not here.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileParams {
    pub name: String,
    /// Silence threshold `S = clamp(L - offset, min, max)` dBFS on `E10`.
    pub silence_offset_db: f64,
    pub silence_min_db: f64,
    pub silence_max_db: f64,
    /// `T_seg = L + segue_lu`; `None` for `longform`, whose segue is its cue-out.
    pub segue_lu: Option<f64>,
    /// `(seconds, lu)`: when `cue_out - segue` exceeds the seconds, recompute at `L + lu`.
    pub long_tail: Option<(f64, f64)>,
    /// `ambient`: the attack is ignored (`attack := cue_in`).
    pub attack_ignored: bool,
    /// The attack is the first `E50` within this many dB of `P_ref`.
    pub attack_below_db: f64,
    /// Threshold of the `long_intro` flag, seconds.
    pub long_intro_s: Option<f64>,
    /// The hold-off rule for items over 30 s.
    pub holdoff: bool,
}

impl ProfileParams {
    pub fn music() -> Self {
        ProfileParams {
            name: "music".into(),
            silence_offset_db: 36.0,
            silence_min_db: -60.0,
            silence_max_db: -45.0,
            segue_lu: Some(-12.0),
            long_tail: Some((15.0, -20.0)),
            attack_ignored: false,
            attack_below_db: 8.0,
            long_intro_s: Some(4.0),
            holdoff: true,
        }
    }
    pub fn liner() -> Self {
        ProfileParams {
            name: "liner".into(),
            segue_lu: Some(-15.0),
            long_tail: None,
            long_intro_s: None,
            ..Self::music()
        }
    }
    pub fn ambient() -> Self {
        ProfileParams {
            name: "ambient".into(),
            silence_offset_db: 42.0,
            silence_min_db: -66.0,
            silence_max_db: -50.0,
            segue_lu: Some(-24.0),
            long_tail: None,
            attack_ignored: true,
            long_intro_s: None,
            ..Self::music()
        }
    }
    pub fn longform() -> Self {
        ProfileParams {
            name: "longform".into(),
            segue_lu: None,
            long_tail: None,
            long_intro_s: None,
            holdoff: false,
            ..Self::music()
        }
    }

    pub fn builtin(name: &str) -> Option<Self> {
        match name {
            "music" => Some(Self::music()),
            "liner" | "id" | "promo" => Some(Self {
                name: name.into(),
                ..Self::liner()
            }),
            "ambient" => Some(Self::ambient()),
            "longform" => Some(Self::longform()),
            _ => None,
        }
    }

    /// Merge a JSON object of parameter overrides (`item_overrides.params`, or a profile row).
    /// Unknown keys are ignored. Keys: `silence_offset_db`, `silence_min_db`, `silence_max_db`,
    /// `segue_lu` (a number, or null to switch it off), `long_tail_s`, `long_tail_lu`
    /// (`long_tail_s` null or 0 switches the rule off), `attack_below_db`, `attack_ignored`,
    /// `long_intro_s`, `holdoff`.
    pub fn apply_json(&mut self, v: &Value) {
        let Some(o) = v.as_object() else { return };
        let num = |k: &str| o.get(k).and_then(Value::as_f64).filter(|x| x.is_finite());
        if let Some(x) = num("silence_offset_db") {
            self.silence_offset_db = x;
        }
        if let Some(x) = num("silence_min_db") {
            self.silence_min_db = x;
        }
        if let Some(x) = num("silence_max_db") {
            self.silence_max_db = x;
        }
        if let Some(val) = o.get("segue_lu") {
            self.segue_lu = val.as_f64().filter(|x| x.is_finite());
        }
        let lt_lu = num("long_tail_lu");
        if let Some(val) = o.get("long_tail_s") {
            match val.as_f64().filter(|x| x.is_finite() && *x > 0.0) {
                Some(s) => {
                    let lu = lt_lu.or(self.long_tail.map(|t| t.1)).unwrap_or(-20.0);
                    self.long_tail = Some((s, lu));
                }
                None => self.long_tail = None,
            }
        } else if let (Some(lu), Some((s, _))) = (lt_lu, self.long_tail) {
            self.long_tail = Some((s, lu));
        }
        if let Some(x) = num("attack_below_db") {
            self.attack_below_db = x.max(0.0);
        }
        if let Some(b) = o.get("attack_ignored").and_then(Value::as_bool) {
            self.attack_ignored = b;
        }
        if let Some(val) = o.get("long_intro_s") {
            self.long_intro_s = val.as_f64().filter(|x| x.is_finite() && *x > 0.0);
        }
        if let Some(b) = o.get("holdoff").and_then(Value::as_bool) {
            self.holdoff = b;
        }
    }

    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "silence_offset_db": self.silence_offset_db,
            "silence_min_db": self.silence_min_db,
            "silence_max_db": self.silence_max_db,
            "segue_lu": self.segue_lu,
            "long_tail_s": self.long_tail.map(|t| t.0),
            "long_tail_lu": self.long_tail.map(|t| t.1),
            "attack_below_db": self.attack_below_db,
            "attack_ignored": self.attack_ignored,
            "long_intro_s": self.long_intro_s,
            "holdoff": self.holdoff,
        })
    }

    /// `S` for a loudness `l` (LUFS).
    pub fn silence_threshold(&self, l: f64) -> f64 {
        let lo = self.silence_min_db.min(self.silence_max_db);
        let hi = self.silence_min_db.max(self.silence_max_db);
        (l - self.silence_offset_db).clamp(lo, hi)
    }
}

/// What the analysis pass knows about the file besides the envelopes.
#[derive(Debug, Clone, Copy)]
pub struct FileFacts {
    pub file_ms: i64,
    pub l_int: f64,
    pub lra: f64,
}

/// Markers found inside a window, before overrides and clamps.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub attack: i64,
    pub segue: i64,
    pub flags: BTreeSet<Flag>,
    /// The `long_intro` threshold that applied (short items use the liner rules), seconds.
    pub long_intro_s: Option<f64>,
}

/// The search for the first sound is limited to this many ms.
const CUE_SEARCH_MS: i64 = 60_000;

/// `cue_in` and `cue_out` from `E10` with silence threshold `s` dBFS. `None` when no sound
/// (3 consecutive 10 ms windows at or above `s`) starts within the first 60 s.
pub fn auto_cues(env: &Envelopes, file_ms: i64, s: f64) -> Option<(i64, i64)> {
    let e10 = &env.e10;
    let s = s as f32;
    let mut cue_in = None;
    let limit = (CUE_SEARCH_MS / 10) as usize;
    let mut i = 0;
    while i + 3 <= e10.len() && i <= limit {
        if e10[i] >= s && e10[i + 1] >= s && e10[i + 2] >= s {
            cue_in = Some((i as i64 * 10 - 20).max(0));
            break;
        }
        i += 1;
    }
    let cue_in = cue_in?;
    // The last run of 3 consecutive windows (30 ms) at or above `s`: the mirror image of the
    // cue-in rule. A single loud block is not the end of the sound: the 10 ms RMS of a noisy
    // signal fades out through the threshold with stray blocks well above it.
    let last = (2..e10.len())
        .rev()
        .find(|&i| e10[i] >= s && e10[i - 1] >= s && e10[i - 2] >= s)?;
    let end = ((last as i64 + 1) * 10).min(file_ms);
    let cue_out = (end + 50).min(file_ms);
    Some((cue_in, cue_out))
}

/// The 95th percentile (nearest rank) of `E50` over the windows lying inside `[c_in, c_out]`.
fn p_ref(env: &Envelopes, c_in: i64, c_out: i64) -> Option<f64> {
    let mut v: Vec<f32> = env
        .e50
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            let s = *i as i64 * 10;
            s >= c_in && s + 50 <= c_out
        })
        .map(|(_, &x)| x)
        .collect();
    if v.is_empty() {
        // A window too short for any 50 ms window: fall back to the loudest 10 ms block in it.
        let m = env
            .e10
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                let s = *i as i64 * 10;
                s >= c_in && s < c_out
            })
            .map(|(_, &x)| x)
            .fold(f32::NEG_INFINITY, f32::max);
        return m.is_finite().then_some(f64::from(m));
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((v.len() as f64 * 0.95).ceil() as usize).clamp(1, v.len());
    Some(f64::from(v[rank - 1]))
}

/// The last `K200` window lying entirely inside `[c_in, c_out]` at or above `thr` LUFS: its
/// centre, ms.
fn last_window_at_or_above(env: &Envelopes, c_in: i64, c_out: i64, thr: f64) -> Option<i64> {
    let thr = thr as f32;
    env.k200
        .iter()
        .enumerate()
        .rev()
        .filter(|(k, _)| {
            let s = *k as i64 * 20;
            s >= c_in && s + 200 <= c_out
        })
        .find(|(_, &v)| v >= thr)
        .map(|(k, _)| k as i64 * 20 + 100)
}

/// The `fm` preset's exit (2.6, `exit.mode = last_crossing`): the last `K200` window lying
/// entirely inside `[c_in, c_out]` at or above `thr_lufs`, its centre in ms. This is the same
/// search as the segue's, with the threshold given as an absolute level.
pub fn last_crossing_ms(env: &Envelopes, c_in: i64, c_out: i64, thr_lufs: f64) -> Option<i64> {
    last_window_at_or_above(env, c_in, c_out, thr_lufs)
}

/// Attack, segue and the flags that come from them, for the window `[c_in, c_out]` and
/// loudness `l` (the item's `L_int`, or `L_W` when a cue is overridden). Requires
/// `c_out - c_in >= MIN_WINDOW_MS`.
pub fn find_in_window(
    env: &Envelopes,
    facts: &FileFacts,
    profile: &ProfileParams,
    c_in: i64,
    c_out: i64,
    l: f64,
) -> Found {
    let mut flags = BTreeSet::new();
    let len = c_out - c_in;
    if len < 15_000 {
        flags.insert(Flag::Short);
    }
    // Very short items: no search (3.5).
    if len < 3_000 {
        return Found {
            attack: c_in,
            segue: (c_out - 50).max(c_in + 10),
            flags: cold_flags(flags, c_in, c_in, c_out, (c_out - 50).max(c_in + 10)),
            long_intro_s: None,
        };
    }

    // ---- attack ----
    let mut attack = c_in;
    if !profile.attack_ignored {
        if let Some(pref) = p_ref(env, c_in, c_out) {
            let thr = (pref - profile.attack_below_db) as f32;
            let search_end = (c_in + CUE_SEARCH_MS).min(c_out);
            // Three consecutive samples at or above the threshold (a lone spike in a noisy
            // swell is not an attack).
            let ok = |i: usize| {
                let s = i as i64 * 10;
                s >= c_in && s + 50 <= search_end && env.e50[i] >= thr
            };
            let hit = (0..env.e50.len())
                .find(|&i| {
                    ok(i)
                        && (ok(i + 1) || i + 1 >= env.e50.len())
                        && (ok(i + 2) || i + 2 >= env.e50.len())
                })
                .map(|i| (i, ()));
            match hit {
                Some((i, ())) => {
                    // The rising edge itself: the first 10 ms block of that window at or above
                    // the same threshold.
                    let b = (i..(i + 5).min(env.e10.len()))
                        .find(|&b| env.e10[b] >= thr)
                        .unwrap_or(i);
                    attack = (b as i64 * 10).max(c_in);
                }
                None => {
                    flags.insert(Flag::SoftStart);
                }
            }
        }
    }

    // ---- segue ----
    let mut segue = c_out;
    if let Some(seg_lu) = profile.segue_lu {
        segue = last_window_at_or_above(env, c_in, c_out, l + seg_lu).unwrap_or(c_out);
        if let Some((secs, lu)) = profile.long_tail {
            if (c_out - segue) as f64 > secs * 1000.0 {
                flags.insert(Flag::LongTail);
                let second = last_window_at_or_above(env, c_in, c_out, l + lu).unwrap_or(c_out);
                segue = segue.max(second);
            }
        }
        if profile.holdoff && len > 30_000 && segue < c_in + len / 2 {
            flags.insert(Flag::SegueSuspect);
            segue = c_out - 3_000;
        }
    }
    segue = segue.min(c_out);
    if attack >= segue {
        attack = (segue - 10).max(c_in);
    }
    if segue <= attack {
        segue = (attack + 10).min(c_out);
    }

    if facts.lra > 15.0 || (flags.contains(&Flag::SoftStart) && flags.contains(&Flag::LongTail)) {
        flags.insert(Flag::AmbientSuspect);
    }
    if let Some(li) = profile.long_intro_s {
        if (attack - c_in) as f64 > li * 1000.0 {
            flags.insert(Flag::LongIntro);
            if attack - c_in > 20_000 {
                flags.insert(Flag::LongIntroReview);
            }
        }
    }
    Found {
        attack,
        segue,
        flags: cold_flags(flags, c_in, attack, c_out, segue),
        long_intro_s: profile.long_intro_s,
    }
}

pub fn cold_flags(
    mut f: BTreeSet<Flag>,
    c_in: i64,
    attack: i64,
    c_out: i64,
    segue: i64,
) -> BTreeSet<Flag> {
    if attack - c_in < 150 {
        f.insert(Flag::ColdStart);
    }
    if c_out - segue < 300 {
        f.insert(Flag::ColdEnd);
    }
    f
}

/// Window loudness `L_W` estimated from the stored `K10` blocks alone: BS.1770 gating over
/// 400 ms windows at a 100 ms hop starting at `c_in` (rounded to a block). Within the 0.05 dB
/// quantization of the blocks of the exact value, which needs a decode.
pub fn estimate_window_loudness(env: &Envelopes, c_in: i64, c_out: i64) -> Option<f64> {
    let kpow = env.kpow();
    let first = (c_in.max(0) / 10) as usize;
    let last = ((c_out.max(0) / 10) as usize).min(kpow.len());
    if last < first + 40 {
        return None;
    }
    let seg = &kpow[first..last];
    let mut powers = vec![];
    let mut i = 0;
    while i + 40 <= seg.len() {
        powers.push(seg[i..i + 40].iter().sum::<f64>() / 40.0);
        i += 10;
    }
    gate(&powers)
}
