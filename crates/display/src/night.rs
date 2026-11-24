//! The night color on the panel: which table ([`Lut`]) the frames go out through now. The math is
//! `common::night`, shared with the library service's Settings page; this holds what is in force on the screen:
//! the `night_*` keys (from the global file at start, then from the library's `display_night` messages, so a save
//! on the Settings page applies with no restart), the place, a preview, and a forced look for measurements.
//!
//! It is evaluated every 5 s (a 60-minute transition moves the brightest blue entry about once every 30 s by one
//! code value, so a step is never visible) and at once after a change. The table is the identity by day and when
//! off, and then the pass over the pixels is skipped.

use common::config::DisplayConfig;
use common::night::{self, Lut, NightSettings, NightState, Place};
use common::timespec::Zone;
use serde_json::{json, Value};
use std::path::PathBuf;

/// How often the look is evaluated when nothing changed.
pub const EVAL_MS: i64 = 5_000;

pub struct NightCtl {
    cfg: DisplayConfig,
    settings: NightSettings,
    zone: Zone,
    place: Result<Place, String>,
    /// The place the library found on the internet (`night_location = "internet"`), kept across restarts.
    internet: Option<Place>,
    /// `(until_ms, kelvin, dim)` of the Preview button.
    preview: Option<(i64, f64, f64)>,
    /// `hotclock display --night-kelvin/--night-dim`: this look, always.
    force: Option<(f64, f64)>,
    cache: Option<PathBuf>,
    next_eval_ms: i64,
    pub lut: Lut,
    pub state: Option<NightState>,
}

impl NightCtl {
    /// From the global file's `[display]` block; `cache` is `night.json` in the state directory (the last payload of
    /// the library, for its internet place).
    pub fn new(cfg: &DisplayConfig, force: Option<(f64, f64)>, cache: Option<PathBuf>) -> NightCtl {
        let internet = cache
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| Place::from_json(&v["place"]).filter(|p| p.from.starts_with("internet")));
        let mut n = NightCtl {
            cfg: cfg.clone(),
            settings: NightSettings::from_config(cfg),
            zone: Zone::utc(),
            place: Err(String::new()),
            internet,
            preview: None,
            force,
            cache,
            next_eval_ms: 0,
            lut: Lut::identity(),
            state: None,
        };
        n.rebuild(None);
        n
    }

    /// The settings, the zone and the place again; `place` is the library's answer when it gave one.
    fn rebuild(&mut self, place: Option<Result<Place, String>>) {
        self.settings = NightSettings::from_config(&self.cfg);
        self.zone = self.settings.zone().unwrap_or_else(|e| {
            tracing::warn!("display: night color: {e}; using UTC");
            Zone::utc()
        });
        self.place = place.unwrap_or_else(|| {
            night::resolve_place(&self.settings, &self.zone, self.internet.as_ref())
        });
        self.next_eval_ms = 0;
    }

    /// The library's `display_night` payload: `{settings, place, error}`.
    pub fn on_library(&mut self, v: &Value) {
        if !v["settings"].is_object() {
            return;
        }
        NightSettings::apply_config_json(&mut self.cfg, &v["settings"]);
        let place = match Place::from_json(&v["place"]) {
            Some(p) => {
                if p.from.starts_with("internet") {
                    self.internet = Some(p.clone());
                }
                Ok(p)
            }
            None => Err(v["error"].as_str().unwrap_or("no location").to_string()),
        };
        self.rebuild(Some(place));
        if let Some(p) = &self.cache {
            let tmp = p.with_extension("tmp");
            if std::fs::write(&tmp, v.to_string())
                .and_then(|()| std::fs::rename(&tmp, p))
                .is_err()
            {
                tracing::debug!("display: cannot write {}", p.display());
            }
        }
    }

    /// The Preview button: the night look (the page's kelvin and dim, else the saved ones) until `until_ms`.
    pub fn on_preview(&mut self, v: &Value, now_ms: i64) {
        let until = v["until_ms"]
            .as_i64()
            .unwrap_or(now_ms + night::PREVIEW_S * 1000)
            .min(now_ms + 60_000);
        let k = v["kelvin"]
            .as_f64()
            .unwrap_or(self.settings.kelvin)
            .clamp(night::MIN_K, night::MAX_K);
        let d = v["dim_percent"]
            .as_f64()
            .map_or(self.settings.dim, |p| p / 100.0)
            .clamp(0.0, night::MAX_DIM);
        tracing::info!(
            "display: night color: preview {k:.0} K, dim {:.0} %, {} s",
            d * 100.0,
            (until - now_ms) / 1000
        );
        self.preview = Some((until, k, d));
        self.next_eval_ms = 0;
    }

    /// Evaluate when due; true when the table changed (the caller then repaints the whole screen).
    pub fn update(&mut self, now_ms: i64) -> bool {
        if now_ms < self.next_eval_ms {
            return false;
        }
        self.next_eval_ms = now_ms + EVAL_MS;
        let st = match self.force {
            Some((k, d)) => NightState {
                phase: night::Phase::Night,
                progress: 1.0,
                kelvin: k,
                dim: d,
                today: None,
                next: None,
                error: None,
            },
            None => {
                let mut s = self.settings.clone();
                let mut until = None;
                if let Some((u, k, d)) = self.preview {
                    if now_ms < u {
                        (s.kelvin, s.dim, until) = (k, d, Some(u));
                        // back to the real look the moment the preview ends
                        self.next_eval_ms = self.next_eval_ms.min(u);
                    } else {
                        self.preview = None;
                    }
                }
                night::evaluate(&s, self.place.as_ref().ok(), &self.zone, now_ms, until)
            }
        };
        let was = self.state.as_ref().map(|s| (s.phase, s.error.clone()));
        if was != Some((st.phase, st.error.clone())) {
            match &st.error {
                Some(e) => tracing::warn!("display: night color: {} ({e})", st.text()),
                None => tracing::info!("display: night color: {}", st.text()),
            }
        }
        let lut = st.lut();
        self.state = Some(st);
        if lut != self.lut {
            self.lut = lut;
            true
        } else {
            false
        }
    }

    /// One line for the log and the tests: `Night (3400 K)`.
    pub fn text(&self) -> String {
        self.state
            .as_ref()
            .map_or_else(|| "not evaluated".into(), NightState::text)
    }

    pub fn place_json(&self) -> Value {
        match &self.place {
            Ok(p) => p.to_json(),
            Err(e) => json!({"error": e}),
        }
    }
}
