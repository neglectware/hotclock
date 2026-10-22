//! `effective_markers`: the single function that produces every row of `item_effective`
//! (architecture 3.3). Nothing else computes effective markers.
//!
//! Steps: resolve the window `W = [c_in, c_out]` from the cue overrides (or the auto cues);
//! take the loudness `L_W` for thresholds; search attack and segue inside W; layer the other
//! manual markers on top (a stored manual marker outside W is kept but ignored and flagged);
//! apply the long-intro policy; clamp; assert. If the assert ever fails, return the auto values
//! with every override ignored and a warning, never a violating row.

use crate::env::Envelopes;
use crate::markers::*;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Overrides {
    pub cue_in: Option<i64>,
    pub attack: Option<i64>,
    pub intro: Option<i64>,
    pub segue: Option<i64>,
    pub fade_out: Option<i64>,
    pub cue_out: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LSource {
    File,
    Window,
    WindowEstimate,
}

impl LSource {
    pub fn name(self) -> &'static str {
        match self {
            LSource::File => "file",
            LSource::Window => "window",
            LSource::WindowEstimate => "window_estimate",
        }
    }
}

pub struct EffectiveInput<'a> {
    pub facts: FileFacts,
    pub env: &'a Envelopes,
    pub profile: &'a ProfileParams,
    pub ovr: Overrides,
    pub policy: LongIntroPolicy,
    /// Flags from the analysis pass that do not depend on markers (`clipped`,
    /// `gapless_unknown`).
    pub base_flags: &'a BTreeSet<Flag>,
    /// Exact window loudness `(c_in, c_out, L_W)` from the background job; used only while
    /// the window still matches.
    pub l_w_exact: Option<(i64, i64, f64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Effective {
    pub cue_in: i64,
    pub attack: i64,
    pub intro: Option<i64>,
    pub segue: i64,
    pub fade_out: Option<i64>,
    pub cue_out: i64,
    pub flags: BTreeSet<Flag>,
    /// The loudness thresholds and gain use.
    pub l_used: f64,
    pub l_source: LSource,
    /// The cue window before the long-intro policy (the key of the exact `L_W` job).
    pub window: (i64, i64),
    /// A manual `ovr_cue_in` is in force.
    pub cue_in_manual: bool,
    /// `item_marker_invalid` when the fallback was used.
    pub warning: Option<&'static str>,
}

/// The invariants of 3.1 and 3.3.
pub fn check_invariants(e: &Effective, file_ms: i64) -> Result<(), String> {
    if !(0 <= e.cue_in && e.cue_in <= e.attack) {
        return Err(format!("cue_in {} attack {}", e.cue_in, e.attack));
    }
    if e.attack >= e.segue {
        return Err(format!("attack {} >= segue {}", e.attack, e.segue));
    }
    if !(e.segue <= e.cue_out && e.cue_out <= file_ms) {
        return Err(format!(
            "segue {} cue_out {} file {}",
            e.segue, e.cue_out, file_ms
        ));
    }
    if e.cue_out - e.cue_in < MIN_WINDOW_MS {
        return Err(format!("window {}", e.cue_out - e.cue_in));
    }
    if let Some(i) = e.intro {
        if !(e.cue_in <= i && i <= e.segue) {
            return Err(format!("intro {i}"));
        }
    }
    if let Some(f) = e.fade_out {
        if !(e.segue - 30_000 <= f && f <= e.cue_out && f >= e.cue_in) {
            return Err(format!("fade_out {f}"));
        }
    }
    Ok(())
}

fn compute(inp: &EffectiveInput) -> Effective {
    let file_ms = inp.facts.file_ms;
    let mut flags: BTreeSet<Flag> = inp.base_flags.clone();

    // ---- auto cues, from the file's own loudness ----
    let s0 = inp.profile.silence_threshold(inp.facts.l_int);
    let (mut a_in, mut a_out) = match auto_cues(inp.env, file_ms, s0) {
        Some(p) => p,
        None => {
            flags.insert(Flag::NoAudio);
            (0, file_ms)
        }
    };
    if a_out - a_in < MIN_WINDOW_MS {
        a_out = (a_in + MIN_WINDOW_MS).min(file_ms);
        if a_out - a_in < MIN_WINDOW_MS {
            a_in = (a_out - MIN_WINDOW_MS).max(0);
        }
    }

    // ---- the window ----
    let o_in = inp.ovr.cue_in.map(|v| v.clamp(0, file_ms));
    let o_out = inp.ovr.cue_out.map(|v| v.clamp(0, file_ms));
    let mut c_in = o_in.unwrap_or(a_in);
    let mut c_out = o_out.unwrap_or(a_out);
    let mut cue_in_manual = o_in.is_some();
    let mut overridden = o_in.is_some() || o_out.is_some();
    if c_out - c_in < MIN_WINDOW_MS {
        // An override pair that leaves no window is treated as absent.
        flags.insert(Flag::CueOverrideInvalid);
        c_in = a_in;
        c_out = a_out;
        cue_in_manual = false;
        overridden = false;
    }

    // ---- L for thresholds ----
    let (l_used, l_source) = if overridden {
        match inp.l_w_exact {
            Some((i, o, l)) if i == c_in && o == c_out && l.is_finite() => (l, LSource::Window),
            _ => {
                flags.insert(Flag::GainPending);
                let l = estimate_window_loudness(inp.env, c_in, c_out)
                    .filter(|l| l.is_finite())
                    .unwrap_or(inp.facts.l_int);
                (l, LSource::WindowEstimate)
            }
        }
    } else {
        (inp.facts.l_int, LSource::File)
    };

    // ---- search inside the window ----
    let found = find_in_window(inp.env, &inp.facts, inp.profile, c_in, c_out, l_used);
    for f in &found.flags {
        flags.insert(*f);
    }
    let mut attack = found.attack;
    let mut segue = found.segue;

    // ---- manual markers: honored only inside the window ----
    let inside = |v: i64| v >= c_in && v <= c_out;
    let mut outside = false;
    for (v, name) in [
        (inp.ovr.attack, 0),
        (inp.ovr.segue, 1),
        (inp.ovr.intro, 2),
        (inp.ovr.fade_out, 3),
    ] {
        if let Some(x) = v {
            if !inside(x) {
                outside = true;
            } else if name == 0 {
                attack = x;
            } else if name == 1 {
                segue = x;
            }
        }
    }
    let mut intro = inp.ovr.intro.filter(|&v| inside(v));
    let mut fade_out = inp.ovr.fade_out.filter(|&v| inside(v));
    if outside {
        flags.insert(Flag::OverrideOutsideWindow);
    }

    let mut e = Effective {
        cue_in: c_in,
        attack,
        intro,
        segue,
        fade_out,
        cue_out: c_out,
        flags,
        l_used,
        l_source,
        window: (c_in, c_out),
        cue_in_manual,
        warning: None,
    };
    let _ = (&mut intro, &mut fade_out);
    clamp(&mut e);
    apply_policy(&mut e, inp.policy, found.long_intro_s);
    refresh_flags(&mut e, found.long_intro_s);
    e
}

/// The clamps of 3.3, in order.
fn clamp(e: &mut Effective) {
    e.attack = e.attack.clamp(e.cue_in, e.cue_out - 50);
    e.segue = e.segue.clamp(e.attack + 10, e.cue_out);
    if let Some(f) = e.fade_out {
        // A fade-out starts no earlier than the attack, and at most 30 s before the segue.
        e.fade_out = Some(f.clamp(e.attack.max(e.segue - 30_000), e.cue_out));
    }
    if let Some(i) = e.intro {
        if i < e.cue_in || i > e.segue {
            e.intro = None;
        }
    }
}

/// Recompute the flags that follow from the final cue-in, attack, segue and cue-out.
fn refresh_flags(e: &mut Effective, long_intro_s: Option<f64>) {
    for f in [
        Flag::ColdStart,
        Flag::ColdEnd,
        Flag::LongIntro,
        Flag::LongIntroReview,
    ] {
        e.flags.remove(&f);
    }
    e.flags = cold_flags(
        std::mem::take(&mut e.flags),
        e.cue_in,
        e.attack,
        e.cue_out,
        e.segue,
    );
    if let Some(li) = long_intro_s {
        if e.cue_out - e.cue_in >= 3_000 && (e.attack - e.cue_in) as f64 > li * 1000.0 {
            e.flags.insert(Flag::LongIntro);
            if e.attack - e.cue_in > 20_000 {
                e.flags.insert(Flag::LongIntroReview);
            }
        }
    }
}

/// `skip_to_attack` (3.5): with no manual cue-in and a long intro, the cue-in becomes the
/// attack minus 20 ms. Refused, with `long_intro_review` already set, when the intro is over
/// 20 s, and when the shortened window would fall under the 250 ms minimum.
pub fn apply_policy(e: &mut Effective, policy: LongIntroPolicy, long_intro_s: Option<f64>) {
    if policy != LongIntroPolicy::SkipToAttack || e.cue_in_manual {
        return;
    }
    let Some(li) = long_intro_s else { return };
    let gap = e.attack - e.cue_in;
    if (gap as f64) <= li * 1000.0 || gap > 20_000 {
        return;
    }
    let new_in = e.attack - 20;
    if e.cue_out - new_in < MIN_WINDOW_MS {
        return;
    }
    e.cue_in = new_in;
    if let Some(i) = e.intro {
        if i < new_in {
            e.intro = None;
        }
    }
    if let Some(f) = e.fade_out {
        e.fade_out = Some(f.max(new_in));
    }
}

/// See the module docs. Precondition: `file_ms >= MIN_WINDOW_MS` (import refuses shorter files).
pub fn effective_markers(inp: &EffectiveInput) -> Effective {
    let file_ms = inp.facts.file_ms;
    let e = compute(inp);
    if check_invariants(&e, file_ms).is_ok() {
        return e;
    }
    // A bug: fall back to the auto values with every override ignored.
    let plain = EffectiveInput {
        facts: inp.facts,
        env: inp.env,
        profile: inp.profile,
        ovr: Overrides::default(),
        policy: LongIntroPolicy::Play,
        base_flags: inp.base_flags,
        l_w_exact: None,
    };
    let mut e = compute(&plain);
    e.warning = Some("item_marker_invalid");
    if check_invariants(&e, file_ms).is_err() {
        // Last resort, valid by construction for file_ms >= 250.
        e = Effective {
            cue_in: 0,
            attack: 0,
            intro: None,
            segue: file_ms,
            fade_out: None,
            cue_out: file_ms,
            flags: inp.base_flags.clone(),
            l_used: inp.facts.l_int,
            l_source: LSource::File,
            window: (0, file_ms),
            cue_in_manual: false,
            warning: Some("item_marker_invalid"),
        };
    }
    e
}

/// Would `compute` alone have produced a violating row (a bug counter for the property test)?
pub fn compute_violates(inp: &EffectiveInput) -> Option<String> {
    check_invariants(&compute(inp), inp.facts.file_ms).err()
}
