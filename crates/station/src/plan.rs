//! The boundary planner: a pure function from the outgoing item's placement, the incoming item,
//! the two resolved parameter sets and "now" to the frames at which everything happens
//! (architecture 2.5 and 2.6). Nothing here touches the mixer, so every rule is unit-testable and
//! the scenario renderer, the preview and (from M1.5) the conductor all use the same numbers.
//!
//! All frames are output frames of the station timeline. Marker milliseconds are converted with
//! rounding to the nearest frame at the station rate (2.3).

use crate::item::Item;
use analysis::ramp::lookup;
use common::transition::{
    AlignMode, Bounds, ExitMode, FadeCurve, LongIntroPolicy, TransitionParams,
};
use engine::envelope::{Curve, Segment};
use engine::mixer::StopFade;

/// Nothing starts earlier than this after "now" (2.5).
pub const MIN_LEAD_S: f64 = 0.25;
/// Manual transitions (2.5, the table).
pub const CUT_FADE_S: f64 = 0.030;
/// How long before A's audio runs out an emergency transition starts B (1.5).
pub const EMERGENCY_LEAD_S: f64 = 0.050;
pub const SEGUE_NOW_FADE_S: f64 = 1.0;
pub const FADE_NEXT_DEFAULT_S: f64 = 2.0;
pub const HARD_TIME_FADE_S: f64 = 1.5;
pub const STOP_FADE_S: f64 = 2.0;
/// The voice-content rule (2.6): an overlap onto a liner, id or promo is capped at this.
pub const VOICE_OVERLAP_CAP_S: f64 = 0.5;
/// `skip_to_attack` starts this long before the attack (3.5).
pub const SKIP_TO_ATTACK_LEAD_MS: i64 = 20;
/// The default length of the automatic fade at the end of an item flagged `truncated_fade` (the station's
/// `truncated_fade_s`).
pub const TRUNCATED_FADE_S: f64 = 2.0;

#[derive(Debug, Clone, PartialEq)]
pub enum TransitionKind {
    /// What the resolved preset says (`segue`, and `sequential` through `gap_s`).
    Auto,
    Cut,
    SegueNow,
    FadeNext {
        fade_s: f64,
    },
    /// A hard-time block (4.6): B's attack lands on this frame.
    HardTime {
        at_frame: u64,
        /// The fade of the item before it; `None` is [`HARD_TIME_FADE_S`].
        fade_s: Option<f64>,
    },
    /// Two consecutive tracks of one album played in order (2.5, 3.7). Ignores every preset.
    Continuity,
    /// The emergency transition of 1.5: A's decoder ended early (a fatal decode error, a truncated file)
    /// and its audio runs out at `drain_frame`. B starts 50 ms before that, A gets a 30 ms fade.
    Emergency {
        drain_frame: u64,
    },
}

/// Where the outgoing item was placed on the timeline.
#[derive(Debug, Clone)]
pub struct PlacedView<'a> {
    pub item: &'a Item,
    /// Frame at which the item's first played sample sounds (`S_A`).
    pub start_frame: u64,
    /// The file position, ms, of that first sample, and of the last one that is to be played.
    pub bound_in_ms: i64,
    pub bound_out_ms: i64,
    /// `bound_out_ms` is the end of the file: nothing stops the voice, the file's own last frame
    /// ends it.
    pub bound_out_is_file_end: bool,
    /// The exact number of frames the voice will produce, when known (the decoder's total).
    pub frames: Option<u64>,
}

pub fn ms_frames(ms: i64, rate: u32) -> i64 {
    (ms as f64 * f64::from(rate) / 1000.0).round() as i64
}

pub fn s_frames(s: f64, rate: u32) -> i64 {
    (s * f64::from(rate)).round() as i64
}

impl PlacedView<'_> {
    /// The frame after A's last played sample (`F_end_A`).
    pub fn end_frame(&self, rate: u32) -> i64 {
        let start = self.start_frame as i64;
        match (self.bound_out_is_file_end, self.frames) {
            (true, Some(n)) => start + n as i64,
            _ => {
                start
                    + (ms_frames(self.bound_out_ms, rate) - ms_frames(self.bound_in_ms, rate))
                        .max(0)
            }
        }
    }

    /// The frame after the last sample of the file, when the voice plays to the end.
    pub fn file_end_frame(&self, rate: u32) -> i64 {
        let start = self.start_frame as i64;
        match self.frames {
            Some(n) => start + n as i64,
            None => {
                start
                    + (ms_frames(self.item.file_ms, rate) - ms_frames(self.bound_in_ms, rate))
                        .max(0)
            }
        }
    }

    /// The frame at which file position `ms` sounds.
    pub fn frame_of(&self, ms: i64, rate: u32) -> i64 {
        self.start_frame as i64 + ms_frames(ms, rate) - ms_frames(self.bound_in_ms, rate)
    }
}

/// Which part of an item plays under `params`: `(bound_in_ms, bound_out_ms, out_is_file_end)`.
/// `bounds = file` plays from 0 to the file's end unless a cue is overridden by hand (then the
/// effective cue is the bound); `bounds = cue` plays the effective window. The long-intro policy
/// `skip_to_attack` moves the start to the attack minus 20 ms when nothing overrides the cue-in
/// (3.5).
pub fn bounds_of(item: &Item, p: &TransitionParams) -> (i64, i64, bool) {
    let m = &item.markers;
    let (mut a, b) = match p.bounds {
        Bounds::Cue => (m.cue_in, m.cue_out),
        Bounds::File => (
            if item.manual_cue_in { m.cue_in } else { 0 },
            if item.manual_cue_out {
                m.cue_out
            } else {
                item.file_ms
            },
        ),
    };
    if p.long_intro_policy == LongIntroPolicy::SkipToAttack
        && !item.manual_cue_in
        && item.flags.contains(&analysis::markers::Flag::LongIntro)
        && !item
            .flags
            .contains(&analysis::markers::Flag::LongIntroReview)
    {
        let new_in = m.attack - SKIP_TO_ATTACK_LEAD_MS;
        if new_in > a && b - new_in >= analysis::markers::MIN_WINDOW_MS {
            a = new_in;
        }
    }
    (a, b, b >= item.file_ms)
}

/// A planned boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub kind: PlanKind,
    /// Frame at which B's first sample sounds (`S_B`).
    pub s_b: u64,
    /// The file position, ms, B starts from; 0 for a continuity joint.
    pub b_bound_in_ms: i64,
    pub b_bound_out_ms: i64,
    pub b_out_is_file_end: bool,
    /// The 3 ms declick in on B (false for continuity).
    pub b_declick: bool,
    /// B's fade-in, a level automation on its envelope (`basic`; never the fade to silence).
    pub b_fade_in: Option<Segment>,
    /// The single stop that ends A (a fade, the tail cap, or the cue-out declick); `None` when A
    /// plays to the end of its file.
    pub a_stop: Option<(u64, StopFade)>,
    /// Frame at which A's stop fade ends (its voice is gone), if it has one.
    pub a_end: Option<u64>,
    /// A's exit point `E_A` and audible end `F_end_A`.
    pub e_a: i64,
    pub f_end_a: i64,
    /// `F_end_A - S_B`: positive is an overlap, negative a gap, in frames.
    pub overlap_frames: i64,
    /// How early B's pickup plays before the exit (`attack_to_exit`), frames.
    pub lead_frames: i64,
    /// `ramp_overlap`: the two crossing times and the threshold levels, seconds and dB.
    pub ramp: Option<RampInfo>,
    /// B's start was pushed to "now + 0.25 s" or to A's start.
    pub clamped: bool,
    pub notes: Vec<String>,
}

impl Plan {
    /// The first frame at which the plan needs a command to be in the mixer: B's start, or A's
    /// stop when that comes first (a manual fade-out marker starts up to 30 s before the segue).
    /// The conductor commits a boundary a safe distance before this frame.
    pub fn first_command_frame(&self) -> u64 {
        self.a_stop.map_or(self.s_b, |(at, _)| at.min(self.s_b))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanKind {
    Segue,
    Sequential,
    Cut,
    SegueNow,
    FadeNext,
    HardTime,
    Continuity,
    Emergency,
}

impl PlanKind {
    pub fn name(self) -> &'static str {
        match self {
            PlanKind::Segue => "segue",
            PlanKind::Sequential => "sequential",
            PlanKind::Cut => "cut",
            PlanKind::Emergency => "emergency",
            PlanKind::SegueNow => "segue_now",
            PlanKind::FadeNext => "fade_next",
            PlanKind::HardTime => "hard_time",
            PlanKind::Continuity => "continuity",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RampInfo {
    pub t_a_s: f64,
    pub t_b_s: f64,
    pub threshold_a_db: f64,
    pub threshold_b_db: f64,
    pub overlap_s: f64,
    pub capped: bool,
}

pub struct PlanInput<'a> {
    pub rate: u32,
    /// The current frame on the station timeline.
    pub now: u64,
    pub a: &'a PlacedView<'a>,
    pub b: &'a Item,
    /// A-side parameters come from this chain's result...
    pub pa: &'a TransitionParams,
    /// ...and B-side ones (alignment, fades in, lead, policy, gap) from this one.
    pub pb: &'a TransitionParams,
    pub kind: TransitionKind,
    /// The automatic fade, seconds, that ends an item flagged `truncated_fade` at its cue-out when nothing else
    /// fades it to silence; 0 is off (the station's `truncated_fade` and `truncated_fade_s`).
    pub truncated_fade_s: f64,
}

fn curve_of(c: FadeCurve) -> Option<Curve> {
    match c {
        FadeCurve::None => None,
        FadeCurve::Linear => Some(Curve::Linear),
        FadeCurve::Cosine => Some(Curve::Cosine),
        FadeCurve::Log => Some(Curve::DbLinear),
    }
}

fn fade_stop(len: i64, curve: Curve) -> StopFade {
    let frames = u32::try_from(len.max(1)).unwrap_or(u32::MAX);
    match curve {
        Curve::Cosine => StopFade::Frames(frames),
        c => StopFade::Curved { frames, curve: c },
    }
}

/// The gain `g` of `T = threshold - g` (2.6): an explicit album gain, else the reference level
/// minus the item's album loudness, else minus its own.
pub fn plex_gain_db(item: &Item, reference_lufs: f64) -> f64 {
    item.album_gain_db
        .unwrap_or_else(|| reference_lufs - item.album_l.unwrap_or(item.l_used))
}

fn declick_frames(rate: u32) -> i64 {
    ((engine::mixer::DECLICK_S * f64::from(rate)).round() as i64).max(1)
}

pub fn plan(inp: &PlanInput) -> Plan {
    let rate = inp.rate;
    let a = inp.a;
    let pb = inp.pb;
    let s_a = a.start_frame as i64;
    // A continuity joint plays A to the last decoded frame of its file, whatever its bounds were.
    let f_end_a = if inp.kind == TransitionKind::Continuity {
        a.file_end_frame(rate)
    } else {
        a.end_frame(rate)
    };
    let now = inp.now as i64;
    let earliest = now + s_frames(MIN_LEAD_S, rate);
    let mut notes = Vec::new();

    // B's bounds. Continuity plays B from frame 0 of its file (2.5, 3.7).
    let (mut b_in, mut b_out, mut b_file_end) = bounds_of(inp.b, pb);
    if inp.kind == TransitionKind::Continuity {
        b_in = 0;
        b_out = inp.b.file_ms;
        b_file_end = true;
    }
    let pickup_ms = (inp.b.markers.attack - b_in).max(0);

    let mut plan = Plan {
        kind: PlanKind::Segue,
        s_b: 0,
        b_bound_in_ms: b_in,
        b_bound_out_ms: b_out,
        b_out_is_file_end: b_file_end,
        b_declick: true,
        b_fade_in: None,
        a_stop: None,
        a_end: None,
        e_a: f_end_a,
        f_end_a,
        overlap_frames: 0,
        lead_frames: 0,
        ramp: None,
        clamped: false,
        notes: vec![],
    };

    let lead_of = |cap_s: f64| -> i64 { ms_frames(pickup_ms, rate).min(s_frames(cap_s, rate)) };

    match &inp.kind {
        TransitionKind::Continuity => {
            plan.kind = PlanKind::Continuity;
            plan.s_b = f_end_a.max(s_a) as u64;
            plan.b_declick = false;
            plan.e_a = f_end_a;
            // A plays to the last decoded frame of its file: nothing stops it.
            if a.frames.is_none() {
                notes.push("gapless_unknown: A's exact length is not known".into());
            }
        }
        TransitionKind::Emergency { drain_frame } => {
            plan.kind = PlanKind::Emergency;
            // B starts 50 ms before A's audio runs out, but never before the earliest safe frame;
            // A fades over 30 ms from that frame (the same click guard as a cut).
            let t = (*drain_frame as i64 - s_frames(EMERGENCY_LEAD_S, rate)).max(earliest);
            plan.s_b = t.max(s_a) as u64;
            plan.e_a = t;
            let len = s_frames(CUT_FADE_S, rate);
            plan.a_stop = Some((t as u64, fade_stop(len, Curve::Cosine)));
            plan.a_end = Some((t + len) as u64);
        }
        TransitionKind::Cut => {
            plan.kind = PlanKind::Cut;
            let t = earliest;
            plan.s_b = t.max(s_a) as u64;
            plan.e_a = t;
            let len = s_frames(CUT_FADE_S, rate);
            plan.a_stop = Some((t as u64, fade_stop(len, Curve::Cosine)));
            plan.a_end = Some((t + len) as u64);
        }
        TransitionKind::SegueNow
        | TransitionKind::FadeNext { .. }
        | TransitionKind::HardTime { .. } => {
            let (kind, fade_s, t) = match &inp.kind {
                TransitionKind::SegueNow => (PlanKind::SegueNow, SEGUE_NOW_FADE_S, earliest),
                TransitionKind::FadeNext { fade_s } => (PlanKind::FadeNext, *fade_s, earliest),
                TransitionKind::HardTime { at_frame, fade_s } => {
                    let f = fade_s.unwrap_or(HARD_TIME_FADE_S);
                    (PlanKind::HardTime, f, *at_frame as i64 - s_frames(f, rate))
                }
                _ => unreachable!(),
            };
            plan.kind = kind;
            let len = s_frames(fade_s, rate);
            let lead = lead_of(pb.lead_max_s);
            // B's attack lands where A's fade ends (for a hard time, on the hard time itself).
            let mut s_b = t + len - lead;
            // Pressed near A's end, A's audio runs out before the fade would finish: B starts where
            // A ends instead, or the station falls silent in between (a `next` 0.6 s before a butt
            // joint left 0.6 s of silence; found in the M1.5 review). A hard time keeps its time.
            if kind != PlanKind::HardTime && s_b > f_end_a {
                s_b = f_end_a;
                notes.push("A ends before its fade would: B starts at A's end".into());
            }
            if s_b < earliest {
                s_b = earliest;
                plan.clamped = true;
                notes.push(
                    "B's start was pushed to now + 0.25 s; its attack lands after the fade".into(),
                );
            }
            if s_b < s_a {
                s_b = s_a;
                plan.clamped = true;
                notes.push("B cannot start before A".into());
            }
            plan.lead_frames = t + len - s_b;
            plan.s_b = s_b as u64;
            plan.e_a = t;
            plan.a_stop = Some((t.max(0) as u64, fade_stop(len, Curve::Cosine)));
            plan.a_end = Some((t.max(0) + len) as u64);
        }
        TransitionKind::Auto => {
            plan_auto(
                inp, &mut plan, &mut notes, pickup_ms, earliest, s_a, f_end_a,
            );
        }
    }

    // A stop at or before A's start frame cancels A outright (the mixer's rule for a voice that
    // has not started): a fade that begins on A's first frame (`basic` with an A shorter than
    // its 5 s overlap, a fade-out marker at the cue-in, a manual action planned before A has
    // started) would silence A instead of fading it. Such a fade starts on A's second frame.
    if let Some((at, fade)) = plan.a_stop {
        if fade != StopFade::Cut && (at as i64) <= s_a {
            let shift = s_a + 1 - at as i64;
            plan.a_stop = Some(((s_a + 1) as u64, fade));
            plan.a_end = plan.a_end.map(|e| e + shift as u64);
        }
    }
    if plan.kind != PlanKind::Continuity {
        plan.overlap_frames = f_end_a - plan.s_b as i64;
    }
    plan.notes = notes;
    plan
}

#[allow(clippy::too_many_arguments)]
fn plan_auto(
    inp: &PlanInput,
    plan: &mut Plan,
    notes: &mut Vec<String>,
    pickup_ms: i64,
    earliest: i64,
    s_a: i64,
    f_end_a: i64,
) {
    let rate = inp.rate;
    let a = inp.a;
    let (pa, pb) = (inp.pa, inp.pb);
    let a_item = a.item;
    let a_voice = a_item.is_voice();
    let declick = declick_frames(rate);

    // ---- A's exit point E_A (A side) ----
    let e_marker = match pa.exit.mode {
        ExitMode::SegueMarker => a.frame_of(a_item.markers.segue, rate),
        ExitMode::LastCrossing => {
            let ms = if a_item.manual_segue {
                Some(a_item.markers.segue)
            } else {
                analysis::markers::last_crossing_ms(
                    &a_item.env,
                    a_item.markers.cue_in,
                    a_item.markers.cue_out,
                    a_item.l_used + pa.exit.threshold_lu,
                )
            };
            a.frame_of(ms.unwrap_or(a_item.markers.cue_out), rate)
        }
        ExitMode::FixedBeforeEnd => f_end_a - s_frames(pa.exit.seconds, rate),
    }
    .clamp(s_a, f_end_a.max(s_a));

    // ---- the start rule (B side) ----
    let accept = if a_voice { 0.0 } else { pa.accept_lead_s };
    let mut e_a = e_marker;
    let mut lead = 0i64;
    let mut ramp = None;
    let s_b_raw;
    if pb.gap_s > 0.0 {
        plan.kind = PlanKind::Sequential;
        s_b_raw = f_end_a + s_frames(pb.gap_s, rate);
        e_a = f_end_a;
    } else {
        match pb.align.mode {
            AlignMode::FixedOverlap => {
                s_b_raw = f_end_a - s_frames(pb.align.overlap_s, rate);
                e_a = s_b_raw;
            }
            AlignMode::AtCueIn => {
                s_b_raw = e_marker;
            }
            AlignMode::AttackToExit => {
                let cap = accept.min(pb.lead_max_s);
                lead = ms_frames(pickup_ms, rate).min(s_frames(cap, rate));
                s_b_raw = e_marker - lead;
            }
            AlignMode::RampOverlap => {
                // A's end ramp over the window A actually plays (it may have come in under other
                // bounds than the ones it leaves under); B's start ramp over its own window.
                let (_, end_a) = a_item.ramps_in_window(a.bound_in_ms, a.bound_out_ms);
                let (b_in, b_out, _) = bounds_of(inp.b, pb);
                let (start_b, _) = inp.b.ramps_in_window(b_in, b_out);
                let ref_lufs = pb.align.gain_reference_lufs;
                let ta_db = pb.align.threshold_db - plex_gain_db(a_item, ref_lufs);
                let tb_db = pb.align.threshold_db - plex_gain_db(inp.b, ref_lufs);
                let t_a = lookup(&end_a, ta_db);
                let t_b = lookup(&start_b, tb_db);
                // A non-finite ramp time (a corrupt stored ramp) is no ramp: `f64::min` would turn
                // NaN into the full 15 s cap.
                let sum = if (t_a + t_b).is_finite() {
                    t_a + t_b
                } else {
                    0.0
                };
                let overlap = sum.min(pb.align.overlap_max_s).max(0.0);
                ramp = Some(RampInfo {
                    t_a_s: t_a,
                    t_b_s: t_b,
                    threshold_a_db: ta_db,
                    threshold_b_db: tb_db,
                    overlap_s: overlap,
                    capped: sum > pb.align.overlap_max_s,
                });
                s_b_raw = f_end_a - s_frames(overlap, rate);
                e_a = s_b_raw;
            }
        }
    }
    let mut s_b = s_b_raw;

    // The voice-content rule: an overlap onto a voice is capped (2.6).
    if a_voice && pb.gap_s <= 0.0 {
        let floor = f_end_a - s_frames(crate::plan::VOICE_OVERLAP_CAP_S, rate);
        if s_b < floor {
            s_b = floor;
            notes.push("voice-content rule: overlap onto a voice capped at 0.5 s".into());
        }
    }
    if s_b < earliest {
        s_b = earliest;
        plan.clamped = true;
        notes.push("B's start was pushed to now + 0.25 s".into());
    }
    if s_b < s_a {
        s_b = s_a;
        plan.clamped = true;
        notes.push("B cannot start before A".into());
    }
    plan.s_b = s_b as u64;
    plan.e_a = e_a;
    plan.lead_frames = if pb.align.mode == AlignMode::AttackToExit {
        (e_a - s_b).max(0)
    } else {
        lead
    };
    plan.ramp = ramp;

    // ---- A's single stop ----
    // A manual fade-out marker beats the preset's own fade and the tail cap (it was set by hand);
    // the preset's out-fade beats the cap; the cap only cuts a tail longer than tail_max_s; and a
    // cue-out inside the file needs a declick at the cue-out. One stop, so nothing has to combine.
    let overlapped = pb.gap_s <= 0.0;
    let mut stop: Option<(i64, StopFade, i64)> = None; // (at, fade, end)
    if let Some(fo) = a_item.markers.fade_out {
        let at = a.frame_of(fo, rate).max(s_a);
        // To A's end: its cue-out under `bounds = cue` (or a manual cue-out), the file's end under
        // `bounds = file`, where B starts at the file's end (ending the fade at the analysed
        // cue-out instead left a gap before B under `plexamp` and `basic`).
        let end = f_end_a;
        let len = end - at;
        if len > 0 {
            stop = Some((at, fade_stop(len, Curve::Cosine), end));
        }
    }
    if stop.is_none() && overlapped && pb.align.mode != AlignMode::RampOverlap {
        if let Some(curve) = curve_of(pa.out_fade.curve).filter(|_| pa.out_fade.seconds > 0.0) {
            // From E_A, or from B's start when that was pushed later (a late plan): A must not
            // fade away before B has started.
            let start = if pb.align.mode == AlignMode::FixedOverlap {
                s_b
            } else {
                e_a.max(s_b)
            };
            let len = s_frames(pa.out_fade.seconds, rate).min(f_end_a - start);
            if len > 0 {
                stop = Some((start, fade_stop(len, curve), start + len));
            }
        } else if pa.out_fade.curve == FadeCurve::None {
            // Measured from E_A, or from B's start when that was pushed later, so a late plan
            // never stops A before B sounds.
            let cap_end = e_a.max(s_b) + s_frames(pa.tail_max_s, rate);
            if cap_end < f_end_a {
                let len = s_frames(pa.tail_fade_s, rate).min(s_frames(pa.tail_max_s, rate));
                if len >= 2 {
                    stop = Some((cap_end - len, fade_stop(len, Curve::Cosine), cap_end));
                } else {
                    stop = Some((cap_end - declick, StopFade::Declick, cap_end));
                }
            }
        }
    }
    if stop.is_none() && !a.bound_out_is_file_end {
        stop = Some((f_end_a - declick, StopFade::Declick, f_end_a));
    }
    // A file cut off in the middle of its own fade (`truncated_fade`): when nothing above fades A to silence
    // (it plays to its end, or stops with only the declick), A gets a short cosine fade that ends at its
    // cue-out, so the tail that normalization lifted never stops dead. The fade never ends before B has
    // started, and a manual fade-out marker (above) wins.
    let fades_to_silence = matches!(
        stop,
        Some((_, StopFade::Frames(_) | StopFade::Curved { .. }, _))
    );
    if !fades_to_silence {
        if let Some(t) =
            truncated_tail_fade(a, inp.truncated_fade_s, plan.s_b as i64, f_end_a, rate)
        {
            notes.push(format!(
                "truncated_fade: A fades over {:.2} s to its cue-out",
                (t.2 - t.0) as f64 / f64::from(rate)
            ));
            stop = Some(t);
        }
    }
    if let Some((at, fade, end)) = stop {
        plan.a_stop = Some((at.max(0) as u64, fade));
        plan.a_end = Some(end.max(0) as u64);
    }

    // ---- B's fade in (level automation) ----
    if let Some(curve) = curve_of(pb.in_fade.curve).filter(|_| pb.in_fade.seconds > 0.0) {
        let cold = inp.b.flags.contains(&analysis::markers::Flag::ColdStart);
        if !(pb.in_fade.skip_if_cold_start && cold) {
            let len = s_frames(pb.in_fade.seconds, rate).max(1) as u64;
            plan.b_fade_in = Some(Segment {
                start: plan.s_b,
                end: plan.s_b + len,
                from: 0.0,
                to: 1.0,
                curve,
            });
        }
    }
}

/// The automatic fade of an item flagged `truncated_fade` with no manual fade-out: `(at, fade, end)` frames, a
/// cosine of `seconds` (shorter when A has not played that long) ending at A's cue-out, but not before
/// `not_before` (B's start) and not after `f_end_a` (where A's audio ends). `None` when it does not apply.
pub fn truncated_tail_fade(
    a: &PlacedView,
    seconds: f64,
    not_before: i64,
    f_end_a: i64,
    rate: u32,
) -> Option<(i64, StopFade, i64)> {
    let item = a.item;
    if seconds <= 0.0
        || item.markers.fade_out.is_some()
        || !item.flags.contains(&analysis::markers::Flag::TruncatedFade)
    {
        return None;
    }
    let s_a = a.start_frame as i64;
    let end = a
        .frame_of(item.markers.cue_out, rate)
        .max(not_before)
        .min(f_end_a);
    let len = s_frames(seconds, rate).min(end - s_a - 1);
    (len >= 2).then(|| (end - len, fade_stop(len, Curve::Cosine), end))
}

/// The plan for an item that has no successor: it plays to its bound end, with the automatic fade of a
/// `truncated_fade` item (see [`truncated_tail_fade`]) when `truncated_fade_s` is above 0.
pub fn last_item_stop(a: &PlacedView, rate: u32, truncated_fade_s: f64) -> Option<(u64, StopFade)> {
    let end = a.end_frame(rate);
    if let Some((at, fade, _)) = truncated_tail_fade(a, truncated_fade_s, 0, end, rate) {
        return Some((at.max(0) as u64, fade));
    }
    if a.bound_out_is_file_end {
        return None;
    }
    Some((
        (end - declick_frames(rate)).max(0) as u64,
        StopFade::Declick,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::Markers;
    use analysis::env::Envelopes;
    use analysis::markers::Flag;
    use analysis::ramp::parse_ramp;
    use common::transition::{resolve, Chain, PresetBook};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    const R: u32 = 48_000;

    fn item(name: &str, kind: &str, file_ms: i64, m: Markers) -> Item {
        Item {
            name: name.into(),
            path: format!("{name}.flac").into(),
            category: kind.into(),
            kind: kind.into(),
            category_preset: None,
            category_patch: serde_json::Value::Null,
            gain_offset_db: 0.0,
            markers: m,
            flags: BTreeSet::new(),
            file_ms,
            manual_cue_in: false,
            manual_cue_out: false,
            manual_segue: false,
            l_used: -14.0,
            true_peak: -3.0,
            gain_trim_db: 0.0,
            album_l: None,
            album_gain_db: None,
            env: Arc::new(Envelopes::default()),
            file_ramps: (vec![], vec![]),
            transition: Default::default(),
            quiet: None,
        }
    }

    fn markers(cue_in: i64, attack: i64, segue: i64, cue_out: i64) -> Markers {
        Markers {
            cue_in,
            attack,
            intro: None,
            segue,
            fade_out: None,
            cue_out,
        }
    }

    fn params(preset: &str) -> TransitionParams {
        resolve(
            &PresetBook::builtin(),
            &Chain {
                station_preset: preset,
                ..Chain::default()
            },
        )
        .unwrap()
    }

    fn placed<'a>(it: &'a Item, start: u64, bounds: (i64, i64, bool)) -> PlacedView<'a> {
        PlacedView {
            item: it,
            start_frame: start,
            bound_in_ms: bounds.0,
            bound_out_ms: bounds.1,
            bound_out_is_file_end: bounds.2,
            frames: None,
        }
    }

    fn go(a: &Item, b: &Item, preset: &str, start: u64, now: u64) -> Plan {
        let p = params(preset);
        let (bi, bo, fe) = bounds_of(a, &p);
        let view = placed(a, start, (bi, bo, fe));
        plan(&PlanInput {
            rate: R,
            now,
            a: &view,
            b,
            pa: &p,
            pb: &p,
            kind: TransitionKind::Auto,
            truncated_fade_s: TRUNCATED_FADE_S,
        })
    }

    #[test]
    fn sam_lands_the_attack_on_the_segue_and_caps_the_tail() {
        // A: 0.5 s cue-in, segue at 20 s, cue-out at 40 s (a 20 s tail); B: cue-in 0.28, attack 1.8 s.
        let a = item("a", "music", 41_000, markers(500, 520, 20_000, 40_000));
        let b = item("b", "music", 30_000, markers(280, 1_800, 25_000, 30_000));
        let pl = go(&a, &b, "sam", 4_800, 0);
        let f_seg = 4_800 + (20_000 - 500) * 48;
        let lead = (1_800 - 280) * 48;
        assert_eq!(pl.e_a, f_seg);
        assert_eq!(pl.s_b as i64, f_seg - lead);
        assert_eq!(pl.lead_frames, lead);
        // The tail cap: a 2 s cosine ending 12 s after the segue.
        let (at, fade) = pl.a_stop.unwrap();
        assert_eq!(fade, StopFade::Frames(96_000));
        assert_eq!(at as i64 + 96_000, f_seg + 12 * 48_000);
        assert!(pl.b_fade_in.is_none());
        assert!(pl.b_declick);
    }

    #[test]
    fn sam_without_a_long_tail_needs_only_the_cue_out_declick() {
        let a = item("a", "music", 30_000, markers(0, 0, 24_000, 28_000));
        let b = item("b", "music", 30_000, markers(0, 0, 25_000, 30_000));
        let pl = go(&a, &b, "sam", 0, 0);
        let (at, fade) = pl.a_stop.unwrap();
        assert_eq!(fade, StopFade::Declick);
        assert_eq!(at as i64, 28_000 * 48 - 144);
        assert_eq!(pl.a_end, Some(28_000 * 48));
    }

    #[test]
    fn a_liner_accepts_no_lead_and_a_manual_fade_out_replaces_the_cap() {
        let mut a = item("liner", "liner", 8_000, markers(180, 180, 7_700, 7_900));
        a.category_patch =
            serde_json::from_str(common::transition::BUILTIN_CATEGORIES[1].4).unwrap();
        let b = item("b", "music", 30_000, markers(280, 1_800, 25_000, 30_000));
        let pl = go(&a, &b, "sam", 0, 0);
        assert!(pl.s_b as i64 >= pl.e_a, "no lead into a voice");
        assert_eq!(pl.lead_frames, 0);
        // A manual fade-out marker beats the tail cap.
        let mut a = item("a", "music", 41_000, markers(500, 520, 20_000, 40_000));
        a.markers.fade_out = Some(30_000);
        let pl = go(&a, &b, "sam", 0, 0);
        let (at, fade) = pl.a_stop.unwrap();
        assert_eq!(at as i64, (30_000 - 500) * 48);
        assert_eq!(fade, StopFade::Frames(((40_000 - 30_000) * 48) as u32));
    }

    fn truncated(mut it: Item) -> Item {
        it.flags.insert(Flag::TruncatedFade);
        it
    }

    fn go_with(a: &Item, b: &Item, preset: &str, truncated_fade_s: f64) -> Plan {
        let p = params(preset);
        let view = placed(a, 0, bounds_of(a, &p));
        plan(&PlanInput {
            rate: R,
            now: 0,
            a: &view,
            b,
            pa: &p,
            pb: &p,
            kind: TransitionKind::Auto,
            truncated_fade_s,
        })
    }

    #[test]
    fn a_truncated_fade_gets_a_short_fade_ending_at_the_cue_out() {
        // A: the file ends at 208.9 s with its fade still going; its cue-out is 208.5 s.
        let a = truncated(item(
            "a",
            "music",
            208_900,
            markers(0, 300, 190_000, 208_500),
        ));
        let b = item("b", "music", 30_000, markers(0, 0, 25_000, 30_000));
        // `none` (bounds = cue): A would stop at its cue-out with only the declick.
        let pl = go_with(&a, &b, "none", 2.0);
        let (at, fade) = pl.a_stop.unwrap();
        assert_eq!(fade, StopFade::Frames(96_000));
        assert_eq!(at as i64, 208_500 * 48 - 96_000);
        assert_eq!(pl.a_end, Some(208_500 * 48));
        assert!(pl.notes.iter().any(|n| n.starts_with("truncated_fade")));
        // The setting's length.
        let pl = go_with(&a, &b, "none", 3.5);
        assert_eq!(pl.a_stop.unwrap().1, StopFade::Frames(168_000));
        // Off: the declick as before. Unflagged: the same.
        let pl = go_with(&a, &b, "none", 0.0);
        assert_eq!(pl.a_stop.unwrap().1, StopFade::Declick);
        let plain = item("a", "music", 208_900, markers(0, 300, 190_000, 208_500));
        assert_eq!(
            go_with(&plain, &b, "none", 2.0).a_stop.unwrap().1,
            StopFade::Declick
        );
    }

    #[test]
    fn under_plexamp_a_truncated_fade_ends_at_the_cue_out_but_not_before_b_starts() {
        // plexamp (bounds = file, no fades): A plays to its file end and nothing would stop it. With no ramps
        // the overlap is 0, so B starts at A's file end, after the cue-out: the fade ends there instead.
        let a = truncated(item(
            "a",
            "music",
            208_900,
            markers(0, 300, 190_000, 208_500),
        ));
        let b = item("b", "music", 30_000, markers(0, 0, 25_000, 30_000));
        let plain = item("a", "music", 208_900, markers(0, 300, 190_000, 208_500));
        assert_eq!(go_with(&plain, &b, "plexamp", 2.0).a_stop, None);
        let pl = go_with(&a, &b, "plexamp", 2.0);
        assert_eq!(pl.s_b as i64, 208_900 * 48);
        let (at, fade) = pl.a_stop.unwrap();
        assert_eq!(fade, StopFade::Frames(96_000));
        assert_eq!(at as i64 + 96_000, 208_900 * 48);
        // B starting 2.4 s before A's end (a ramp overlap): the fade ends at the cue-out, under B.
        let p = params("plexamp");
        let view = placed(&a, 0, bounds_of(&a, &p));
        let f_end = 208_900 * 48;
        let t = truncated_tail_fade(&view, 2.0, f_end - 2_400 * 48, f_end, R).unwrap();
        assert_eq!(t.2, 208_500 * 48);
        assert_eq!(t.0, 208_500 * 48 - 96_000);
    }

    #[test]
    fn a_manual_fade_out_or_a_preset_fade_wins_over_the_automatic_one() {
        let b = item("b", "music", 30_000, markers(0, 0, 25_000, 30_000));
        // A manual fade-out marker (5 s before the cue-out).
        let mut a = truncated(item(
            "a",
            "music",
            208_900,
            markers(0, 300, 190_000, 208_500),
        ));
        a.markers.fade_out = Some(203_500);
        let (at, fade) = go_with(&a, &b, "none", 2.0).a_stop.unwrap();
        assert_eq!(at as i64, 203_500 * 48);
        assert_eq!(fade, StopFade::Frames(5_000 * 48));
        // sam's tail cap already fades A to silence (a 20 s tail after the segue).
        let a = truncated(item(
            "a",
            "music",
            41_000,
            markers(500, 520, 20_000, 40_000),
        ));
        let (at, fade) = go_with(&a, &b, "sam", 2.0).a_stop.unwrap();
        assert_eq!(fade, StopFade::Frames(96_000));
        assert_eq!(at as i64 + 96_000, (20_000 - 500 + 12_000) * 48);
        // basic's own 5 s linear fade.
        let a = truncated(item(
            "a",
            "music",
            60_000,
            markers(500, 520, 50_000, 58_000),
        ));
        let fade = go_with(&a, &b, "basic", 2.0).a_stop.unwrap().1;
        assert!(matches!(
            fade,
            StopFade::Curved {
                frames: 240_000,
                ..
            }
        ));
    }

    #[test]
    fn the_last_item_gets_the_automatic_fade_too() {
        let a = truncated(item(
            "a",
            "music",
            208_900,
            markers(0, 300, 190_000, 208_500),
        ));
        let p = params("plexamp");
        let view = placed(&a, 0, bounds_of(&a, &p));
        assert_eq!(
            last_item_stop(&view, R, 2.0),
            Some(((208_500 * 48 - 96_000) as u64, StopFade::Frames(96_000)))
        );
        assert_eq!(last_item_stop(&view, R, 0.0), None);
    }

    #[test]
    fn basic_overlaps_five_seconds_from_the_file_ends() {
        let a = item("a", "music", 60_000, markers(500, 520, 50_000, 58_000));
        let b = item("b", "music", 30_000, markers(280, 1_800, 25_000, 30_000));
        let pl = go(&a, &b, "basic", 0, 0);
        // bounds = file: A plays 0 to 60 s.
        assert_eq!(pl.f_end_a, 60_000 * 48);
        assert_eq!(pl.s_b as i64, 55_000 * 48);
        assert_eq!(pl.overlap_frames, 240_000);
        assert_eq!(pl.b_bound_in_ms, 0);
        let seg = pl.b_fade_in.unwrap();
        assert_eq!(
            (seg.start, seg.end, seg.curve),
            (pl.s_b, pl.s_b + 240_000, Curve::Linear)
        );
        let (at, fade) = pl.a_stop.unwrap();
        assert_eq!(at, pl.s_b);
        assert_eq!(
            fade,
            StopFade::Curved {
                frames: 240_000,
                curve: Curve::Linear
            }
        );
    }

    #[test]
    fn plexamp_overlap_is_the_capped_sum_of_the_two_ramp_times() {
        let mut a = item("a", "music", 60_000, markers(500, 520, 50_000, 58_000));
        let mut b = item("b", "music", 30_000, markers(280, 1_800, 25_000, 30_000));
        // T = -16 - g with g = -16 - L: T = L = -14 here.
        a.file_ramps.1 = parse_ramp("-51.00 0.10;-20.00 4.00;-14.00 6.00;-10.00 20.00;");
        b.file_ramps.0 = parse_ramp("-51.00 0.00;-20.00 1.00;-14.00 3.00;-10.00 9.00;");
        let pl = go(&a, &b, "plexamp", 0, 0);
        let ramp = pl.ramp.unwrap();
        assert!((ramp.t_a_s - 6.0).abs() < 1e-9 && (ramp.t_b_s - 3.0).abs() < 1e-9);
        assert_eq!(pl.overlap_frames, 9 * 48_000);
        assert_eq!(pl.s_b as i64, 60_000 * 48 - 9 * 48_000);
        assert!(
            pl.a_stop.is_none(),
            "A plays to its end and nothing touches it"
        );
        assert!(pl.b_fade_in.is_none());
        // The cap.
        a.file_ramps.1 = parse_ramp("-51.00 0.10;-14.00 13.00;-10.00 20.00;");
        let pl = go(&a, &b, "plexamp", 0, 0);
        assert_eq!(pl.overlap_frames, 15 * 48_000);
        assert!(pl.ramp.unwrap().capped);
        // Empty ramps: a butt joint.
        a.file_ramps.1 = vec![];
        b.file_ramps.0 = vec![];
        let pl = go(&a, &b, "plexamp", 0, 0);
        assert_eq!(pl.overlap_frames, 0);
        assert_eq!(pl.s_b as i64, pl.f_end_a);
    }

    #[test]
    fn none_ends_a_at_its_cue_out_and_starts_b_on_the_next_frame() {
        let a = item("a", "music", 30_000, markers(500, 520, 20_000, 28_000));
        let b = item("b", "music", 30_000, markers(280, 1_800, 25_000, 30_000));
        let pl = go(&a, &b, "none", 0, 0);
        assert_eq!(pl.s_b as i64, pl.f_end_a);
        assert_eq!(pl.f_end_a, (28_000 - 500) * 48);
        assert_eq!(pl.a_stop, Some((pl.s_b - 144, StopFade::Declick)));
    }

    #[test]
    fn continuity_starts_b_at_the_last_frame_of_a_with_no_ramp() {
        let a = item("a", "music", 30_000, markers(500, 520, 20_000, 28_000));
        let b = item("b", "music", 30_000, markers(280, 1_800, 25_000, 30_000));
        let p = params("plexamp");
        let view = PlacedView {
            frames: Some(1_000_000),
            ..placed(&a, 10_000, (500, 30_000, true))
        };
        let pl = plan(&PlanInput {
            rate: R,
            now: 0,
            a: &view,
            b: &b,
            pa: &p,
            pb: &p,
            kind: TransitionKind::Continuity,
            truncated_fade_s: TRUNCATED_FADE_S,
        });
        assert_eq!(pl.kind, PlanKind::Continuity);
        assert_eq!(pl.s_b, 1_010_000);
        assert!(!pl.b_declick && pl.a_stop.is_none() && pl.b_fade_in.is_none());
        assert_eq!(pl.b_bound_in_ms, 0);
    }

    #[test]
    fn fm_skips_a_long_intro_to_its_attack() {
        let a = item("a", "music", 30_000, markers(0, 0, 20_000, 28_000));
        let mut b = item("b", "music", 60_000, markers(0, 8_000, 50_000, 58_000));
        b.flags.insert(Flag::LongIntro);
        let (bin, _, _) = bounds_of(&b, &params("fm"));
        assert_eq!(bin, 7_980);
        b.manual_cue_in = true;
        assert_eq!(bounds_of(&b, &params("fm")).0, 0, "a manual cue-in wins");
        b.manual_cue_in = false;
        let pl = go(&a, &b, "fm", 0, 0);
        assert_eq!(pl.b_bound_in_ms, 7_980);
        assert_eq!(pl.lead_frames, 20 * 48);
    }

    #[test]
    fn manual_transitions_follow_the_table() {
        let a = item("a", "music", 60_000, markers(0, 0, 50_000, 58_000));
        let b = item("b", "music", 30_000, markers(0, 0, 25_000, 30_000));
        let p = params("sam");
        let view = placed(&a, 0, (0, 58_000, false));
        let mk = |kind| {
            plan(&PlanInput {
                rate: R,
                now: 100_000,
                a: &view,
                b: &b,
                pa: &p,
                pb: &p,
                kind,
                truncated_fade_s: TRUNCATED_FADE_S,
            })
        };
        let t = 100_000 + 12_000;
        let pl = mk(TransitionKind::SegueNow);
        assert_eq!(pl.a_stop, Some((t, StopFade::Frames(48_000))));
        assert_eq!(
            pl.s_b,
            t + 48_000,
            "a cold start lands at the end of the fade"
        );
        let pl = mk(TransitionKind::FadeNext { fade_s: 2.0 });
        assert_eq!(pl.a_stop, Some((t, StopFade::Frames(96_000))));
        assert_eq!(pl.s_b, t + 96_000);
        let pl = mk(TransitionKind::Cut);
        assert_eq!(pl.a_stop, Some((t, StopFade::Frames(1_440))));
        assert_eq!(pl.s_b, t);
        let pl = mk(TransitionKind::HardTime {
            at_frame: 1_000_000,
            fade_s: None,
        });
        assert_eq!(
            pl.a_stop,
            Some((1_000_000 - 72_000, StopFade::Frames(72_000)))
        );
        assert_eq!(pl.s_b, 1_000_000);
    }

    #[test]
    fn b_never_starts_before_now_plus_a_quarter_second() {
        let a = item("a", "music", 30_000, markers(0, 0, 20_000, 28_000));
        let b = item("b", "music", 30_000, markers(0, 0, 25_000, 30_000));
        let now = 20_000 * 48;
        let pl = go(&a, &b, "sam", 0, now);
        assert_eq!(pl.s_b, now + 12_000);
        assert!(pl.clamped);
    }

    /// The end of A's sound: its stop's end, else the end of its bounds.
    fn a_gone(pl: &Plan) -> i64 {
        pl.a_end.map_or(pl.f_end_a, |e| (e as i64).min(pl.f_end_a))
    }

    /// Review M1.4: every preset against degenerate and very short items. Invariants: B never
    /// starts before A; a fading stop never lands on or before A's first frame (the mixer would
    /// cancel A: `basic` with a 3 s A silenced it); A's stop ends after it starts; and with no gap
    /// asked for, B starts no later than A's sound ends (no dead air).
    #[test]
    fn degenerate_and_short_items_never_cancel_a_or_leave_a_gap() {
        let s_a = 1_000_000u64;
        let items = [
            // 3 s, normal markers.
            item("short", "music", 3_000, markers(20, 40, 2_500, 2_900)),
            // 50 ms.
            item("tiny", "music", 50, markers(0, 0, 40, 50)),
            // Segue before the cue-in, attack after the cue-out.
            item(
                "inverted",
                "music",
                10_000,
                markers(4_000, 9_500, 1_000, 5_000),
            ),
            // Cue-in = cue-out.
            item(
                "empty",
                "music",
                10_000,
                markers(5_000, 5_000, 5_000, 5_000),
            ),
            // A fade-out marker on the cue-in.
            {
                let mut i = item(
                    "fade_at_in",
                    "music",
                    10_000,
                    markers(500, 600, 8_000, 9_000),
                );
                i.markers.fade_out = Some(500);
                i
            },
            // A long, ordinary item.
            item("long", "music", 60_000, markers(500, 2_000, 50_000, 58_000)),
        ];
        for preset in ["basic", "plexamp", "sam", "fm", "none"] {
            let p = params(preset);
            for a in &items {
                for b in &items {
                    let (bi, bo, fe) = bounds_of(a, &p);
                    let view = placed(a, s_a, (bi, bo, fe));
                    let pl = plan(&PlanInput {
                        rate: R,
                        now: 0,
                        a: &view,
                        b,
                        pa: &p,
                        pb: &p,
                        kind: TransitionKind::Auto,
                        truncated_fade_s: TRUNCATED_FADE_S,
                    });
                    let ctx = format!("{preset}: {} -> {}: {pl:?}", a.name, b.name);
                    assert!(pl.s_b >= s_a, "B before A: {ctx}");
                    if let Some((at, fade)) = pl.a_stop {
                        if fade != StopFade::Cut {
                            assert!(at > s_a, "A's stop cancels A: {ctx}");
                        }
                        assert!(pl.a_end.unwrap() >= at, "{ctx}");
                    }
                    if pl.kind != PlanKind::Sequential {
                        assert!(
                            pl.s_b as i64 <= a_gone(&pl).max(s_a as i64),
                            "dead air: {ctx}"
                        );
                    }
                }
            }
        }
    }

    /// Manual transitions planned before A has started (an operator's action after the boundary
    /// into A was committed) never cancel A or start B before it.
    #[test]
    fn manual_transitions_before_a_has_started_never_cancel_it() {
        let a = item("a", "music", 60_000, markers(0, 0, 50_000, 58_000));
        let b = item("b", "music", 30_000, markers(0, 800, 25_000, 30_000));
        let p = params("sam");
        let s_a = 500_000u64;
        let view = placed(&a, s_a, (0, 58_000, false));
        for kind in [
            TransitionKind::SegueNow,
            TransitionKind::FadeNext { fade_s: 2.0 },
            TransitionKind::Cut,
            TransitionKind::HardTime {
                at_frame: 300_000,
                fade_s: None,
            },
        ] {
            let pl = plan(&PlanInput {
                rate: R,
                now: 100_000,
                a: &view,
                b: &b,
                pa: &p,
                pb: &p,
                kind: kind.clone(),
                truncated_fade_s: TRUNCATED_FADE_S,
            });
            let (at, _) = pl.a_stop.unwrap();
            assert!(
                at > s_a,
                "{kind:?}: A's stop at {at} cancels A (start {s_a})"
            );
            assert!(pl.s_b >= s_a, "{kind:?}: B at {} before A", pl.s_b);
        }
    }

    /// M1.6, architecture 1.5: A's audio runs out early (a fatal decode error, a truncated file). B starts 50 ms
    /// before that and A gets a 30 ms fade from the same frame; never earlier than now + 0.25 s.
    #[test]
    fn the_emergency_transition_starts_b_50_ms_before_a_runs_out() {
        let a = item("a", "music", 60_000, markers(0, 0, 58_000, 58_000));
        let b = item("b", "music", 30_000, markers(0, 0, 25_000, 30_000));
        let p = params("plexamp");
        let view = placed(&a, 0, (0, 58_000, false));
        let drain = 24 * R as u64;
        let now = 5 * R as u64;
        let pl = plan(&PlanInput {
            rate: R,
            now,
            a: &view,
            b: &b,
            pa: &p,
            pb: &p,
            kind: TransitionKind::Emergency { drain_frame: drain },
            truncated_fade_s: TRUNCATED_FADE_S,
        });
        assert_eq!(pl.kind, PlanKind::Emergency);
        assert_eq!(pl.s_b, drain - 2_400);
        assert_eq!(pl.a_stop, Some((drain - 2_400, StopFade::Frames(1_440))));
        assert!(
            pl.a_end.unwrap() <= drain,
            "A is gone before its audio would have run out"
        );
        // Too late to do it that early: now + 0.25 s.
        let pl = plan(&PlanInput {
            rate: R,
            now: drain - 100,
            a: &view,
            b: &b,
            pa: &p,
            pb: &p,
            kind: TransitionKind::Emergency { drain_frame: drain },
            truncated_fade_s: TRUNCATED_FADE_S,
        });
        assert_eq!(pl.s_b, drain - 100 + 12_000);
    }

    /// `next` or `fade_next` pressed less than a fade before A's end: B starts where A's audio ends,
    /// not where the fade would have finished (review of M1.5: a `next` 0.6 s before a butt joint
    /// left 0.6 s of silence). A hard time keeps its time.
    #[test]
    fn a_manual_fade_near_as_end_starts_b_by_as_end() {
        let a = item("a", "music", 60_000, markers(0, 0, 58_000, 58_000));
        let b = item("b", "music", 30_000, markers(0, 0, 25_000, 30_000));
        let p = params("sam");
        let view = placed(&a, 0, (0, 58_000, false));
        let f_end_a = 58 * R as u64;
        for kind in [
            TransitionKind::SegueNow,
            TransitionKind::FadeNext { fade_s: 2.0 },
        ] {
            // Pressed 0.6 s before A ends: the fade would end 0.65 s (segue now) or 1.65 s (fade
            // next) after it.
            let now = f_end_a - (0.6 * f64::from(R)) as u64;
            let pl = plan(&PlanInput {
                rate: R,
                now,
                a: &view,
                b: &b,
                pa: &p,
                pb: &p,
                kind: kind.clone(),
                truncated_fade_s: TRUNCATED_FADE_S,
            });
            println!(
                "{kind:?} 0.6 s before A's end: B at {} (A ends {f_end_a}), gap {} frames (limit 0)",
                pl.s_b,
                pl.s_b as i64 - f_end_a as i64
            );
            assert!(pl.s_b <= f_end_a, "{kind:?}: B at {} after A's end", pl.s_b);
        }
    }

    /// `ramp_overlap` at the edges: a threshold above every point and empty ramps give 0; a
    /// non-finite ramp time gives 0 (not the 15 s cap); an overlap longer than A starts B with A.
    #[test]
    fn ramp_overlap_edges() {
        let mut a = item("a", "music", 60_000, markers(500, 520, 50_000, 58_000));
        let mut b = item("b", "music", 30_000, markers(280, 1_800, 25_000, 30_000));
        // T = -14 dB; both ramps top out below it.
        a.file_ramps.1 = parse_ramp("-51.00 0.10;-20.00 4.00;");
        b.file_ramps.0 = parse_ramp("-51.00 0.00;-20.00 1.00;");
        let pl = go(&a, &b, "plexamp", 0, 0);
        assert_eq!(pl.overlap_frames, 0, "T above every point");
        a.file_ramps.1 = parse_ramp("-51.00 0.10;-14.00 NaN;");
        let pl = go(&a, &b, "plexamp", 0, 0);
        println!("NaN ramp: {:?}", pl.ramp);
        assert_eq!(pl.overlap_frames, 0, "a NaN ramp time");
        // A 3 s A with a 10 s overlap: B starts with A.
        let mut short = item("short", "music", 3_000, markers(0, 0, 2_500, 3_000));
        short.file_ramps.1 = parse_ramp("-51.00 0.10;-14.00 5.00;");
        b.file_ramps.0 = parse_ramp("-51.00 0.00;-14.00 5.00;");
        let pl = go(&short, &b, "plexamp", 100_000, 0);
        assert_eq!(pl.s_b, 100_000);
        assert!(pl.clamped && pl.a_stop.is_none());
    }

    /// A plan made late (B pushed to now + 0.25 s, after A's exit) keeps A sounding until B
    /// starts: the tail cap and an out-fade are measured from B's start then.
    /// M1.4 review, left for M1.5: A's end ramp is measured over the window A actually plays, not
    /// over the bounds of the chain it leaves under. A came in under `bounds = cue` (playing 0.5 to
    /// 50 s of a 60 s file whose last 10 s are quiet) and leaves under `plexamp` (`bounds = file`):
    /// the stored whole-file ramp says 10 s, the played window ends loud.
    #[test]
    fn a_ramp_is_measured_over_the_window_a_was_placed_with() {
        let mut a = item("a", "music", 60_000, markers(500, 520, 49_000, 50_000));
        let mut k10 = vec![-14.0f32; 5000];
        k10.extend(vec![-60.0f32; 1000]);
        a.env = Arc::new(Envelopes {
            k10,
            ..Envelopes::default()
        });
        a.file_ramps.1 = parse_ramp("-51.00 0.10;-14.00 10.00;");
        let mut b = item("b", "music", 30_000, markers(0, 20, 25_000, 30_000));
        b.file_ramps.0 = parse_ramp("-51.00 0.00;-14.00 1.00;");
        let p = params("plexamp");
        let view = placed(&a, 0, (500, 50_000, false));
        let pl = plan(&PlanInput {
            rate: R,
            now: 0,
            a: &view,
            b: &b,
            pa: &p,
            pb: &p,
            kind: TransitionKind::Auto,
            truncated_fade_s: TRUNCATED_FADE_S,
        });
        let r = pl.ramp.expect("plexamp reads ramps");
        assert!(
            r.t_a_s < 2.0,
            "A's end ramp came from the whole-file ramp ({} s), not its played window",
            r.t_a_s
        );
        // Placed over the whole file the stored ramp is the right one.
        let view = placed(&a, 0, (0, 60_000, true));
        let pl = plan(&PlanInput {
            rate: R,
            now: 0,
            a: &view,
            b: &b,
            pa: &p,
            pb: &p,
            kind: TransitionKind::Auto,
            truncated_fade_s: TRUNCATED_FADE_S,
        });
        assert!((pl.ramp.unwrap().t_a_s - 10.0).abs() < 1e-9);
    }

    #[test]
    fn a_late_plan_never_stops_a_before_b_starts() {
        let mut a = item("a", "music", 60_000, markers(0, 0, 20_000, 58_000));
        a.category_patch = serde_json::json!({"tail_max_s": 1.0, "tail_fade_s": 0.5});
        let b = item("b", "music", 30_000, markers(0, 0, 25_000, 30_000));
        let now = 22_000 * 48; // 2 s after A's exit
        let mut p = params("sam");
        p.tail_max_s = 1.0;
        p.tail_fade_s = 0.5;
        let (bi, bo, fe) = bounds_of(&a, &p);
        let view = placed(&a, 0, (bi, bo, fe));
        let pl = plan(&PlanInput {
            rate: R,
            now,
            a: &view,
            b: &b,
            pa: &p,
            pb: &p,
            kind: TransitionKind::Auto,
            truncated_fade_s: TRUNCATED_FADE_S,
        });
        assert!(pl.clamped);
        let end = pl.a_end.unwrap();
        println!("late plan: B at {}, A's stop ends at {end}", pl.s_b);
        assert!(end > pl.s_b, "A gone at {end}, B starts at {}", pl.s_b);
    }
}
