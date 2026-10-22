//! What the network API (M1.7) needs to read from a running station, without reaching into its
//! private fields: what is on air with its timing, a sync request, the alert acknowledgement by id.

use crate::runtime::Station;
use crate::sched::QEntry;

/// The item on air, as the API describes it.
#[derive(Debug, Clone)]
pub struct OnAirInfo {
    pub entry: QEntry,
    /// The as-run row of this play, when the station has written it.
    pub as_run_id: Option<i64>,
    /// Audible wall time the item began, ms since the Unix epoch.
    pub started_ms: i64,
    /// The file positions the play is bounded by, ms from the start of the file.
    pub bound_in_ms: i64,
    pub bound_out_ms: i64,
    /// Whether an emergency source (deck or RAM loop) is on air.
    pub alt: bool,
    /// The gain the play was given (normalization and run gain), dB.
    pub gain_db: Option<f64>,
}

impl Station {
    pub fn on_air_info(&self) -> Option<OnAirInfo> {
        let entry = self.now_entry()?.clone();
        let id = self.on_air_voice?;
        let placed = self.cond.placed.iter().find(|p| p.id == id)?;
        let as_run_id = self.voices.get(&(id.slot, id.gen)).and_then(|t| t.as_run);
        let started_ms = as_run_id
            .and_then(|rid| self.as_run.iter().find(|r| r.id == rid))
            .map(|r| r.started_ms)?;
        Some(OnAirInfo {
            entry,
            as_run_id,
            started_ms,
            bound_in_ms: placed.bound_in_ms,
            bound_out_ms: placed.bound_out_ms,
            alt: self.fail.on_air_is_alt,
            gain_db: self
                .cond
                .sidecar
                .voices
                .get(placed.record)
                .map(|r| r.gain_db),
        })
    }

    /// The carts that are playing now: `(item_id, started_ms)` of each, once it is audible (the as-run row exists).
    /// Cart voices are not the on-air item, so `now` never shows them; the Stream Deck's Fire Item key reads this.
    pub fn playing_carts(&self) -> Vec<(i64, i64)> {
        let mut out: Vec<(i64, i64)> = self
            .voices
            .values()
            .filter(|t| t.kind == crate::runtime::Kind::Cart)
            .filter_map(|t| {
                let rid = t.as_run?;
                let started = self.as_run.iter().find(|r| r.id == rid)?.started_ms;
                Some((t.entry.item_id, started))
            })
            .collect();
        out.sort_by_key(|c| c.1);
        out
    }

    /// Read the library again at the next step (`POST /api/v1/internal/reload`).
    pub fn request_sync(&mut self) {
        self.last_sync_wall = i64::MIN / 2;
    }

    /// Acknowledge the active alert with this id; `Some(code)` when there was one.
    pub fn ack_alert_id(&mut self, alert_id: i64) -> Option<String> {
        let code = self
            .fail
            .alerts
            .active()
            .into_iter()
            .find(|a| a.id == alert_id)
            .map(|a| a.code.clone())?;
        self.ack(&code);
        Some(code)
    }
}

impl Station {
    /// Test hook: raise an alert by hand.
    pub fn hook_raise_alert(&mut self, code: &str, critical: bool) {
        use crate::alerts::Severity;
        let sev = if critical {
            Severity::Critical
        } else {
            Severity::Warning
        };
        self.fail.alerts.raise(
            code,
            "hook",
            sev,
            "raised by a test hook",
            None,
            self.wall_ms,
        );
    }

    /// Test hook: clear an alert raised by hand.
    pub fn hook_clear_alert(&mut self, code: &str) {
        self.fail.alerts.clear(code, "hook", self.wall_ms);
    }
}
