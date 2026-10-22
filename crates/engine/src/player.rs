//! A sequential player: the M1.2 test harness command (`hotclock play`) and the offline renderer
//! the tests use. It plays a list of files one after another, gaplessly, with no segues (those
//! come with the conductor in M1.4): file `n + 1` starts at the frame where file `n` ends, with
//! no declick and no overlap, exactly like an album-continuity joint (architecture 2.5).
//!
//! Two modes:
//! * [`play_realtime`] runs the real rt-out thread against any [`Output`] (ALSA, null, paced
//!   file). This is the path that goes on air.
//! * [`render_offline`] drives the mixer itself, one block at a time, waiting for the decoder
//!   pool whenever a ring is empty, so it is deterministic and as fast as the CPU allows. It is
//!   for unpaced file output and for tests, and is never used on air.
//!
//! The control loop is the same in both ([`Seq::tick`]): keep at most three voices loaded, open
//! the next file once the previous one's exact length is known (the decoder reports it 20 s
//! before the end at the latest), and start it at the previous file's end frame.

use crate::feed::FeedStatus;
use crate::mixer::{EndReason, LoadParams, Mixer, MixerControl, MixerEvent, VoiceId};
use crate::output::Output;
use crate::pool::{DecoderPool, VoiceSource};
use crate::rt::{RtHandle, RtOptions};
use common::Clock;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::*};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_LOADED: usize = 3;

#[derive(Debug, Clone)]
pub struct PlayOptions {
    pub decoder_threads: usize,
    /// `SCHED_FIFO` priority for rt-out (Linux; realtime mode).
    pub rt_priority: Option<u8>,
    /// How long to wait for the first file's ring to fill before starting (seconds).
    pub prebuffer_s: f64,
}

impl Default for PlayOptions {
    fn default() -> Self {
        Self {
            decoder_threads: 3,
            rt_priority: None,
            prebuffer_s: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileReport {
    pub path: PathBuf,
    /// Frame at which the file's first sample plays.
    pub start_frame: u64,
    /// Frames the decoder produced (exact, once the file has been decoded).
    pub decoded_frames: Option<u64>,
    /// Recoverable decode errors (each padded with silence).
    pub glitches: u32,
    /// Why the file could not be played (or was cut short).
    pub error: Option<String>,
    pub end_reason: Option<EndReason>,
}

#[derive(Debug, Clone, Default)]
pub struct PlayReport {
    pub files: Vec<FileReport>,
    /// Frame at which the last file's audio ends.
    pub end_frame: u64,
    pub rate: u32,
    pub xruns: u32,
    pub starved_frames: u64,
    pub starve_episodes: u32,
    pub late_starts: u32,
    pub limiter_active_frames: u64,
    pub limiter_over_1db_frames: u64,
}

struct Playing {
    id: VoiceId,
    status: Arc<FeedStatus>,
    file: usize,
    start: u64,
}

/// Per-file extras for the offline renderer (M6a): each file's constant gain in dB, like the station's
/// per-item normalization (3.6). Empty means 0 dB for every file.
#[derive(Debug, Clone, Default)]
pub struct PlayExtras {
    pub gains_db: Vec<f64>,
    /// `(enabled, ceiling_dbtp, gain_db)` for the loudness link before the first block: the controller's
    /// gain stage and limiter are in the path from frame 0.
    pub loudness: Option<(bool, f64, f64)>,
}

struct Seq {
    gains_db: Vec<f64>,
    files: Vec<PathBuf>,
    pool: DecoderPool,
    ctl: MixerControl,
    next: usize,
    next_start: u64,
    playing: Vec<Playing>,
    reports: Vec<FileReport>,
    by_voice: HashMap<(u8, u32), usize>,
    /// The latest end frame seen in an `Ended` event.
    end_frame: u64,
    /// The latest `start + total` of a file whose length is known.
    known_end: u64,
}

impl Seq {
    fn new(
        files: Vec<PathBuf>,
        gains_db: Vec<f64>,
        rate: u32,
        threads: usize,
        ctl: MixerControl,
    ) -> Self {
        let reports = files
            .iter()
            .map(|p| FileReport {
                path: p.clone(),
                start_frame: 0,
                decoded_frames: None,
                glitches: 0,
                error: None,
                end_reason: None,
            })
            .collect();
        Self {
            gains_db,
            pool: DecoderPool::new(threads, rate),
            files,
            ctl,
            next: 0,
            next_start: 0,
            playing: Vec::new(),
            reports,
            by_voice: HashMap::new(),
            end_frame: 0,
            known_end: 0,
        }
    }

    /// Everything loaded and every voice ended.
    fn done(&self) -> bool {
        self.next >= self.files.len() && self.playing.is_empty()
    }

    /// The frame where the last file ends, once every file is loaded and its length is exact.
    fn planned_end(&self) -> Option<u64> {
        let all_known = self.next >= self.files.len()
            && self.playing.iter().all(|p| p.status.total().is_some());
        all_known.then_some(self.known_end.max(self.end_frame))
    }

    /// One pass of the control loop. Never blocks.
    fn tick(&mut self) {
        while let Some(ev) = self.ctl.poll_event() {
            match ev {
                MixerEvent::Ended {
                    voice,
                    gen,
                    frame,
                    reason,
                    ..
                } => {
                    if let Some(&i) = self.by_voice.get(&(voice, gen)) {
                        self.reports[i].end_reason = Some(reason);
                        self.end_frame = self.end_frame.max(frame);
                    }
                    self.playing
                        .retain(|p| !(p.id.slot == voice && p.id.gen == gen));
                }
                MixerEvent::Started { .. }
                | MixerEvent::Starved { .. }
                | MixerEvent::Late { .. } => {}
                MixerEvent::LoadRejected { voice, gen } => {
                    if let Some(&i) = self.by_voice.get(&(voice, gen)) {
                        self.reports[i].error = Some("the mixer rejected the load".into());
                    }
                }
            }
        }
        for p in &self.playing {
            let r = &mut self.reports[p.file];
            r.glitches = p.status.glitches.load(Relaxed);
            if r.error.is_none() {
                r.error = p.status.error_message();
            }
            if let Some(t) = p.status.total() {
                r.decoded_frames = Some(t);
                self.known_end = self.known_end.max(p.start + t);
            }
        }
        // Open the next file once the previous one's length is exact.
        while self.next < self.files.len() && self.playing.len() < MAX_LOADED {
            if let Some(last) = self.playing.last() {
                if last.status.total().is_none() {
                    break;
                }
            }
            let idx = self.next;
            let start = match self.playing.last() {
                Some(l) => l.start + l.status.total().unwrap_or(0),
                None => self.next_start,
            };
            let feed = self.pool.open(VoiceSource::File {
                path: self.files[idx].clone(),
                start_s: 0.0,
            });
            let status = feed.status.clone();
            let params = LoadParams {
                declick_in: false,
                gain: 10f32.powf(self.gains_db.get(idx).copied().unwrap_or(0.0) as f32 / 20.0),
                ..LoadParams::at(start)
            };
            match self.ctl.load(feed, params) {
                Ok(id) => {
                    self.by_voice.insert((id.slot, id.gen), idx);
                    self.reports[idx].start_frame = start;
                    self.playing.push(Playing {
                        id,
                        status,
                        file: idx,
                        start,
                    });
                    self.next += 1;
                }
                Err(_) => break,
            }
        }
    }

    fn finish_reports(&mut self, report: &mut PlayReport) {
        // A last look at the status of files that ended between ticks.
        self.tick();
        report.files = self.reports.clone();
        report.end_frame = self.end_frame.max(self.known_end);
        let m = self.ctl.stats();
        report.starved_frames = m.starved_frames;
        report.starve_episodes = m.starve_episodes;
        report.late_starts = m.late_starts;
        report.limiter_active_frames = m.limiter_active_frames;
        report.limiter_over_1db_frames = m.limiter_over_1db_frames;
    }
}

/// Wait (bounded) until the first voice's ring holds `seconds` of audio or its decoder is done.
fn prebuffer(seq: &Seq, rate: u32, seconds: f64) {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(20) {
        let ready = seq.playing.first().is_none_or(|p| {
            p.status.eof.load(Acquire)
                || p.status.pushed_frames.load(Acquire) as f64 >= seconds * f64::from(rate)
        });
        if ready {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Play `files` through the real rt-out thread on `out`. Returns when the last file has been
/// heard (or `stop` was set), with the output handed back for `finish`.
pub fn play_realtime(
    files: Vec<PathBuf>,
    out: Box<dyn Output>,
    clock: Arc<dyn Clock>,
    opts: &PlayOptions,
    stop: Option<&AtomicBool>,
) -> Result<(PlayReport, Box<dyn Output>), String> {
    let rate = out.granted().rate;
    let (mixer, ctl) = Mixer::new(rate);
    let mut seq = Seq::new(files, Vec::new(), rate, opts.decoder_threads, ctl);
    seq.tick();
    prebuffer(&seq, rate, opts.prebuffer_s);
    let rt_opts = RtOptions {
        realtime_priority: opts.rt_priority,
        ..RtOptions::default()
    };
    let mut h = RtHandle::start(out, Box::new(mixer), clock.clone(), rt_opts)
        .map_err(|e| format!("cannot start rt-out: {e}"))?;
    let mut report = PlayReport {
        rate,
        ..PlayReport::default()
    };
    loop {
        if stop.is_some_and(|s| s.load(Relaxed)) {
            break;
        }
        while h.poll_event().is_some() {}
        seq.tick();
        if seq.done() {
            // Wait until the last frame has left the device.
            let t0 = Instant::now();
            let end = seq.end_frame.max(seq.known_end) as i64;
            while t0.elapsed() < Duration::from_secs(5) {
                let s = h.stats();
                if s.audible_frames_at(clock.monotonic_ns(), rate) >= end {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    report.xruns = h.stats().xruns;
    seq.finish_reports(&mut report);
    let out = h.stop();
    Ok((report, out))
}

/// Play `files` by rendering the mixer directly into `out`, as fast as decoding allows. The
/// output receives exactly `end_frame` frames.
pub fn render_offline(
    files: Vec<PathBuf>,
    out: &mut dyn Output,
    opts: &PlayOptions,
) -> Result<PlayReport, String> {
    render_offline_with(files, out, opts, |_, _| {})
}

/// As [`render_offline`], calling `on_block(first_frame, block)` with every block rendered.
pub fn render_offline_with(
    files: Vec<PathBuf>,
    out: &mut dyn Output,
    opts: &PlayOptions,
    mut on_block: impl FnMut(u64, &[f32]),
) -> Result<PlayReport, String> {
    render_offline_tapped(
        files,
        &PlayExtras::default(),
        out,
        opts,
        false,
        |t, b, _| on_block(t, b),
    )
}

/// The renderer behind the others, with per-file gains (`extras`) and, when `with_tap`, the mixer's meter
/// tap handed to `on_block(first_frame, block, tap)` after every block, so a test can run the loudness
/// controller (M6a) on the real taps exactly as the meter thread does, one block behind the audio.
pub fn render_offline_tapped(
    files: Vec<PathBuf>,
    extras: &PlayExtras,
    out: &mut dyn Output,
    opts: &PlayOptions,
    with_tap: bool,
    mut on_block: impl FnMut(u64, &[f32], Option<&mut crate::mixer::MeterTap>),
) -> Result<PlayReport, String> {
    use crate::mixer::MAX_BLOCK;
    use crate::source::Source;
    let rate = out.granted().rate;
    let (mut mixer, ctl) = Mixer::new(rate);
    let mut seq = Seq::new(
        files,
        extras.gains_db.clone(),
        rate,
        opts.decoder_threads,
        ctl,
    );
    let mut tap = if with_tap {
        seq.ctl.take_meter_tap()
    } else {
        None
    };
    if let Some((on, ceiling, gain)) = extras.loudness {
        let link = seq.ctl.loudness_link();
        link.enabled.store(on, Relaxed);
        link.set_ceiling_db(ceiling);
        link.set_gain_db(gain);
    }
    let lookahead = mixer.limiter_latency_frames() as u64;
    let mut block = vec![0.0f32; MAX_BLOCK * 2];
    let mut t: u64 = 0;
    let mut report = PlayReport {
        rate,
        ..PlayReport::default()
    };
    let mut idle_since = Instant::now();
    loop {
        seq.tick();
        let planned = seq.planned_end();
        if planned.is_some_and(|e| t >= e) {
            break;
        }
        // Wait for data: every voice that will sound in this block needs frames in its ring.
        let upto = t + MAX_BLOCK as u64 + lookahead;
        let starving = seq.playing.iter().any(|p| {
            p.start < upto && {
                let avail = p
                    .status
                    .pushed_frames
                    .load(Acquire)
                    .saturating_sub(p.status.consumed_frames.load(Acquire));
                !p.status.eof.load(Acquire) && avail < 2 * MAX_BLOCK as u64
            }
        });
        // Also wait for a file that is still opening, if it starts within this block.
        let opening = seq
            .playing
            .iter()
            .any(|p| p.start < upto && !p.status.opened.load(Acquire));
        if starving || opening {
            if idle_since.elapsed() > Duration::from_secs(60) {
                return Err("timed out waiting for the decoder".into());
            }
            std::thread::sleep(Duration::from_micros(200));
            continue;
        }
        idle_since = Instant::now();
        let n = match planned {
            Some(e) => ((e - t) as usize).min(MAX_BLOCK),
            None => MAX_BLOCK,
        };
        mixer.render(&mut block[..n * 2]);
        on_block(t, &block[..n * 2], tap.as_mut());
        out.write(&block[..n * 2])
            .map_err(|e| format!("output: {e}"))?;
        t += n as u64;
    }
    seq.finish_reports(&mut report);
    Ok(report)
}
