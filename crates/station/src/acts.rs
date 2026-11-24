//! What the station does for the acts the scheduler hands it (milestone M5): the master gain (`set_gain`,
//! `set_mute`, `fade_stop`, architecture 2.4) and the side effects (`osc_out`, `midi_out`,
//! `http_request`, 4.9).

use crate::runtime::{ArmedStop, Station};
use crate::side::{SideJob, SideSink};
use crate::timed::{ActKind, TimedAct};
use engine::envelope::{Curve, Segment};
use engine::mixer::master_ramp;
use engine::rt::Command;

/// The station's master gain as the blocks left it. The mixer holds the real envelope; this is the
/// control side's record of where it was sent.
#[derive(Debug, Clone, Copy)]
pub struct MasterState {
    /// The level `set_gain` asked for, linear, at most 1.
    pub gain: f32,
    pub mute: bool,
    /// Where the master envelope ends up after the last ramp sent.
    pub cur: f32,
    /// A `fade_stop` took the master to silence and nothing has started since.
    pub faded: bool,
}

impl Default for MasterState {
    fn default() -> Self {
        MasterState {
            gain: 1.0,
            mute: false,
            cur: 1.0,
            faded: false,
        }
    }
}

impl MasterState {
    /// The level the master should rest at when nothing is fading it.
    pub fn rest(&self) -> f32 {
        if self.mute {
            0.0
        } else {
            self.gain
        }
    }
}

/// The sink a station starts with: the worker thread, or a logging sink when `HOTCLOCK_SIDE_DRYRUN` is set.
pub fn default_side_sink() -> Box<dyn SideSink> {
    if std::env::var_os("HOTCLOCK_SIDE_DRYRUN").is_some() {
        Box::new(crate::side::LogSink::default())
    } else {
        Box::new(crate::side::SideWorker::spawn())
    }
}

pub fn db_to_lin(db: f64) -> f32 {
    10f64.powf(db / 20.0) as f32
}

impl Station {
    fn master_ramp_to(
        &mut self,
        level: f32,
        at: u64,
        frames: u64,
        curve: Curve,
        send: &mut dyn FnMut(Command),
    ) {
        let end = at + frames.max(1);
        send(master_ramp(Segment {
            start: at,
            end,
            from: self.master.cur,
            to: level,
            curve,
        }));
        self.master.cur = level;
    }

    /// The first sound after a `fade_stop` brings the master back to where `set_gain` and `set_mute` left
    /// it (2.4), in the millisecond before `start_frame`.
    pub(crate) fn master_restore(&mut self, start_frame: u64, send: &mut dyn FnMut(Command)) {
        if !self.master.faded {
            return;
        }
        self.master.faded = false;
        let rest = self.master.rest();
        let ramp = u64::from(self.rate() / 1000).max(1);
        self.master_ramp_to(
            rest,
            start_frame.saturating_sub(ramp).max(self.now_frame),
            ramp,
            Curve::Linear,
            send,
        );
    }

    /// Hand a side effect to the sink. Never blocks; a refusal is logged and counted as a failure.
    pub(crate) fn submit_side(
        &mut self,
        block_id: i64,
        name: &str,
        at_wall_ms: i64,
        a: crate::side::SideAction,
    ) {
        self.side_seq += 1;
        let job = SideJob {
            id: self.side_seq,
            block_id,
            name: name.into(),
            at_wall_ms,
            handed_ms: self.wall_ms,
            action: a,
        };
        if !self.side.submit(job.clone()) {
            tracing::warn!(block = %name, "side effect dropped: the queue is full");
            self.side_failed(&job, "the side-effect queue is full");
        }
    }

    fn side_failed(&mut self, job: &SideJob, why: &str) {
        let wall = self.wall_ms;
        self.sched.log_failure(
            job.block_id,
            &job.name,
            job.action.type_name(),
            job.at_wall_ms,
            &format!("{}: {why}", job.action.describe()),
        );
        self.fail.alerts.raise(
            crate::alerts::code::BLOCK_FAILED,
            &job.block_id.to_string(),
            crate::alerts::Severity::Warning,
            format!("block `{}` could not send: {why}", job.name),
            None,
            wall,
        );
        self.dirty = true;
    }

    /// Collect what the side-effect worker finished since the last step.
    pub(crate) fn side_poll(&mut self) {
        for r in self.side.results() {
            if r.ok {
                tracing::info!(
                    block = %r.job.name,
                    attempts = r.attempts,
                    late_ms = r.sent_wall_ms - r.job.at_wall_ms,
                    "side effect sent: {}",
                    r.job.action.describe()
                );
                // A later success clears the alert of the same block.
                self.fail.alerts.clear(
                    crate::alerts::code::BLOCK_FAILED,
                    &r.job.block_id.to_string(),
                    self.wall_ms,
                );
            } else {
                let why = r.error.clone().unwrap_or_default();
                tracing::warn!(block = %r.job.name, "side effect failed: {why}");
                self.side_failed(&r.job, &why);
            }
        }
    }

    /// The acts of M5 handed over by `Scheduler::due_acts`.
    pub(crate) fn run_act(&mut self, act: TimedAct, send: &mut dyn FnMut(Command)) {
        let earliest = self.now_frame
            + crate::plan::s_frames(crate::plan::MIN_LEAD_S, self.rate()).max(0) as u64;
        match act.kind {
            ActKind::Side(a) => self.submit_side(act.block_id, &act.name, act.target_ms, a),
            ActKind::Gain { gain_db, ramp_ms } => {
                let g = db_to_lin(gain_db.min(0.0));
                self.master.gain = g;
                if !self.master.faded {
                    let at = self.frame_of_wall(act.target_ms).max(earliest);
                    let frames = ramp_ms.max(0) as u64 * u64::from(self.rate()) / 1000;
                    let rest = self.master.rest();
                    self.master_ramp_to(rest, at, frames, Curve::Cosine, send);
                }
            }
            ActKind::Mute { on, ramp_ms } => {
                self.master.mute = on;
                if !self.master.faded {
                    let at = self.frame_of_wall(act.target_ms).max(earliest);
                    let frames = ramp_ms.max(0) as u64 * u64::from(self.rate()) / 1000;
                    let rest = self.master.rest();
                    self.master_ramp_to(rest, at, frames, Curve::Cosine, send);
                }
            }
            ActKind::FadeStop { fade_ms } => {
                self.undo_pending(send);
                let at = self.frame_of_wall(act.target_ms).max(earliest);
                let frames = (fade_ms.max(1) as u64 * u64::from(self.rate())) / 1000;
                self.master_ramp_to(0.0, at, frames, Curve::Cosine, send);
                self.master.faded = true;
                // Everything stops when the master reaches silence (a few milliseconds of declick).
                let end = at + frames;
                self.cond.stop_all(end, 0.005, send);
                if self.armed_stop.is_none_or(|a| end < a.at) {
                    self.armed_stop = Some(ArmedStop {
                        at: end,
                        fade_s: 0.005,
                    });
                }
                tracing::info!(block = %act.name, "fade_stop armed {} ms ahead, {} ms long", act.target_ms - self.wall_ms, fade_ms);
            }
            _ => {}
        }
    }
}

/// How `POST /program/show-time` moves the show time.
#[derive(Debug, Clone, PartialEq)]
pub enum ShowTimeSet {
    /// To this instant, ms.
    At(i64),
    /// To a local time of day on the date of the program's T0.
    Local(String),
    /// By this much from where it is now (negative is earlier), ms.
    Delay(i64),
    /// Back to the playlist's own show time.
    Reset,
}

impl crate::sched::Scheduler {
    /// Move the running program's show time for this run, or put back the one on its playlist. Every block
    /// timed from the show time moves with it; one that already acted in this run does not act again. The
    /// caller regenerates the plan. Returns the show time now in force.
    pub fn move_show_time(&mut self, set: ShowTimeSet) -> Result<i64, String> {
        let zone = self.cfg.zone.clone();
        let p = self
            .st
            .program
            .as_ref()
            .filter(|p| !p.ended)
            .ok_or("no program is running")?;
        let root = p.root_playlist();
        let root_t0 = p.stack.first().map_or(p.t0_ms, |c| c.t0_ms);
        let own = self
            .prog
            .playlists
            .get(&root)
            .and_then(|pl| pl.show_time())
            .ok_or("the program's playlist has no show time (set one on its start block)")?
            .instant(&zone, root_t0)
            .ok_or("the playlist's show time is not a valid local time")?;
        let current = p.show_override_ms.unwrap_or(own);
        let new = match set {
            ShowTimeSet::Reset => None,
            ShowTimeSet::At(ms) => Some(ms),
            ShowTimeSet::Delay(ms) => Some(current + ms),
            ShowTimeSet::Local(at) => Some(
                common::timespec::local_on_t0_date(&zone, root_t0, &at, 0)
                    .ok_or_else(|| format!("`{at}` is not a local time of day (HH:MM[:SS])"))?,
            ),
        };
        if let Some(n) = new {
            if n < root_t0 {
                return Err("the show time cannot be before the program's T0".into());
            }
        }
        if let Some(p) = self.st.program.as_mut() {
            p.show_override_ms = new.filter(|n| *n != own);
            p.epoch += 1;
        }
        tracing::info!(
            "show time {} for this run",
            new.map_or("put back".to_string(), |n| format!(
                "moved to {}",
                zone.local_string(n)
            ))
        );
        Ok(new.unwrap_or(own))
    }
}

// ---- program actions (4.7, 7.2) -------------------------------------------------------------------------------

impl Station {
    fn timing_for<'a>(&'a self) -> crate::runtime::RuntimeTiming<'a> {
        crate::runtime::RuntimeTiming {
            cat: &self.cat,
            presets: &self.presets,
            preset: &self.opts.preset,
            rate: self.opts.rate,
        }
    }

    /// `continue`: release the `wait` block that waits for the operator.
    pub fn program_continue(&mut self) -> Result<String, String> {
        let wall = self.wall_ms;
        let t = crate::runtime::RuntimeTiming {
            cat: &self.cat,
            presets: &self.presets,
            preset: &self.opts.preset,
            rate: self.opts.rate,
        };
        let name = self.sched.continue_program(wall, &self.cat, &t)?;
        self.unload_touched();
        self.dirty = true;
        Ok(name)
    }

    /// `POST /program/show-time`: move the running program's show time for this run, or put back the one on
    /// its playlist ([`crate::sched::Scheduler::move_show_time`]). Returns the show time now in force.
    pub fn program_show_time(&mut self, set: ShowTimeSet) -> Result<i64, String> {
        let at = self.sched.move_show_time(set)?;
        self.regenerate_now();
        self.dirty = true;
        Ok(at)
    }

    fn regenerate_now(&mut self) {
        let wall = self.wall_ms;
        let t = crate::runtime::RuntimeTiming {
            cat: &self.cat,
            presets: &self.presets,
            preset: &self.opts.preset,
            rate: self.opts.rate,
        };
        self.sched.regenerate(wall, &self.cat, &t, false);
    }

    /// `abort`: the program ends without further blocks; what plays keeps playing unless `stop_audio`. The
    /// operator's End program (the "⋯" menu of the Program unit, `POST /program/abort`).
    pub fn program_abort(
        &mut self,
        stop_audio: bool,
        send: &mut dyn FnMut(Command),
    ) -> Result<(), String> {
        self.program_abort_by(stop_audio, crate::unload::BY_OPERATOR, send)
    }

    /// [`Station::program_abort`] on behalf of `by`: the operator, or the automatic unload of a stopped program
    /// (`crate::unload`). The one code path of both; the block log row says which it was.
    pub fn program_abort_by(
        &mut self,
        stop_audio: bool,
        by: &str,
        send: &mut dyn FnMut(Command),
    ) -> Result<(), String> {
        let wall = self.wall_ms;
        let ended = self.sched.st.program.as_ref().map(|p| {
            let root = p.root_playlist();
            let name = self
                .sched
                .prog
                .playlists
                .get(&root)
                .map_or_else(|| format!("playlist {root}"), |pl| pl.name.clone());
            (p.run_id, name)
        });
        let stopped_for_s = self
            .sched
            .st
            .unload
            .since_ms
            .map(|s| (wall - s).max(0) / 1000);
        {
            let t = crate::runtime::RuntimeTiming {
                cat: &self.cat,
                presets: &self.presets,
                preset: &self.opts.preset,
                rate: self.opts.rate,
            };
            self.sched.abort_program(wall, &self.cat, &t)?;
        }
        // The program's own stop or fade out that was set up, and a start that waits for a fade, go with it.
        self.armed_stop = None;
        self.program_start_at = None;
        self.undo_pending(send);
        if stop_audio {
            let earliest = self.now_frame
                + crate::plan::s_frames(crate::plan::MIN_LEAD_S, self.rate()).max(0) as u64;
            self.cond.stop_all(earliest, crate::plan::STOP_FADE_S, send);
            for t in self.voices.values_mut() {
                t.end_hint = "stop";
            }
            self.transport = crate::runtime::Transport::Stopped;
            self.finished_last = true;
            self.announce_clear = true;
        }
        if let Some((run_id, playlist)) = ended {
            let what = if stop_audio {
                "End program and stop the audio"
            } else {
                "End program, music continues"
            };
            let detail = match (by, stopped_for_s) {
                (crate::unload::BY_AUTOMATIC, Some(s)) => {
                    format!(
                        "{playlist}: unloaded automatically after {} min {} s stopped",
                        s / 60,
                        s % 60
                    )
                }
                (crate::unload::BY_AUTOMATIC, None) => {
                    format!("{playlist}: unloaded automatically")
                }
                _ => format!("{playlist}: by the operator"),
            };
            tracing::info!(run_id, by, "{what}: {detail}");
            self.sched.log_program_end(run_id, what, by, wall, &detail);
        }
        self.sched.st.unload = Default::default();
        self.dirty = true;
        Ok(())
    }

    /// `fire block`: the block acts now, as if its time had come (4.7).
    pub fn program_fire(
        &mut self,
        block_id: i64,
        send: &mut dyn FnMut(Command),
    ) -> Result<(), String> {
        let wall = self.wall_ms;
        let acts = {
            let t = crate::runtime::RuntimeTiming {
                cat: &self.cat,
                presets: &self.presets,
                preset: &self.opts.preset,
                rate: self.opts.rate,
            };
            self.sched.fire_block(block_id, wall, &self.cat, &t)?
        };
        for a in acts {
            self.apply_act(a, send);
        }
        self.unload_touched();
        self.dirty = true;
        Ok(())
    }

    pub fn program_skip(&mut self) -> Result<String, String> {
        let wall = self.wall_ms;
        let name = self.sched.skip_next_block(wall)?;
        self.unload_touched();
        self.regenerate_now();
        self.dirty = true;
        Ok(name)
    }

    /// The fallback auto-start of a `manual` start block (4.9): off unless the block enables it and the
    /// occurrence is armed. Called every step.
    pub(crate) fn fallback_step(&mut self, send: &mut dyn FnMut(Command)) {
        let wall = self.wall_ms;
        let Some((pid, block, occ)) = self.sched.fallback_due(wall) else {
            return;
        };
        self.sched.mark_fallback_fired(block, occ);
        let _ = self.timing_for();
        tracing::info!(playlist = pid, "fallback auto-start of a manual program");
        let op = crate::runtime::Op::StartProgram {
            playlist_id: pid,
            t0: crate::program::T0Req::Now,
            how: crate::sched::SwitchHow::Now,
            source: "auto",
        };
        let (frame, wall) = (self.now_frame, self.wall_ms);
        if let Err(e) = self.op(op, frame, wall, send) {
            tracing::warn!("the fallback start did not run: {e:?}");
        }
    }
}

impl Station {
    /// The scheduler's state changed outside a step (an API call): write it at the next flush.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}
