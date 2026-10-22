//! Alerts (architecture 1.5, 12, 12.1): what the station has had to work around or is suffering.
//! Every failure has an alert code; an alert is *active* from the moment it is raised until it is
//! cleared, and the station's health is the worst severity among the active ones (`/health` answers
//! 503 for `critical`, 12).
//!
//! The registry is plain data driven by the station's step; raising and clearing are idempotent (an
//! alert already active is not raised again, so a condition that persists does not flood the log or the
//! notification spool). Each raise and clear becomes an [`AlertEvent`] that the station drains and
//! writes to `state.db`'s `alerts` and `notify_spool` tables.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

impl Severity {
    pub fn name(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }
}

/// The alert codes of 1.5 and 12 that milestone M1.6 raises.
pub mod code {
    pub const SILENCE: &str = "silence";
    pub const ITEM_MISSING: &str = "item_missing";
    pub const ITEM_TRUNCATED: &str = "item_truncated";
    pub const ITEM_SILENT: &str = "item_silent";
    pub const ITEM_DECODE_FAILED: &str = "item_decode_failed";
    pub const ITEM_QUARANTINED: &str = "item_quarantined";
    pub const ITEM_MARKER_INVALID: &str = "item_marker_invalid";
    pub const ANALYSIS_FAILED: &str = "analysis_failed";
    pub const DECODE_GLITCH: &str = "decode_glitch";
    /// A voice's decoder fell behind and its ring ran dry (1.4, 1.5).
    pub const DECODE_SLOW: &str = "decode_slow";
    pub const MEDIA_STORE_FAILING: &str = "media_store_failing";
    /// The emergency playlist itself cannot be played (its files are gone or unreadable).
    pub const EMERGENCY_FAILED: &str = "emergency_failed";
    pub const PLAYLIST_DEPLETED: &str = "playlist_depleted";
    pub const QUEUE_STARVED: &str = "queue_starved";
    pub const DEVICE_LOST: &str = "device_lost";
    pub const XRUN_BURST: &str = "xrun_burst";
    pub const ENGINE_STALL: &str = "engine_stall";
    pub const RESTARTED: &str = "restarted";
    pub const SAFE_MODE: &str = "safe_mode";
    pub const DISK_LOW: &str = "disk_low";
    pub const DISK_FULL: &str = "disk_full";
    pub const STATE_DB_CORRUPT: &str = "state_db_corrupt";
    pub const DB_UNAVAILABLE: &str = "db_unavailable";
    pub const LIBRARY_DB_UNAVAILABLE: &str = "library_db_unavailable";
    pub const CLOCK_JUMP: &str = "clock_jump";
    /// The nightly `VACUUM INTO` snapshot failed or did not pass `integrity_check` (5.5, 12.1).
    pub const BACKUP_CHECK_FAILED: &str = "backup_check_failed";
    /// The stream has had no listener for `[stream] no_listener_alert_s` (60 s by default) and
    /// `expect_listener` is set (1.3a, 12). A warning: OBS may be closed on purpose.
    pub const STREAM_NO_LISTENER: &str = "stream_no_listener";
    /// More than 1 s of audio is queued for the stream encoder (1.3a).
    pub const STREAM_ENCODER_LAG: &str = "stream_encoder_lag";
    /// The stream encoder cannot run, or is failing over and over (1.3a). Critical on a stream-only station
    /// (it is off the air), a warning when a card also plays the program.
    pub const STREAM_ENCODER_FAILED: &str = "stream_encoder_failed";
    /// The loudness controller's gain has sat at a range limit for 10 minutes while the program is further
    /// from the target (6.1). A warning.
    pub const LOUDNESS_AT_LIMIT: &str = "loudness_at_limit";
    /// A block's side effect (an OSC or MIDI message, a web request) could not be sent, after one retry (4.9).
    /// A warning; the audio is never affected.
    pub const BLOCK_FAILED: &str = "block_failed";
    /// The loudness controller has been frozen for more than 30 minutes while the station was playing
    /// (6.1). Info.
    pub const LOUDNESS_FROZEN: &str = "loudness_frozen";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    pub id: i64,
    pub code: String,
    /// What the alert is about within its code (an item id for the per-item codes), `""` for a
    /// station-wide condition. One alert is active per `(code, key)`.
    pub key: String,
    pub severity: Severity,
    pub message: String,
    pub raised_ms: i64,
    pub item_id: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertChange {
    Raised,
    /// The severity of an active alert changed (a `silence` that persists goes critical).
    Escalated,
    Cleared,
}

/// One raise, escalation or clear, for the log and the spool.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertEvent {
    pub change: AlertChange,
    pub alert: Alert,
    pub at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Ok,
    Warning,
    Critical,
}

impl HealthStatus {
    pub fn name(self) -> &'static str {
        match self {
            HealthStatus::Ok => "ok",
            HealthStatus::Warning => "warning",
            HealthStatus::Critical => "critical",
        }
    }

    /// `/health`'s HTTP status (12): 200 for ok and warning, 503 for critical.
    pub fn http_status(self) -> u16 {
        if self == HealthStatus::Critical {
            503
        } else {
            200
        }
    }
}

#[derive(Debug, Default)]
pub struct Alerts {
    active: BTreeMap<(String, String), Alert>,
    next_id: i64,
    pending: Vec<AlertEvent>,
    /// Every event since the process started (bounded), for tests and the alerts page.
    pub history: Vec<AlertEvent>,
    /// How many times each code was raised (`/metrics`).
    pub raised_total: BTreeMap<String, u64>,
}

const HISTORY_MAX: usize = 2000;

impl Alerts {
    pub fn new(next_id: i64) -> Self {
        Self {
            next_id: next_id.max(1),
            ..Self::default()
        }
    }

    fn emit(&mut self, change: AlertChange, alert: Alert, at_ms: i64) {
        let ev = AlertEvent {
            change,
            alert,
            at_ms,
        };
        self.history.push(ev.clone());
        if self.history.len() > HISTORY_MAX {
            self.history.drain(..HISTORY_MAX / 2);
        }
        self.pending.push(ev);
    }

    /// Raise `(code, key)`. Returns true when it was not active (a new alert); an active alert keeps
    /// its message, and a higher severity escalates it.
    pub fn raise(
        &mut self,
        code: &str,
        key: &str,
        severity: Severity,
        message: impl Into<String>,
        item_id: Option<i64>,
        now_ms: i64,
    ) -> bool {
        let k = (code.to_string(), key.to_string());
        if let Some(a) = self.active.get_mut(&k) {
            if severity > a.severity {
                a.severity = severity;
                let a = a.clone();
                self.emit(AlertChange::Escalated, a, now_ms);
            }
            return false;
        }
        let a = Alert {
            id: self.next_id,
            code: code.into(),
            key: key.into(),
            severity,
            message: message.into(),
            raised_ms: now_ms,
            item_id,
        };
        self.next_id += 1;
        *self.raised_total.entry(code.into()).or_default() += 1;
        self.active.insert(k, a.clone());
        self.emit(AlertChange::Raised, a, now_ms);
        true
    }

    /// Clear `(code, key)`. Returns true when it was active.
    pub fn clear(&mut self, code: &str, key: &str, now_ms: i64) -> bool {
        match self.active.remove(&(code.to_string(), key.to_string())) {
            Some(a) => {
                self.emit(AlertChange::Cleared, a, now_ms);
                true
            }
            None => false,
        }
    }

    /// Clear every active alert with this code.
    pub fn clear_code(&mut self, code: &str, now_ms: i64) -> usize {
        let keys: Vec<String> = self
            .active
            .keys()
            .filter(|(c, _)| c == code)
            .map(|(_, k)| k.clone())
            .collect();
        keys.iter().filter(|k| self.clear(code, k, now_ms)).count()
    }

    pub fn is_active(&self, code: &str) -> bool {
        self.active.keys().any(|(c, _)| c == code)
    }

    pub fn is_active_key(&self, code: &str, key: &str) -> bool {
        self.active
            .contains_key(&(code.to_string(), key.to_string()))
    }

    pub fn severity_of(&self, code: &str) -> Option<Severity> {
        self.active
            .iter()
            .filter(|((c, _), _)| c == code)
            .map(|(_, a)| a.severity)
            .max()
    }

    pub fn active(&self) -> Vec<&Alert> {
        let mut v: Vec<&Alert> = self.active.values().collect();
        v.sort_by_key(|a| a.id);
        v
    }

    pub fn status(&self) -> HealthStatus {
        match self.active.values().map(|a| a.severity).max() {
            Some(Severity::Critical) => HealthStatus::Critical,
            Some(Severity::Warning) => HealthStatus::Warning,
            _ => HealthStatus::Ok,
        }
    }

    /// The events since the last call (the station writes them to `state.db`).
    pub fn take_events(&mut self) -> Vec<AlertEvent> {
        std::mem::take(&mut self.pending)
    }

    /// How many times `code` was raised in the history (tests).
    pub fn count_raised(&self, code: &str) -> usize {
        self.history
            .iter()
            .filter(|e| e.change == AlertChange::Raised && e.alert.code == code)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raising_is_idempotent_and_clearing_reports_it() {
        let mut a = Alerts::new(1);
        assert!(a.raise("silence", "", Severity::Warning, "quiet", None, 10));
        assert!(!a.raise("silence", "", Severity::Warning, "quiet", None, 20));
        assert_eq!(a.status(), HealthStatus::Warning);
        assert!(!a.raise("silence", "", Severity::Critical, "", None, 30));
        assert_eq!(a.status(), HealthStatus::Critical);
        assert_eq!(a.status().http_status(), 503);
        assert!(a.clear("silence", "", 40));
        assert!(!a.clear("silence", "", 50));
        assert_eq!(a.status().http_status(), 200);
        let ev = a.take_events();
        assert_eq!(
            ev.iter().map(|e| e.change).collect::<Vec<_>>(),
            vec![
                AlertChange::Raised,
                AlertChange::Escalated,
                AlertChange::Cleared
            ]
        );
        assert!(a.take_events().is_empty());
    }

    #[test]
    fn per_item_alerts_are_keyed_by_item() {
        let mut a = Alerts::new(5);
        assert!(a.raise("item_quarantined", "7", Severity::Warning, "x", Some(7), 1));
        assert!(a.raise("item_quarantined", "8", Severity::Warning, "y", Some(8), 1));
        assert_eq!(a.active().len(), 2);
        assert_eq!(a.active()[0].id, 5);
        assert_eq!(a.clear_code("item_quarantined", 2), 2);
        assert_eq!(a.status(), HealthStatus::Ok);
    }
}
