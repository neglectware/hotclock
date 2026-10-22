//! `hotclock station`: in M1.1 only the sound-card test, `hotclock station test --tone`
//! (milestone acceptance 3 and 4, run by hand on the target box; see `docs/hardware-tests.md`).

use clap::Args;
use common::config::{GlobalConfig, StationConfig};
use common::{Clock, RealClock};
use engine::rt::{Command, CommandKind, EventKind, RtHandle, RtOptions};
use engine::{open_output, HwRequest, OutputSpec, ToneSource};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Args)]
pub struct StationArgs {
    /// A station name from `/etc/hotclock/stations/<name>.toml`, or `test` for the sound-card
    /// test (a station cannot be named `test`).
    name: String,
    /// Station config file (default: `<stations_dir>/<name>.toml`). With `test`, its `device`
    /// and format settings are used when `--device` is not given.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Play a test tone (`test` only).
    #[arg(long)]
    tone: bool,
    /// Output: an ALSA device such as `hw:CARD=PCH,DEV=0`, `null`, or `file:PATH.wav`.
    #[arg(long)]
    device: Option<String>,
    #[arg(long, default_value_t = 1000.0)]
    frequency: f64,
    /// RMS level of the tone in dBFS.
    #[arg(long, default_value_t = -20.0, allow_negative_numbers = true)]
    level: f64,
    /// Stop after this many seconds (default: run until Ctrl-C).
    #[arg(long)]
    seconds: Option<f64>,
    #[arg(long, default_value_t = 48_000)]
    rate: u32,
    #[arg(long, default_value_t = 1024)]
    period_frames: u32,
    #[arg(long, default_value_t = 8)]
    periods: u32,
    /// SCHED_FIFO priority for rt-out (Linux); 0 disables.
    #[arg(long, default_value_t = 20)]
    rt_priority: u8,
    /// Run the station with the drift soak's program instead of its music: a 1 kHz tone with a timing
    /// marker on every second and a counter every 10 s (needs `[stream]`; architecture 1.3a).
    #[arg(long)]
    stream_test: bool,
    /// Test hook for acceptance 4: sleep the rt-out thread this many milliseconds ...
    #[arg(long)]
    inject_stall_ms: Option<u32>,
    /// ... this many seconds after start.
    #[arg(long, default_value_t = 5.0)]
    inject_stall_after: f64,
}

pub fn run(a: StationArgs, global_path: &Path) -> ExitCode {
    if a.name != "test" {
        return crate::station_run::run(
            &a.name,
            a.config.as_deref(),
            a.device.as_deref(),
            a.seconds,
            a.rt_priority,
            a.stream_test,
            global_path,
        );
    }
    if !a.tone {
        eprintln!("usage: hotclock station test --tone [--device hw:CARD=...,DEV=0] [--seconds N]");
        return ExitCode::from(2);
    }
    let global = match GlobalConfig::load_or_default(global_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    // Device and format: flags first, then the station file if one was named.
    let mut req = HwRequest {
        rate: a.rate,
        period_frames: a.period_frames,
        periods: a.periods,
        ..HwRequest::defaults()
    };
    let mut device = a.device.clone();
    if let Some(cfg_path) = &a.config {
        match StationConfig::load(cfg_path) {
            Ok(s) => {
                device.get_or_insert(s.device.clone());
                req.rate = s.rate;
                req.period_frames = s.period_frames;
                req.periods = s.periods;
                req.formats = s.formats.clone();
            }
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        }
    }
    tracing::info!("{}: sound-card test", crate::brand::display_name(&global));
    let device = device.unwrap_or_else(|| "null".to_string());
    let spec: OutputSpec = match device.parse() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    let clock: Arc<dyn Clock> = Arc::new(RealClock::new());
    let out = match open_output(&spec, &req, clock.clone()) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let granted = out.granted().clone();
    tracing::info!("device `{device}` opened: {}", granted.describe());
    if granted.rate != req.rate {
        tracing::warn!(
            "the card refused {} Hz and granted {} Hz; the tone is generated at the granted rate",
            req.rate,
            granted.rate
        );
    }

    if let Err(e) = engine::sys::lock_memory() {
        tracing::debug!("mlockall: {e}");
    }
    let opts = RtOptions {
        realtime_priority: (a.rt_priority > 0).then_some(a.rt_priority),
        ..RtOptions::default()
    };
    let source = ToneSource::new(granted.rate, a.frequency, a.level);
    let mut h = match RtHandle::start(out, Box::new(source), clock.clone(), opts) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("cannot start rt-out: {e}");
            return ExitCode::FAILURE;
        }
    };
    match h.priority() {
        Some(p) => tracing::info!("rt-out priority: {}", p.describe()),
        None => tracing::info!("rt-out priority: default (--rt-priority 0)"),
    }
    if matches!(
        h.priority(),
        Some(engine::sys::Priority::Nice(_)) | Some(engine::sys::Priority::Failed(_))
    ) {
        tracing::warn!("SCHED_FIFO was refused; check LimitRTPRIO in the systemd unit");
    }

    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let _ = ctrlc::set_handler(move || s2.store(true, Ordering::SeqCst));

    let t0 = Instant::now();
    let mut stall_sent = a.inject_stall_ms.is_none();
    let mut last_report = Instant::now();
    tracing::info!(
        "playing {} Hz at {} dBFS RMS; Ctrl-C to stop",
        a.frequency,
        a.level
    );
    while !stop.load(Ordering::SeqCst) {
        if let Some(limit) = a.seconds {
            if t0.elapsed().as_secs_f64() >= limit {
                break;
            }
        }
        if !stall_sent && t0.elapsed().as_secs_f64() >= a.inject_stall_after {
            if let Some(ms) = a.inject_stall_ms {
                tracing::warn!("injecting a {ms} ms stall into rt-out");
                h.send(Command::now(CommandKind::Stall { ms }));
            }
            stall_sent = true;
        }
        while let Some(ev) = h.poll_event() {
            match ev.kind {
                EventKind::XrunRecovered => tracing::warn!("xrun recovered at frame {}", ev.frame),
                EventKind::Resumed => tracing::warn!("device resumed from suspend"),
                EventKind::DeviceLost => {
                    tracing::error!("device_lost: the sound card has gone; retrying every 2 s")
                }
                EventKind::DeviceRestored { outage_ms } => {
                    tracing::info!("device restored after {outage_ms} ms; tone resumes")
                }
                EventKind::Fault(f) => {
                    tracing::error!("unrecoverable fault {f:?}, treating the device as lost")
                }
                EventKind::Started | EventKind::Stopped => {}
            }
        }
        if last_report.elapsed() >= Duration::from_secs(1) {
            let s = h.stats();
            tracing::info!(
                "state {:?}  frames {}  audible {}  xruns {}  lost {}  restored {}  max write {:.1} ms",
                s.state,
                s.frames_written,
                s.audible_frames(),
                s.xruns,
                s.device_lost,
                s.device_restored,
                s.max_write_ns as f64 / 1e6
            );
            last_report = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let s = h.stats();
    let mut out = h.stop();
    let _ = out.finish();
    println!(
        "stopped: {} frames written, {} xruns, {} device losses, {} restores",
        s.frames_written, s.xruns, s.device_lost, s.device_restored
    );
    ExitCode::SUCCESS
}
