//! A tiny control channel to a running station, until the API (M1.7) exists: command files dropped in
//! `<state dir>/control/`. `hotclock ctl <station> ...` writes them, the station's loop reads and removes
//! them (twice a second). The API will call the same [`crate::runtime::Station`] methods directly; this
//! channel stays as the operator's fallback with no network involved.
//!
//! The commands marked *test hook* are honoured only when the station runs with `HOTCLOCK_TEST_HOOKS=1`:
//! they inject the faults the M1.6 acceptance tests need (a stalled rt thread, an unplugged card, failing
//! media reads, a read-only or full `state.db`).

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Cmd {
    /// Acknowledge an alert (`safe_mode`, `state_db_corrupt`, ...).
    Ack { code: String },
    /// Release an item from quarantine.
    Release { item_id: i64 },
    /// Quarantine an item by hand.
    Quarantine { item_id: i64, reason: String },
    /// Test hook: sleep the rt-out thread this long.
    Stall { ms: u32 },
    /// Test hook: unplug the sound card (writes fail with "device lost" and reopen fails).
    Unplug,
    /// Test hook: plug it back in.
    Plug,
    /// Test hook: every media read fails.
    MediaFail { on: bool },
    /// Test hook: `state.db` writes fail as `none`, `readonly` or `full`.
    DbFault { mode: String },
    /// Test hook: sleep the stream encoder thread this long (M2a acceptance 4a).
    StreamStall { ms: u32 },
    /// Test hook: make the stream encoder restart as after a failure (M2a acceptance 4d and 5).
    StreamRestart,
    /// Test hook: make the stream encoder panic while it encodes (M2a review).
    StreamPanic,
    /// Test hook: raise an alert by hand (M1.7's `/health` acceptance forces a `silence`).
    RaiseAlert { code: String, critical: bool },
    /// Test hook: clear an alert raised by hand.
    ClearAlert { code: String },
    /// Test hook: set the loudness controller's gain directly, dB (M6a: the restart test).
    LoudnessGain { db: f64 },
    /// Test hook: the meter thread stops draining the taps for this long (M6a: a stalled thread holds
    /// the gain).
    LoudnessStall { ms: u32 },
}

impl Cmd {
    pub fn is_test_hook(&self) -> bool {
        !matches!(
            self,
            Cmd::Ack { .. } | Cmd::Release { .. } | Cmd::Quarantine { .. }
        )
    }
}

pub fn control_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("control")
}

/// Drop a command for the station whose state directory is `state_dir`.
pub fn send(state_dir: &Path, cmd: &Cmd) -> io::Result<()> {
    let dir = control_dir(state_dir);
    std::fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = dir.join(format!(".{stamp}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(cmd).map_err(io::Error::other)?)?;
    std::fs::rename(
        &tmp,
        dir.join(format!("{stamp}-{}.cmd", std::process::id())),
    )
}

/// Read and remove the pending commands, oldest first. A file that does not parse is removed and logged.
pub fn drain(state_dir: &Path) -> Vec<Cmd> {
    let dir = control_dir(state_dir);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "cmd"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for f in files {
        match std::fs::read(&f)
            .ok()
            .and_then(|b| serde_json::from_slice::<Cmd>(&b).ok())
        {
            Some(c) => out.push(c),
            None => tracing::warn!("control file {} does not parse; removed", f.display()),
        }
        let _ = std::fs::remove_file(&f);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip_in_order_and_are_consumed() {
        let d = fixtures::testdir::process_dir().join(format!("ctl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        send(
            &d,
            &Cmd::Ack {
                code: "safe_mode".into(),
            },
        )
        .unwrap();
        send(&d, &Cmd::Release { item_id: 7 }).unwrap();
        send(&d, &Cmd::Stall { ms: 20_000 }).unwrap();
        let got = drain(&d);
        assert_eq!(got.len(), 3);
        assert_eq!(
            got[0],
            Cmd::Ack {
                code: "safe_mode".into()
            }
        );
        assert!(got[2].is_test_hook());
        assert!(!got[0].is_test_hook());
        assert!(drain(&d).is_empty());
    }
}
