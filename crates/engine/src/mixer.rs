//! The mixer (architecture 2.1 to 2.4): eight voices, a bus, the safety limiter and the master
//! gain. It is the [`Source`] that rt-out renders, so **everything in this file that runs from
//! `render` or `command` is real-time code**: no allocation, no locks, no logging, no syscalls. It
//! talks to the rest of the process through `rtrb` rings and atomics only.
//!
//! # Timeline
//!
//! All frames in commands and events are on **one timeline: the output timeline**, frame 0 being
//! the first frame the mixer ever renders (it equals `Stats::source_frames` of rt-out at the end
//! of a block; a gap after an xrun or a device loss does not advance it, because voices do not
//! advance during a gap, 1.5). The safety limiter delays audio by its lookahead (2 ms); the
//! mixer compensates by mixing the bus that far ahead of the output, so a voice commanded to start
//! at frame `F` is audible at output frame `F`, exactly. The cost is that a command whose frame is
//! less than the lookahead ahead of the output is already too late ("late start"; the voice then
//! starts at the earliest possible frame and a `Late` event is posted). The conductor sends
//! commands at least 2 s ahead.
//!
//! # Voices
//!
//! A voice is loaded through the load ring ([`MixerControl::load`]): its ring consumer, status,
//! start frame, gain, and whether it gets a 3 ms declick in. Loads are not commands because they
//! carry the ring consumer (not `Copy`). Later changes (stop, envelope segments) are
//! [`MixerCmd`]s in the ordinary command ring, addressed by `(voice, generation)` so a command for
//! a voice that has since been released is ignored, never applied to its successor. rt-out drains
//! the load ring before it applies a command, so a command sent after a load always finds its voice.
//!
//! A voice starts at `start_frame` with a 3 ms cosine fade in (unless the load asked for none,
//! which is what gapless album continuity does), plays until its ring runs dry after the decoder
//! reported end of file (no ramp: the file's own last frame is the last frame), or until its stop
//! frame, where it fades out over 3 ms (or another length; or `Cut`). Gain per frame is
//! `gain * envelope(frame) * declick_in * declick_out`.
//!
//! If a voice's ring runs empty before the decoder has finished, the missing frames are silence
//! (counted, one `Starved` event per episode) and the voice does not skip ahead: the audio that
//! arrives is played late, not dropped.
//!
//! When a voice finishes, its ring consumer goes back on the **garbage ring** and is freed by the
//! control side; the mixer never frees. The slot returns to `Idle` only when both the garbage and
//! the `Ended` event have been delivered, so the control side cannot reuse a slot too early and
//! an `Ended` event is never lost.

use crate::chain::{self, ChainControl, ChainHost, ChainMeters};
use crate::envelope::{Curve, Envelope, Segment};
use crate::feed::{FeedStatus, VoiceFeed};
use crate::limiter::Limiter;
use crate::loudness::LoudnessLink;
use crate::rt::{Command, CommandKind};
use crate::source::Source;
use crate::tplimiter::TpLimiter;
use rtrb::{Consumer, Producer, RingBuffer};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::*};
use std::sync::Arc;

pub const VOICES: usize = 8;
/// The largest block `render` handles in one piece (longer requests are split).
pub const MAX_BLOCK: usize = 1024;
/// Length of the automatic declick ramps (architecture 2.1).
pub const DECLICK_S: f64 = 0.003;
pub const LOAD_RING: usize = 16;
pub const EVENT_RING: usize = 256;
pub const GARBAGE_RING: usize = 64;
/// Frames each meter-tap ring holds (1.2, 11.3): about 1.4 s at 48 kHz. The meter thread drains them every
/// few milliseconds; when it does not, the mixer drops whole blocks and counts them (`tap_drops`).
pub const TAP_FRAMES: usize = 65_536;
/// Entries of the block-info ring that travels with the post tap (one per rendered chunk of at most
/// [`MAX_BLOCK`] frames; 4,096 of them cover about 85 s).
pub const INFO_RING: usize = 4096;
/// The program-silence content gate for the loudness windows (6.1): a block counts as silence once the
/// output has been below the silence threshold this long.
pub const LOUD_SILENCE_GATE_S: f64 = 1.0;

/// What travels beside each block of the post tap (and, when `HAS_LOUD`, of the loudness tap): its
/// length and the content-gate flags the loudness windows need (architecture 6.1). Pushed after the
/// audio, so a reader that sees an entry finds all of its audio in the rings.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BlockInfo {
    pub frames: u32,
    pub flags: u8,
    /// Which item is on air: a counter that moves each time a voice of a non-gated category starts (the
    /// loudness windows judge a block by its item, `within_item = hold`).
    pub item: u32,
    /// The largest sample magnitude of the bus in this block (linear, before the loudness gain).
    pub peak: f32,
}

impl BlockInfo {
    /// The station's transport is not `playing` (stopped, held, paused, waiting).
    pub const NOT_PLAYING: u8 = 1;
    /// A master-gain ramp is running or the master is not at unity.
    pub const MASTER_RAMP: u8 = 2;
    /// A voice of a gated category (`gate_categories`) is audible.
    pub const GATED_VOICE: u8 = 4;
    /// The program-silence detector reports silence.
    pub const SILENCE: u8 = 8;
    /// The loudness tap carries this block's audio too.
    pub const HAS_LOUD: u8 = 16;
    /// The flags that keep a block out of the loudness windows.
    pub const CONTENT_GATES: u8 = 1 | 2 | 4 | 8;
}

// ---------------------------------------------------------------------------------------------
// Commands, loads, events

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceId {
    pub slot: u8,
    pub gen: u32,
}

/// How a voice leaves at its stop frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopFade {
    /// The 3 ms cosine declick.
    Declick,
    /// A cosine fade of this many frames, starting at the stop frame.
    Frames(u32),
    /// A fade of this many frames with the given curve, starting at the stop frame. The way to
    /// fade anything to silence (`basic`'s linear fade, the `log` fade, 2.6): fades to silence go
    /// through a stop, never through the envelope, because a stop composes with another stop
    /// (the earlier end wins and the gain never jumps) and overlapping envelope segments do not.
    Curved { frames: u32, curve: Curve },
    /// Silence from the stop frame on, no ramp (only for a signal that is already silent).
    Cut,
}

/// Commands for the mixer, carried in the rt command ring as `CommandKind::Mixer`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MixerCmd {
    /// Fade the voice out starting at `at` (its cue-out or a fade end), then release it. A stop at
    /// or before the voice's start cancels it. On a playing voice a stop frame already mixed
    /// is moved to the next frame, and a stop during a stop fade can only shorten that fade
    /// (the gain never jumps; see `GainState::set_stop`). A stop can only bring the voice's end
    /// earlier: one that would end later than the stop already set is ignored, whether or not
    /// that stop has begun, so two stops for one voice never lengthen it. To move a stop that has
    /// not begun to a later frame, send [`MixerCmd::ClearStop`] first.
    Stop {
        voice: u8,
        gen: u32,
        at: u64,
        fade: StopFade,
    },
    /// Add a ramp segment to the voice's envelope (at most 4 pending).
    Envelope { voice: u8, gen: u32, seg: Segment },
    /// Add a ramp segment to the master gain (after the limiter; output frames).
    Master { seg: Segment },
    /// Remove the voice's stop if its fade has not begun (an operator's "hold" after a boundary
    /// was committed: the voice then plays to its own end or to the next stop). A stop whose fade
    /// is under way is kept, since undoing it would raise the gain.
    ClearStop { voice: u8, gen: u32 },
}

impl VoiceId {
    pub fn stop(self, at: u64, fade: StopFade) -> Command {
        Command::now(CommandKind::Mixer(MixerCmd::Stop {
            voice: self.slot,
            gen: self.gen,
            at,
            fade,
        }))
    }
    /// See [`MixerCmd::ClearStop`].
    pub fn clear_stop(self) -> Command {
        Command::now(CommandKind::Mixer(MixerCmd::ClearStop {
            voice: self.slot,
            gen: self.gen,
        }))
    }
    pub fn envelope(self, seg: Segment) -> Command {
        Command::now(CommandKind::Mixer(MixerCmd::Envelope {
            voice: self.slot,
            gen: self.gen,
            seg,
        }))
    }
    /// A cosine fade to silence from `start` over `frames`; the voice is released at `start +
    /// frames`. One stop command, so a second fade or stop composes with it (the earlier end
    /// wins and the gain never jumps). Fades to silence never go through the envelope.
    pub fn fade_out(self, start: u64, frames: u64) -> Command {
        self.stop(
            start,
            StopFade::Frames(u32::try_from(frames).unwrap_or(u32::MAX)),
        )
    }
}

pub fn master_ramp(seg: Segment) -> Command {
    Command::now(CommandKind::Mixer(MixerCmd::Master { seg }))
}

/// What the control side hands over with a voice ring.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoadParams {
    /// Frame at which the first sample plays.
    pub start_frame: u64,
    /// Constant linear (normalization) gain.
    pub gain: f32,
    /// A 3 ms fade in. False for gapless continuity, where the joint carries no ramp.
    pub declick_in: bool,
    /// An optional stop, set at load time.
    pub stop: Option<(u64, StopFade)>,
}

impl LoadParams {
    pub fn at(start_frame: u64) -> Self {
        Self {
            start_frame,
            gain: 1.0,
            declick_in: true,
            stop: None,
        }
    }
}

pub struct VoiceLoad {
    voice: u8,
    gen: u32,
    ring: Consumer<f32>,
    status: Arc<FeedStatus>,
    params: LoadParams,
    /// The voice's category is gated out of the loudness measurement (6.1).
    loud_gated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// The decoder reached the end of the file and the ring drained.
    Eof,
    /// As `Eof`, but the decoder had failed or the file was truncated.
    Failed,
    /// The stop frame was reached.
    Stopped,
    /// Stopped before it started.
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MixerEvent {
    /// The voice's first sample is at `frame`.
    Started { voice: u8, gen: u32, frame: u64 },
    /// The voice's output ended at `frame` (the first frame it no longer contributes to).
    /// `played` is the number of frames it took from its ring.
    Ended {
        voice: u8,
        gen: u32,
        frame: u64,
        reason: EndReason,
        played: u64,
    },
    /// The ring ran dry before the decoder finished; silence from `frame`.
    Starved { voice: u8, gen: u32, frame: u64 },
    /// The start frame had already passed; the voice started `by` frames late.
    Late { voice: u8, gen: u32, by: u64 },
    /// A load for a slot that was not idle (a control-side bug); the ring was discarded.
    LoadRejected { voice: u8, gen: u32 },
}

enum Garbage {
    Voice {
        _ring: Consumer<f32>,
        _status: Arc<FeedStatus>,
    },
}

// ---------------------------------------------------------------------------------------------
// Shared counters

#[derive(Default)]
struct Shared {
    out_frame: AtomicU64,
    starved_frames: AtomicU64,
    starve_episodes: AtomicU32,
    late_starts: AtomicU32,
    stale_commands: AtomicU32,
    load_rejected: AtomicU32,
    envelope_dropped: AtomicU32,
    event_drops: AtomicU32,
    garbage_stalls: AtomicU32,
    garbage_leaks: AtomicU32,
    voices_started: AtomicU32,
    /// Starts of voices whose category is not gated out of the loudness windows (`BlockInfo::item`).
    item_seq: AtomicU32,
    voices_ended: AtomicU32,
    /// Blocks the meter taps could not take because the meter thread was behind (11.3).
    tap_drops: AtomicU64,
    limiter_active_frames: AtomicU64,
    limiter_over_1db_frames: AtomicU64,
    limiter_min_gain_bits: AtomicU32,
    /// Consecutive frames of blocks whose mean square is below the silence threshold (1.5, program
    /// silence): the station's detector reads it.
    silent_run_frames: AtomicU64,
    /// Mean square of the last block (both channels), as `f32` bits.
    last_mean_square_bits: AtomicU32,
    /// The silence threshold as a mean square, `f32` bits (default -50 dBFS RMS).
    silence_thr_bits: AtomicU32,
    /// The station's output trim as a linear gain, `f32` bits (2.4: after the master gain; default 1.0).
    trim_gain_bits: AtomicU32,
}

/// The time a change of the output trim takes to arrive (linear in gain): long enough that a step of
/// 40 dB has no audible click, short enough that the knob feels immediate.
pub const TRIM_RAMP_S: f64 = 0.05;

#[derive(Debug, Clone, PartialEq)]
pub struct MixerStats {
    /// Frames rendered (the output timeline position at the end of the last block).
    pub out_frame: u64,
    pub starved_frames: u64,
    pub starve_episodes: u32,
    pub late_starts: u32,
    pub stale_commands: u32,
    pub load_rejected: u32,
    pub envelope_dropped: u32,
    pub event_drops: u32,
    /// Times a finished voice had to wait because the garbage ring was full.
    pub garbage_stalls: u32,
    /// Objects that could not be handed back and were leaked instead of freed on rt-out.
    pub garbage_leaks: u32,
    pub voices_started: u32,
    pub voices_ended: u32,
    /// Frames with any limiter gain reduction, and with more than 1 dB of it.
    pub limiter_active_frames: u64,
    pub limiter_over_1db_frames: u64,
    pub limiter_min_gain: f32,
    /// Blocks the meter taps dropped (`hotclock_meter_tap_drops_total`).
    pub tap_drops: u64,
    /// Frames since the output was last louder than the silence threshold (block granular).
    pub silent_run_frames: u64,
    /// RMS of the last block, dBFS (both channels together); -200 for digital silence.
    pub last_block_rms_dbfs: f32,
}

// ---------------------------------------------------------------------------------------------
// The control side

pub struct MixerControl {
    load_tx: Producer<VoiceLoad>,
    events_rx: Consumer<MixerEvent>,
    garbage_rx: Consumer<Garbage>,
    shared: Arc<Shared>,
    gens: [u32; VOICES],
    busy: [bool; VOICES],
    inbox: VecDeque<MixerEvent>,
    rate: u32,
    tap: Option<MeterTap>,
    link: Arc<LoudnessLink>,
    chain: Arc<ChainControl>,
}

/// The consumer ends of the two meter taps (architecture 1.2 and 11.3): interleaved stereo `f32` at the
/// mixer's rate. `post` is the program output (after the safety limiter and the master gain), `pre` is the
/// bus before the chain (here: the mixed voices, before the limiter and the master gain).
pub struct MeterTap {
    pub post: Consumer<f32>,
    pub pre: Consumer<f32>,
    pub rate: u32,
    /// Non-zero while a meter client wants the pre-chain spectrum (11.2a); zero when nobody does, and then
    /// the mixer does not copy the pre tap at all. Each request takes a new non-zero number
    /// ([`next_pre_request`]). The mixer answers a new number by starting a *run* in the pre ring with
    /// [`PRE_SENTINEL`] and the number of the first post frame the run's audio belongs to, so the reader
    /// can line the two taps up however long the switch took to be seen ([`pre_run_marker`]).
    pub pre_wanted: Arc<AtomicU32>,
    /// The loudness tap (1.2): the bus entering the controller's gain, copied only while the controller
    /// is enabled.
    pub loud: Consumer<f32>,
    /// One entry per block of the post tap (see [`BlockInfo`]).
    pub info: Consumer<BlockInfo>,
    /// The atomics the controller, the station and rt-out share.
    pub link: Arc<LoudnessLink>,
    /// What the processing chain publishes (stage levels, reductions, the AGC gain; 11.3).
    pub chain: Arc<ChainMeters>,
}

/// The word that starts a run in the pre tap ring: a quiet NaN with a payload no audio path makes, followed
/// by the low and the high 32 bits of the post-frame index the run begins at.
pub const PRE_SENTINEL: u32 = 0x7fc0_a5a5;
/// Words a run marker takes in the ring.
pub const PRE_MARKER_WORDS: usize = 3;

/// The next request number after `n` (never zero).
pub fn next_pre_request(n: u32) -> u32 {
    n.wrapping_add(1).max(1)
}

/// The marker that opens a run of the pre tap at post frame `start`.
pub fn pre_run_marker(start: u64) -> [f32; PRE_MARKER_WORDS] {
    [
        f32::from_bits(PRE_SENTINEL),
        f32::from_bits(start as u32),
        f32::from_bits((start >> 32) as u32),
    ]
}

#[derive(Debug, PartialEq, Eq)]
pub enum LoadError {
    /// All eight voices are in use.
    NoFreeVoice,
    /// The load ring is full (rt-out is not draining it).
    RingFull,
}

impl MixerControl {
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// The atomics shared with the loudness controller (6.1): the station sets `playing`, the API sets
    /// `enabled` and the ceiling, the meter thread publishes the gain.
    pub fn loudness_link(&self) -> Arc<LoudnessLink> {
        self.link.clone()
    }

    /// The processing chain's control end: build a chain from a configuration and start it on rt-out.
    pub fn chain_control(&self) -> Arc<ChainControl> {
        self.chain.clone()
    }

    /// The meter thread's ends of the taps; `Some` once.
    pub fn take_meter_tap(&mut self) -> Option<MeterTap> {
        self.tap.take()
    }

    /// Free what rt-out handed back and collect its events.
    #[allow(clippy::collapsible_match)]
    pub fn pump(&mut self) {
        self.chain.pump();
        while self.garbage_rx.pop().is_ok() {}
        while let Ok(ev) = self.events_rx.pop() {
            match ev {
                MixerEvent::Ended { voice, gen, .. } | MixerEvent::LoadRejected { voice, gen } => {
                    if self.gens[voice as usize] == gen {
                        self.busy[voice as usize] = false;
                    }
                }
                _ => {}
            }
            self.inbox.push_back(ev);
        }
    }

    pub fn poll_event(&mut self) -> Option<MixerEvent> {
        self.pump();
        self.inbox.pop_front()
    }

    /// The lowest voice slot that is free.
    pub fn free_voice(&mut self) -> Option<u8> {
        self.pump();
        self.busy.iter().position(|b| !b).map(|i| i as u8)
    }

    pub fn busy_voices(&mut self) -> usize {
        self.pump();
        self.busy.iter().filter(|b| **b).count()
    }

    /// Load a feed into a free voice.
    pub fn load(&mut self, feed: VoiceFeed, params: LoadParams) -> Result<VoiceId, LoadError> {
        self.load_with(feed, params, false)
    }

    /// As [`load`](Self::load), saying whether the voice's category is gated out of the loudness
    /// windows while it is audible (`loudness.gate_categories`, 6.1).
    pub fn load_with(
        &mut self,
        feed: VoiceFeed,
        params: LoadParams,
        loudness_gated: bool,
    ) -> Result<VoiceId, LoadError> {
        let slot = self.free_voice().ok_or(LoadError::NoFreeVoice)?;
        let gen = self.gens[slot as usize].wrapping_add(1);
        let load = VoiceLoad {
            voice: slot,
            gen,
            ring: feed.ring,
            status: feed.status,
            params,
            loud_gated: loudness_gated,
        };
        match self.load_tx.push(load) {
            Ok(()) => {
                self.gens[slot as usize] = gen;
                self.busy[slot as usize] = true;
                Ok(VoiceId { slot, gen })
            }
            Err(rtrb::PushError::Full(_)) => Err(LoadError::RingFull),
        }
    }

    pub fn stats(&self) -> MixerStats {
        let s = &self.shared;
        MixerStats {
            out_frame: s.out_frame.load(Acquire),
            starved_frames: s.starved_frames.load(Relaxed),
            starve_episodes: s.starve_episodes.load(Relaxed),
            late_starts: s.late_starts.load(Relaxed),
            stale_commands: s.stale_commands.load(Relaxed),
            load_rejected: s.load_rejected.load(Relaxed),
            envelope_dropped: s.envelope_dropped.load(Relaxed),
            event_drops: s.event_drops.load(Relaxed),
            garbage_stalls: s.garbage_stalls.load(Relaxed),
            garbage_leaks: s.garbage_leaks.load(Relaxed),
            voices_started: s.voices_started.load(Relaxed),
            voices_ended: s.voices_ended.load(Relaxed),
            limiter_active_frames: s.limiter_active_frames.load(Relaxed),
            limiter_over_1db_frames: s.limiter_over_1db_frames.load(Relaxed),
            limiter_min_gain: f32::from_bits(s.limiter_min_gain_bits.load(Relaxed)),
            tap_drops: s.tap_drops.load(Relaxed),
            silent_run_frames: s.silent_run_frames.load(Relaxed),
            last_block_rms_dbfs: {
                let ms = f32::from_bits(s.last_mean_square_bits.load(Relaxed));
                if ms <= 1e-20 {
                    -200.0
                } else {
                    10.0 * ms.log10()
                }
            },
        }
    }

    /// The output trim, dB (2.4): a gain after the safety limiter and the master gain, on both outputs alike.
    /// The mixer ramps to it; 0 is unity. Not clamped here (the station's config and the API keep it in range).
    pub fn set_output_trim_db(&self, db: f32) {
        let g = 10f32.powf(db / 20.0);
        self.shared.trim_gain_bits.store(g.to_bits(), Relaxed);
    }

    /// The level below which a block counts as silent, dBFS RMS (default -50).
    pub fn set_silence_threshold_dbfs(&self, dbfs: f32) {
        let ms = 10f32.powf(dbfs / 10.0);
        self.shared.silence_thr_bits.store(ms.to_bits(), Relaxed);
    }
}

// ---------------------------------------------------------------------------------------------
// Voices (real-time side)

#[derive(Clone, Copy, PartialEq, Eq)]
enum VState {
    Idle,
    Armed,
    Playing,
    Releasing,
}

/// Everything that decides a voice's gain at a frame; `Copy` so the mixing loop can hold it
/// while the ring is borrowed.
#[derive(Clone, Copy)]
struct GainState {
    gain: f32,
    env: Envelope,
    /// Output frame of the first sample, and the length of the fade in (0 = none).
    actual_start: u64,
    declick_in: u32,
    stop_at: u64,
    stop_len: u32,
    stop_curve: Curve,
    /// Level the stop fade starts from: 1, or less when a second stop shortened a fade that
    /// was already under way (see [`GainState::set_stop`]).
    stop_scale: f32,
}

impl GainState {
    #[inline]
    fn at(&self, f: u64) -> f32 {
        let mut g = self.gain * self.env.value_at(f);
        let k = f - self.actual_start;
        if k < u64::from(self.declick_in) {
            g *= fade_in(k as u32, self.declick_in);
        }
        if f >= self.stop_at && self.stop_len > 0 {
            g *= self.stop_scale
                * fade_out(
                    (f - self.stop_at).min(u64::from(u32::MAX)) as u32,
                    self.stop_len,
                    self.stop_curve,
                );
        }
        g
    }

    /// Set the stop of a playing voice whose frames before `now` have been mixed. A stop never
    /// makes the gain jump: a stop frame in the past is moved to `now` (so its fade is played
    /// whole, not entered half-way), and a stop that arrives while a stop fade is under way can
    /// only shorten it, fading from the level reached at `now` to silence at the new end. A stop
    /// that would end later than the stop already set is ignored, begun or not (review M1.4:
    /// replacing a pending stop by a later one silently lengthened the voice).
    fn set_stop(&mut self, at: u64, len: u32, curve: Curve, now: u64) {
        let at = at.max(now);
        let begun = self.stop_at < now || self.stop_scale < 1.0;
        if !begun {
            self.arm_stop(at, len, curve);
            return;
        }
        let old_end = self.stop_at.saturating_add(u64::from(self.stop_len));
        let new_end = at.saturating_add(u64::from(len));
        if new_end >= old_end {
            return;
        }
        let k = now - self.stop_at;
        // k < stop_len here: a voice past its old end has ended and takes no commands.
        let level = self.stop_scale
            * fade_out(
                k.min(u64::from(u32::MAX)) as u32,
                self.stop_len,
                self.stop_curve,
            );
        self.stop_at = now;
        self.stop_curve = curve;
        // new_end - now < old_end - old stop_at <= u32::MAX.
        self.stop_len = (new_end - now) as u32;
        self.stop_scale = level;
    }

    /// Set a stop whose fade has not begun (on an armed voice, literally). It replaces the stop
    /// already set only if it ends no later than that one.
    fn arm_stop(&mut self, at: u64, len: u32, curve: Curve) {
        if self.stop_at != u64::MAX {
            let old_end = self.stop_at.saturating_add(u64::from(self.stop_len));
            if at.saturating_add(u64::from(len)) > old_end {
                return;
            }
        }
        self.stop_at = at;
        self.stop_len = len;
        self.stop_curve = curve;
        self.stop_scale = 1.0;
    }

    /// Remove a stop whose fade has not begun at `now` (see [`MixerCmd::ClearStop`]).
    fn clear_stop(&mut self, now: u64) {
        if self.stop_at >= now && self.stop_scale == 1.0 {
            self.stop_at = u64::MAX;
            self.stop_len = 0;
            self.stop_curve = Curve::Cosine;
        }
    }

    /// True when the gain is the constant `gain` over `[f0, f1)`.
    #[inline]
    fn is_flat(&self, f0: u64, f1: u64) -> bool {
        self.env.is_unity()
            && f0 >= self.actual_start + u64::from(self.declick_in)
            && self.stop_at >= f1
    }
}

/// Cosine fade in over `len` frames: gain at frame `k` (0-based) is in `(0, 1)`, so the very
/// first sample already sounds (a start at frame F is audible at F).
#[inline]
fn fade_in(k: u32, len: u32) -> f32 {
    (0.5 - 0.5 * (std::f64::consts::PI * f64::from(k + 1) / (f64::from(len) + 1.0)).cos()) as f32
}

/// Fade out over `len` frames with `curve`; frame `k` = `len - 1` is the last one that sounds.
#[inline]
fn fade_out(k: u32, len: u32, curve: Curve) -> f32 {
    if k >= len {
        return 0.0;
    }
    let x = f64::from(k + 1) / (f64::from(len) + 1.0);
    if curve == Curve::Cosine {
        return (0.5 + 0.5 * (std::f64::consts::PI * x).cos()) as f32;
    }
    curve.gain(1.0, 0.0, x) as f32
}

struct Voice {
    gen: u32,
    state: VState,
    ring: Option<Consumer<f32>>,
    status: Option<Arc<FeedStatus>>,
    start_frame: u64,
    gs: GainState,
    played: u64,
    starving: bool,
    end_frame: u64,
    end_reason: EndReason,
    garbage: Option<Garbage>,
    event_pending: bool,
    /// Gated out of the loudness windows while audible (6.1).
    loud_gated: bool,
}

impl Voice {
    /// The voice contributes audio to the chunk starting at `t0` (it is playing, or it ended inside it).
    fn audible(&self, t0: u64) -> bool {
        self.state == VState::Playing || (self.state == VState::Releasing && self.end_frame > t0)
    }

    fn idle() -> Self {
        Self {
            gen: 0,
            state: VState::Idle,
            ring: None,
            status: None,
            start_frame: 0,
            gs: GainState {
                gain: 1.0,
                env: Envelope::new(),
                actual_start: 0,
                declick_in: 0,
                stop_at: u64::MAX,
                stop_len: 0,
                stop_curve: Curve::Cosine,
                stop_scale: 1.0,
            },
            played: 0,
            starving: false,
            end_frame: 0,
            end_reason: EndReason::Eof,
            garbage: None,
            event_pending: false,
            loud_gated: false,
        }
    }
}

/// The rest of the mixer's state a voice needs while it mixes.
struct Cx<'a> {
    events: &'a mut Producer<MixerEvent>,
    garbage: &'a mut Producer<Garbage>,
    shared: &'a Shared,
}

impl Cx<'_> {
    fn emit(&mut self, ev: MixerEvent) {
        if self.events.push(ev).is_err() {
            self.shared.event_drops.fetch_add(1, Relaxed);
        }
    }
}

impl Voice {
    fn mix(&mut self, idx: u8, t0: u64, n: usize, bus: &mut [f32], cx: &mut Cx) {
        match self.state {
            VState::Idle => return,
            VState::Releasing => {
                self.try_release(idx, cx);
                return;
            }
            _ => {}
        }
        let t1 = t0 + n as u64;
        let mut first = 0usize;
        if self.state == VState::Armed {
            if self.gs.stop_at <= self.start_frame {
                self.end(idx, EndReason::Cancelled, self.start_frame.max(t0), cx);
                return;
            }
            if self.start_frame >= t1 {
                return;
            }
            if self.start_frame < t0 {
                cx.shared.late_starts.fetch_add(1, Relaxed);
                let by = t0 - self.start_frame;
                cx.emit(MixerEvent::Late {
                    voice: idx,
                    gen: self.gen,
                    by,
                });
                self.gs.actual_start = t0;
            } else {
                first = (self.start_frame - t0) as usize;
                self.gs.actual_start = self.start_frame;
            }
            self.state = VState::Playing;
            cx.shared.voices_started.fetch_add(1, Relaxed);
            if !self.loud_gated {
                cx.shared.item_seq.fetch_add(1, Relaxed);
            }
            cx.emit(MixerEvent::Started {
                voice: idx,
                gen: self.gen,
                frame: self.gs.actual_start,
            });
        }

        // Where this voice's output ends inside the chunk (a stop, with its fade).
        let mut last = n;
        let mut stop_end = None;
        if self.gs.stop_at != u64::MAX {
            let end_f = self.gs.stop_at.saturating_add(u64::from(self.gs.stop_len));
            if end_f < t1 {
                last = end_f.saturating_sub(t0) as usize;
                stop_end = Some(end_f);
            }
        }
        let last = last.max(first);
        let want = last - first;

        let gs = self.gs;
        let (Some(status), Some(ring)) = (self.status.as_ref(), self.ring.as_mut()) else {
            return;
        };
        // Order matters: `eof` first, then the ring. The decoder pushes the last frames before
        // it sets `eof` (Release), so a reader that sees `eof` sees every frame.
        let eof = status.eof.load(Acquire);
        let avail = ring.slots() / 2;
        let take = want.min(avail);
        let starved = take < want && !eof;
        let ended_eof = take < want && eof;

        if take > 0 {
            if let Ok(chunk) = ring.read_chunk(take * 2) {
                let (a, b) = chunk.as_slices();
                let mut src = a.iter().chain(b.iter()).copied();
                let f0 = t0 + first as u64;
                let dst = &mut bus[first * 2..(first + take) * 2];
                if gs.is_flat(f0, f0 + take as u64) {
                    let g = gs.gain;
                    for d in dst.iter_mut() {
                        *d += src.next().unwrap_or(0.0) * g;
                    }
                } else {
                    for (i, d) in dst.chunks_exact_mut(2).enumerate() {
                        let g = gs.at(f0 + i as u64);
                        d[0] += src.next().unwrap_or(0.0) * g;
                        d[1] += src.next().unwrap_or(0.0) * g;
                    }
                }
                chunk.commit_all();
                self.played += take as u64;
                status.consumed_frames.store(self.played, Relaxed);
            }
        }

        let frame_after = t0 + (first + take) as u64;
        if starved {
            cx.shared
                .starved_frames
                .fetch_add((want - take) as u64, Relaxed);
            if !self.starving {
                self.starving = true;
                cx.shared.starve_episodes.fetch_add(1, Relaxed);
                cx.emit(MixerEvent::Starved {
                    voice: idx,
                    gen: self.gen,
                    frame: frame_after,
                });
            }
        } else {
            self.starving = false;
        }

        if ended_eof {
            let reason = if status.failed.load(Acquire) {
                EndReason::Failed
            } else {
                EndReason::Eof
            };
            self.end(idx, reason, frame_after, cx);
        } else if let Some(end_f) = stop_end {
            self.end(idx, EndReason::Stopped, end_f, cx);
        }
    }

    /// The voice's output is over: queue its ring for the control side to free.
    fn end(&mut self, idx: u8, reason: EndReason, frame: u64, cx: &mut Cx) {
        self.state = VState::Releasing;
        self.end_frame = frame;
        self.end_reason = reason;
        if let (Some(ring), Some(status)) = (self.ring.take(), self.status.take()) {
            self.garbage = Some(Garbage::Voice {
                _ring: ring,
                _status: status,
            });
        }
        self.event_pending = true;
        self.try_release(idx, cx);
    }

    fn try_release(&mut self, idx: u8, cx: &mut Cx) {
        if let Some(g) = self.garbage.take() {
            if let Err(rtrb::PushError::Full(g)) = cx.garbage.push(g) {
                self.garbage = Some(g);
                cx.shared.garbage_stalls.fetch_add(1, Relaxed);
                return;
            }
        }
        if self.event_pending {
            let ev = MixerEvent::Ended {
                voice: idx,
                gen: self.gen,
                frame: self.end_frame,
                reason: self.end_reason,
                played: self.played,
            };
            if cx.events.push(ev).is_err() {
                return; // retried next block; an Ended event is never dropped
            }
            self.event_pending = false;
            cx.shared.voices_ended.fetch_add(1, Relaxed);
        }
        self.state = VState::Idle;
    }
}

// ---------------------------------------------------------------------------------------------
// The mixer

pub struct Mixer {
    voices: Vec<Voice>,
    bus: Vec<f32>,
    scratch: Vec<f32>,
    limiter: Limiter,
    master: Envelope,
    /// The output trim in use now, where it is going, and the per-frame step of the ramp between them.
    trim_cur: f32,
    trim_to: f32,
    trim_step: f32,
    trim_ramp_frames: f32,
    loads: Consumer<VoiceLoad>,
    events: Producer<MixerEvent>,
    garbage: Producer<Garbage>,
    shared: Arc<Shared>,
    declick: u32,
    /// Next frame of the bus to mix. Runs `limiter.latency_frames()` ahead of the output.
    bus_t: u64,
    out_t: u64,
    primed: bool,
    tap_post: Producer<f32>,
    tap_pre: Producer<f32>,
    pre_wanted: Arc<AtomicU32>,
    /// The request number the pre tap's current run answers.
    pre_seen: u32,
    /// The pre tap is inside a run (its marker is in the ring).
    pre_on: bool,
    /// Frames pushed into the post tap so far.
    post_total: u64,
    /// The loudness controller's gain stage and true-peak limiter (6, 6.1), between the bus and the
    /// safety limiter. The limiter's delay is always in the path, so enabling the controller never moves
    /// the program in time.
    tp: TpLimiter,
    link: Arc<LoudnessLink>,
    /// The linear gain applied at the end of the last chunk (ramped to the published one each chunk).
    gain_applied: f32,
    gbus: Vec<f32>,
    tbus: Vec<f32>,
    /// The bus after the processing chain.
    cbus: Vec<f32>,
    chain: ChainHost,
    tap_loud: Producer<f32>,
    tap_info: Producer<BlockInfo>,
    silent_gate_frames: u64,
    /// The ceiling the safety limiter currently uses, dBFS.
    safety_ceiling_db: f32,
}

impl Mixer {
    /// A mixer for `rate` Hz and the control end for it. All memory is allocated here.
    pub fn new(rate: u32) -> (Mixer, MixerControl) {
        let (load_tx, loads) = RingBuffer::<VoiceLoad>::new(LOAD_RING);
        let (events, events_rx) = RingBuffer::<MixerEvent>::new(EVENT_RING);
        let (garbage, garbage_rx) = RingBuffer::<Garbage>::new(GARBAGE_RING);
        let shared = Arc::new(Shared::default());
        shared
            .limiter_min_gain_bits
            .store(1.0f32.to_bits(), Relaxed);
        shared
            .silence_thr_bits
            .store(10f32.powf(-5.0).to_bits(), Relaxed);
        shared.trim_gain_bits.store(1.0f32.to_bits(), Relaxed);
        let declick = ((DECLICK_S * f64::from(rate)).round() as u32).max(1);
        let (tap_post, post_rx) = RingBuffer::<f32>::new(TAP_FRAMES * 2);
        let (tap_pre, pre_rx) = RingBuffer::<f32>::new(TAP_FRAMES * 2);
        let pre_wanted = Arc::new(AtomicU32::new(0));
        let (tap_loud, loud_rx) = RingBuffer::<f32>::new(TAP_FRAMES * 2);
        let (tap_info, info_rx) = RingBuffer::<BlockInfo>::new(INFO_RING);
        let link = Arc::new(LoudnessLink::new());
        let (chost, cctl) = chain::new_pair(rate, MAX_BLOCK);
        let cmeters = cctl.meters.clone();
        let m = Mixer {
            voices: (0..VOICES).map(|_| Voice::idle()).collect(),
            bus: vec![0.0; MAX_BLOCK * 2],
            scratch: vec![0.0; MAX_BLOCK * 2],
            limiter: Limiter::new(rate),
            master: Envelope::new(),
            trim_cur: 1.0,
            trim_to: 1.0,
            trim_step: 0.0,
            trim_ramp_frames: ((TRIM_RAMP_S * f64::from(rate)).round() as f32).max(1.0),
            loads,
            events,
            garbage,
            shared: shared.clone(),
            declick,
            bus_t: 0,
            out_t: 0,
            primed: false,
            tap_post,
            tap_pre,
            pre_wanted: pre_wanted.clone(),
            pre_seen: 0,
            pre_on: false,
            post_total: 0,
            tp: TpLimiter::new(rate, crate::tplimiter::CEILING_OFF_DB),
            link: link.clone(),
            gain_applied: 1.0,
            gbus: vec![0.0; MAX_BLOCK * 2],
            tbus: vec![0.0; MAX_BLOCK * 2],
            cbus: vec![0.0; MAX_BLOCK * 2],
            chain: chost,
            tap_loud,
            tap_info,
            silent_gate_frames: (LOUD_SILENCE_GATE_S * f64::from(rate)) as u64,
            safety_ceiling_db: crate::limiter::CEILING_DBFS as f32,
        };
        let c = MixerControl {
            load_tx,
            events_rx,
            garbage_rx,
            shared,
            gens: [0; VOICES],
            busy: [false; VOICES],
            inbox: VecDeque::new(),
            rate,
            tap: Some(MeterTap {
                post: post_rx,
                pre: pre_rx,
                rate,
                pre_wanted,
                loud: loud_rx,
                info: info_rx,
                link: link.clone(),
                chain: cmeters,
            }),
            link,
            chain: cctl,
        };
        (m, c)
    }

    /// The delay of the limiters (the true-peak limiter's 5 ms, always in the path, plus the safety
    /// limiter's 2 ms), which the mixer absorbs by mixing ahead (frames).
    pub fn limiter_latency_frames(&self) -> usize {
        self.limiter.latency_frames() + self.tp.latency_frames()
    }

    fn service_loads(&mut self) {
        while let Ok(l) = self.loads.pop() {
            let i = l.voice as usize;
            // A non-finite gain would reach the bus as NaN or infinity, and the limiter would
            // then silence every voice on those samples: refuse the load instead.
            if i >= VOICES || self.voices[i].state != VState::Idle || !l.params.gain.is_finite() {
                self.shared.load_rejected.fetch_add(1, Relaxed);
                if self
                    .events
                    .push(MixerEvent::LoadRejected {
                        voice: l.voice,
                        gen: l.gen,
                    })
                    .is_err()
                {
                    self.shared.event_drops.fetch_add(1, Relaxed);
                }
                let g = Garbage::Voice {
                    _ring: l.ring,
                    _status: l.status,
                };
                if let Err(rtrb::PushError::Full(g)) = self.garbage.push(g) {
                    // Never free on rt-out: leak, and say so.
                    self.shared.garbage_leaks.fetch_add(1, Relaxed);
                    std::mem::forget(g);
                }
                continue;
            }
            let (stop_at, stop_len, stop_curve) = match l.params.stop {
                Some((at, fade)) => {
                    let (len, curve) = self.stop_spec(fade);
                    (at, len, curve)
                }
                None => (u64::MAX, 0, Curve::Cosine),
            };
            let declick = self.declick;
            let v = &mut self.voices[i];
            v.gen = l.gen;
            v.state = VState::Armed;
            v.ring = Some(l.ring);
            v.status = Some(l.status);
            v.start_frame = l.params.start_frame;
            v.played = 0;
            v.starving = false;
            v.garbage = None;
            v.event_pending = false;
            v.loud_gated = l.loud_gated;
            v.gs = GainState {
                gain: l.params.gain,
                env: Envelope::new(),
                actual_start: l.params.start_frame,
                declick_in: if l.params.declick_in { declick } else { 0 },
                stop_at,
                stop_len,
                stop_curve,
                stop_scale: 1.0,
            };
        }
    }

    fn stop_spec(&self, fade: StopFade) -> (u32, Curve) {
        match fade {
            StopFade::Declick => (self.declick, Curve::Cosine),
            StopFade::Frames(n) => (n, Curve::Cosine),
            StopFade::Curved { frames, curve } => (frames, curve),
            StopFade::Cut => (0, Curve::Cosine),
        }
    }

    fn apply(&mut self, cmd: &MixerCmd) {
        match *cmd {
            MixerCmd::Stop {
                voice,
                gen,
                at,
                fade,
            } => {
                let (len, curve) = self.stop_spec(fade);
                let now = self.bus_t;
                match self.voices.get_mut(voice as usize) {
                    // Not started yet: taken literally (a stop at or before the start cancels),
                    // except that a stop never ends the voice later than the stop already set.
                    Some(v) if v.gen == gen && v.state == VState::Armed => {
                        v.gs.arm_stop(at, len, curve);
                    }
                    Some(v) if v.gen == gen && v.state == VState::Playing => {
                        v.gs.set_stop(at, len, curve, now);
                    }
                    _ => {
                        self.shared.stale_commands.fetch_add(1, Relaxed);
                    }
                }
            }
            MixerCmd::ClearStop { voice, gen } => {
                let now = self.bus_t;
                match self.voices.get_mut(voice as usize) {
                    Some(v) if v.gen == gen && v.state == VState::Armed => {
                        v.gs.clear_stop(0);
                    }
                    Some(v) if v.gen == gen && v.state == VState::Playing => {
                        v.gs.clear_stop(now);
                    }
                    _ => {
                        self.shared.stale_commands.fetch_add(1, Relaxed);
                    }
                }
            }
            MixerCmd::Envelope { seg, .. } | MixerCmd::Master { seg }
                if !(seg.from.is_finite() && seg.to.is_finite()) =>
            {
                self.shared.envelope_dropped.fetch_add(1, Relaxed);
            }
            MixerCmd::Envelope { voice, gen, seg } => {
                let now = self.bus_t;
                match self.voices.get_mut(voice as usize) {
                    Some(v)
                        if v.gen == gen && matches!(v.state, VState::Armed | VState::Playing) =>
                    {
                        if !v.gs.env.push(seg, now) {
                            self.shared.envelope_dropped.fetch_add(1, Relaxed);
                        }
                    }
                    _ => {
                        self.shared.stale_commands.fetch_add(1, Relaxed);
                    }
                }
            }
            MixerCmd::Master { mut seg } => {
                // The master gain sits after the safety limiter, so a value above 1 would defeat
                // its ceiling. The conductor keeps to 1 or below; the mixer enforces it.
                seg.from = seg.from.clamp(0.0, 1.0);
                seg.to = seg.to.clamp(0.0, 1.0);
                if !self.master.push(seg, self.out_t) {
                    self.shared.envelope_dropped.fetch_add(1, Relaxed);
                }
            }
        }
    }

    /// Mix `n` frames of the bus at `bus_t`. Returns whether a voice of a gated category is audible in
    /// the chunk (a loudness content gate, 6.1).
    fn mix_bus(&mut self, n: usize) -> bool {
        self.bus[..n * 2].fill(0.0);
        let t0 = self.bus_t;
        let mut cx = Cx {
            events: &mut self.events,
            garbage: &mut self.garbage,
            shared: &self.shared,
        };
        let mut gated = false;
        for (i, v) in self.voices.iter_mut().enumerate() {
            v.mix(i as u8, t0, n, &mut self.bus[..n * 2], &mut cx);
            if v.loud_gated && v.audible(t0) {
                gated = true;
            }
        }
        self.bus_t += n as u64;
        gated
    }

    /// The processing chain, the loudness gain stage, the true-peak limiter and the safety limiter, in that
    /// order, on the bus of `n` frames; the result is in `out`. The chain's output is left in `cbus`
    /// (the loudness tap and the block peak read it).
    fn run_chain(&mut self, n: usize, out: &mut [f32], enabled: bool) {
        self.chain
            .process(&self.bus[..n * 2], &mut self.cbus[..n * 2]);
        let cm = self.chain.meters().clone();
        // The gain stage: a linear ramp across the chunk toward the gain the meter thread last published
        // (unity while the controller is off).
        let target = if enabled {
            f32::from_bits(self.link.gain_bits.load(Relaxed))
        } else {
            1.0
        };
        let target = if target.is_finite() && target > 0.0 {
            target
        } else {
            1.0
        };
        let g0 = self.gain_applied;
        let src: &[f32] = if g0 == 1.0 && target == 1.0 {
            &self.cbus[..n * 2]
        } else {
            let step = (target - g0) / n as f32;
            for (j, (d, s)) in self.gbus[..n * 2]
                .chunks_exact_mut(2)
                .zip(self.cbus[..n * 2].chunks_exact(2))
                .enumerate()
            {
                let g = g0 + step * (j + 1) as f32;
                d[0] = s[0] * g;
                d[1] = s[1] * g;
            }
            &self.gbus[..n * 2]
        };
        self.gain_applied = target;
        // The rack's block peaks for the loudness gain stage, the true-peak limiter and the safety limiter.
        let loud = chain::peak_lr(src);
        chain::blk_max(&cm.blk_out, chain::BLK_LOUD, loud);
        // The true-peak limiter: at the loudness controller's ceiling while the controller is on, at the
        // chain's limiter ceiling while that block is on (the lower of the two when both are), else off but
        // still delaying. The safety limiter keeps -1.0 dBFS unless a ceiling above that is set.
        let lim_on = cm.limiter_on.load(Relaxed);
        let chain_ceiling = f32::from_bits(cm.limiter_ceiling_bits.load(Relaxed));
        let ceiling = match (enabled, lim_on) {
            (true, true) => (self.link.ceiling_db() as f32).min(chain_ceiling),
            (true, false) => self.link.ceiling_db() as f32,
            (false, true) => chain_ceiling,
            (false, false) => crate::tplimiter::CEILING_OFF_DB,
        };
        if ceiling != self.tp.ceiling_db() {
            self.tp.set_ceiling_db(ceiling);
        }
        let safety = if ceiling < 100.0 {
            ceiling.max(crate::limiter::CEILING_DBFS as f32)
        } else {
            crate::limiter::CEILING_DBFS as f32
        };
        if safety != self.safety_ceiling_db {
            self.safety_ceiling_db = safety;
            self.limiter.set_ceiling_db(safety);
        }
        let limiting = ceiling < 100.0;
        let pin = if limiting { peak_of(src) } else { 0.0 };
        self.tp.process(src, &mut self.tbus[..n * 2]);
        let sin = peak_of(&self.tbus[..n * 2]);
        chain::blk_max(
            &cm.blk_out,
            chain::BLK_LIM,
            if limiting {
                chain::peak_lr(&self.tbus[..n * 2])
            } else {
                loud
            },
        );
        self.limiter.process(&self.tbus[..n * 2], out);
        let mg = self.tp.take_min_gain();
        if mg < 1.0 {
            self.link.tp_min_gain_bits.fetch_min(mg.to_bits(), Relaxed);
        }
        let sg = self.limiter.take_window_min();
        if sg < 1.0 {
            cm.safety_min_gain.fetch_min(sg.to_bits(), Relaxed);
        }
        let upd = |a: &std::sync::atomic::AtomicU32, v: f32| {
            if v > 0.0 && v.is_finite() {
                a.fetch_max(v.to_bits(), Relaxed);
            }
        };
        if limiting {
            upd(&cm.stage_in[chain::ST_LIM], pin);
            upd(&cm.stage_out[chain::ST_LIM], sin);
        }
        upd(&cm.stage_in[chain::ST_SAFE], sin);
        upd(&cm.stage_out[chain::ST_SAFE], peak_of(&out[..n * 2]));
        chain::blk_max(&cm.blk_out, chain::BLK_SAFE, chain::peak_lr(&out[..n * 2]));
    }

    fn prime(&mut self) {
        let mut left = self.limiter_latency_frames();
        while left > 0 {
            let n = left.min(MAX_BLOCK);
            self.mix_bus(n);
            let mut sink = std::mem::take(&mut self.scratch);
            self.run_chain(n, &mut sink[..n * 2], false);
            self.scratch = sink;
            left -= n;
        }
        // The trim set before the first sound is in force from the first frame (no ramp up from unity).
        self.trim_to = f32::from_bits(self.shared.trim_gain_bits.load(Relaxed));
        self.trim_cur = self.trim_to;
        self.trim_step = 0.0;
        self.primed = true;
    }

    fn render_chunk(&mut self, out: &mut [f32]) {
        let n = out.len() / 2;
        let gated_voice = self.mix_bus(n);
        let enabled = self.link.enabled.load(Relaxed);
        self.run_chain(n, out, enabled);
        // The master gain, and whether it makes this chunk a loudness gate (a ramp is running, or the
        // master is held away from unity).
        let mut master_gate = false;
        if !self.master.is_unity() {
            let (v0, v1) = (
                self.master.value_at(self.out_t),
                self.master.value_at(self.out_t + n as u64),
            );
            master_gate =
                v0 != 1.0 || v1 != 1.0 || self.master.ramping_in(self.out_t, self.out_t + n as u64);
            for (j, f) in out.chunks_exact_mut(2).enumerate() {
                let g = self.master.value_at(self.out_t + j as u64);
                f[0] *= g;
                f[1] *= g;
            }
        }
        self.out_t += n as u64;
        let s = &self.shared;
        // Program silence (1.5): the mean square of the block after the mix and before the trim, so a
        // trim of -40 dB does not make a playing program look silent. Two atomics, no allocation, one
        // pass over a block that is already in cache.
        let ms = out[..n * 2].iter().map(|v| v * v).sum::<f32>() / (n * 2).max(1) as f32;
        s.last_mean_square_bits.store(ms.to_bits(), Relaxed);
        if ms < f32::from_bits(s.silence_thr_bits.load(Relaxed)) {
            s.silent_run_frames.fetch_add(n as u64, Relaxed);
        } else {
            s.silent_run_frames.store(0, Relaxed);
        }
        // The output trim (2.4), after the master gain and the safety limiter: both outputs alike, because
        // the taps and the sound card read `out` below. A new target is reached by a linear ramp of
        // `TRIM_RAMP_S`; at rest and at unity this costs one compare.
        let want = f32::from_bits(s.trim_gain_bits.load(Relaxed));
        if want != self.trim_to {
            self.trim_to = want;
            self.trim_step = (want - self.trim_cur) / self.trim_ramp_frames;
        }
        if self.trim_cur != self.trim_to || self.trim_to != 1.0 {
            for f in out[..n * 2].chunks_exact_mut(2) {
                if self.trim_cur != self.trim_to {
                    self.trim_cur += self.trim_step;
                    if (self.trim_step >= 0.0 && self.trim_cur >= self.trim_to)
                        || (self.trim_step < 0.0 && self.trim_cur <= self.trim_to)
                    {
                        self.trim_cur = self.trim_to;
                    }
                }
                f[0] *= self.trim_cur;
                f[1] *= self.trim_cur;
            }
        }
        // The meter taps. The post tap takes every block. The pre tap takes it only while a client wants
        // the pre-chain spectrum (`pre_wanted`), and a run of it opens with a marker (`pre_run_marker`).
        // The loudness tap takes the bus while the controller is enabled. Every block comes with a
        // `BlockInfo` entry, pushed last. The whole block goes into the rings it is due in or into none
        // (they must stay aligned), no allocation. A full ring means the meter thread is behind: count it
        // and move on.
        let request = self.pre_wanted.load(Relaxed);
        if request != self.pre_seen {
            // A new request (or a withdrawal): whatever run was open is over.
            self.pre_seen = request;
            self.pre_on = false;
        }
        let want_pre = request != 0;
        let pre_need = if want_pre {
            n * 2 + if self.pre_on { 0 } else { PRE_MARKER_WORDS }
        } else {
            0
        };
        if self.tap_post.slots() >= n * 2
            && self.tap_info.slots() >= 1
            && (!enabled || self.tap_loud.slots() >= n * 2)
            && (!want_pre || self.tap_pre.slots() >= pre_need)
        {
            if want_pre {
                if !self.pre_on {
                    if let Ok(chunk) = self.tap_pre.write_chunk_uninit(PRE_MARKER_WORDS) {
                        chunk.fill_from_iter(pre_run_marker(self.post_total));
                    }
                    self.pre_on = true;
                }
                push_block(&mut self.tap_pre, &self.bus[..n * 2]);
            } else {
                self.pre_on = false;
            }
            if enabled {
                push_block(&mut self.tap_loud, &self.cbus[..n * 2]);
            }
            push_block(&mut self.tap_post, &out[..n * 2]);
            let mut flags = 0u8;
            if !self.link.playing.load(Relaxed) {
                flags |= BlockInfo::NOT_PLAYING;
            }
            if master_gate {
                flags |= BlockInfo::MASTER_RAMP;
            }
            if gated_voice {
                flags |= BlockInfo::GATED_VOICE;
            }
            if s.silent_run_frames.load(Relaxed) >= self.silent_gate_frames {
                flags |= BlockInfo::SILENCE;
            }
            if enabled {
                flags |= BlockInfo::HAS_LOUD;
            }
            let _ = self.tap_info.push(BlockInfo {
                frames: n as u32,
                flags,
                item: s.item_seq.load(Relaxed),
                peak: peak_of(&self.cbus[..n * 2]),
            });
            self.post_total += n as u64;
        } else {
            s.tap_drops.fetch_add(1, Relaxed);
        }
        s.limiter_active_frames
            .store(self.limiter.active_frames, Relaxed);
        s.limiter_over_1db_frames
            .store(self.limiter.over_1db_frames, Relaxed);
        s.limiter_min_gain_bits
            .store(self.limiter.min_gain.to_bits(), Relaxed);
        s.out_frame.store(self.out_t, Release);
    }
}

fn peak_of(b: &[f32]) -> f32 {
    b.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

/// Copy a block into a tap ring (the caller checked there is room).
fn push_block(p: &mut Producer<f32>, block: &[f32]) {
    if let Ok(chunk) = p.write_chunk_uninit(block.len()) {
        chunk.fill_from_iter(block.iter().copied());
    }
}

impl Source for Mixer {
    fn render(&mut self, out: &mut [f32]) {
        self.service_loads();
        if !self.primed {
            self.prime();
        }
        for block in out.chunks_mut(MAX_BLOCK * 2) {
            self.render_chunk(block);
        }
    }

    fn command(&mut self, kind: &CommandKind) {
        if let CommandKind::Mixer(cmd) = kind {
            // Loads first, so that a command sent after a load always finds its voice (the load
            // ring and the command ring are separate, and rt-out drains commands before it
            // renders).
            self.service_loads();
            self.apply(cmd);
        }
    }
}
