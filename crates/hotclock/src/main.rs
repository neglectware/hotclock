//! `hotclock`: the one binary (architecture 1.1). Implemented so far: `render --tone`, `census`,
//! `station test --tone` (M1.1), `play` (M1.2) and `import`, `library`, `analyze`, `ramp-fit` (M1.3); the other subcommands are stubs that say which
//! milestone brings them.

// The package denies unsafe code. The exceptions are each confined to one module with its own `allow`: the soaks'
// counting allocator (`alloc-count`, never packaged) in `procstats::counting`, and `malloc_trim` in `heap`.
#![deny(unsafe_code)]

mod backup_cmd;
mod brand;
mod census_cmd;
mod churn_cmd;
mod ctl_cmd;
mod display_cmd;
mod explain;
mod heap;
mod hwsoak_cmd;
mod library_cmd;
mod loopcheck;
mod loopcheck_cmd;
mod notify_cmd;
mod play_cmd;
mod playlist_cmd;
mod plex_cmd;
mod procstats;
mod soak_cmd;
mod station_run;
mod station_test;

#[cfg(feature = "alloc-count")]
#[global_allocator]
static ALLOC: procstats::counting::Counting = procstats::counting::Counting;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "hotclock", version, about = brand::DISPLAY_NAME)]
struct Cli {
    /// Global config file.
    #[arg(long, global = true, default_value = common::config::DEFAULT_GLOBAL_PATH)]
    global_config: PathBuf,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a station (`hotclock station office`), or the sound-card test (`hotclock station test --tone`).
    Station(station_test::StationArgs),
    /// Send a command to a running station: acknowledge an alert, release an item from quarantine (M1.6).
    Ctl(ctl_cmd::CtlArgs),
    /// Create and list playlists and their blocks in the library (M1.5; the editor is M1.7).
    Playlist(playlist_cmd::PlaylistArgs),
    /// The library service: opens the library and watches the inbox (M1.3; the API is M1.7).
    Library(library_cmd::LibraryArgs),
    /// Render offline to a WAV file.
    Render(RenderArgs),
    /// Play files one after another (gapless, no segues) through any output: the M1.2 harness.
    Play(play_cmd::PlayArgs),
    /// Import files into the library: decode, analyze, store, tag (M1.3).
    Import(library_cmd::ImportArgs),
    /// Analyze one file and print its loudness, markers, flags and ramps as JSON (M1.3).
    Analyze(library_cmd::AnalyzeArgs),
    /// Analyze library items again from their stored files, without importing them again.
    Reanalyze(library_cmd::ReanalyzeArgs),
    /// Fit the Plex-compatible ramps against Plex's saved ramps and print the report (M1.3).
    RampFit(library_cmd::RampFitArgs),
    /// Replay the logged Plexamp transitions through the planner's `plexamp` rule (M1.4a).
    PlexReplay(PlexReplayArgs),
    /// Copy Plex playlists into the library, read-only against the server (M2).
    ImportPlex(plex_cmd::ImportPlexArgs),
    /// Send one event through the notifier's channels without the library service; the units' OnFailure= runs it (M2).
    Notify(notify_cmd::NotifyArgs),
    /// Record a card's output through another card's input and report dropouts (M1.8).
    Loopcheck(loopcheck_cmd::LoopcheckArgs),
    /// The accelerated soak: 30 virtual days of the office set with random operator actions and faults (M1.8).
    Soak(soak_cmd::SoakArgs),
    /// The clients of a long soak: status polling, WebSocket sessions that come, stall and vanish, an operator (M1.8).
    SoakChurn(churn_cmd::ChurnArgs),
    /// The hardware soak's sampler: memory, threads, descriptors, CPU and health of the services, every minute (M1.8).
    SoakWatch(hwsoak_cmd::WatchArgs),
    /// Judge a hardware soak directory: the acceptance 3 checks, as a report (M1.8).
    SoakReport(hwsoak_cmd::ReportArgs),
    /// Write the snapshots of `library.db` and every `state.db` now (M1.8; the services do it nightly).
    Backup(backup_cmd::BackupArgs),
    /// Put the newest snapshots back in place of the live databases, services stopped (M1.8).
    Restore(backup_cmd::RestoreArgs),
    /// The native status display on the box's own monitor (M7); `--render-png` draws a feed into a picture instead.
    Display(display_cmd::DisplayArgs),
    /// List every file's container, codec, rate, channels, decode result, tags and artwork.
    Census(census_cmd::CensusArgs),
}

#[derive(clap::Args)]
struct PlexReplayArgs {
    /// The test data directory (default: `HOTCLOCK_TESTDATA`).
    #[arg(long)]
    testdata: Option<PathBuf>,
}

fn plex_replay(a: PlexReplayArgs) -> ExitCode {
    let root = a
        .testdata
        .or_else(|| std::env::var_os("HOTCLOCK_TESTDATA").map(PathBuf::from));
    let Some(root) = root else {
        eprintln!("plex-replay: give --testdata or set HOTCLOCK_TESTDATA");
        return ExitCode::from(2);
    };
    match station::replay::plexamp_replay(&root, 48_000) {
        Some(r) => {
            print!("{}", r.format());
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("plex-replay: no calibration data under {}", root.display());
            ExitCode::FAILURE
        }
    }
}

#[derive(clap::Args)]
struct RenderArgs {
    /// Render a sine of this frequency in Hz.
    #[arg(long, value_name = "HZ")]
    tone: Option<f64>,
    /// Length in seconds.
    #[arg(long, default_value_t = 10.0)]
    seconds: f64,
    /// RMS level of the tone in dBFS.
    #[arg(long, default_value_t = -20.0, allow_negative_numbers = true)]
    level: f64,
    /// Sample rate.
    #[arg(long, default_value_t = 48_000)]
    rate: u32,
    /// Sample format of the WAV: f32, s32, s24 or s16.
    #[arg(long, default_value = "f32")]
    format: String,
    /// Output WAV file.
    #[arg(long)]
    out: Option<PathBuf>,
    /// A scenario file (YAML, M1.4): renders through the real conductor and mixer.
    #[arg(long)]
    scenario: Option<PathBuf>,
    /// Directory the scenario's relative file names are read from (overrides its `dir`).
    #[arg(long)]
    fixtures: Option<PathBuf>,
    /// Write the sidecar JSON (voice starts, ramps, stops, events) here.
    #[arg(long)]
    sidecar: Option<PathBuf>,
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("HOTCLOCK_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,symphonia=warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn render(a: RenderArgs) -> ExitCode {
    if let Some(sc) = &a.scenario {
        return render_scenario(sc, &a);
    }
    let (Some(hz), Some(out)) = (a.tone, a.out) else {
        eprintln!("usage: hotclock render --tone <HZ> --seconds <S> --out <FILE.wav>");
        return ExitCode::from(2);
    };
    let Some(format) = engine::render::parse_wav_format(&a.format) else {
        eprintln!("--format must be f32, s32, s24 or s16, got `{}`", a.format);
        return ExitCode::from(2);
    };
    let job = engine::render::ToneRender {
        out: out.clone(),
        rate: a.rate,
        seconds: a.seconds,
        hz,
        level_dbfs: a.level,
        format,
    };
    match engine::render::render_tone(&job) {
        Ok(r) => {
            println!(
                "wrote {} ({} frames at {} Hz, {} bytes)",
                out.display(),
                r.frames,
                r.rate,
                r.bytes
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("render failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn render_scenario(path: &std::path::Path, a: &RenderArgs) -> ExitCode {
    let run = || -> Result<(), String> {
        let sc = station::scenario::Scenario::from_file(path)?;
        let r = station::scenario::render(&sc, a.fixtures.as_deref())?;
        if let Some(out) = &a.out {
            r.write_wav(out)?;
            println!(
                "wrote {} ({} frames at {} Hz)",
                out.display(),
                r.sidecar.render_frames,
                r.rate
            );
        }
        if let Some(sidecar) = &a.sidecar {
            std::fs::write(sidecar, r.sidecar.to_json())
                .map_err(|e| format!("cannot write {}: {e}", sidecar.display()))?;
            println!("wrote {}", sidecar.display());
        }
        for b in &r.sidecar.boundaries {
            println!(
                "{} -> {}: {} at frame {} (overlap {} frames{})",
                b.from,
                b.to,
                b.kind,
                b.s_b,
                b.overlap_frames,
                if b.notes.is_empty() {
                    String::new()
                } else {
                    format!("; {}", b.notes.join("; "))
                }
            );
        }
        Ok(())
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("render --scenario failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    init_logging();
    procstats::log_live_heap_from_env();
    let cli = Cli::parse();
    match cli.command {
        Cmd::Station(a) => station_test::run(a, &cli.global_config),
        Cmd::Ctl(a) => ctl_cmd::run(a, &cli.global_config),
        Cmd::Library(a) => library_cmd::library(a, &cli.global_config),
        Cmd::Playlist(a) => playlist_cmd::run(a, &cli.global_config),
        Cmd::Render(a) => render(a),
        Cmd::Import(a) => library_cmd::import(a, &cli.global_config),
        Cmd::Analyze(a) => library_cmd::analyze(a),
        Cmd::Reanalyze(a) => library_cmd::reanalyze(a, &cli.global_config),
        Cmd::RampFit(a) => library_cmd::ramp_fit(a),
        Cmd::PlexReplay(a) => plex_replay(a),
        Cmd::ImportPlex(a) => plex_cmd::run(a, &cli.global_config),
        Cmd::Notify(a) => notify_cmd::run(a, &cli.global_config),
        Cmd::Loopcheck(a) => loopcheck_cmd::run(a),
        Cmd::Soak(a) => soak_cmd::run(a),
        Cmd::SoakChurn(a) => churn_cmd::run(a),
        Cmd::SoakWatch(a) => hwsoak_cmd::watch(a),
        Cmd::SoakReport(a) => hwsoak_cmd::report(a),
        Cmd::Backup(a) => backup_cmd::backup_now(a, &cli.global_config),
        Cmd::Restore(a) => backup_cmd::restore(a, &cli.global_config),
        Cmd::Census(a) => census_cmd::run(a),
        Cmd::Display(a) => display_cmd::run(a, &cli.global_config),
        Cmd::Play(a) => play_cmd::run(a),
    }
}
