//! The `stream-enc` thread (architecture 1.2, 1.3a): pops periods from the stream tap ring, keeps the
//! stream's timeline exact, encodes FLAC, muxes Ogg pages and pushes them to the mount.
//!
//! **Timeline.** Each period arrives with the station frame it starts at. The stream's own timeline
//! (the Ogg granule position) starts at 0 with the first period after a (re)start and advances by
//! exactly the frames that went into it. A period that starts later than the previous one ended (rt-out
//! dropped periods because this thread was behind, or wrote silence after an underrun) leaves a gap, and
//! the gap is filled with the same number of frames of silence, with a 2 ms fade at each edge. The stream
//! is neither shortened nor repeated: a receiver that counts samples sees the right length.
//!
//! **Fade-out of the audio before a gap.** The last 2 ms of audio are held back (not yet encoded), so
//! that when a gap follows they can be faded out. The cost is 2 ms of latency.
//!
//! **A restart continues the stream.** When encoding fails (an error, or a panic caught at the thread's
//! loop), the pipeline drops what had not yet reached a page and carries on in the **same** logical Ogg
//! stream: same serial number, page sequence, header pages, granule positions and FLAC frame numbers. The
//! frames it dropped come back as a gap of silence, so the timeline stays exact, and the listeners stay
//! connected: to a client the restart is a short silence, not a new stream. Only a FLAC frame counter that
//! runs out (after about 1.4 years at 48 kHz) starts a new logical stream (new serial, timeline at 0); that
//! closes the listeners, which reconnect and get the new headers.

use crate::flac::{to_i24, FlacEncoder};
use crate::mount::{EncoderState, Mount};
use crate::ogg::{self, OggWriter};
use engine::tap::{TapConsumer, TapPeriod};
use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering::*};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct EncoderOptions {
    pub rate: u32,
    /// FLAC block size in frames (the station's period, 1,024 by default).
    pub block: usize,
    pub flac_level: u8,
    /// Written into the stream's comment header.
    pub station: String,
    pub vendor: String,
    /// The product's display name (`brand::display_name`), written as the stream's ARTIST.
    pub product: String,
    /// FLAC packets per Ogg page.
    pub packets_per_page: usize,
}

impl EncoderOptions {
    pub fn new(rate: u32, station: &str) -> Self {
        Self {
            rate,
            block: 1024,
            flac_level: 5,
            station: station.to_string(),
            vendor: "hotclock".into(),
            product: "hotclock".into(),
            packets_per_page: 4,
        }
    }
}

/// Fade length: 2 ms.
fn fade_frames(rate: u32) -> usize {
    (rate as usize * 2 / 1000).max(2)
}

/// The encoding pipeline without its thread, so tests can drive it directly.
pub struct Pipeline {
    opts: EncoderOptions,
    mount: Arc<Mount>,
    fade: usize,
    flac: FlacEncoder,
    ogg: OggWriter,
    /// Interleaved 24-bit samples not yet encoded.
    acc: Vec<i32>,
    next_frame: Option<u64>,
    fade_in: bool,
    /// Frames encoded into packets: the granule position at the end of the last packet.
    timeline: u64,
    /// The station frame at timeline 0 of this logical stream (known once a period has arrived). The
    /// timeline advances one for one with the station clock, gaps included.
    origin: Option<u64>,
    pkts: Vec<Vec<u8>>,
    pkt_segs: usize,
    page_start: u64,
    last_packet: Instant,
    generation: u32,
}

impl Pipeline {
    pub fn new(opts: EncoderOptions, mount: Arc<Mount>) -> Result<Self, String> {
        let flac = FlacEncoder::new(opts.rate, opts.block, opts.flac_level)?;
        let mut p = Self {
            fade: fade_frames(opts.rate),
            opts,
            mount,
            flac,
            ogg: OggWriter::new(1),
            acc: Vec::new(),
            next_frame: None,
            fade_in: true,
            timeline: 0,
            origin: None,
            pkts: Vec::new(),
            pkt_segs: 0,
            page_start: 0,
            last_packet: Instant::now(),
            generation: 0,
        };
        p.begin()?;
        Ok(p)
    }

    /// Start a new logical stream: new serial, new headers, empty timeline.
    fn begin(&mut self) -> Result<(), String> {
        self.generation += 1;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        self.ogg = OggWriter::new(nanos ^ self.generation.wrapping_mul(0x9E37_79B9));
        self.flac = FlacEncoder::new(self.opts.rate, self.opts.block, self.opts.flac_level)?;
        self.acc.clear();
        self.next_frame = None;
        self.fade_in = true;
        self.timeline = 0;
        self.origin = None;
        self.page_start = 0;
        self.pkts.clear();
        self.pkt_segs = 0;
        let info = ogg::streaminfo(self.opts.rate, self.opts.block as u16, 2, 24);
        let comments = vec![
            format!("TITLE={}", self.opts.station),
            format!("ARTIST={}", self.opts.product),
            format!("ENCODER={} FLAC", self.opts.vendor),
        ];
        let first = ogg::first_packet(&info, 1);
        let mut headers = self.ogg.page(ogg::FLAG_BOS, 0, &[&first]);
        let comment = ogg::comment_packet(&self.opts.vendor, &comments);
        headers.extend_from_slice(&self.ogg.page(0, 0, &[&comment]));
        self.mount.reset(headers);
        Ok(())
    }

    /// Start over after a failure or on request. Whatever had not reached a page is dropped and the same
    /// logical stream continues (see the module notes); the dropped frames become a gap of silence when the
    /// next period arrives. A FLAC frame counter that has run out starts a new logical stream instead.
    pub fn restart(&mut self) -> Result<(), String> {
        self.mount.stats.encoder_restarts.fetch_add(1, Relaxed);
        if self.flac.exhausted() {
            return self.begin();
        }
        self.flac = FlacEncoder::new(self.opts.rate, self.opts.block, self.opts.flac_level)?;
        // Every packet is one block, so the frame number of the next packet follows from the timeline.
        self.flac
            .set_frame_number((self.page_start / self.opts.block as u64) as usize);
        self.acc.clear();
        self.pkts.clear();
        self.pkt_segs = 0;
        self.timeline = self.page_start;
        self.next_frame = self.origin.map(|o| o + self.page_start);
        self.fade_in = true;
        Ok(())
    }

    /// Frames waiting to be encoded (the 2 ms hold-back, normally).
    pub fn staged_frames(&self) -> usize {
        self.acc.len() / 2
    }

    pub fn timeline_frames(&self) -> u64 {
        self.timeline
    }

    /// Feed one period.
    pub fn feed(&mut self, period: TapPeriod, samples: &[f32]) -> Result<(), String> {
        let mut start = period.start_frame;
        let mut samples = &samples[..period.frames * 2];
        match self.next_frame {
            None => {
                self.origin = Some(start.saturating_sub(self.timeline));
            }
            Some(n) if start > n => self.fill_gap(start - n)?,
            Some(n) if start < n => {
                // Overlap (cannot happen from rt-out): drop the part already encoded.
                let skip = (n - start) as usize;
                if skip >= period.frames {
                    return Ok(());
                }
                samples = &samples[skip * 2..];
                start = n;
            }
            Some(_) => {}
        }
        let frames = samples.len() / 2;
        let base = self.acc.len();
        self.acc.extend(samples.iter().map(|s| to_i24(*s)));
        if self.fade_in {
            self.fade_in = false;
            let n = self.fade.min(frames);
            for i in 0..n {
                let g = (i + 1) as f32 / (n + 1) as f32;
                for c in 0..2 {
                    let k = base + i * 2 + c;
                    self.acc[k] = (self.acc[k] as f32 * g).round() as i32;
                }
            }
        }
        self.next_frame = Some(start + frames as u64);
        self.encode_ready()
    }

    /// Silence for `gap` frames, with a fade out of what was staged before it.
    fn fill_gap(&mut self, gap: u64) -> Result<(), String> {
        self.mount.stats.gaps.fetch_add(1, Relaxed);
        self.mount.stats.gap_frames.fetch_add(gap, Relaxed);
        let staged = self.acc.len() / 2;
        let n = self.fade.min(staged);
        for i in 0..n {
            let g = (n - i) as f32 / (n + 1) as f32;
            for c in 0..2 {
                let k = (staged - n + i) * 2 + c;
                self.acc[k] = (self.acc[k] as f32 * g).round() as i32;
            }
        }
        let mut left = gap;
        while left > 0 {
            let chunk = left.min(16_384) as usize;
            self.acc.resize(self.acc.len() + chunk * 2, 0);
            left -= chunk as u64;
            self.encode_ready()?;
        }
        self.fade_in = true;
        Ok(())
    }

    /// Encode every full block, keeping the fade hold-back staged.
    fn encode_ready(&mut self) -> Result<(), String> {
        let block = self.opts.block;
        let mut done = 0;
        while (self.acc.len() - done * block * 2) / 2 >= block + self.fade {
            let from = done * block * 2;
            let t0 = Instant::now();
            let pkt = self.flac.encode_block(&self.acc[from..from + block * 2])?;
            let ns = t0.elapsed().as_nanos() as u64;
            self.mount.stats.encode_ns.fetch_add(ns, Relaxed);
            self.mount.stats.max_encode_ns.fetch_max(ns, Relaxed);
            done += 1;
            self.timeline += block as u64;
            self.mount
                .stats
                .frames_encoded
                .fetch_add(block as u64, Relaxed);
            self.pkt_segs += ogg::segments_for(pkt.len());
            self.pkts.push(pkt);
            self.last_packet = Instant::now();
            if self.mount.take_panic_request() {
                // The test hook: a real panic with a packet staged and the page half built.
                panic!("stream encoder: panic induced by the test hook");
            }
            if self.pkts.len() >= self.opts.packets_per_page || self.pkt_segs > 200 {
                self.emit_page();
            }
        }
        if done > 0 {
            self.acc.drain(..done * block * 2);
        }
        if self.flac.exhausted() {
            return Err("FLAC frame counter exhausted".into());
        }
        Ok(())
    }

    fn emit_page(&mut self) {
        self.emit_page_flagged(0);
    }

    fn emit_page_flagged(&mut self, flags: u8) {
        if self.pkts.is_empty() {
            return;
        }
        let refs: Vec<&[u8]> = self.pkts.iter().map(Vec::as_slice).collect();
        let page = self.ogg.page(flags, self.timeline as i64, &refs);
        self.mount.push_page(page, self.page_start, self.timeline);
        self.page_start = self.timeline;
        self.pkts.clear();
        self.pkt_segs = 0;
    }

    /// Send a partly filled page that has waited `after` (called when no period is waiting).
    pub fn flush_idle(&mut self, after: Duration) {
        if !self.pkts.is_empty() && self.last_packet.elapsed() >= after {
            self.emit_page();
        }
    }

    /// Fade out and pad the staged audio to whole blocks, encode it and send the last page, marked as the
    /// end of the stream (at stop).
    pub fn finish(&mut self) -> Result<(), String> {
        let staged = self.acc.len() / 2;
        if staged == 0 && self.pkts.is_empty() {
            return Ok(());
        }
        let n = self.fade.min(staged);
        for i in 0..n {
            let g = (n - i) as f32 / (n + 1) as f32;
            for c in 0..2 {
                let k = (staged - n + i) * 2 + c;
                self.acc[k] = (self.acc[k] as f32 * g).round() as i32;
            }
        }
        // Whole blocks covering every staged frame, plus the hold-back `encode_ready` keeps (silence).
        let block = self.opts.block;
        let pad = staged.div_ceil(block) * block + self.fade - staged;
        self.acc.resize(self.acc.len() + pad * 2, 0);
        self.encode_ready()?;
        if self.pkts.is_empty() {
            // The audio ended on a page boundary: the end of the stream is an empty page (no packet ends on
            // it, so its granule position is -1).
            let page = self.ogg.page(ogg::FLAG_EOS, -1, &[]);
            self.mount.push_page(page, self.timeline, self.timeline);
        } else {
            self.emit_page_flagged(ogg::FLAG_EOS);
        }
        Ok(())
    }
}

/// Restarts within [`STORM_WINDOW`] that make the encoder `failed` rather than `restarting`.
const STORM_RESTARTS: usize = 5;
const STORM_WINDOW: Duration = Duration::from_secs(30);

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown".into())
}

pub struct EncoderThread {
    join: Option<std::thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

impl EncoderThread {
    /// Stop after flushing, and wait.
    pub fn stop(mut self) {
        self.stop.store(true, Release);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }

    pub fn is_finished(&self) -> bool {
        self.join.as_ref().is_none_or(|j| j.is_finished())
    }
}

impl Drop for EncoderThread {
    fn drop(&mut self) {
        self.stop.store(true, Release);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Start the encoder thread.
pub fn spawn(
    opts: EncoderOptions,
    mut tap: TapConsumer,
    mount: Arc<Mount>,
) -> std::io::Result<EncoderThread> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let rate = u64::from(opts.rate);
    mount.attach_tap(tap.stats().clone());
    let join = std::thread::Builder::new()
        .name("stream-enc".into())
        .spawn(move || {
            let mut pipe = match Pipeline::new(opts.clone(), mount.clone()) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!("stream encoder cannot start: {e}");
                    mount.set_encoder_state(EncoderState::Failed);
                    return;
                }
            };
            mount.set_encoder_state(EncoderState::Running);
            let mut buf: Vec<f32> = Vec::with_capacity(opts.block * 4);
            let mut recent: VecDeque<Instant> = VecDeque::new();
            while !stop2.load(Acquire) {
                let queued = tap.queued_frames() as u64;
                let lag_ms = queued * 1000 / rate;
                mount.stats.encoder_lag_ms.store(lag_ms, Relaxed);
                mount.stats.encoder_lag_max_ms.fetch_max(lag_ms, Relaxed);
                if mount.stalled() {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                // Everything that touches the pipeline runs under `catch_unwind`: a panic anywhere in it is
                // a failure like an error, and the thread carries on rather than dying with the state at
                // `running`.
                let step = catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
                    if mount.take_restart_request() {
                        return Err("restart requested".into());
                    }
                    match tap.pop_into(&mut buf) {
                        Some(p) => pipe.feed(p, &buf),
                        None => {
                            pipe.flush_idle(Duration::from_millis(30));
                            std::thread::sleep(Duration::from_millis(3));
                            Ok(())
                        }
                    }
                }));
                let why = match step {
                    Ok(Ok(())) => {
                        if mount.encoder_state() == EncoderState::Failed
                            && recent.back().is_none_or(|t| t.elapsed() >= STORM_WINDOW)
                        {
                            mount.set_encoder_state(EncoderState::Running);
                        }
                        continue;
                    }
                    Ok(Err(e)) => e,
                    Err(p) => format!("panic: {}", panic_text(&p)),
                };
                tracing::warn!("stream encoder failed ({why}); restarting");
                mount.set_encoder_state(EncoderState::Restarting);
                let restarted = catch_unwind(AssertUnwindSafe(|| pipe.restart()))
                    .unwrap_or_else(|p| Err(format!("panic: {}", panic_text(&p))));
                if let Err(e) = restarted {
                    tracing::error!("stream encoder cannot restart: {e}");
                    mount.set_encoder_state(EncoderState::Failed);
                    return;
                }
                let now = Instant::now();
                recent.push_back(now);
                while recent
                    .front()
                    .is_some_and(|t| now.duration_since(*t) > STORM_WINDOW)
                {
                    recent.pop_front();
                }
                if recent.len() >= STORM_RESTARTS {
                    // Failing over and over: report it, and do not spin.
                    tracing::error!(
                        "stream encoder restarted {} times in {} s",
                        recent.len(),
                        STORM_WINDOW.as_secs()
                    );
                    mount.set_encoder_state(EncoderState::Failed);
                    std::thread::sleep(Duration::from_secs(1));
                } else {
                    mount.set_encoder_state(EncoderState::Running);
                }
            }
            // Stopping: encode what rt-out had already pushed (at shutdown rt-out has stopped first, so this is
            // the end of the program), bounded by what is queued now, then close the stream.
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let mut left = tap.queued_frames();
                while left > 0 {
                    let Some(p) = tap.pop_into(&mut buf) else {
                        break;
                    };
                    left = left.saturating_sub(p.frames);
                    if pipe.feed(p, &buf).is_err() {
                        break;
                    }
                }
                pipe.finish()
            }));
            mount.stats.encoder_lag_ms.store(0, Relaxed);
            mount.set_encoder_state(EncoderState::Stopped);
        })?;
    Ok(EncoderThread {
        join: Some(join),
        stop,
    })
}
