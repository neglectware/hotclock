//! The segue executor (architecture 2.5, 2.6): places items on the mixer's timeline. It owns the
//! decoder pool and the mixer's control end; for each boundary it resolves the two parameter
//! chains, asks the planner (`plan`) for the frames, then loads B's voice and sends A's stop and
//! B's fade-in.
//!
//! Rules it keeps (the M1.2 review, carried into here):
//! * **Every fade to silence is a `Stop`** with a fade length (`StopFade::Frames` or `Curved`), one
//!   per voice, sent when the boundary is committed. The envelope carries level automation only
//!   (a fade in, ducking), so overlapping ramps on one voice never have to combine.
//! * **Master gain stays at or below 1**: `master` refuses a segment above it (the mixer clamps as
//!   well).
//! * A stop is sent after B is loaded and before A's frames run out; the planner keeps at least
//!   0.25 s between "now" and B's first sample.
//!
//! Not here (M1.5): the queue, the scheduler, timed blocks, preflight, emergency transitions on a
//! starved voice.

use crate::item::{gain_db, Item, RunGain};
use crate::plan::{plan, PlacedView, Plan, PlanInput, TransitionKind};
use crate::sidecar::{BoundaryRecord, RampRecord, Sidecar, StopRecord, VoiceRecord};
use common::transition::{resolve, Chain, PresetBook, PresetPatch, TransitionParams};
use engine::envelope::{Curve, Segment};
use engine::feed::FeedStatus;
use engine::mixer::{
    master_ramp, EndReason, LoadParams, Mixer, MixerControl, MixerEvent, StopFade, VoiceId,
};
use engine::pool::{DecoderPool, VoiceSource};
use engine::rt::Command;
use std::sync::atomic::Ordering::*;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ConductorConfig {
    pub rate: u32,
    pub target_lufs: f64,
    pub max_boost_db: f64,
    pub peak_allowance_db: f64,
    /// The station's default transition preset (`plexamp` unless configured otherwise).
    pub station_preset: String,
    pub presets: PresetBook,
    pub decoder_threads: usize,
    /// Categories (or category kinds) left out of the loudness windows while audible (6.1).
    pub loudness_gate: Vec<String>,
    /// The automatic fade at the end of an item flagged `truncated_fade`, seconds; 0 is off.
    pub truncated_fade_s: f64,
}

impl ConductorConfig {
    pub fn new(rate: u32, station_preset: &str) -> Self {
        Self {
            rate,
            target_lufs: -18.0,
            max_boost_db: 12.0,
            peak_allowance_db: 6.0,
            station_preset: station_preset.to_string(),
            presets: PresetBook::builtin(),
            decoder_threads: 3,
            loudness_gate: vec!["liner".into(), "id".into(), "promo".into()],
            truncated_fade_s: crate::plan::TRUNCATED_FADE_S,
        }
    }
}

/// What the API may change while the station plays (M6a): the categories left out of the loudness windows and
/// the peak allowance normalization uses while the controller is on. Read by the control side when it loads a
/// voice, never by rt-out.
pub struct LoudnessLive {
    pub gate: std::sync::RwLock<Vec<String>>,
    /// `f64` bits of the allowance, or NaN for the station's own `peak_allowance_db`.
    allowance_bits: std::sync::atomic::AtomicU64,
}

impl LoudnessLive {
    pub fn new(gate: Vec<String>) -> Self {
        Self {
            gate: std::sync::RwLock::new(gate),
            allowance_bits: std::sync::atomic::AtomicU64::new(f64::NAN.to_bits()),
        }
    }
    pub fn set_allowance(&self, db: Option<f64>) {
        self.allowance_bits.store(
            db.unwrap_or(f64::NAN).to_bits(),
            std::sync::atomic::Ordering::Relaxed,
        );
    }
    fn allowance(&self) -> Option<f64> {
        let v = f64::from_bits(
            self.allowance_bits
                .load(std::sync::atomic::Ordering::Relaxed),
        );
        (!v.is_nan()).then_some(v)
    }
}

/// One item on the timeline.
pub struct Placed {
    pub item: Arc<Item>,
    pub id: VoiceId,
    pub status: Arc<FeedStatus>,
    pub start_frame: u64,
    pub bound_in_ms: i64,
    pub bound_out_ms: i64,
    pub out_is_file_end: bool,
    /// The length the placement assumed for the file (frames), if it was known.
    pub planned_frames: Option<u64>,
    /// The per-boundary override that was in force on the way in.
    pub entry: Option<PresetPatch>,
    pub record: usize,
    pub ended: bool,
}

/// What the mixer reported about a voice, for the station's as-run log (M1.5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VoiceEvent {
    Started {
        id: VoiceId,
        frame: u64,
    },
    Ended {
        id: VoiceId,
        frame: u64,
        played: u64,
        reason: EndReason,
    },
    /// The voice's ring ran dry (its decoder fell 20 s behind): the mixer fills with zeros and the rest
    /// of its audio plays late.
    Starved {
        id: VoiceId,
        frame: u64,
    },
}

pub struct Conductor {
    live: Arc<LoudnessLive>,
    pub cfg: ConductorConfig,
    pool: DecoderPool,
    ctl: MixerControl,
    pub placed: Vec<Placed>,
    /// Items fired over the programme (`fire cart`): they sound on a free voice and take no part in
    /// planning boundaries.
    pub carts: Vec<Placed>,
    pub sidecar: Sidecar,
    /// Started and Ended events since the station last drained them.
    pub events: Vec<VoiceEvent>,
    /// Audio held in memory, by the `ram://` path of the item that plays it (the RAM loop, 1.5).
    pub ram: std::collections::HashMap<std::path::PathBuf, Arc<engine::pool::RamClip>>,
    /// Items whose transition layers had to be dropped, warned about once.
    resolve_warned: std::sync::Mutex<std::collections::HashSet<String>>,
}

fn curve_name(c: Curve) -> &'static str {
    match c {
        Curve::Linear => "linear",
        Curve::Cosine => "cosine",
        Curve::EqualPower => "equal_power",
        Curve::DbLinear => "db_linear",
    }
}

fn stop_record(at: u64, fade: StopFade, declick: u32) -> StopRecord {
    match fade {
        StopFade::Declick => StopRecord {
            at,
            frames: declick,
            curve: "declick".into(),
        },
        StopFade::Frames(n) => StopRecord {
            at,
            frames: n,
            curve: "cosine".into(),
        },
        StopFade::Curved { frames, curve } => StopRecord {
            at,
            frames,
            curve: curve_name(curve).into(),
        },
        StopFade::Cut => StopRecord {
            at,
            frames: 0,
            curve: "cut".into(),
        },
    }
}

impl Conductor {
    /// A conductor and the mixer it drives (the caller renders the mixer: offline, or on rt-out).
    pub fn new(cfg: ConductorConfig) -> (Conductor, Mixer) {
        let live = Arc::new(LoudnessLive::new(cfg.loudness_gate.clone()));
        let (mixer, ctl) = Mixer::new(cfg.rate);
        let pool = DecoderPool::new(cfg.decoder_threads, cfg.rate);
        let sidecar = Sidecar {
            rate: cfg.rate,
            ..Sidecar::default()
        };
        (
            Conductor {
                live,
                cfg,
                pool,
                ctl,
                placed: Vec::new(),
                carts: Vec::new(),
                sidecar,
                events: Vec::new(),
                ram: std::collections::HashMap::new(),
                resolve_warned: Default::default(),
            },
            mixer,
        )
    }

    pub fn rate(&self) -> u32 {
        self.cfg.rate
    }

    /// The loudness controller's atomics (6.1): the station sets `playing`, the meter thread the gain.
    /// The settings the API changes live (see [`LoudnessLive`]).
    pub fn loudness_live(&self) -> std::sync::Arc<LoudnessLive> {
        self.live.clone()
    }

    pub fn loudness_link(&self) -> std::sync::Arc<engine::loudness::LoudnessLink> {
        self.ctl.loudness_link()
    }

    /// The processing chain's control end (6): build a chain from settings and start it on rt-out.
    pub fn chain_control(&self) -> std::sync::Arc<engine::chain::ChainControl> {
        self.ctl.chain_control()
    }

    /// The level below which the mixer counts an output block as silent, dBFS RMS (1.5).
    pub fn set_silence_threshold(&self, dbfs: f32) {
        self.ctl.set_silence_threshold_dbfs(dbfs);
    }

    /// The output trim, dB (2.4), on both outputs alike; the mixer ramps to it.
    pub fn set_output_trim_db(&self, db: f32) {
        self.ctl.set_output_trim_db(db);
    }

    /// Resolve one item's chain of layers (2.6): station preset, category, item, and the queue
    /// entry's override for this boundary.
    ///
    /// A layer that cannot be resolved (a preset that was deleted or misspelt, a patch with a bad value) is
    /// the item's or the entry's data, not a reason to stop playing it (1.5): the layers are dropped from the
    /// most specific up until the chain resolves, down to the station preset alone, with a warning (once per
    /// item). Only a station preset that does not resolve is an error.
    pub fn resolve_chain(
        &self,
        item: &Item,
        entry: Option<&PresetPatch>,
    ) -> Result<TransitionParams, String> {
        let full = Chain {
            station_preset: &self.cfg.station_preset,
            category_preset: item.category_preset.as_deref(),
            category_patch: Some(&item.category_patch),
            item: Some(&item.transition),
            entry,
        };
        let err = match resolve(&self.cfg.presets, &full) {
            Ok(p) => return Ok(p),
            Err(e) => e,
        };
        let fallbacks = [
            Chain {
                entry: None,
                ..full
            },
            Chain {
                item: None,
                entry: None,
                ..full
            },
            Chain {
                station_preset: &self.cfg.station_preset,
                category_preset: None,
                category_patch: None,
                item: None,
                entry: None,
            },
        ];
        for (i, c) in fallbacks.iter().enumerate() {
            if let Ok(p) = resolve(&self.cfg.presets, c) {
                let mut warned = self
                    .resolve_warned
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if warned.insert(item.name.clone()) {
                    tracing::warn!(
                        item = %item.name,
                        "its transition layers do not resolve ({err}); playing it with {}",
                        ["the entry's override left out", "its own override left out", "the station preset alone"][i]
                    );
                }
                return Ok(p);
            }
        }
        Err(err)
    }

    fn frames_known(p: &Placed) -> Option<u64> {
        p.status.total().or_else(|| {
            if p.item
                .flags
                .contains(&analysis::markers::Flag::GaplessUnknown)
            {
                None
            } else {
                p.status.expected()
            }
        })
    }

    /// The frame at which a voice's audio runs out, once its decoder has finished and the count is
    /// known: the voice's start frame plus the frames the decoder produced. `None` while the decoder is
    /// still running.
    pub fn drain_frame(p: &Placed) -> Option<u64> {
        if !p.status.eof.load(Acquire) {
            return None;
        }
        p.status.total().map(|t| p.start_frame + t)
    }

    /// The last item placed.
    pub fn last(&self) -> Option<&Placed> {
        self.placed.last()
    }

    /// Can the boundary after the last item be planned yet? For a continuity joint the exact
    /// length is needed; for the others the planner falls back to the markers.
    pub fn length_known(&self) -> bool {
        self.placed
            .last()
            .is_some_and(|p| Self::frames_known(p).is_some())
    }

    /// Plan the boundary from the last placed item into `b`. Pure: nothing is loaded or sent.
    pub fn plan_next(
        &self,
        b: &Item,
        kind: TransitionKind,
        entry: Option<&PresetPatch>,
        now: u64,
    ) -> Result<(Plan, TransitionParams, TransitionParams), String> {
        let a = self.placed.last().ok_or("nothing is playing")?;
        let pa = self.resolve_chain(&a.item, entry)?;
        let pb = self.resolve_chain(b, entry)?;
        let view = PlacedView {
            item: &a.item,
            start_frame: a.start_frame,
            bound_in_ms: a.bound_in_ms,
            bound_out_ms: a.bound_out_ms,
            bound_out_is_file_end: a.out_is_file_end,
            frames: Self::frames_known(a),
        };
        let p = plan(&PlanInput {
            rate: self.cfg.rate,
            now,
            a: &view,
            b,
            pa: &pa,
            pb: &pb,
            kind,
            truncated_fade_s: self.cfg.truncated_fade_s,
        });
        Ok((p, pa, pb))
    }

    fn open_voice(
        &mut self,
        item: &Arc<Item>,
        start_frame: u64,
        from_ms: i64,
        gain_db: f64,
        declick_in: bool,
    ) -> Result<(VoiceId, Arc<FeedStatus>), String> {
        let feed = match self.ram.get(&item.path) {
            Some(clip) => self.pool.open(VoiceSource::Memory {
                clip: clip.clone(),
                start_s: from_ms as f64 / 1000.0,
            }),
            None => self.pool.open(VoiceSource::File {
                path: item.path.clone(),
                start_s: from_ms as f64 / 1000.0,
            }),
        };
        let status = feed.status.clone();
        let params = LoadParams {
            start_frame,
            gain: 10f32.powf(gain_db as f32 / 20.0),
            declick_in,
            stop: None,
        };
        let gated = self
            .live
            .gate
            .read()
            .map(|g| g.iter().any(|c| *c == item.category || *c == item.kind))
            .unwrap_or(false);
        let id = self
            .ctl
            .load_with(feed, params, gated)
            .map_err(|e| format!("cannot load `{}`: {e:?}", item.name))?;
        Ok((id, status))
    }

    fn gain_for(&self, item: &Item, run: Option<RunGain>) -> f64 {
        gain_db(
            item,
            self.cfg.target_lufs,
            self.cfg.max_boost_db,
            self.live.allowance().unwrap_or(self.cfg.peak_allowance_db),
            run,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record_for(
        &self,
        item: &Item,
        id: VoiceId,
        start: u64,
        from_ms: i64,
        gain: f64,
        declick: bool,
        in_ms: i64,
        planned_frames: Option<u64>,
    ) -> VoiceRecord {
        let rate = self.cfg.rate;
        let at = |ms: i64| {
            (start as i64 + crate::plan::ms_frames(ms, rate) - crate::plan::ms_frames(in_ms, rate))
                .max(0) as u64
        };
        VoiceRecord {
            item: item.name.clone(),
            path: item.path.display().to_string(),
            slot: id.slot,
            gen: id.gen,
            start_frame: start,
            file_start_ms: from_ms,
            gain_db: gain,
            declick_in: declick,
            planned_frames,
            attack_frame: at(item.markers.attack),
            segue_frame: at(item.markers.segue),
            cue_out_frame: at(item.markers.cue_out),
            ..VoiceRecord::default()
        }
    }

    /// Place the first item. It starts from its bound in (per the station preset's `bounds` and
    /// long-intro policy) at `start_frame`.
    pub fn place_first(
        &mut self,
        item: Arc<Item>,
        start_frame: u64,
        run: Option<RunGain>,
    ) -> Result<(), String> {
        let p = self.resolve_chain(&item, None)?;
        let (bin, bout, file_end) = crate::plan::bounds_of(&item, &p);
        let gain = self.gain_for(&item, run);
        let (id, status) = self.open_voice(&item, start_frame, bin, gain, true)?;
        let rec = self.record_for(&item, id, start_frame, bin, gain, true, bin, None);
        self.sidecar.voices.push(rec);
        self.placed.push(Placed {
            item,
            id,
            status,
            start_frame,
            bound_in_ms: bin,
            bound_out_ms: bout,
            out_is_file_end: file_end,
            planned_frames: None,
            entry: None,
            record: self.sidecar.voices.len() - 1,
            ended: false,
        });
        Ok(())
    }

    /// Place the first item after a restart, from `from_ms` into the file (an item that was on air when
    /// the process went down, 1.7). The voice starts at `start_frame` with a `fade_in_ms` fade (a
    /// click guard, not a musical fade); its bounds start at `from_ms`, so every later marker maps to the
    /// frame it sounds at.
    pub fn place_resumed(
        &mut self,
        item: Arc<Item>,
        start_frame: u64,
        from_ms: i64,
        fade_in_ms: f64,
        run: Option<RunGain>,
        send: &mut dyn FnMut(Command),
    ) -> Result<(), String> {
        let p = self.resolve_chain(&item, None)?;
        let (_, bout, file_end) = crate::plan::bounds_of(&item, &p);
        let gain = self.gain_for(&item, run);
        let (id, status) = self.open_voice(&item, start_frame, from_ms, gain, true)?;
        let rate = self.cfg.rate;
        let mut rec = self.record_for(&item, id, start_frame, from_ms, gain, true, from_ms, None);
        let len = crate::plan::s_frames(fade_in_ms / 1000.0, rate).max(1) as u64;
        let seg = Segment {
            start: start_frame,
            end: start_frame + len,
            from: 0.0,
            to: 1.0,
            curve: Curve::Cosine,
        };
        send(id.envelope(seg));
        rec.envelope.push(RampRecord {
            start: seg.start,
            end: seg.end,
            from: seg.from,
            to: seg.to,
            curve: curve_name(seg.curve).into(),
        });
        self.sidecar.voices.push(rec);
        self.placed.push(Placed {
            item,
            id,
            status,
            start_frame,
            bound_in_ms: from_ms,
            bound_out_ms: bout,
            out_is_file_end: file_end,
            planned_frames: None,
            entry: None,
            record: self.sidecar.voices.len() - 1,
            ended: false,
        });
        Ok(())
    }

    /// Play an item over what is sounding, now (`fire cart`, 4.7): a free voice from its cue-in with a
    /// declick in, ending at its cue-out. The chain the boundary planner reads is not touched.
    pub fn fire_cart(
        &mut self,
        item: Arc<Item>,
        start_frame: u64,
        send: &mut dyn FnMut(Command),
    ) -> Result<(), String> {
        let p = self.resolve_chain(&item, None)?;
        let (bin, bout, file_end) = crate::plan::bounds_of(&item, &p);
        let gain = self.gain_for(&item, None);
        let (id, status) = self.open_voice(&item, start_frame, bin, gain, true)?;
        let mut rec = self.record_for(&item, id, start_frame, bin, gain, true, bin, None);
        let placed = Placed {
            item,
            id,
            status,
            start_frame,
            bound_in_ms: bin,
            bound_out_ms: bout,
            out_is_file_end: file_end,
            planned_frames: None,
            entry: None,
            record: 0,
            ended: false,
        };
        let view = PlacedView {
            item: &placed.item,
            start_frame,
            bound_in_ms: bin,
            bound_out_ms: bout,
            bound_out_is_file_end: file_end,
            frames: None,
        };
        if let Some((at, fade)) =
            crate::plan::last_item_stop(&view, self.cfg.rate, self.cfg.truncated_fade_s)
        {
            send(id.stop(at, fade));
            let declick = (engine::mixer::DECLICK_S * f64::from(self.cfg.rate)).round() as u32;
            rec.stop = Some(stop_record(at, fade, declick.max(1)));
        }
        rec.end_reason = None;
        self.sidecar.voices.push(rec);
        self.carts.push(Placed {
            record: self.sidecar.voices.len() - 1,
            ..placed
        });
        Ok(())
    }

    /// Commit a planned boundary: load B, send A's stop and B's fade in. `send` delivers a
    /// command to the mixer (offline: straight into it; live: through the rt-out command ring).
    #[allow(clippy::too_many_arguments)]
    pub fn commit(
        &mut self,
        b: Arc<Item>,
        plan: &Plan,
        preset_a: &TransitionParams,
        preset_b: &TransitionParams,
        entry: Option<PresetPatch>,
        run: Option<RunGain>,
        send: &mut dyn FnMut(Command),
    ) -> Result<(), String> {
        let a_idx = self
            .placed
            .len()
            .checked_sub(1)
            .ok_or("nothing is playing")?;
        let gain = self.gain_for(&b, run);
        let from_ms = plan.b_bound_in_ms;
        let (id, status) = self.open_voice(&b, plan.s_b, from_ms, gain, plan.b_declick)?;
        // A's stop, one command.
        let declick = (engine::mixer::DECLICK_S * f64::from(self.cfg.rate)).round() as u32;
        let mut a_rec_stop = None;
        if let Some((at, fade)) = plan.a_stop {
            send(self.placed[a_idx].id.stop(at, fade));
            a_rec_stop = Some(stop_record(at, fade, declick.max(1)));
        }
        // B's fade in: level automation on B's envelope.
        let mut env = Vec::new();
        if let Some(seg) = plan.b_fade_in {
            send(id.envelope(seg));
            env.push(RampRecord {
                start: seg.start,
                end: seg.end,
                from: seg.from,
                to: seg.to,
                curve: curve_name(seg.curve).into(),
            });
        }
        let a_name = self.placed[a_idx].item.name.clone();
        let planned = Self::frames_known(&self.placed[a_idx]);
        let a_rec = self.placed[a_idx].record;
        if a_rec_stop.is_some() {
            self.sidecar.voices[a_rec].stop = a_rec_stop;
        }
        if self.sidecar.voices[a_rec].planned_frames.is_none() {
            self.sidecar.voices[a_rec].planned_frames = planned;
        }
        self.placed[a_idx].planned_frames = planned;
        let mut rec = self.record_for(
            &b,
            id,
            plan.s_b,
            from_ms,
            gain,
            plan.b_declick,
            plan.b_bound_in_ms,
            None,
        );
        rec.envelope = env;
        self.sidecar.voices.push(rec);
        self.sidecar.boundaries.push(BoundaryRecord {
            from: a_name,
            to: b.name.clone(),
            kind: plan.kind.name().to_string(),
            preset_a: format!("{:?}", preset_a.align.mode),
            preset_b: format!("{:?}", preset_b.align.mode),
            s_b: plan.s_b,
            e_a: plan.e_a,
            f_end_a: plan.f_end_a,
            overlap_frames: plan.overlap_frames,
            lead_frames: plan.lead_frames,
            clamped: plan.clamped,
            ramp: plan.ramp.map(|r| {
                serde_json::json!({
                    "t_a_s": r.t_a_s, "t_b_s": r.t_b_s,
                    "threshold_a_db": r.threshold_a_db, "threshold_b_db": r.threshold_b_db,
                    "overlap_s": r.overlap_s, "capped": r.capped,
                })
            }),
            notes: plan.notes.clone(),
        });
        self.placed.push(Placed {
            item: b,
            id,
            status,
            start_frame: plan.s_b,
            bound_in_ms: from_ms,
            bound_out_ms: plan.b_bound_out_ms,
            out_is_file_end: plan.b_out_is_file_end,
            planned_frames: None,
            entry,
            record: self.sidecar.voices.len() - 1,
            ended: false,
        });
        Ok(())
    }

    /// The last item has no successor: it plays to its bound end (a declick stop at the cue-out
    /// when that is inside the file).
    pub fn finish_last(&mut self, send: &mut dyn FnMut(Command)) {
        let Some(p) = self.placed.last() else { return };
        let view = PlacedView {
            item: &p.item,
            start_frame: p.start_frame,
            bound_in_ms: p.bound_in_ms,
            bound_out_ms: p.bound_out_ms,
            bound_out_is_file_end: p.out_is_file_end,
            frames: Self::frames_known(p),
        };
        if let Some((at, fade)) =
            crate::plan::last_item_stop(&view, self.cfg.rate, self.cfg.truncated_fade_s)
        {
            send(p.id.stop(at, fade));
            let rec = p.record;
            let declick = (engine::mixer::DECLICK_S * f64::from(self.cfg.rate)).round() as u32;
            self.sidecar.voices[rec].stop = Some(stop_record(at, fade, declick.max(1)));
        }
    }

    /// The manual `stop` (2.5): a cosine fade of `fade_s` from `at` on **every** voice still
    /// placed, not only the last one: the tail of the item before it (a 12 s `sam` tail, or a
    /// `plexamp` A that plays to its file end) would otherwise keep playing after "stop", and an
    /// item whose boundary was committed but has not started is cancelled (a stop before its
    /// start). A voice already fading keeps the earlier end (the mixer only shortens).
    pub fn stop_all(&mut self, at: u64, fade_s: f64, send: &mut dyn FnMut(Command)) {
        let len = crate::plan::s_frames(fade_s, self.cfg.rate).max(1) as u64;
        let fade = StopFade::Frames(u32::try_from(len).unwrap_or(u32::MAX));
        // Carts too (`fire cart`, M1.5): "stop" silences the station, not only the programme.
        for p in self
            .placed
            .iter()
            .chain(self.carts.iter())
            .filter(|p| !p.ended)
        {
            send(p.id.stop(at, fade));
            let rec = &mut self.sidecar.voices[p.record];
            let end = at + len;
            let earlier = rec
                .stop
                .as_ref()
                .is_some_and(|s| s.at + u64::from(s.frames) <= end);
            if !earlier {
                rec.stop = Some(stop_record(at, fade, 0));
            }
        }
    }

    /// Take back the last placed item while it has not started (its first frame is after `now`,
    /// the next frame the mixer has not mixed): a manual action after the boundary into it was
    /// committed acts on what is sounding, so the boundary is undone. Its voice is cancelled (a
    /// stop at its start frame), the stop that boundary gave the item before it is cleared (if
    /// its fade has not begun), and the item is returned to be planned again. `None` when there
    /// is nothing to take back.
    ///
    /// The cancel needs B's load to have reached the mixer first: on rt-out loads are serviced
    /// before commands in each block; the offline renderer renders a block in between.
    pub fn uncommit_last(&mut self, now: u64, send: &mut dyn FnMut(Command)) -> Option<Arc<Item>> {
        if self.placed.len() < 2 {
            return None;
        }
        self.uncommit_any(now, send)
    }

    /// As [`Conductor::uncommit_last`], also for the first item placed after silence (a station takes
    /// back the item it was about to start; nothing precedes it to give its end back).
    pub fn uncommit_any(&mut self, now: u64, send: &mut dyn FnMut(Command)) -> Option<Arc<Item>> {
        let last = self.placed.last()?;
        if last.ended || last.start_frame <= now {
            return None;
        }
        send(last.id.stop(last.start_frame, StopFade::Cut));
        let b = self.placed.pop()?;
        let rec = &mut self.sidecar.voices[b.record];
        rec.end_reason = Some("cancelled".into());
        rec.played = Some(0);
        rec.stop = None;
        // The item before it (if there is one) gets its own end back.
        if let Some(a) = self.placed.last() {
            send(a.id.clear_stop());
            self.sidecar.voices[a.record].stop = None;
            if let Some(bd) = self.sidecar.boundaries.last_mut() {
                bd.notes
                    .push("undone by a manual action before its incoming item started".into());
            }
        }
        Some(b.item)
    }

    /// A voice ended before it sounded because its file would not open or decode: take it out of the
    /// placed list and give the item before it its own end back (the stop the boundary gave it is
    /// cleared), so the boundary can be planned again. Only the last placed voice can be dropped.
    pub fn drop_failed(&mut self, id: VoiceId, send: &mut dyn FnMut(Command)) -> bool {
        let Some(pos) = self.placed.iter().position(|p| p.id == id) else {
            return false;
        };
        if pos + 1 != self.placed.len() {
            return false;
        }
        self.placed.pop();
        if let Some(a) = self.placed.last() {
            if !a.ended {
                send(a.id.clear_stop());
                self.sidecar.voices[a.record].stop = None;
            }
        }
        true
    }

    /// A voice that starved plays the rest of its audio late, so a successor placed where its audio was due
    /// to end (a continuity joint, or an item that plays to its file end with no stop) would sound on top of
    /// its tail. Give it an end at the successor's start (a 30 ms fade, the emergency transition's) unless
    /// it has one already. Returns true when a stop was sent.
    pub fn guard_late(&mut self, id: VoiceId, now: u64, send: &mut dyn FnMut(Command)) -> bool {
        let Some(i) = self.placed.iter().position(|p| p.id == id) else {
            return false;
        };
        if self.placed[i].ended || i + 1 >= self.placed.len() {
            return false;
        }
        let rec = self.placed[i].record;
        if self
            .sidecar
            .voices
            .get(rec)
            .is_some_and(|r| r.stop.is_some())
        {
            return false;
        }
        let fade = (0.030 * f64::from(self.cfg.rate)).round() as u32;
        let at = self.placed[i + 1]
            .start_frame
            .saturating_sub(u64::from(fade))
            .max(now);
        send(self.placed[i].id.stop(at, StopFade::Frames(fade)));
        if let Some(r) = self.sidecar.voices.get_mut(rec) {
            r.stop = Some(stop_record(at, StopFade::Frames(fade), 0));
        }
        true
    }

    /// A master-gain ramp. The master sits after the safety limiter, so a segment above 1 would
    /// defeat its ceiling: refused here (and clamped by the mixer).
    pub fn master(&self, seg: Segment) -> Result<Command, String> {
        if seg.from > 1.0 || seg.to > 1.0 {
            return Err("the master gain may not exceed 1".into());
        }
        Ok(master_ramp(seg))
    }

    /// Collect the mixer's events into the sidecar and mark ended voices; the Started and Ended events
    /// are also queued in `events` for the station.
    pub fn tick(&mut self) {
        while let Some(ev) = self.ctl.poll_event() {
            let find = |placed: &[Placed], carts: &[Placed], voice: u8, gen: u32| {
                placed
                    .iter()
                    .position(|p| p.id.slot == voice && p.id.gen == gen)
                    .map(|i| (false, i))
                    .or_else(|| {
                        carts
                            .iter()
                            .position(|p| p.id.slot == voice && p.id.gen == gen)
                            .map(|i| (true, i))
                    })
            };
            match ev {
                MixerEvent::Started { voice, gen, frame } => {
                    if let Some((cart, i)) = find(&self.placed, &self.carts, voice, gen) {
                        let p = if cart {
                            &self.carts[i]
                        } else {
                            &self.placed[i]
                        };
                        if let Some(r) = self.sidecar.voices.get_mut(p.record) {
                            r.started_frame = Some(frame);
                        }
                        self.events.push(VoiceEvent::Started { id: p.id, frame });
                    }
                }
                MixerEvent::Ended {
                    voice,
                    gen,
                    frame,
                    reason,
                    played,
                } => {
                    if let Some((cart, i)) = find(&self.placed, &self.carts, voice, gen) {
                        let p = if cart {
                            &mut self.carts[i]
                        } else {
                            &mut self.placed[i]
                        };
                        if let Some(rec) = self.sidecar.voices.get_mut(p.record) {
                            rec.ended_frame = Some(frame);
                            rec.played = Some(played);
                            rec.end_reason = Some(
                                match reason {
                                    EndReason::Eof => "eof",
                                    EndReason::Failed => "failed",
                                    EndReason::Stopped => "stopped",
                                    EndReason::Cancelled => "cancelled",
                                }
                                .to_string(),
                            );
                        }
                        p.ended = true;
                        self.events.push(VoiceEvent::Ended {
                            id: p.id,
                            frame,
                            played,
                            reason,
                        });
                    }
                }
                MixerEvent::Starved { voice, gen, frame } => {
                    if let Some((cart, i)) = find(&self.placed, &self.carts, voice, gen) {
                        let p = if cart {
                            &self.carts[i]
                        } else {
                            &self.placed[i]
                        };
                        if let Some(r) = self.sidecar.voices.get_mut(p.record) {
                            r.starved_frames_events += 1;
                        }
                        if !cart {
                            self.events.push(VoiceEvent::Starved { id: p.id, frame });
                        }
                    }
                }
                MixerEvent::Late { voice, gen, by } => {
                    if let Some((cart, i)) = find(&self.placed, &self.carts, voice, gen) {
                        let p = if cart {
                            &self.carts[i]
                        } else {
                            &self.placed[i]
                        };
                        if let Some(r) = self.sidecar.voices.get_mut(p.record) {
                            r.late_frames += by;
                        }
                    }
                }
                MixerEvent::LoadRejected { .. } => {
                    self.sidecar.notes.push("the mixer rejected a load".into());
                }
            }
        }
        for p in self.placed.iter().chain(self.carts.iter()) {
            if let Some(t) = p.status.total() {
                if let Some(rec) = self.sidecar.voices.get_mut(p.record) {
                    if rec.total_frames.is_none() {
                        rec.total_frames = Some(t);
                        rec.length_mismatch_frames = rec
                            .planned_frames
                            .map(|pl| t as i64 - pl as i64)
                            .filter(|d| *d != 0);
                    }
                }
            }
        }
    }

    /// Keep the sidecar (and `placed`) from growing for ever in a station that runs for months: keep
    /// the last `keep` placed items and drop the rest, with their sidecar records.
    pub fn compact(&mut self, keep: usize) {
        if self.placed.len() <= keep + 16 {
            return;
        }
        let drop = self.placed.len() - keep;
        let kept: Vec<Placed> = self.placed.split_off(drop);
        let mut voices = Vec::with_capacity(kept.len() + self.carts.len());
        self.placed = kept;
        for p in self.placed.iter_mut().chain(self.carts.iter_mut()) {
            if let Some(v) = self.sidecar.voices.get(p.record) {
                voices.push(v.clone());
                p.record = voices.len() - 1;
            }
        }
        self.sidecar.voices = voices;
        let nb = self.sidecar.boundaries.len();
        if nb > keep {
            self.sidecar.boundaries.drain(..nb - keep);
        }
        self.carts.retain(|c| !c.ended);
    }

    /// True while a voice that starts before `upto` has not opened its file or has nothing
    /// buffered yet (an offline render waits for the decoder here; a live station never waits).
    pub fn not_ready(&self, upto: u64, need_frames: u64) -> bool {
        self.placed.iter().chain(self.carts.iter()).any(|p| {
            if p.ended || p.start_frame >= upto {
                return false;
            }
            if !p.status.opened.load(Acquire) {
                return true;
            }
            let avail = p
                .status
                .pushed_frames
                .load(Acquire)
                .saturating_sub(p.status.consumed_frames.load(Acquire));
            !p.status.eof.load(Acquire) && avail < need_frames
        })
    }

    pub fn live_voices(&self) -> usize {
        self.placed.iter().filter(|p| !p.ended).count()
    }

    pub fn finish(&mut self, mixer_stats: engine::mixer::MixerStats, frames: u64) {
        self.tick();
        self.sidecar.render_frames = frames;
        self.sidecar.starved_frames = mixer_stats.starved_frames;
        self.sidecar.late_starts = mixer_stats.late_starts;
        self.sidecar.limiter_over_1db_frames = mixer_stats.limiter_over_1db_frames;
    }

    pub fn stats(&self) -> engine::mixer::MixerStats {
        self.ctl.stats()
    }

    /// The meter thread's ends of the mixer's taps (11.3); `Some` once.
    pub fn take_meter_tap(&mut self) -> Option<engine::mixer::MeterTap> {
        self.ctl.take_meter_tap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_master_gain_may_not_exceed_one() {
        let (c, _mixer) = Conductor::new(ConductorConfig::new(48_000, "plexamp"));
        let seg = |from, to| Segment {
            start: 0,
            end: 100,
            from,
            to,
            curve: Curve::Linear,
        };
        assert!(c.master(seg(1.0, 0.0)).is_ok());
        assert!(c.master(seg(0.0, 1.0)).is_ok());
        assert!(c.master(seg(1.0, 1.01)).is_err());
        assert!(c.master(seg(1.5, 0.5)).is_err());
    }
}
