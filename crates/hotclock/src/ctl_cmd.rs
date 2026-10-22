//! `hotclock ctl <station> ...`: send a command to a running station through its control directory
//! (M1.6; the API of M1.7 does the same in process). `ack <code>`, `release <item>`, `quarantine <item>
//! <reason>`, and the test hooks (`hook stall <ms>`, `hook unplug`, `hook plug`, `hook media-fail
//! on|off`, `hook db-fault none|readonly|full`), which the station honours only with
//! `HOTCLOCK_TEST_HOOKS=1`.

use clap::Args;
use common::config::GlobalConfig;
use station::control::{send, Cmd};
use std::path::Path;
use std::process::ExitCode;

#[derive(Args)]
pub struct CtlArgs {
    /// The station's name.
    station: String,
    /// `ack CODE`, `release ITEM`, `quarantine ITEM REASON`, `hook NAME [ARG]`.
    words: Vec<String>,
}

pub fn run(a: CtlArgs, global_path: &Path) -> ExitCode {
    let global = match GlobalConfig::load_or_default(global_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let w: Vec<&str> = a.words.iter().map(String::as_str).collect();
    let cmd = match w.as_slice() {
        ["ack", code] => Cmd::Ack {
            code: (*code).into(),
        },
        ["release", id] => match id.parse() {
            Ok(item_id) => Cmd::Release { item_id },
            Err(_) => return usage(),
        },
        ["quarantine", id, reason] => match id.parse() {
            Ok(item_id) => Cmd::Quarantine {
                item_id,
                reason: (*reason).into(),
            },
            Err(_) => return usage(),
        },
        ["hook", "stall", ms] => match ms.parse() {
            Ok(ms) => Cmd::Stall { ms },
            Err(_) => return usage(),
        },
        ["hook", "stream-stall", ms] => match ms.parse() {
            Ok(ms) => Cmd::StreamStall { ms },
            Err(_) => return usage(),
        },
        ["hook", "stream-restart"] => Cmd::StreamRestart,
        ["hook", "stream-panic"] => Cmd::StreamPanic,
        ["hook", "unplug"] => Cmd::Unplug,
        ["hook", "plug"] => Cmd::Plug,
        ["hook", "media-fail", on] => Cmd::MediaFail { on: *on == "on" },
        ["hook", "db-fault", mode] => Cmd::DbFault {
            mode: (*mode).into(),
        },
        ["hook", "alert", code, sev] => Cmd::RaiseAlert {
            code: (*code).into(),
            critical: *sev == "critical",
        },
        ["hook", "loudness-gain", db] => match db.parse() {
            Ok(db) => Cmd::LoudnessGain { db },
            Err(_) => return usage(),
        },
        ["hook", "loudness-stall", ms] => match ms.parse() {
            Ok(ms) => Cmd::LoudnessStall { ms },
            Err(_) => return usage(),
        },
        ["hook", "clear-alert", code] => Cmd::ClearAlert {
            code: (*code).into(),
        },
        _ => return usage(),
    };
    let dir = global.station_state_dir(&a.station);
    match send(&dir, &cmd) {
        Ok(()) => {
            println!("sent {cmd:?} to `{}` ({})", a.station, dir.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}: {e}", dir.display());
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: hotclock ctl <station> ack <code> | release <item> | quarantine <item> <reason> | hook <name> [arg]"
    );
    ExitCode::from(2)
}
