//! What the display knows about one station: the JSON events of 7.4 and the meter frames of 11.3 folded into
//! one state, plus the client-side dynamics (peak hold, spectrum fall) that the frame leaves to the client.
//! Nothing here draws and nothing here reads the clock: every function takes the wall time in milliseconds.

use crate::frame::Frame;
use common::meters::{Ballistics, GrMeter, Meter, Source};
use serde_json::Value;

/// RFC 3339 with a `Z` or a numeric offset, to Unix milliseconds. The station writes UTC with milliseconds.
pub fn parse_iso_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') {
        return None;
    }
    let n = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (
        n(0, 4)?,
        n(5, 7)?,
        n(8, 10)?,
        n(11, 13)?,
        n(14, 16)?,
        n(17, 19)?,
    );
    let mut i = 19;
    let mut ms = 0i64;
    if b.get(i) == Some(&b'.') {
        let start = i + 1;
        i = start;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let frac = &s[start..i.min(start + 3)];
        ms = frac.parse::<i64>().ok()? * 10i64.pow(3 - frac.len() as u32);
    }
    let off = match b.get(i) {
        Some(b'+') | Some(b'-') => {
            let sign = if b[i] == b'+' { 1 } else { -1 };
            let hh = s.get(i + 1..i + 3)?.parse::<i64>().ok()?;
            let mm = s
                .get(i + 4..i + 6)
                .and_then(|x| x.parse::<i64>().ok())
                .unwrap_or(0);
            sign * (hh * 60 + mm)
        }
        _ => 0,
    };
    // days from civil (Howard Hinnant)
    let (yy, m) = if mo <= 2 {
        (y - 1, mo + 9)
    } else {
        (y, mo - 3)
    };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + h) * 60 + mi - off) * 60_000 + se * 1000 + ms)
}

/// Unix milliseconds to RFC 3339 UTC (the inverse of [`parse_iso_ms`], for feeds and tests).
pub fn iso_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms.rem_euclid(1000)
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct Alert {
    pub id: i64,
    pub code: String,
    pub severity: String,
    pub message: String,
    pub raised_ms: Option<i64>,
}

impl Alert {
    pub fn critical(&self) -> bool {
        self.severity == "critical"
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Now {
    pub entry_id: i64,
    pub item_id: Option<i64>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub category: String,
    pub art_url: Option<String>,
    pub started_ms: i64,
    pub segue_ms: Option<i64>,
    pub ends_ms: i64,
    pub continuity_next: bool,
    pub origin: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueueEntry {
    /// `track` or `block`.
    pub kind: String,
    pub block_type: String,
    pub name: String,
    pub title: String,
    pub artist: String,
    pub art_url: Option<String>,
    pub origin: String,
    pub pinned: bool,
    pub planned_start_ms: Option<i64>,
    /// For a block: milliseconds from the snapshot until it fires, and when the snapshot was taken.
    pub in_ms: Option<i64>,
    /// `END +3 s`: the back-timing plan, on the block that ends the back-timed fill.
    pub end_label: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AsRun {
    pub started_ms: i64,
    pub title: String,
    pub artist: String,
    pub category: String,
    pub end_reason: String,
    pub deviation_ms: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chain {
    pub rev: u16,
    pub bypassed: bool,
    pub bands: usize,
    pub crossovers: Vec<f64>,
    pub makeup_db: Vec<f64>,
    /// `(name, enabled)` in chain order, as the station lists them.
    pub stages: Vec<(String, bool)>,
    /// The true-peak limiter's ceiling in dBTP, when the station says one.
    pub ceiling_dbtp: Option<f64>,
}

impl Chain {
    pub fn from_json(v: &Value) -> Option<Chain> {
        let mb = v.get("multiband")?;
        let bands = mb.get("bands").and_then(Value::as_u64).unwrap_or(0) as usize;
        let crossovers: Vec<f64> = mb
            .get("crossovers_hz")
            .or_else(|| mb.get("crossovers"))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        let makeup_db = match v.get("makeup_db") {
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_f64).collect(),
            Some(n) if n.is_number() => vec![n.as_f64().unwrap_or(0.0); bands.max(1)],
            _ => vec![],
        };
        let stages = v
            .get("stages")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|s| {
                        Some((
                            s.get("name")?.as_str()?.to_string(),
                            s.get("enabled").and_then(Value::as_bool).unwrap_or(false),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let ceiling_dbtp = v
            .get("stages")
            .and_then(Value::as_array)
            .and_then(|a| {
                a.iter()
                    .find(|s| s.get("name").and_then(Value::as_str) == Some("limiter"))
            })
            .and_then(|s| s.get("ceiling_dbtp"))
            .and_then(Value::as_f64);
        Some(Chain {
            ceiling_dbtp,
            rev: v.get("chain_cfg_rev").and_then(Value::as_u64).unwrap_or(0) as u16,
            bypassed: v.get("bypassed").and_then(Value::as_bool).unwrap_or(false),
            bands,
            crossovers,
            makeup_db,
            stages,
        })
    }
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn ms(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(Value::as_str).and_then(parse_iso_ms)
}

pub fn now_from_json(v: &Value) -> Option<Now> {
    if v.is_null() || v.get("title").is_none() {
        return None;
    }
    Some(Now {
        entry_id: v.get("entry_id").and_then(Value::as_i64).unwrap_or(0),
        item_id: v.get("item_id").and_then(Value::as_i64),
        title: s(v, "title"),
        artist: s(v, "artist"),
        album: s(v, "album"),
        category: s(v, "category"),
        art_url: v.get("art_url").and_then(Value::as_str).map(str::to_string),
        started_ms: ms(v, "started_at")?,
        segue_ms: ms(v, "segue_at"),
        ends_ms: ms(v, "ends_at")?,
        continuity_next: v
            .get("continuity_next")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        origin: s(v, "origin"),
    })
}

pub fn queue_from_json(v: &Value) -> Vec<QueueEntry> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|e| QueueEntry {
                    kind: e
                        .get("kind")
                        .and_then(Value::as_str)
                        .unwrap_or("track")
                        .to_string(),
                    block_type: s(e, "block_type"),
                    end_label: s(e, "end_label"),
                    name: s(e, "name"),
                    title: s(e, "title"),
                    artist: s(e, "artist"),
                    art_url: e.get("art_url").and_then(Value::as_str).map(str::to_string),
                    origin: s(e, "origin"),
                    pinned: e.get("pinned").and_then(Value::as_bool).unwrap_or(false),
                    planned_start_ms: ms(e, "planned_start"),
                    in_ms: e.get("in_ms").and_then(Value::as_i64),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn alert_from_json(v: &Value) -> Option<Alert> {
    Some(Alert {
        id: v.get("id").and_then(Value::as_i64)?,
        code: s(v, "code"),
        severity: s(v, "severity"),
        message: s(v, "message"),
        raised_ms: ms(v, "raised_at"),
    })
}

pub fn asrun_from_json(v: &Value) -> AsRun {
    AsRun {
        started_ms: ms(v, "started_at").unwrap_or(0),
        title: s(v, "title"),
        artist: s(v, "artist"),
        category: s(v, "category"),
        end_reason: s(v, "end_reason"),
        deviation_ms: v.get("deviation_ms").and_then(Value::as_i64),
    }
}

/// The client's part of the meters (11.2, 11.3): the program bars and their hold ticks run the box's ballistics
/// (`common::meters`), and the spectrum bars fall at 15 dB/s with a cap that waits 1 s and falls at 20 dB/s.
#[derive(Clone, Debug)]
pub struct Dynamics {
    pub params: Ballistics,
    meters: [Meter; 2],
    /// The hold tick of each bar, as `(dB, time)`.
    pub peak_hold: [Option<(f64, i64)>; 2],
    /// The ballisticked bar levels (used when the source is the sample peak; a PPM source is the frame's own).
    pub bar: [Option<f64>; 2],
    pub post: [f64; 64],
    pub pre: [f64; 64],
    pub cap_post: [(f64, i64); 64],
    pub cap_pre: [(f64, i64); 64],
    /// The processing view's gain-reduction bars: bands 1 to 6, then the wideband compressor, the limiter and the
    /// clipper (`GR_SLOTS`), each with the box's gain-reduction ballistics.
    pub gr: [GrMeter; GR_SLOTS],
    /// The level into the chain, with the program bars' ballistics (the sample-peak branch, whatever the source).
    input: Meter,
    last_ms: Option<i64>,
}

/// Bands 1 to 6, wideband, limiter, clipper.
pub const GR_SLOTS: usize = 9;
pub const GR_WIDEBAND: usize = 6;
pub const GR_LIMITER: usize = 7;
pub const GR_CLIPPER: usize = 8;

impl Default for Dynamics {
    fn default() -> Self {
        Dynamics {
            params: Ballistics::default(),
            meters: [Meter::default(), Meter::default()],
            peak_hold: [None, None],
            bar: [None, None],
            post: [-90.0; 64],
            pre: [-90.0; 64],
            cap_post: [(-90.0, 0); 64],
            cap_pre: [(-90.0, 0); 64],
            gr: Default::default(),
            input: Meter::default(),
            last_ms: None,
        }
    }
}

pub const SPECTRUM_FALL_DB_S: f64 = 15.0;
pub const CAP_FALL_DB_S: f64 = 20.0;
pub const CAP_HOLD_MS: i64 = 1000;

impl Dynamics {
    /// The bar the scene draws for channel `i`: the frame's PPM as measured, or the ballisticked peak.
    pub fn level(&self, i: usize, frame: Option<&Frame>) -> Option<f64> {
        match self.params.source {
            Source::Ppm => frame.and_then(|f| f.ppm[i]),
            Source::Peak => self.bar[i],
        }
    }

    /// The level into the chain as the program bars show it, and its peak tick.
    pub fn input(&self) -> (Option<f64>, Option<f64>) {
        (self.input.bar, self.input.hold)
    }

    pub fn update(&mut self, f: Option<&Frame>, now_ms: i64) {
        let dt = self
            .last_ms
            .map_or(0.0, |l| (now_ms - l).clamp(0, 2000) as f64 / 1000.0);
        self.last_ms = Some(now_ms);
        for i in 0..2 {
            let (ppm, peak) = (f.and_then(|f| f.ppm[i]), f.and_then(|f| f.peak[i]));
            let before = self.meters[i].hold;
            self.meters[i].step(&self.params, ppm, peak, dt);
            self.bar[i] = self.meters[i].bar;
            let h = self.meters[i].hold;
            // the stamp is when the tick last changed, for the tests and the log
            self.peak_hold[i] = h.map(|v| {
                (
                    v,
                    if h == before {
                        self.peak_hold[i].map_or(now_ms, |x| x.1)
                    } else {
                        now_ms
                    },
                )
            });
        }
        // the processing view: the gain-reduction bars (a stage that is off reads no reduction) and the input level
        let gr_of = |f: &Frame, i: usize| -> Option<f64> {
            if f.bypassed() {
                return None;
            }
            match i {
                0..=5 => f.stage_enabled(1).then(|| f.band_gr[i]).flatten(),
                GR_WIDEBAND => f.stage_enabled(2).then(|| f.stages[2].gr).flatten(),
                GR_LIMITER => f.stage_enabled(3).then(|| f.stages[3].gr).flatten(),
                _ => f.stage_enabled(4).then(|| f.stages[4].gr).flatten(),
            }
        };
        for i in 0..GR_SLOTS {
            self.gr[i].step(&self.params, f.and_then(|f| gr_of(f, i)), dt);
        }
        let input_peak = f.and_then(|f| f.input_peak());
        let mut as_peak = self.params.clone();
        as_peak.source = Source::Peak;
        self.input.step(&as_peak, None, input_peak, dt);
        for i in 0..64 {
            let (np, nq) = f.map_or((-90.0, -90.0), |f| (f.post[i], f.pre[i]));
            self.post[i] = np.max(self.post[i] - SPECTRUM_FALL_DB_S * dt);
            self.pre[i] = nq.max(self.pre[i] - SPECTRUM_FALL_DB_S * dt);
            for (cap, bar) in [
                (&mut self.cap_post[i], self.post[i]),
                (&mut self.cap_pre[i], self.pre[i]),
            ] {
                if bar >= cap.0 {
                    *cap = (bar, now_ms);
                } else if now_ms - cap.1 > CAP_HOLD_MS {
                    cap.0 = (cap.0 - CAP_FALL_DB_S * dt).max(bar);
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct StationModel {
    pub id: String,
    pub display_name: String,
    pub connected: bool,
    /// `playing`, `held`, `stopped`, `safe` ...
    pub state: String,
    pub mode: String,
    pub hold: bool,
    pub safe_mode: bool,
    pub alerts: Vec<Alert>,
    pub now: Option<Now>,
    pub queue: Vec<QueueEntry>,
    /// When `queue`'s `in_ms` values were true.
    pub queue_at_ms: i64,
    pub asrun: Vec<AsRun>,
    pub frame: Option<Frame>,
    pub frame_at_ms: i64,
    pub chain: Option<Chain>,
    /// The station's own idea of the elapsed time minus the local one, from `timing` (11.3).
    pub skew_ms: i64,
    pub dynamics: Dynamics,
    /// Bumped on every change that needs an artwork fetch or a chain fetch decision.
    pub chain_rev_seen: Option<u16>,
    /// The loudness controller's gain range `(loudness.gain_min_db, gain_max_db)`, from `GET /api/v1/loudness`.
    pub lc_limits: Option<(f64, f64)>,
}

impl StationModel {
    pub fn new(id: &str, display_name: &str) -> StationModel {
        StationModel {
            id: id.into(),
            display_name: display_name.into(),
            connected: false,
            state: "stopped".into(),
            mode: "auto".into(),
            hold: false,
            safe_mode: false,
            alerts: vec![],
            now: None,
            queue: vec![],
            queue_at_ms: 0,
            asrun: vec![],
            frame: None,
            frame_at_ms: 0,
            chain: None,
            skew_ms: 0,
            dynamics: Dynamics::default(),
            chain_rev_seen: None,
            lc_limits: None,
        }
    }

    /// The controller's gain range from the station's `GET /api/v1/loudness`.
    pub fn set_loudness(&mut self, v: &Value) {
        let s = &v["settings"];
        if let (Some(lo), Some(hi)) = (s["gain_min_db"].as_f64(), s["gain_max_db"].as_f64()) {
            self.lc_limits = Some((lo, hi));
        }
    }

    pub fn name(&self) -> &str {
        if self.display_name.is_empty() {
            &self.id
        } else {
            &self.display_name
        }
    }

    pub fn critical(&self) -> bool {
        self.alerts.iter().any(Alert::critical)
    }

    /// The header goes red on a critical alert, and on a station the display cannot reach.
    pub fn alarming(&self) -> bool {
        !self.connected || self.critical()
    }

    pub fn warning(&self) -> bool {
        self.alerts.iter().any(|a| a.severity == "warning")
    }

    pub fn pill(&self) -> &'static str {
        if self.safe_mode {
            "SAFE MODE"
        } else if self.state == "playing" && self.mode == "assist" {
            "ASSIST"
        } else if self.hold || self.state == "held" {
            "HELD"
        } else if self.state == "playing" {
            "PLAYING"
        } else {
            "STOPPED"
        }
    }

    pub fn set_connected(&mut self, c: bool) {
        self.connected = c;
        if !c {
            self.frame = None;
        }
    }

    /// Apply one JSON message of 7.4 (`{"type": ..., "data": ...}`). `now_ms` is the local wall time.
    pub fn apply_json(&mut self, m: &Value, now_ms: i64) {
        let d = &m["data"];
        match m["type"].as_str().unwrap_or("") {
            "snapshot" => {
                self.apply_state(d);
                self.now = now_from_json(&d["now"]);
                self.queue = queue_from_json(&d["queue"]);
                self.queue_at_ms = now_ms;
                self.alerts = d["alerts"]
                    .as_array()
                    .map(|a| a.iter().filter_map(alert_from_json).collect())
                    .unwrap_or_default();
                if let Some(n) = d["display_name"].as_str() {
                    self.display_name = n.to_string();
                }
            }
            "state" => self.apply_state(d),
            "now" => {
                self.now = now_from_json(d);
                self.skew_ms = 0;
            }
            "timing" => {
                if let (Some(n), Some(el)) = (&self.now, d["elapsed_ms"].as_i64()) {
                    if d["entry_id"].as_i64() == Some(n.entry_id) {
                        self.skew_ms = (el - (now_ms - n.started_ms)).clamp(-5000, 5000);
                    }
                }
            }
            "queue" => {
                self.queue = queue_from_json(&d["entries"]);
                self.queue_at_ms = now_ms;
            }
            "alert" => {
                if let Some(a) = alert_from_json(d) {
                    self.alerts.retain(|x| x.id != a.id);
                    self.alerts.push(a);
                }
            }
            "alert_cleared" => {
                if let Some(id) = d["id"].as_i64() {
                    self.alerts.retain(|x| x.id != id);
                }
            }
            "asrun" => {
                self.asrun.insert(0, asrun_from_json(d));
                self.asrun.truncate(8);
            }
            "chain" => {
                if let Some(c) = Chain::from_json(d) {
                    self.chain_rev_seen = Some(c.rev);
                    self.chain = Some(c);
                }
            }
            _ => {}
        }
    }

    fn apply_state(&mut self, d: &Value) {
        if let Some(x) = d["state"].as_str() {
            self.state = x.to_string();
        }
        if let Some(x) = d["mode"].as_str() {
            self.mode = x.to_string();
        }
        if let Some(x) = d["hold"].as_bool() {
            self.hold = x;
        }
        if let Some(x) = d["safe_mode"].as_bool() {
            self.safe_mode = x;
        }
    }

    pub fn set_history(&mut self, rows: &Value) {
        self.asrun = rows["rows"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|r| !r["ended_at"].is_null())
                    .map(asrun_from_json)
                    .collect()
            })
            .unwrap_or_default();
    }

    pub fn apply_frame(&mut self, f: Frame, now_ms: i64) {
        // A new chain revision means the static configuration must be fetched again (11.2a).
        if self.chain_rev_seen.is_some_and(|r| r != f.chain_cfg_rev) {
            self.chain_rev_seen = None;
        }
        self.frame = Some(f);
        self.frame_at_ms = now_ms;
    }

    /// The current frame, or `None` when none has come for half a second (the meters fall to nothing).
    pub fn live_frame(&self, now_ms: i64) -> Option<&Frame> {
        self.frame
            .as_ref()
            .filter(|_| now_ms - self.frame_at_ms <= 500)
    }

    pub fn needs_chain(&self) -> bool {
        self.connected && self.frame.is_some() && self.chain_rev_seen.is_none()
    }

    /// The item is not (or no longer) being played: the station is held, stopped or in safe mode, or the clock has
    /// passed the item's end. Nothing is extrapolated then: a station held after its last item reads `0:00`, not a
    /// count that grows negative.
    pub fn idle(&self, now_ms: i64) -> bool {
        self.now
            .as_ref()
            .is_some_and(|n| self.state != "playing" || now_ms - self.skew_ms >= n.ends_ms)
    }

    /// How long past the item's end the station has been standing (0 while the item is playing).
    pub fn over_ms(&self, now_ms: i64) -> i64 {
        self.now
            .as_ref()
            .map_or(0, |n| (now_ms - self.skew_ms - n.ends_ms).max(0))
    }

    /// Elapsed time of the item on air, counted locally and corrected by the last `timing` (11.3), never past its length.
    pub fn elapsed_ms(&self, now_ms: i64) -> Option<i64> {
        self.now.as_ref().map(|n| {
            (now_ms - n.started_ms + self.skew_ms).clamp(0, (n.ends_ms - n.started_ms).max(0))
        })
    }

    pub fn remaining_ms(&self, now_ms: i64) -> Option<i64> {
        let n = self.now.as_ref()?;
        Some(if self.idle(now_ms) {
            0
        } else {
            (n.ends_ms - now_ms - self.skew_ms).max(0)
        })
    }

    pub fn segue_left_ms(&self, now_ms: i64) -> Option<i64> {
        let n = self.now.as_ref()?;
        if self.idle(now_ms) {
            return None;
        }
        n.segue_ms.map(|g| (g - now_ms - self.skew_ms).max(0))
    }

    /// A queue entry's planned start, or for a block the time it will fire.
    pub fn block_countdown_ms(&self, e: &QueueEntry, now_ms: i64) -> Option<i64> {
        e.in_ms.map(|m| (m - (now_ms - self.queue_at_ms)).max(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn iso_round_trip() {
        let t = 1_790_000_000_123i64;
        assert_eq!(parse_iso_ms(&iso_ms(t)), Some(t));
        assert_eq!(
            parse_iso_ms("2026-10-06T20:13:37.363Z"),
            Some(1_791_317_617_363)
        );
        assert_eq!(
            parse_iso_ms("2026-10-06T16:13:37-04:00"),
            parse_iso_ms("2026-10-06T20:13:37Z")
        );
        assert_eq!(parse_iso_ms("nonsense"), None);
    }

    #[test]
    fn snapshot_timing_and_alerts() {
        let mut m = StationModel::new("office", "office");
        let t0 = parse_iso_ms("2026-10-06T20:00:00.000Z").unwrap();
        m.apply_json(
            &json!({"type": "snapshot", "data": {
                "state": "playing", "mode": "auto", "hold": false, "safe_mode": false, "display_name": "Office",
                "now": {"entry_id": 5, "title": "T", "artist": "A", "album": "", "started_at": "2026-10-06T20:00:00.000Z",
                        "segue_at": "2026-10-06T20:03:00.000Z", "ends_at": "2026-10-06T20:03:15.000Z", "art_url": "/x"},
                "queue": [{"kind": "track", "title": "N", "artist": "B", "planned_start": "2026-10-06T20:03:00.000Z"}],
                "alerts": [{"id": 1, "code": "restarted", "severity": "warning", "message": "m", "raised_at": "2026-10-06T19:59:00.000Z"}]
            }}),
            t0,
        );
        assert_eq!(m.name(), "Office");
        assert_eq!(m.pill(), "PLAYING");
        assert_eq!(m.elapsed_ms(t0 + 10_000), Some(10_000));
        assert_eq!(m.remaining_ms(t0 + 10_000), Some(185_000));
        assert_eq!(m.segue_left_ms(t0 + 10_000), Some(170_000));
        assert!(!m.critical() && m.warning());
        m.apply_json(&json!({"type": "alert", "data": {"id": 2, "code": "silence", "severity": "critical", "message": "x"}}), t0);
        assert!(m.critical() && m.alarming());
        m.apply_json(&json!({"type": "alert_cleared", "data": {"id": 2}}), t0);
        assert!(!m.critical());
        // a timing event shifts the local count by the difference
        m.apply_json(
            &json!({"type": "timing", "data": {"entry_id": 5, "elapsed_ms": 10_400}}),
            t0 + 10_000,
        );
        assert_eq!(m.elapsed_ms(t0 + 10_000), Some(10_400));
        assert_eq!(m.remaining_ms(t0 + 10_000), Some(184_600));
        m.set_connected(false);
        assert!(m.alarming());
    }

    #[test]
    fn the_bar_follows_the_presets_ballistics_and_the_ppm_preset_passes_the_frame_through() {
        let mut f = Frame::silent();
        f.ppm = [Some(-20.0), Some(-20.0)];
        f.peak = [Some(0.0), Some(0.0)];
        // default: the frame's PPM as measured
        let mut d = Dynamics::default();
        d.update(Some(&f), 1000);
        assert_eq!(d.level(0, Some(&f)), Some(-20.0));
        // digital fast: instant attack on the peak, then 60 dB/s
        d.params = Ballistics::preset("digital_fast").unwrap();
        d.update(Some(&f), 1033);
        assert_eq!(d.level(0, Some(&f)), Some(0.0));
        let mut t = 0;
        while d.level(0, None).is_some_and(|v| v > -20.0) {
            t += 33;
            d.update(None, 1033 + t);
        }
        // 20 dB at 60 dB/s is 333 ms, to within one frame
        assert!((t - 333).abs() <= 33, "{t} ms");
    }

    #[test]
    fn dynamics_hold_and_fall() {
        let mut d = Dynamics::default();
        let mut f = Frame::silent();
        f.peak = [Some(-6.0), Some(-20.0)];
        f.post[10] = -20.0;
        d.update(Some(&f), 1000);
        assert_eq!(d.peak_hold[0].map(|h| h.0), Some(-6.0));
        assert_eq!(d.post[10], -20.0);
        // the signal stops: the bar falls 15 dB per second, the cap waits 1 s
        d.update(None, 2000);
        assert!((d.post[10] + 35.0).abs() < 1e-9);
        assert_eq!(d.cap_post[10].0, -20.0);
        d.update(None, 3500);
        assert!(d.cap_post[10].0 < -20.0 && d.cap_post[10].0 > -60.0);
        // the hold (set at 1000) expired after 2 s
        assert_eq!(d.peak_hold[0], None);
    }
}
