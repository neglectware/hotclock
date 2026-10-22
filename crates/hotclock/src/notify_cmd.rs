//! `hotclock notify` (M2, architecture 12.1): send one event through the notifier's channels and rules without the
//! library service. The units' `OnFailure=` runs `hotclock notify --event service_failed --unit %i`, so a failed
//! library service (or station) still reaches the operator. It reads the same `library.db` channels and `secrets.toml`.

use clap::Args;
use library::Paths;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

#[derive(Args)]
pub struct NotifyArgs {
    /// The event type (an alert code of 12.1), for example `service_failed`.
    #[arg(long)]
    event: String,
    /// The systemd unit that failed (adds its recent log to the message).
    #[arg(long)]
    unit: Option<String>,
    /// The station the event is about (default: the unit name, or `-`).
    #[arg(long)]
    station: Option<String>,
    #[arg(long, default_value = "critical")]
    severity: String,
    /// The message (default: a line naming the unit).
    #[arg(long)]
    message: Option<String>,
    /// Override `data_dir` of the global config.
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

/// The last lines of a unit's journal, best effort and bounded to a few seconds.
fn journal_tail(unit: &str) -> String {
    let child = Command::new("journalctl")
        .args(["-u", unit, "-n", "15", "--no-pager", "-o", "cat"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return String::new();
    };
    let t = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                break;
            }
        }
    }
    child
        .wait_with_output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

pub fn run(a: NotifyArgs, global: &Path) -> ExitCode {
    let cfg = match common::config::GlobalConfig::load_or_default(global) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("notify: {e}");
            return ExitCode::FAILURE;
        }
    };
    let paths = match &a.data_dir {
        Some(d) => Paths::from_data_dir(d),
        None => Paths::from_config(&cfg),
    };
    let mut message = a.message.clone().unwrap_or_else(|| match &a.unit {
        Some(u) => format!("The unit {u} entered the failed state."),
        None => format!("Event {}.", a.event),
    });
    if let (Some(u), None) = (&a.unit, &a.message) {
        let tail = journal_tail(u);
        if !tail.is_empty() {
            message.push_str(&format!("\n\nIts last log lines:\n{tail}"));
        }
    }
    let station = a
        .station
        .clone()
        .or(a.unit.clone())
        .unwrap_or_else(|| "-".into());
    match serve::notify::one_shot(
        &paths.db,
        &cfg.secrets_file,
        crate::brand::display_name(&cfg),
        &a.event,
        &station,
        &a.severity,
        &message,
    ) {
        Ok(r) => {
            println!("notify: {} {}: {r}", a.event, station);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("notify: {} {}: {e}", a.event, station);
            ExitCode::FAILURE
        }
    }
}
