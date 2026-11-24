//! Unload a program that has stopped and stays stopped (`programs.unload_after_min` of the global file).
//!
//! A program that ends with a `stop` or `fade_stop` block (the Eve example's "Stop after midnight"), or that the
//! operator stops, stays loaded: the Program unit shows it and its last item stays on the status display. After
//! the setting's minutes with nothing playing and no new start, the station runs the operator's End program and
//! stop the audio ([`Station::program_abort_by`]), the same code the "⋯" menu runs, and logs it as automatic.
//!
//! The countdown starts when the transport is `stopped`, a program is loaded and no voice sounds (a stop's fade
//! has ended). Anything that leaves that state (a start, a resume, a switch) ends it; any other operator action
//! while it runs (a cue, a queue edit, Keep loaded) cancels it for this stop. Its start time is kept in
//! `state.db` with the scheduler's state, so a restart neither loses nor restarts it: a program that has been
//! stopped longer than the setting when the station comes back is unloaded at its first step.

use crate::runtime::{Station, Transport};
use engine::rt::Command;
use serde::{Deserialize, Serialize};

/// The countdown, persisted in `SchedState::unload`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnloadTimer {
    /// When the program was first seen stopped with nothing sounding, wall ms; `None` while it is not.
    pub since_ms: Option<i64>,
    /// The program it counts for: its root playlist and that level's T0 (the run id moves while a nested level
    /// deals again, so it does not name the program).
    pub program: Option<(i64, i64)>,
    /// Keep loaded, or another operator action, cancelled the countdown for this stop.
    pub kept: bool,
}

/// Who ended a program: the block log's `how` of an End program row.
pub const BY_OPERATOR: &str = "operator";
pub const BY_AUTOMATIC: &str = "automatic";

impl Station {
    /// The loaded program (root playlist, T0) when the station is stopped with nothing sounding; `None` otherwise.
    fn unload_candidate(&self) -> Option<(i64, i64)> {
        if self.transport != Transport::Stopped
            || self.fail.safe_mode
            || self.cond.live_voices() > 0
        {
            return None;
        }
        self.sched.st.program.as_ref().map(|p| {
            (
                p.root_playlist(),
                p.stack.first().map_or(p.t0_ms, |c| c.t0_ms),
            )
        })
    }

    /// The wall time the loaded program unloads at, while the countdown runs (`None`: no countdown, Never, or
    /// cancelled for this stop).
    pub fn unload_due_ms(&self) -> Option<i64> {
        let t = &self.sched.st.unload;
        if t.kept {
            return None;
        }
        Some(t.since_ms? + i64::from(self.opts.unload_after_min?) * 60_000)
    }

    /// Called every step: start, end or carry out the countdown.
    pub(crate) fn unload_step(&mut self, send: &mut dyn FnMut(Command)) {
        let wall = self.wall_ms;
        let Some(prog) = self.unload_candidate() else {
            if self.sched.st.unload != UnloadTimer::default() {
                self.sched.st.unload = UnloadTimer::default();
                self.dirty = true;
            }
            return;
        };
        let t = &mut self.sched.st.unload;
        if t.since_ms.is_none() || t.program != Some(prog) {
            *t = UnloadTimer {
                since_ms: Some(wall),
                program: Some(prog),
                kept: false,
            };
            self.dirty = true;
            if let Some(m) = self.opts.unload_after_min {
                tracing::info!(
                    playlist = prog.0,
                    "the program is stopped: it unloads in {m} min unless something starts"
                );
            }
            return;
        }
        if self.unload_due_ms().is_some_and(|due| wall >= due) {
            let since = self.sched.st.unload.since_ms.unwrap_or(wall);
            tracing::info!(
                playlist = prog.0,
                "the program has been stopped for {} s: End program and stop the audio, automatically",
                (wall - since) / 1000
            );
            if let Err(e) = self.program_abort_by(true, BY_AUTOMATIC, send) {
                tracing::warn!("the automatic unload did not run: {e}");
                self.sched.st.unload.kept = true;
                self.dirty = true;
            }
        }
    }

    /// Keep loaded: cancel the countdown for this stop. Refused when none runs.
    pub fn keep_loaded(&mut self) -> Result<(), String> {
        if self.unload_due_ms().is_none() {
            return Err("no unload is counting down".into());
        }
        self.sched.st.unload.kept = true;
        self.dirty = true;
        tracing::info!("Keep loaded: the stopped program stays loaded");
        Ok(())
    }

    /// An operator action other than Stop: a countdown that runs is cancelled for this stop.
    pub(crate) fn unload_touched(&mut self) {
        let t = &mut self.sched.st.unload;
        if t.since_ms.is_some() && !t.kept {
            t.kept = true;
            self.dirty = true;
        }
    }

    /// Test hook: the countdown began `seconds` earlier than it did (as if the station had been stopped that much
    /// longer). The end-to-end tests use it instead of waiting minutes.
    pub fn hook_unload_age(&mut self, seconds: i64) {
        if let Some(s) = self.sched.st.unload.since_ms.as_mut() {
            *s -= seconds * 1000;
            self.dirty = true;
        }
    }
}
