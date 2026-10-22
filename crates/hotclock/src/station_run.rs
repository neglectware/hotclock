//! `hotclock station <name>`: run a station (M1.5). The scheduler, conductor and mixer of the
//! `station` crate on the real rt-out thread, with `state.db`, library sync and resume after a restart.
//! The output is whatever the station config's `device` says: an ALSA device, `null`, `file:PATH.wav`
//! or `file-paced:PATH.wav` (real time, which the restart tests use).

use common::config::{GlobalConfig, StationConfig};
use common::{Clock, RealClock};
use engine::rt::{CommandKind, EventKind, RtHandle, RtOptions, RtState};
use engine::testing::{FaultControl, FaultyOutput};
use engine::{open_output_trimmed, HwRequest, Output, OutputSpec};
use station::control::{self, Cmd};
use station::failure::StreamStatus;
use station::runtime::{Station, StationOptions};
use station::state::FaultMode;
use station::supervise::{crashloop, Notifier, StallWatch};
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serve::http::Server;
use serve::security::Security;
use serve::station_api::{ApiOptions, AudioInfo, Driver, StationApi};
use serve::station_http::StationHandler;

fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

pub fn run(
    name: &str,
    config: Option<&Path>,
    device: Option<&str>,
    seconds: Option<f64>,
    rt_priority: u8,
    stream_test: bool,
    global_path: &Path,
) -> ExitCode {
    let global = match GlobalConfig::load_or_default(global_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let cfg_path = config
        .map(Path::to_path_buf)
        .unwrap_or_else(|| global.stations_dir.join(format!("{name}.toml")));
    let cfg = match StationConfig::load(&cfg_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let device = device.map_or_else(|| cfg.device.clone(), str::to_string);
    let spec: OutputSpec = match device.parse() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let req = HwRequest {
        rate: cfg.rate,
        period_frames: cfg.period_frames,
        periods: cfg.periods,
        formats: cfg.formats.clone(),
        ..HwRequest::defaults()
    };
    let clock: Arc<dyn Clock> = Arc::new(RealClock::new());
    let out = match open_output_trimmed(&spec, &req, clock.clone(), cfg.rate_trim_ppm) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let granted = out.granted().clone();
    // The test hooks (unplug the card, stall rt-out, fail media reads, break state.db) exist only when the
    // process is started with HOTCLOCK_TEST_HOOKS=1: the M1.6 fault-injection tests use them.
    let hooks_on = std::env::var("HOTCLOCK_TEST_HOOKS").is_ok_and(|v| v == "1");
    let (out, fault): (Box<dyn Output>, Option<Arc<FaultControl>>) = if hooks_on {
        let (f, c) = FaultyOutput::new(out);
        (Box::new(f), Some(c))
    } else {
        (out, None)
    };
    tracing::info!(
        "{}: station `{}` output `{device}` opened: {}",
        crate::brand::display_name(&global),
        cfg.name,
        granted.describe()
    );

    let mut opts = StationOptions::new(&cfg.name, global.media_dir());
    opts.rate = granted.rate;
    opts.preset = cfg.transition_preset.clone();
    opts.target_lufs = cfg.target_lufs;
    opts.max_boost_db = cfg.max_boost_db;
    opts.output_trim_db = cfg.output_trim_db;
    opts.peak_allowance_db = cfg.peak_allowance_db;
    opts.truncated_fade = cfg.truncated_fade;
    opts.truncated_fade_s = cfg.truncated_fade_s;
    opts.lock_ahead_s = cfg.lock_ahead_s;
    opts.default_playlist = cfg.default_playlist.clone();
    opts.emergency_playlist = cfg.emergency_playlist.clone();
    opts.stopped_is_normal = cfg.stopped_is_normal;
    opts.ram_loop_seconds = cfg.ram_loop_seconds;
    opts.silence_s = f64::from(cfg.silence_s);
    opts.silence_dbfs = cfg.silence_dbfs;
    opts.restart_alert_clear_s = f64::from(cfg.restart_alert_clear_s);
    opts.backup_dir = Some(global.backup_dir());
    opts.loudness_gate = cfg.loudness.gate_categories.clone();
    opts.time_zone = global.time_zone.clone();
    // The start counter (1.5): five starts within 120 s is a crash loop, and the station comes up in
    // safe mode, playing the emergency playlist until an operator acknowledges it.
    let starts_path = global.run_dir.join(format!("{}.starts", cfg.name));
    let starts = crashloop::record_start(&starts_path, wall_ms());
    let state_path = global.station_state_dir(&cfg.name).join("state.db");
    let library_db = cfg
        .library
        .clone()
        .unwrap_or_else(|| global.library_db_path());
    let state_dir = state_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let (mut station, mixer) = match Station::open(
        opts,
        Some(state_path.clone()),
        Some(library_db.clone()),
        wall_ms(),
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open the station: {e}");
            return ExitCode::FAILURE;
        }
    };
    // The loudness controller (6.1): its link to the mixer is set before the first step, so the first
    // checkpoint already carries the gain. The gain resumes from the checkpoint; with none it starts at the
    // static estimate (the target less the track reference level while the chain does nothing, else 0 dB).
    let lc = cfg.loudness.clone();
    let static_db = if cfg.chain_config() == common::chain::ChainConfig::default() {
        (lc.target_lufs - cfg.target_lufs).clamp(lc.gain_min_db, lc.gain_max_db)
    } else {
        0.0
    };
    let initial_gain_db = station
        .loudness_restored_db
        .unwrap_or(static_db)
        .clamp(lc.gain_min_db, lc.gain_max_db);
    {
        let link = station.cond.loudness_link();
        link.enabled.store(lc.enabled, Ordering::Relaxed);
        link.set_ceiling_db(lc.true_peak_dbtp);
        link.set_gain_db(if lc.enabled { initial_gain_db } else { 0.0 });
        // 3.6 with the controller on: how far normalization may push an item's peaks into the limiter is the
        // loudness setting `peak_allowance_db` (the controller lifts every item again afterwards).
        station
            .cond
            .loudness_live()
            .set_allowance(lc.enabled.then_some(lc.peak_allowance_db));
    }
    if starts >= crashloop::LIMIT {
        tracing::error!("{starts} starts within 120 s: safe mode");
        station.fail.safe_mode = true;
    }
    if let Err(e) = engine::sys::lock_memory() {
        tracing::debug!("mlockall: {e}");
    }
    let mut rt_opts = RtOptions {
        realtime_priority: (rt_priority > 0).then_some(rt_priority),
        ..RtOptions::default()
    };
    if stream_test && !cfg.stream.enabled {
        eprintln!(
            "--stream-test needs `[stream]` with `enabled = true` in {}",
            cfg_path.display()
        );
        return ExitCode::FAILURE;
    }
    // The network stream (1.3a): the tap ring rt-out writes into, and the mount the encoder fills.
    let display_name = cfg.display_name.clone().unwrap_or_else(|| cfg.name.clone());
    let mut stream_parts: Option<(engine::tap::TapConsumer, Arc<stream::Mount>)> = None;
    if cfg.stream.enabled {
        let s = &cfg.stream;
        let secs = 4;
        let max_periods = (secs * granted.rate / granted.period_frames.max(1)) as usize + 64;
        let (tx, rx) = engine::tap::stream_tap((secs * granted.rate) as usize, max_periods);
        rt_opts.stream_tap = Some(tx);
        let mount = stream::Mount::new(stream::MountConfig {
            name: s.mount.clone(),
            display_name: display_name.clone(),
            rate: granted.rate,
            codec: "flac",
            burst_ms: s.burst_ms,
            max_listeners: s.max_listeners,
            listener_lag_s: s.listener_lag_s,
            expect_listener: s.expect_listener,
        });
        stream_parts = Some((rx, mount));
    }
    let source: Box<dyn engine::Source> = if stream_test {
        tracing::warn!("--stream-test: the program is the timing tone, not the station's music");
        Box::new(stream::testsrc::TimingSource::new(
            granted.rate,
            Some(Box::new(mixer)),
        ))
    } else {
        Box::new(mixer)
    };
    let mut h = match RtHandle::start(out, source, clock.clone(), rt_opts) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("cannot start rt-out: {e}");
            return ExitCode::FAILURE;
        }
    };
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let _ = ctrlc::set_handler(move || s2.store(true, Ordering::SeqCst));

    let rate = i64::from(granted.rate);
    // The mixer's timeline is the frames the source has rendered (`source_frames`: it stands still
    // through the silence an xrun writes, so command frames and voice positions stay on it), and
    // the last of them becomes audible after the device delay.
    let wall_audible = |h: &RtHandle| -> (u64, i64) {
        let s = h.stats();
        (
            s.source_frames,
            wall_ms() + (s.delay_frames.max(0) * 1000) / rate,
        )
    };
    let started = {
        let (frame, wall) = wall_audible(&h);
        let mut send = |c: engine::rt::Command| {
            h.send(c);
        };
        station.start(frame, wall, &mut send)
    };
    match started {
        Ok(info) => tracing::info!("station started: {info:?}"),
        // The drift soak needs no music: a station with no playlist still serves its timing tone.
        Err(e) if stream_test => {
            tracing::warn!("--stream-test: the station has nothing to play ({e})")
        }
        Err(e) => {
            eprintln!("cannot start the station: {e}");
            return ExitCode::FAILURE;
        }
    }
    let mut stream_mount: Option<Arc<stream::Mount>> = None;
    let mut stream_enc: Option<stream::EncoderThread> = None;
    if let Some((rx, mount)) = stream_parts.take() {
        let mut eo = stream::EncoderOptions::new(granted.rate, &display_name);
        eo.flac_level = cfg.stream.flac_level;
        eo.product = crate::brand::display_name(&global).to_string();
        match stream::spawn(eo, rx, mount.clone()) {
            Ok(t) => stream_enc = Some(t),
            Err(e) => {
                tracing::error!("cannot start the stream encoder: {e}");
                mount.set_encoder_state(stream::EncoderState::Failed);
            }
        }
        tracing::info!(
            "stream: http://{}/stream/{}.ogg (FLAC level {}, up to {} listeners)",
            cfg.listen,
            cfg.stream.mount,
            cfg.stream.flac_level,
            cfg.stream.max_listeners
        );
        stream_mount = Some(mount);
    }
    // The configuration file's history: the file as it was loaded is a version if it is not the newest one
    // (a hand edit, a restore outside the page).
    let history = serve::config_history::ConfigHistory::beside(&state_path, &cfg_path);
    if let Err(e) = history.capture_start(wall_ms()) {
        tracing::warn!("configuration history: {e}");
    }
    // The network API (7.2) and the meter thread (11.3).
    let (api, api_rx) = StationApi::new(ApiOptions {
        name: cfg.name.clone(),
        display_name,
        product: crate::brand::display_name(&global).to_string(),
        media_dir: global.media_dir(),
        library_db: library_db.clone(),
        state_db: Some(state_path.clone()),
        device: device.clone(),
        version: crate::brand::version(),
        config_path: Some(cfg_path.clone()),
        loudness: lc.clone(),
        loudness_static_db: static_db,
    });
    if let Ok(mut p) = api.meter.ctl.params.lock() {
        *p = serve::meters::params_of(&lc);
    }
    if let Ok(mut g) = api.meter.ctl.initial_gain_db.lock() {
        *g = initial_gain_db;
    }
    let _ = api.loudness_live.set(station.cond.loudness_live());
    // The processing chain (6): the station file's preset and `[chain]` keys, started before the first sound.
    let chain_api = Arc::new(
        serve::chain_api::ChainApi::new(station.cond.chain_control(), &cfg, Some(cfg_path.clone()))
            .with_history(Some(history)),
    );
    if let Err(e) = chain_api.start() {
        tracing::error!("cannot start the processing chain: {e}");
    }
    let _ = api.chain.set(chain_api);
    if let Some(m) = &stream_mount {
        let _ = api.stream.set(m.clone());
    }
    let mut driver = Driver::new(api.clone(), api_rx);
    {
        let sp = starts_path.clone();
        driver.on_ack = Some(Box::new(move |code: &str| {
            if code == "safe_mode" {
                crashloop::clear(&sp);
            }
        }));
    }
    driver.restarts = starts as u64;
    let mut server = match start_server(&global, &cfg.listen, &api) {
        Ok(s) => Some(s),
        Err(e) => {
            // The station plays on: dead air is worse than a missing API. The health page is gone with it.
            tracing::error!("cannot serve the API on {}: {e}", cfg.listen);
            None
        }
    };
    let meter_thread = station.cond.take_meter_tap().and_then(|tap| {
        let a2 = api.clone();
        let a3 = api.clone();
        let wanted: serve::meters::Wanted = Arc::new(move || {
            a2.hub.wants("meters")
                || (wall_ms() as u64) < a2.meter.want_until_ms.load(Ordering::Relaxed)
        });
        let sink: serve::meters::FrameSink = Arc::new(move |f| a3.hub.send_binary("meters", f));
        let a4 = api.clone();
        let context: serve::meters::StepContext = Arc::new(move || {
            let now = a4
                .view
                .try_read()
                .ok()
                .and_then(|v| v.now.as_ref().map(|n| (n.json.clone(), n.started_ms)));
            let item = now.as_ref().map_or_else(
                || "nothing on air".to_string(),
                |(n, started)| {
                    format!(
                        "item {} \"{}\" ({} ms in), item gain {} dB",
                        n["item_id"],
                        n["title"].as_str().unwrap_or(""),
                        wall_ms() - started,
                        n["item_gain_db"]
                    )
                },
            );
            format!("station {}, {item}", a4.opts.name)
        });
        serve::meters::spawn(tap, 240.0, api.meter.clone(), wanted, sink, Some(context)).ok()
    });
    let analyzer_thread = serve::analyzer::spawn(api.clone()).ok();
    let mut audio = AudioInfo {
        open: true,
        rt_state: "playing",
        format: granted.format.name().to_string(),
        rate: granted.rate,
        period_frames: granted.period_frames,
        periods: granted.periods,
        restarts: u64::from(starts as u32),
        ..AudioInfo::default()
    };
    let mut device_restored = 0u32;
    let notifier = Notifier::from_env();
    notifier.ready();
    notifier.status("playing");
    let mut stall = StallWatch::default();
    let mut stream_lag_on = false;
    // `stream_no_listener` raises after `no_listener_alert_s` (60 s by default, 1.3a); the test hook shortens it
    // below the configurable minimum so tests need not wait.
    let no_listener_after_s: f64 = f64::from(cfg.stream.no_listener_alert_s);
    let no_listener_after_s: f64 = if hooks_on {
        std::env::var("HOTCLOCK_TEST_NO_LISTENER_S")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(no_listener_after_s)
    } else {
        no_listener_after_s
    };
    // No card: the stream is the only output, so a failed encoder takes the station off the air.
    let stream_only = !matches!(
        device.parse::<engine::output::OutputSpec>(),
        Ok(engine::output::OutputSpec::Alsa(_))
    );
    let mut last_stream_check = Instant::now();
    let mut stalled_now = false;
    let mut last_feed = Instant::now();
    let mut last_control = Instant::now();
    let mut last_loud_alerts = Instant::now();
    let t0 = Instant::now();
    let mut last_log = Instant::now();
    let mut last_title = String::new();
    let mut exit = ExitCode::SUCCESS;
    let mut step_errors = 0u64;
    let mut last_step_error: Option<Instant> = None;
    while !stop.load(Ordering::SeqCst) {
        if seconds.is_some_and(|s| t0.elapsed().as_secs_f64() >= s) {
            break;
        }
        while let Some(ev) = h.poll_event() {
            match ev.kind {
                EventKind::DeviceLost => station.on_device_lost(),
                EventKind::DeviceRestored { outage_ms } => {
                    device_restored += 1;
                    station.on_device_restored(outage_ms)
                }
                EventKind::XrunRecovered => station.on_xrun(),
                _ => {}
            }
        }
        let (frame, wall) = wall_audible(&h);
        let step = {
            let mut send = |c: engine::rt::Command| {
                h.send(c);
            };
            station.step(frame, wall, &mut send)
        };
        if let Err(e) = step {
            // A step that fails keeps failing every 20 ms until what caused it changes: log the first and then
            // once every 10 s with a count, not 50 lines a second.
            step_errors += 1;
            if last_step_error.is_none_or(|t: Instant| t.elapsed() >= Duration::from_secs(10)) {
                tracing::error!("station step failed ({step_errors} so far): {e}");
                last_step_error = Some(Instant::now());
            }
        }
        // The network API: commands from HTTP, the view for its readers, the events for WebSocket clients.
        {
            let s = h.stats();
            audio.frames_written = s.frames_written;
            audio.xruns = s.xruns;
            audio.device_restored = device_restored;
            audio.delay_frames = s.delay_frames;
            audio.open = s.state != RtState::DeviceLost;
            audio.rt_state = match s.state {
                RtState::Playing => "playing",
                RtState::DeviceLost => "device_lost",
                RtState::Starting => "starting",
                RtState::Stopped => "stopped",
            };
            let mixer_stats = station.cond.stats();
            let mut send = |c: engine::rt::Command| {
                h.send(c);
            };
            driver.step(&mut station, frame, wall, &audio, &mixer_stats, &mut send);
        }
        // rt-out services a block's loads before its commands, so a boundary committed in this step
        // can be taken back by the next command at once: nothing is "in flight" here (the offline
        // renderer clears the flag after each rendered block instead). Left set, every operator
        // action would answer `Busy` for the ten seconds before each boundary.
        station.load_in_flight = false;
        if let Some(n) = station.now() {
            if n.title != last_title {
                tracing::info!("now: {} - {} ({})", n.artist, n.title, n.category);
                last_title = n.title;
                if station.fail.wants_restart {
                    // state.db went corrupt while running (1.5): restart at this item boundary into the
                    // start path, which moves the file aside and starts from a fresh one.
                    tracing::warn!("restarting to repair state.db");
                    exit = ExitCode::from(75);
                    break;
                }
            }
        }
        // The engine watchdog (1.5): rt-out not advancing for 3 s while the card is open means it is
        // stuck. Stop feeding systemd's watchdog and let it restart the process. A card that is gone is
        // not a stall: the process is healthy, and the watchdog keeps being fed.
        {
            let s = h.stats();
            let now_ms = t0.elapsed().as_millis() as i64;
            let stalled = stall.observe(s.frames_written, s.state == RtState::Playing, now_ms);
            if stalled != stalled_now {
                stalled_now = stalled;
                station.on_engine_stall(stalled);
                if stalled {
                    // Let the alert reach state.db, then ask systemd to restart us now rather than when the
                    // watchdog runs out (the feed has already stopped: the backstop).
                    station.flush_writer();
                    notifier.trigger();
                }
            }
            if !stalled && stall.feed_ok(s.state == RtState::Playing, now_ms) {
                if let Some(iv) = notifier.feed_interval() {
                    if last_feed.elapsed() >= iv {
                        notifier.watchdog();
                        last_feed = Instant::now();
                    }
                }
            }
        }
        if let Some(m) = &stream_mount {
            if last_stream_check.elapsed() >= Duration::from_millis(250) {
                last_stream_check = Instant::now();
                let lag = m.stats.encoder_lag_ms.load(Ordering::Relaxed);
                if lag > 1000 {
                    stream_lag_on = true;
                } else if lag < 500 {
                    stream_lag_on = false;
                }
                let idle = m.no_listener_s();
                station.on_stream(&StreamStatus {
                    no_listener: m.cfg.expect_listener && idle >= no_listener_after_s,
                    no_listener_s: idle,
                    encoder_lag: stream_lag_on,
                    lag_ms: lag,
                    encoder_failed: matches!(
                        m.encoder_state(),
                        stream::EncoderState::Failed | stream::EncoderState::Stopped
                    ) || stream_enc.as_ref().is_none_or(|e| e.is_finished()),
                    stream_only,
                });
            }
        }
        if last_loud_alerts.elapsed() >= Duration::from_millis(500) {
            last_loud_alerts = Instant::now();
            let st = api.meter.ctl.status.lock().ok().and_then(|s| s.clone());
            if let Some(s) = st {
                station.on_loudness(s.at_limit_s, s.frozen_playing_s);
            }
        }
        if last_control.elapsed() >= Duration::from_millis(500) {
            last_control = Instant::now();
            for cmd in control::drain(&state_dir) {
                if cmd.is_test_hook() && !hooks_on {
                    tracing::warn!("test hook {cmd:?} ignored: HOTCLOCK_TEST_HOOKS is not 1");
                    continue;
                }
                match cmd {
                    Cmd::Ack { code } => {
                        if code == "safe_mode" {
                            crashloop::clear(&starts_path);
                        }
                        station.ack(&code);
                    }
                    Cmd::Release { item_id } => station.release_item(item_id),
                    Cmd::Quarantine { item_id, reason } => {
                        station.quarantine_item(item_id, &reason)
                    }
                    Cmd::Stall { ms } => {
                        h.send(engine::rt::Command::now(CommandKind::Stall { ms }));
                    }
                    Cmd::StreamStall { ms } => {
                        if let Some(m) = &stream_mount {
                            m.hook_stall(u64::from(ms));
                        }
                    }
                    Cmd::StreamPanic => {
                        if let Some(m) = &stream_mount {
                            m.hook_panic();
                        }
                    }
                    Cmd::StreamRestart => {
                        if let Some(m) = &stream_mount {
                            m.hook_restart();
                        }
                    }
                    Cmd::Unplug => {
                        if let Some(f) = &fault {
                            f.unplugged.store(true, Ordering::SeqCst);
                        }
                    }
                    Cmd::Plug => {
                        if let Some(f) = &fault {
                            f.unplugged.store(false, Ordering::SeqCst);
                        }
                    }
                    Cmd::MediaFail { on } => {
                        station.media_faults().fail_all.store(on, Ordering::SeqCst)
                    }
                    Cmd::LoudnessGain { db } => {
                        if let Ok(mut g) = api.meter.ctl.set_gain.lock() {
                            *g = Some(db);
                        }
                    }
                    Cmd::LoudnessStall { ms } => {
                        api.meter
                            .ctl
                            .stall_until_ms
                            .store((wall_ms() + i64::from(ms)) as u64, Ordering::Relaxed);
                    }
                    Cmd::RaiseAlert { code, critical } => station.hook_raise_alert(&code, critical),
                    Cmd::ClearAlert { code } => station.hook_clear_alert(&code),
                    Cmd::DbFault { mode } => station.set_state_fault(match mode.as_str() {
                        "readonly" => FaultMode::ReadOnly,
                        "full" => FaultMode::Full,
                        _ => FaultMode::None,
                    }),
                }
            }
        }
        if last_log.elapsed() >= Duration::from_secs(10) {
            let s = h.stats();
            tracing::info!(
                "frames {}  xruns {}  queue {}",
                s.frames_written,
                s.xruns,
                station.sched.st.queue.len()
            );
            last_log = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    station.flush();
    api.meter.stop.store(true, Ordering::Relaxed);
    if let Some(mut srv) = server.take() {
        srv.stop();
    }
    if let Some(t) = meter_thread {
        let _ = t.join();
    }
    if let Some(t) = analyzer_thread {
        let _ = t.join();
    }
    let mut out = h.stop();
    let _ = out.finish();
    // The encoder after rt-out has stopped, so it drains what rt-out pushed; then the listeners go.
    if let Some(e) = stream_enc.take() {
        e.stop();
    }
    if let Some(m) = &stream_mount {
        m.shutdown();
    }
    exit
}

/// Bind the station's API on `listen`, with the security rules of architecture 13.
fn start_server(
    global: &GlobalConfig,
    listen: &str,
    api: &Arc<StationApi>,
) -> std::io::Result<Server> {
    // This box's own ports: the library's and every station's (an `Origin` is ours only on one of them).
    let mut ports: Vec<u16> = vec![];
    if let Ok(a) = global.library.listen.parse::<std::net::SocketAddr>() {
        ports.push(a.port());
    }
    if let Ok(rd) = std::fs::read_dir(&global.stations_dir) {
        for e in rd.flatten() {
            if let Ok(c) = StationConfig::load(&e.path()) {
                if let Ok(a) = c.listen.parse::<std::net::SocketAddr>() {
                    ports.push(a.port());
                }
            }
        }
    }
    if let Ok(a) = listen.parse::<std::net::SocketAddr>() {
        ports.push(a.port());
    }
    ports.sort_unstable();
    ports.dedup();
    let sec = Arc::new(Security::new(&global.allowed_hosts, ports));
    let srv = Server::start(listen, Arc::new(StationHandler { api: api.clone() }), sec)?;
    tracing::info!("station API on http://{}", srv.addr);
    Ok(srv)
}
