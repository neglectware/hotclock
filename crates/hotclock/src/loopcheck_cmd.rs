//! `hotclock loopcheck` (M1.8): record one card's output through another card's input and report the
//! gaps and glitches (`loopcheck.rs` holds the rules). Three ways to use it:
//!
//! * beside a running station: `hotclock loopcheck --input hw:CARD=PCH,DEV=0 --report loop.jsonl`, with a
//!   cable from the station's card to this card's line input. It listens and reports every stretch quieter
//!   than -60 dBFS (10 ms windows) that lasts 20 ms or more.
//! * with its own tone: `--tone-out hw:CARD=Device,DEV=0 --tone-hz 1000` plays a steady tone on the other
//!   card and, besides gaps, reports any phase slip, level step or dropped sample, and the two cards' clock
//!   offset in ppm.
//! * on a recording: `--wav FILE` analyses a file made some other way.
//!
//! The report is JSON lines: one `status` line per minute, one line per event, and a final `summary`.
//! Every gap also saves the last three seconds as a WAV clip (at most 50). The exit code is 0 when the run
//! had no gap, no glitch, no capture overrun and no device loss, and 1 otherwise.

use crate::loopcheck::{Analyzer, Config, Event};
use clap::Args;
use common::{Clock, RealClock};
use engine::capture::{Capture, ReadOutcome};
use engine::rt::{RtHandle, RtOptions};
use engine::wav::{WavFormat, WavWriter};
use engine::{open_output, HwRequest, OutputSpec, ToneSource};
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Args)]
pub struct LoopcheckArgs {
    /// The capture device, `hw:CARD=<id>,DEV=0`.
    #[arg(long)]
    input: Option<String>,
    /// Analyse this WAV file instead of a card.
    #[arg(long, conflicts_with = "input")]
    wav: Option<PathBuf>,
    /// Play a steady tone on this output device while recording (`hw:CARD=...`, `null` for a dry run).
    #[arg(long, requires = "input")]
    tone_out: Option<String>,
    #[arg(long, default_value_t = 1000.0)]
    tone_hz: f64,
    /// Tone level, dBFS RMS.
    #[arg(long, default_value_t = -20.0, allow_negative_numbers = true)]
    tone_level: f64,
    /// Stop after this many seconds of recording (default: until Ctrl-C or SIGTERM).
    #[arg(long)]
    seconds: Option<f64>,
    /// A window quieter than this (dBFS RMS) is silence.
    #[arg(long, default_value_t = -60.0, allow_negative_numbers = true)]
    threshold_db: f64,
    /// The shortest silence reported, ms.
    #[arg(long, default_value_t = 20)]
    min_gap_ms: u32,
    /// JSON-lines report file (appended to).
    #[arg(long)]
    report: Option<PathBuf>,
    /// Where the clips of the last 3 s go on each gap (default: none).
    #[arg(long)]
    clips_dir: Option<PathBuf>,
    /// Seconds between status lines.
    #[arg(long, default_value_t = 60)]
    status_s: u64,
    #[arg(long, default_value_t = 48_000)]
    rate: u32,
    #[arg(long, default_value_t = 1024)]
    period_frames: u32,
    #[arg(long, default_value_t = 8)]
    periods: u32,
}

const MAX_CLIPS: usize = 50;
const CLIP_S: usize = 3;

fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

struct Report {
    file: Option<std::fs::File>,
}

impl Report {
    fn line(&mut self, v: &Value) {
        let s = v.to_string();
        eprintln!("loopcheck: {s}");
        if let Some(f) = &mut self.file {
            let _ = writeln!(f, "{s}");
        }
    }
}

#[derive(Default)]
struct Faults {
    overruns: u64,
    device_lost: u64,
}

pub fn run(a: LoopcheckArgs) -> ExitCode {
    if a.input.is_none() && a.wav.is_none() {
        eprintln!("usage: hotclock loopcheck --input hw:CARD=...,DEV=0 [--tone-out hw:CARD=...,DEV=0] [--seconds N]");
        eprintln!("       hotclock loopcheck --wav recording.wav");
        return ExitCode::from(2);
    }
    let file = match &a.report {
        Some(p) => match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
        {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("loopcheck: {}: {e}", p.display());
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };
    let mut rep = Report { file };
    if let Some(d) = &a.clips_dir {
        if let Err(e) = std::fs::create_dir_all(d) {
            eprintln!("loopcheck: {}: {e}", d.display());
            return ExitCode::FAILURE;
        }
    }
    if let Some(w) = &a.wav {
        return run_wav(&a, w, &mut rep);
    }
    run_card(&a, &mut rep)
}

fn config(a: &LoopcheckArgs, rate: u32) -> Config {
    let mut c = Config::new(rate);
    c.threshold_db = a.threshold_db;
    c.min_gap_ms = a.min_gap_ms;
    c.tone_hz = a.tone_out.as_ref().map(|_| a.tone_hz);
    c
}

fn run_wav(a: &LoopcheckArgs, path: &std::path::Path, rep: &mut Report) -> ExitCode {
    let wav = match engine::wav::read_wav(path) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("loopcheck: {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let stereo: Vec<f32> = match wav.channels {
        1 => wav.samples.iter().flat_map(|&x| [x, x]).collect(),
        2 => wav.samples.clone(),
        n => wav
            .samples
            .chunks_exact(n as usize)
            .flat_map(|f| [f[0], f[1]])
            .collect(),
    };
    let mut an = Analyzer::new(config(a, wav.rate));
    let mut ring = Ring::new(wav.rate);
    let start = wall_ms();
    for chunk in stereo.chunks(2048) {
        an.feed(chunk);
        ring.push(chunk);
        let now = start + (an.totals.frames * 1000 / u64::from(wav.rate)) as i64;
        emit_events(&mut an, &ring, a, wav.rate, now, rep);
    }
    an.finish();
    let now = start + (an.totals.frames * 1000 / u64::from(wav.rate)) as i64;
    emit_events(&mut an, &ring, a, wav.rate, now, rep);
    summary(rep, &an, &Faults::default(), wav.rate)
}

/// The last three seconds, for the clip a gap saves.
struct Ring {
    buf: Vec<f32>,
    cap: usize,
    pos: usize,
    filled: bool,
}

impl Ring {
    fn new(rate: u32) -> Self {
        let cap = rate as usize * CLIP_S * 2;
        Self {
            buf: vec![0.0; cap],
            cap,
            pos: 0,
            filled: false,
        }
    }
    fn push(&mut self, s: &[f32]) {
        for &x in s {
            self.buf[self.pos] = x;
            self.pos += 1;
            if self.pos == self.cap {
                self.pos = 0;
                self.filled = true;
            }
        }
    }
    fn snapshot(&self) -> Vec<f32> {
        if self.filled {
            let mut v = self.buf[self.pos..].to_vec();
            v.extend_from_slice(&self.buf[..self.pos]);
            v
        } else {
            self.buf[..self.pos].to_vec()
        }
    }
}

fn emit_events(
    an: &mut Analyzer,
    ring: &Ring,
    a: &LoopcheckArgs,
    rate: u32,
    now_wall: i64,
    rep: &mut Report,
) {
    let now_frame = an.totals.frames;
    for ev in an.take_events() {
        let frame = match &ev {
            Event::Gap { start_frame, .. } => *start_frame,
            Event::ToneGlitch { frame, .. } => *frame,
        };
        // Wall time of the event: now, minus the frames since it (a recording read from a file has no
        // wall clock of its own, so its times count from the start of the file).
        let behind_ms = (now_frame.saturating_sub(frame) * 1000 / u64::from(rate)) as i64;
        let at_ms = now_wall - behind_ms;
        let rel_s = (frame as f64) / f64::from(rate);
        match ev {
            Event::Gap {
                ms,
                level_before_db,
                level_after_db,
                level_db,
                abrupt,
                ..
            } => {
                let clip = save_clip(a, ring, at_ms, rate);
                rep.line(&json!({
                    "type": "gap", "at_ms": at_ms, "t_s": (rel_s * 1000.0).round() / 1000.0, "ms": ms,
                    "level_before_db": (level_before_db * 10.0).round() / 10.0,
                    "level_after_db": (level_after_db * 10.0).round() / 10.0,
                    "level_db": (level_db * 10.0).round() / 10.0,
                    "abrupt": abrupt, "clip": clip,
                }));
            }
            Event::ToneGlitch {
                phase_err_rad,
                amp_err_db,
                ..
            } => rep.line(&json!({
                "type": "tone_glitch", "at_ms": at_ms, "t_s": (rel_s * 1000.0).round() / 1000.0,
                "phase_err_rad": (phase_err_rad * 1000.0).round() / 1000.0,
                "amp_err_db": (amp_err_db * 100.0).round() / 100.0,
            })),
        }
    }
}

fn save_clip(a: &LoopcheckArgs, ring: &Ring, at_ms: i64, rate: u32) -> Option<String> {
    let dir = a.clips_dir.as_ref()?;
    let n = std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("gap-"))
                .count()
        })
        .unwrap_or(0);
    if n >= MAX_CLIPS {
        return None;
    }
    let path = dir.join(format!("gap-{at_ms}.wav"));
    let mut w = WavWriter::create(&path, rate, 2, WavFormat::F32).ok()?;
    let snap = ring.snapshot();
    let bytes: Vec<u8> = snap.iter().flat_map(|x| x.to_le_bytes()).collect();
    w.write_bytes(&bytes).ok()?;
    w.finish().ok()?;
    Some(path.to_string_lossy().into_owned())
}

fn summary(rep: &mut Report, an: &Analyzer, f: &Faults, rate: u32) -> ExitCode {
    let t = &an.totals;
    rep.line(&json!({
        "type": "summary", "seconds": (t.frames as f64 / f64::from(rate) * 10.0).round() / 10.0,
        "gaps": t.gaps, "gap_ms_total": t.gap_ms_total, "longest_gap_ms": t.longest_gap_ms,
        "tone_glitches": t.glitches, "capture_overruns": f.overruns, "device_lost": f.device_lost,
        "peak_db": (t.peak_db * 10.0).round() / 10.0,
        "clock_offset_ppm": t.tone_ppm.map(|p| (p * 10.0).round() / 10.0),
    }));
    if t.gaps == 0 && t.glitches == 0 && f.overruns == 0 && f.device_lost == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run_card(a: &LoopcheckArgs, rep: &mut Report) -> ExitCode {
    let Some(input) = a.input.clone() else {
        return ExitCode::from(2);
    };
    let req = HwRequest {
        rate: a.rate,
        period_frames: a.period_frames,
        periods: a.periods,
        ..HwRequest::defaults()
    };
    let mut cap = match Capture::open(&input, &req) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("loopcheck: {e}");
            return ExitCode::FAILURE;
        }
    };
    let granted = cap.granted().clone();
    if granted.channels != 2 {
        eprintln!(
            "loopcheck: the input granted {} channels; 2 needed",
            granted.channels
        );
        return ExitCode::FAILURE;
    }
    let rate = granted.rate;
    rep.line(
        &json!({"type": "start", "at_ms": wall_ms(), "input": input, "granted": granted.describe(),
        "tone_out": a.tone_out, "tone_hz": a.tone_out.as_ref().map(|_| a.tone_hz),
        "threshold_db": a.threshold_db, "min_gap_ms": a.min_gap_ms}),
    );
    // The tone side.
    let mut tone_handle: Option<RtHandle> = None;
    if let Some(dev) = &a.tone_out {
        let spec: OutputSpec = match dev.parse() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("loopcheck: {e}");
                return ExitCode::FAILURE;
            }
        };
        let clock: Arc<dyn Clock> = Arc::new(RealClock::new());
        let out = match open_output(
            &spec,
            &HwRequest {
                rate,
                ..req.clone()
            },
            clock.clone(),
        ) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("loopcheck: {e}");
                return ExitCode::FAILURE;
            }
        };
        let g = out.granted().clone();
        let src = ToneSource::new(g.rate, a.tone_hz, a.tone_level);
        match RtHandle::start(out, Box::new(src), clock, RtOptions::default()) {
            Ok(h) => tone_handle = Some(h),
            Err(e) => {
                eprintln!("loopcheck: cannot start the tone: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let _ = ctrlc::set_handler(move || s2.store(true, Ordering::SeqCst));

    let mut an = Analyzer::new(config(a, rate));
    let mut ring = Ring::new(rate);
    let mut faults = Faults::default();
    let mut buf: Vec<f32> = Vec::with_capacity(a.period_frames as usize * 4);
    let t0 = Instant::now();
    let mut last_status = Instant::now();
    // With `--seconds`, the run also has a wall-clock limit: frames stop counting while the card is
    // gone, so the frame count alone would never end a run that lost its device.
    let deadline = a
        .seconds
        .map(|s| t0 + Duration::from_secs_f64(s) + DEADLINE_GRACE);
    let mut timed_out = false;
    // Wall time of the end of the latest read, for stamping events.
    while !stop.load(Ordering::SeqCst) {
        if a.seconds
            .is_some_and(|s| an.totals.frames as f64 / f64::from(rate) >= s)
        {
            break;
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            timed_out = true;
            break;
        }
        buf.clear();
        match cap.read(&mut buf) {
            ReadOutcome::Frames(_) => {
                an.feed(&buf);
                ring.push(&buf);
                emit_events(&mut an, &ring, a, rate, wall_ms(), rep);
            }
            ReadOutcome::Overrun => {
                faults.overruns += 1;
                rep.line(
                    &json!({"type": "capture_overrun", "at_ms": wall_ms(), "n": faults.overruns}),
                );
            }
            ReadOutcome::Lost(m) => {
                // The input card went away (unplugged, or a fault injected on purpose): note it, and try again
                // every 2 s until it is back. The recording has a hole for the outage, which the report ties
                // to the fault that caused it.
                faults.device_lost += 1;
                rep.line(&json!({"type": "device_lost", "at_ms": wall_ms(), "message": m}));
                let lost_at = Instant::now();
                match reacquire(
                    || Capture::open(&input, &req).ok(),
                    &stop,
                    deadline,
                    REOPEN_EVERY,
                ) {
                    Reacquire::Back(c) => {
                        cap = c;
                        rep.line(&json!({"type": "device_restored", "at_ms": wall_ms(),
                            "outage_s": (lost_at.elapsed().as_secs_f64() * 10.0).round() / 10.0}));
                        // The stream restarts: what came before is not continuous with what follows.
                        an.reset_continuity();
                    }
                    Reacquire::Stopped => break,
                    Reacquire::TimedOut => {
                        timed_out = true;
                        break;
                    }
                }
            }
        }
        if last_status.elapsed() >= Duration::from_secs(a.status_s) {
            last_status = Instant::now();
            let peak = 20.0 * f64::from(an.take_peak().max(1e-6)).log10();
            let t = &an.totals;
            rep.line(&json!({
                "type": "status", "at_ms": wall_ms(), "run_s": t0.elapsed().as_secs(),
                "frames": t.frames, "gaps": t.gaps, "longest_gap_ms": t.longest_gap_ms,
                "tone_glitches": t.glitches, "capture_overruns": faults.overruns,
                "level_db": (an.last_level_db * 10.0).round() / 10.0,
                "peak_db": (peak * 10.0).round() / 10.0,
                "clock_offset_ppm": t.tone_ppm.map(|p| (p * 10.0).round() / 10.0),
            }));
        }
    }
    an.finish();
    emit_events(&mut an, &ring, a, rate, wall_ms(), rep);
    if let Some(h) = tone_handle.take() {
        let mut out = h.stop();
        let _ = out.finish();
    }
    let code = summary(rep, &an, &faults, rate);
    if timed_out {
        eprintln!(
            "loopcheck: timed out: the input `{input}` did not deliver {} s of audio within {} s of wall time \
             (device lost and not back); exit code {EXIT_TIMEOUT}",
            a.seconds.unwrap_or(0.0),
            a.seconds.unwrap_or(0.0) + DEADLINE_GRACE.as_secs_f64(),
        );
        return ExitCode::from(EXIT_TIMEOUT);
    }
    code
}

/// Exit code of a `--seconds` run that ran out of wall time (the device was lost and did not come back).
pub const EXIT_TIMEOUT: u8 = 3;
/// Wall time allowed beyond `--seconds` before a run gives up.
const DEADLINE_GRACE: Duration = Duration::from_secs(5);
const REOPEN_EVERY: Duration = Duration::from_secs(2);

enum Reacquire<T> {
    Back(T),
    Stopped,
    TimedOut,
}

/// Try `open` every `every` until it gives a device, `stop` is set, or `deadline` passes.
fn reacquire<T>(
    mut open: impl FnMut() -> Option<T>,
    stop: &AtomicBool,
    deadline: Option<Instant>,
    every: Duration,
) -> Reacquire<T> {
    loop {
        if stop.load(Ordering::SeqCst) {
            return Reacquire::Stopped;
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Reacquire::TimedOut;
        }
        std::thread::sleep(
            every
                .min(Duration::from_millis(200))
                .max(Duration::from_millis(1)),
        );
        if let Some(t) = open() {
            return Reacquire::Back(t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lost_device_that_never_returns_times_out() {
        let stop = AtomicBool::new(false);
        let t = Instant::now();
        let r = reacquire(
            || None::<u8>,
            &stop,
            Some(t + Duration::from_millis(300)),
            Duration::from_millis(10),
        );
        assert!(matches!(r, Reacquire::TimedOut));
        assert!(t.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_device_that_returns_is_reacquired_and_stop_wins() {
        let stop = AtomicBool::new(false);
        let mut n = 0;
        let r = reacquire(
            || {
                n += 1;
                (n == 3).then_some(n)
            },
            &stop,
            None,
            Duration::from_millis(1),
        );
        assert!(matches!(r, Reacquire::Back(3)));
        stop.store(true, Ordering::SeqCst);
        let r = reacquire(|| None::<u8>, &stop, None, Duration::from_millis(1));
        assert!(matches!(r, Reacquire::Stopped));
    }
}
