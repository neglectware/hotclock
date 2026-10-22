//! Offline rendering of a whole station on a virtual clock (tests of M1.5): the real conductor, the real
//! mixer and the same [`Station::step`] the rt-out loop calls, one block at a time, waiting for the
//! decoders. Operator actions are given as `(seconds, Op)` and applied at the first block at or after
//! their time (retried after a block when the station says `Busy`).

use crate::runtime::{Op, OpError, Station};
use engine::mixer::{Mixer, MAX_BLOCK};
use engine::rt::Command;
use engine::source::Source;
use std::time::{Duration, Instant};

pub struct Offline {
    pub station: Station,
    pub mixer: Mixer,
    /// Frames rendered so far.
    pub t: u64,
    /// Interleaved stereo. Empty when `keep_samples` is off.
    pub samples: Vec<f32>,
    /// Keep every rendered sample in `samples` (on by default; a two-hour render turns it off and reads
    /// the blocks through `sink`).
    pub keep_samples: bool,
    /// Called with the frame at which each rendered block starts and its interleaved samples.
    #[allow(clippy::type_complexity)]
    pub sink: Option<Box<dyn FnMut(u64, &[f32]) + Send>>,
    /// The wall time (unix ms) at frame 0.
    pub wall_origin_ms: i64,
    pub op_log: Vec<(u64, Op, Result<(), String>)>,
    block: Vec<f32>,
    pub time_step: Duration,
    pub time_render: Duration,
    pub time_wait: Duration,
}

impl Offline {
    pub fn new(station: Station, mixer: Mixer, wall_origin_ms: i64) -> Self {
        Self {
            station,
            mixer,
            t: 0,
            samples: Vec::new(),
            keep_samples: true,
            sink: None,
            wall_origin_ms,
            op_log: Vec::new(),
            block: vec![0.0; MAX_BLOCK * 2],
            time_step: Duration::ZERO,
            time_render: Duration::ZERO,
            time_wait: Duration::ZERO,
        }
    }

    pub fn wall_of(&self, frame: u64) -> i64 {
        self.wall_origin_ms + (frame as i64 * 1000) / i64::from(self.station.opts.rate)
    }

    /// Step the wall clock by `delta_ms` at the current frame (a test of 1.5's clock jump): the audio
    /// does not move, every wall time after this one does.
    pub fn step_clock(&mut self, delta_ms: i64) {
        self.wall_origin_ms += delta_ms;
    }

    /// The wall time of `frame` without the rounding to milliseconds.
    pub fn wall_exact(&self, frame: u64) -> f64 {
        self.wall_origin_ms as f64 + frame as f64 * 1000.0 / f64::from(self.station.opts.rate)
    }

    /// Start the station at frame 0 (resume or first item).
    pub fn start(&mut self) -> Result<crate::runtime::StartInfo, String> {
        let wall = self.wall_of(0);
        let mixer = &mut self.mixer;
        let mut send = |c: Command| mixer.command(&c.kind);
        self.station.start(0, wall, &mut send)
    }

    /// Render until `until_s` seconds, applying `ops` (sorted by time).
    pub fn run_until(&mut self, until_s: f64, ops: &[(f64, Op)]) -> Result<(), String> {
        let rate = f64::from(self.station.opts.rate);
        let until = (until_s * rate) as u64;
        let mut next_op = 0usize;
        let mut waiting_since = Instant::now();
        let lookahead = self.station.opts.lookahead_frames;
        while self.t < until {
            let wall = self.wall_of(self.t);
            self.station.wall_frac_ms = self.wall_exact(self.t) - wall as f64;
            let t = self.t;
            let t_step = Instant::now();
            {
                let mixer = &mut self.mixer;
                let mut send = |c: Command| mixer.command(&c.kind);
                self.station.step(t, wall, &mut send)?;
                while let Some((at, op)) = ops.get(next_op) {
                    if (*at * rate) as u64 > t {
                        break;
                    }
                    match self.station.op(op.clone(), t, wall, &mut send) {
                        Ok(_) => {
                            self.op_log.push((t, op.clone(), Ok(())));
                            next_op += 1;
                        }
                        Err(OpError::Busy) => break,
                        Err(OpError::Refused(m)) => {
                            self.op_log.push((t, op.clone(), Err(m)));
                            next_op += 1;
                        }
                    }
                }
            }
            self.time_step += t_step.elapsed();
            // The pre-flight worker is real-time too, and its requests time out on the station's clock: a read
            // that takes 30 ms would be "hung" after four virtual seconds at 200 times real time. Let it answer
            // (a read of a local file takes a few milliseconds).
            {
                let w = Instant::now();
                self.station.fail.preflight.settle(Duration::from_secs(10));
                self.time_wait += w.elapsed();
            }
            // The decoder pool works in real time and this loop does not: at hundreds of times real time a
            // file that will not open would be found long after the boundary it was committed for. Let the
            // pool open what was just committed before virtual time moves on (a station in real time has
            // seconds for it), so an accelerated run sees what a real one sees.
            if let Some(p) = self.station.cond.placed.last() {
                if !p.status.opened.load(std::sync::atomic::Ordering::Acquire) {
                    let w = Instant::now();
                    while !p.status.opened.load(std::sync::atomic::Ordering::Acquire)
                        && w.elapsed() < Duration::from_secs(10)
                    {
                        std::thread::sleep(Duration::from_micros(100));
                    }
                    self.time_wait += w.elapsed();
                }
            }
            if self
                .station
                .cond
                .not_ready(t + MAX_BLOCK as u64 + lookahead, 2 * MAX_BLOCK as u64)
            {
                if waiting_since.elapsed() > Duration::from_secs(60) {
                    return Err("timed out waiting for the decoder".into());
                }
                let w = Instant::now();
                std::thread::sleep(Duration::from_micros(200));
                self.time_wait += w.elapsed();
                continue;
            }
            waiting_since = Instant::now();
            let r = Instant::now();
            self.mixer.render(&mut self.block);
            self.time_render += r.elapsed();
            self.station.load_in_flight = false;
            if let Some(f) = self.sink.as_mut() {
                f(self.t, &self.block);
            }
            if self.keep_samples {
                self.samples.extend_from_slice(&self.block);
            }
            self.t += MAX_BLOCK as u64;
        }
        // Pick up the events of the last block.
        let wall = self.wall_of(self.t);
        let t = self.t;
        let mixer = &mut self.mixer;
        let mut send = |c: Command| mixer.command(&c.kind);
        self.station.step(t, wall, &mut send)
    }

    pub fn left(&self) -> Vec<f32> {
        self.samples.chunks_exact(2).map(|f| f[0]).collect()
    }
}
