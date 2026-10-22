//! `hotclock play`: the M1.2 test harness. Plays a list of files one after another (gapless, no
//! segues) through any output backend: `null`, `file:PATH.wav` (rendered offline, as fast as the
//! decoders allow), `file-paced:PATH.wav`, or an ALSA device.

use analysis::census;
use clap::Args;
use common::{Clock, RealClock};
use engine::output::FileOutput;
use engine::player::{play_realtime, render_offline, PlayOptions, PlayReport};
use engine::{open_output, HwRequest, OutputSpec};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Args)]
pub struct PlayArgs {
    /// Files, directories (searched recursively) or `.m3u`/`.m3u8` lists, played in order.
    /// Do not mix the three test sets in one run.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
    /// Output: an ALSA device such as `hw:CARD=PCH,DEV=0`, `null` (paced), `file:PATH.wav`
    /// (offline, as fast as possible) or `file-paced:PATH.wav`.
    #[arg(long, default_value = "null")]
    device: String,
    /// Device rate; every file is resampled to it.
    #[arg(long, default_value_t = 48_000)]
    rate: u32,
    /// Sample format of a `file:` WAV: f32, s32, s24 or s16.
    #[arg(long, default_value = "f32")]
    format: String,
    /// SCHED_FIFO priority for rt-out (Linux); 0 disables.
    #[arg(long, default_value_t = 20)]
    rt_priority: u8,
}

pub fn run(a: PlayArgs) -> ExitCode {
    let mut files = vec![];
    for i in &a.inputs {
        match census::collect_files(i) {
            Ok(f) => files.extend(f),
            Err(e) => {
                eprintln!("play: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    if files.is_empty() {
        eprintln!("play: no audio files");
        return ExitCode::FAILURE;
    }
    let spec: OutputSpec = match a.device.parse() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(fmt) = engine::render::parse_wav_format(&a.format) else {
        eprintln!("--format must be f32, s32, s24 or s16, got `{}`", a.format);
        return ExitCode::from(2);
    };
    let req = HwRequest {
        rate: a.rate,
        ..HwRequest::defaults()
    };
    let opts = PlayOptions {
        rt_priority: (a.rt_priority > 0).then_some(a.rt_priority),
        ..PlayOptions::default()
    };
    let clock: Arc<dyn Clock> = Arc::new(RealClock::new());
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let _ = ctrlc::set_handler(move || s2.store(true, Ordering::SeqCst));
    let n = files.len();

    let result: Result<PlayReport, String> = match &spec {
        OutputSpec::File { path, paced: false } => {
            let mut out = match FileOutput::create(path, &req, fmt, None) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
            };
            let r = render_offline(files, &mut out, &opts);
            use engine::Output;
            if let Err(e) = out.finish() {
                eprintln!("play: cannot finish {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
            r
        }
        _ => {
            let out = match open_output(&spec, &req, clock.clone()) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
            };
            tracing::info!("device `{}` opened: {}", a.device, out.granted().describe());
            if let Err(e) = engine::sys::lock_memory() {
                tracing::debug!("mlockall: {e}");
            }
            play_realtime(files, out, clock, &opts, Some(&stop)).map(|(r, mut out)| {
                let _ = out.finish();
                r
            })
        }
    };
    let report = match result {
        Ok(r) => r,
        Err(e) => {
            eprintln!("play failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut bad = 0;
    for f in &report.files {
        let secs = f.decoded_frames.map(|n| n as f64 / f64::from(report.rate));
        println!(
            "{:>10} {:>9}  glitches {:>2}  {:?}  {}{}",
            f.start_frame,
            secs.map_or("?".into(), |s| format!("{s:.2} s")),
            f.glitches,
            f.end_reason,
            f.path.display(),
            f.error
                .as_ref()
                .map(|e| format!("  ERROR: {e}"))
                .unwrap_or_default()
        );
        if f.error.is_some() {
            bad += 1;
        }
    }
    println!(
        "played {n} files: {} frames ({:.2} s at {} Hz); xruns {}; starved {} frames; late starts {}; \
         limiter active {} frames, over 1 dB {}",
        report.end_frame,
        report.end_frame as f64 / f64::from(report.rate),
        report.rate,
        report.xruns,
        report.starved_frames,
        report.late_starts,
        report.limiter_active_frames,
        report.limiter_over_1db_frames
    );
    if bad > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
