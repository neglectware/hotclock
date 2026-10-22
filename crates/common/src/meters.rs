//! Meter ballistics (architecture 11.2, 11.3), applied by each viewer to the program bars. The meter frame carries
//! both the as-measured PPM (IEC type I, 5 ms integration) and the sample peak of the last 33 ms, un-ballisticked;
//! a viewer either shows the PPM as it is or runs its own attack and fall on the peak. The settings are the box's
//! (one set for the native display and the web meters), so changing them never touches the audio path or the
//! measured PPM. The JavaScript twin is `crates/serve/web/ballistics.js`.

use serde_json::{json, Value};

/// Where a bar's level comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The frame's PPM, shown as measured.
    Ppm,
    /// The frame's sample peak, with the viewer's own attack and release.
    Peak,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ballistics {
    pub preset: String,
    pub source: Source,
    /// Time for the bar to rise 20 dB, in ms; 0 is instant. (Peak source only.)
    pub attack_ms: f64,
    /// How fast the bar falls, dB per second. (Peak source only.)
    pub release_db_s: f64,
    pub hold: bool,
    pub hold_ms: f64,
    /// After the hold, the tick falls at this rate toward the bar; 0 drops it to the current peak at once.
    pub hold_fall_db_s: f64,
    /// The gain-reduction bars of the processing view: how fast a bar lets go, dB per second. The attack is always
    /// instant (the frame already carries the deepest reduction of its 33 ms). Not part of a preset: choosing a
    /// preset keeps these two, and editing them never turns the preset into `custom`.
    pub gr_release_db_s: f64,
    /// How long the peak tick of a gain-reduction bar waits before it falls with the bar; 0 turns the tick off.
    pub gr_hold_ms: f64,
}

/// The Optimod-like default of the gain-reduction bars: instant attack, 15 dB/s release, a 1 s peak tick.
pub const GR_RELEASE_DEFAULT: f64 = 15.0;
pub const GR_HOLD_DEFAULT: f64 = 1000.0;

pub const PRESETS: [&str; 5] = ["ppm_iec1", "ppm_bbc", "vu", "digital_fast", "custom"];

pub fn preset_label(p: &str) -> &'static str {
    match p {
        "ppm_iec1" => "PPM (IEC Type I)",
        "ppm_bbc" => "PPM (BBC)",
        "vu" => "VU (300 ms)",
        "digital_fast" => "Digital fast (fast falloff)",
        _ => "Custom",
    }
}

impl Default for Ballistics {
    fn default() -> Self {
        Ballistics::preset("ppm_iec1").expect("preset")
    }
}

impl Ballistics {
    /// A named preset. `ppm_iec1` is today's look: the frame's PPM and a 2 s peak hold.
    pub fn preset(name: &str) -> Option<Ballistics> {
        let b = |preset: &str, source, attack_ms, release_db_s, hold, hold_ms, hold_fall_db_s| {
            Ballistics {
                preset: preset.into(),
                source,
                attack_ms,
                release_db_s,
                hold,
                hold_ms,
                hold_fall_db_s,
                gr_release_db_s: GR_RELEASE_DEFAULT,
                gr_hold_ms: GR_HOLD_DEFAULT,
            }
        };
        Some(match name {
            "ppm_iec1" => b("ppm_iec1", Source::Ppm, 0.0, 11.8, true, 2000.0, 0.0),
            // BBC PPM: 10 ms integration, 24 dB in 2.8 s
            "ppm_bbc" => b("ppm_bbc", Source::Peak, 10.0, 24.0 / 2.8, true, 2000.0, 0.0),
            // VU: 300 ms to rise and to fall (about 20 dB in 300 ms); the frame carries no RMS, so this is
            // peak-driven with the VU's time constants
            "vu" => b("vu", Source::Peak, 300.0, 20.0 / 0.3, false, 0.0, 0.0),
            "digital_fast" => b("digital_fast", Source::Peak, 0.0, 60.0, true, 1000.0, 60.0),
            "custom" => b("custom", Source::Peak, 0.0, 30.0, true, 1500.0, 30.0),
            _ => return None,
        })
    }

    /// Parse a settings object: `{preset}` alone is that preset; any other field makes it custom over `base`.
    pub fn from_json(v: &Value, base: &Ballistics) -> Result<Ballistics, String> {
        let name = v.get("preset").and_then(Value::as_str);
        let fields = [
            "source",
            "attack_ms",
            "release_db_s",
            "hold",
            "hold_ms",
            "hold_fall_db_s",
        ];
        let has_fields = fields.iter().any(|k| v.get(*k).is_some());
        let mut b = match name {
            Some(n) if !has_fields && n != "custom" => {
                Ballistics::preset(n).ok_or_else(|| format!("unknown preset `{n}`"))?
            }
            Some("custom") | None if has_fields || name == Some("custom") => {
                let mut c = base.clone();
                c.preset = "custom".into();
                c
            }
            Some(n) => {
                let mut c = Ballistics::preset(n).ok_or_else(|| format!("unknown preset `{n}`"))?;
                c.preset = "custom".into();
                c
            }
            None => base.clone(),
        };
        // the gain-reduction bars are not part of a preset: whatever was chosen keeps what the box has
        b.gr_release_db_s = base.gr_release_db_s;
        b.gr_hold_ms = base.gr_hold_ms;
        if let Some(s) = v.get("source") {
            b.source = match s.as_str() {
                Some("ppm") => Source::Ppm,
                Some("peak") => Source::Peak,
                _ => return Err("source is `ppm` or `peak`".into()),
            };
        }
        let num = |k: &str, lo: f64, hi: f64, out: &mut f64| -> Result<(), String> {
            if let Some(x) = v.get(k) {
                let n = x.as_f64().ok_or_else(|| format!("{k} must be a number"))?;
                if !(lo..=hi).contains(&n) {
                    return Err(format!("{k} must be between {lo} and {hi}"));
                }
                *out = n;
            }
            Ok(())
        };
        num("attack_ms", 0.0, 5000.0, &mut b.attack_ms)?;
        num("release_db_s", 1.0, 400.0, &mut b.release_db_s)?;
        num("hold_ms", 0.0, 10_000.0, &mut b.hold_ms)?;
        num("hold_fall_db_s", 0.0, 400.0, &mut b.hold_fall_db_s)?;
        num("gr_release_db_s", 1.0, 400.0, &mut b.gr_release_db_s)?;
        num("gr_hold_ms", 0.0, 10_000.0, &mut b.gr_hold_ms)?;
        if let Some(h) = v.get("hold") {
            b.hold = h.as_bool().ok_or("hold must be true or false")?;
        }
        // a full settings object that equals its named preset (what `to_json` writes) is that preset, not a custom one
        if let Some(n) = name.filter(|n| *n != "custom") {
            if let Some(p) = Ballistics::preset(n) {
                let p = Ballistics {
                    gr_release_db_s: b.gr_release_db_s,
                    gr_hold_ms: b.gr_hold_ms,
                    ..p
                };
                if (Ballistics {
                    preset: p.preset.clone(),
                    ..b.clone()
                }) == p
                {
                    return Ok(p);
                }
            }
        }
        Ok(b)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "preset": self.preset, "label": preset_label(&self.preset),
            "source": if self.source == Source::Ppm { "ppm" } else { "peak" },
            "attack_ms": self.attack_ms, "release_db_s": self.release_db_s,
            "hold": self.hold, "hold_ms": self.hold_ms, "hold_fall_db_s": self.hold_fall_db_s,
            "gr_release_db_s": self.gr_release_db_s, "gr_hold_ms": self.gr_hold_ms,
        })
    }

    /// All presets with their values, for the settings page.
    pub fn presets_json() -> Value {
        Value::Array(
            PRESETS
                .iter()
                .filter_map(|p| Ballistics::preset(p))
                .map(|b| b.to_json())
                .collect(),
        )
    }
}

/// The box's meter settings: a default and an override per station (a station without one inherits the default).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeterSettings {
    pub default: Ballistics,
    pub stations: std::collections::BTreeMap<String, Ballistics>,
}

impl MeterSettings {
    pub fn effective(&self, station: &str) -> &Ballistics {
        self.stations.get(station).unwrap_or(&self.default)
    }

    pub fn inherits(&self, station: &str) -> bool {
        !self.stations.contains_key(station)
    }

    /// `{"default": {...}, "stations": {"office": {...}}}`; the earlier flat object is the default alone.
    pub fn from_json(v: &Value) -> Result<MeterSettings, String> {
        let base = Ballistics::default();
        if v.get("default").is_none() && v.get("stations").is_none() {
            return Ok(MeterSettings {
                default: Ballistics::from_json(v, &base)?,
                stations: Default::default(),
            });
        }
        let default = match v.get("default") {
            Some(d) => Ballistics::from_json(d, &base)?,
            None => base,
        };
        let mut stations = std::collections::BTreeMap::new();
        if let Some(o) = v.get("stations").and_then(Value::as_object) {
            for (k, b) in o {
                stations.insert(k.clone(), Ballistics::from_json(b, &default)?);
            }
        }
        Ok(MeterSettings { default, stations })
    }

    pub fn to_json(&self) -> Value {
        json!({"default": self.default.to_json(),
               "stations": self.stations.iter().map(|(k, b)| (k.clone(), b.to_json())).collect::<serde_json::Map<_, _>>()})
    }

    /// A change: to the default (no station), to one station, or `{"inherit": true}` to give a station's override up.
    pub fn apply(&mut self, station: Option<&str>, body: &Value) -> Result<(), String> {
        match station {
            None => self.default = Ballistics::from_json(body, &self.default)?,
            Some(st) if body.get("inherit").and_then(Value::as_bool) == Some(true) => {
                self.stations.remove(st);
            }
            Some(st) => {
                let n = Ballistics::from_json(body, self.effective(st))?;
                self.stations.insert(st.to_string(), n);
            }
        }
        Ok(())
    }
}

/// The state of one bar and its hold tick.
#[derive(Clone, Debug)]
pub struct Meter {
    pub bar: Option<f64>,
    level: f64,
    pub hold: Option<f64>,
    hold_age_ms: f64,
}

pub const FLOOR_DB: f64 = -90.0;

impl Default for Meter {
    fn default() -> Self {
        Meter {
            bar: None,
            level: FLOOR_DB,
            hold: None,
            hold_age_ms: 0.0,
        }
    }
}

impl Meter {
    /// Advance by `dt_s` seconds with the frame's `ppm` and sample `peak` (None when there is no frame).
    pub fn step(&mut self, p: &Ballistics, ppm: Option<f64>, peak: Option<f64>, dt_s: f64) {
        match p.source {
            Source::Ppm => {
                self.bar = ppm;
                self.level = ppm.unwrap_or(FLOOR_DB);
            }
            Source::Peak => {
                let t = peak.unwrap_or(FLOOR_DB);
                let cur = self.level;
                let next = if t > cur {
                    if p.attack_ms <= 0.0 {
                        t
                    } else {
                        t.min(cur + 20.0 / (p.attack_ms / 1000.0) * dt_s)
                    }
                } else {
                    t.max(cur - p.release_db_s * dt_s)
                };
                self.level = next.max(FLOOR_DB);
                self.bar = (next > FLOOR_DB + 0.1).then_some(next);
            }
        }
        if !p.hold {
            self.hold = None;
            return;
        }
        match (peak, self.hold) {
            (Some(pk), None) => {
                self.hold = Some(pk);
                self.hold_age_ms = 0.0;
            }
            (Some(pk), Some(h)) if pk >= h => {
                self.hold = Some(pk);
                self.hold_age_ms = 0.0;
            }
            (_, Some(h)) => {
                self.hold_age_ms += dt_s * 1000.0;
                if self.hold_age_ms > p.hold_ms {
                    if p.hold_fall_db_s > 0.0 {
                        let floor = self.bar.unwrap_or(FLOOR_DB);
                        let n = (h - p.hold_fall_db_s * dt_s).max(floor);
                        self.hold = (n > FLOOR_DB + 0.1).then_some(n);
                    } else {
                        self.hold = peak;
                        self.hold_age_ms = 0.0;
                    }
                }
            }
            (None, None) => {}
        }
    }
}

/// The state of one gain-reduction bar of the processing view and its peak tick. Gain reduction is a positive number
/// of dB here (0 is none); the bar grows downward from the top of its scale. The attack is instant, the release is
/// `gr_release_db_s`, and the tick waits `gr_hold_ms` before it falls toward the bar at the same rate.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GrMeter {
    pub bar: f64,
    pub hold: Option<f64>,
    hold_age_ms: f64,
}

impl GrMeter {
    /// Advance by `dt_s` seconds with the frame's reduction (`None`: no value, which reads as none).
    pub fn step(&mut self, p: &Ballistics, gr_db: Option<f64>, dt_s: f64) {
        let g = gr_db.map_or(0.0, |v| (-v).max(0.0));
        self.bar = if g >= self.bar {
            g
        } else {
            (self.bar - p.gr_release_db_s * dt_s).max(g)
        };
        if p.gr_hold_ms <= 0.0 {
            self.hold = None;
            return;
        }
        match self.hold {
            None => {
                if g > 0.05 {
                    self.hold = Some(g);
                    self.hold_age_ms = 0.0;
                }
            }
            Some(h) if g >= h => {
                self.hold = Some(g);
                self.hold_age_ms = 0.0;
            }
            Some(h) => {
                self.hold_age_ms += dt_s * 1000.0;
                if self.hold_age_ms > p.gr_hold_ms {
                    let n = (h - p.gr_release_db_s * dt_s).max(self.bar);
                    self.hold = (n > 0.05).then_some(n);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(p: &Ballistics, peaks: &[Option<f64>], dt: f64) -> Meter {
        let mut m = Meter::default();
        for pk in peaks {
            m.step(p, *pk, *pk, dt);
        }
        m
    }

    #[test]
    fn falloff_from_0_to_minus_20_takes_20_over_the_rate() {
        let dt = 1.0 / 30.0;
        for name in ["ppm_bbc", "vu", "digital_fast"] {
            let p = Ballistics::preset(name).unwrap();
            let mut m = run(&p, &[Some(0.0); 400], dt);
            assert!(m.bar.is_some_and(|b| b > -0.001), "{name} settles at 0 dB");
            let mut t = 0.0;
            while m.bar.is_some_and(|b| b > -20.0) {
                m.step(&p, None, None, dt);
                t += dt;
            }
            let want = 20.0 / p.release_db_s;
            assert!(
                (t - want).abs() <= dt + 1e-9,
                "{name}: {t} s against {want} s"
            );
        }
    }

    #[test]
    fn attack_time_is_the_time_to_rise_20_db() {
        let dt = 0.001;
        let p = Ballistics::preset("vu").unwrap();
        let mut m = Meter::default();
        for _ in 0..3000 {
            m.step(&p, Some(-20.0), Some(-20.0), dt);
        }
        assert!(m.bar.is_some_and(|b| (b + 20.0).abs() < 1e-6));
        let mut t = 0.0;
        while m.bar.is_none_or(|b| b < -0.001) {
            m.step(&p, Some(0.0), Some(0.0), dt);
            t += dt;
        }
        // from the floor (-90) the bar needs 90 dB; from -20 it needs 20 dB: 300 ms
        assert!((t - 0.3).abs() < 0.01, "{t}");
        // instant attack
        let p = Ballistics::preset("digital_fast").unwrap();
        let m = run(&p, &[Some(-3.0)], dt);
        assert_eq!(m.bar, Some(-3.0));
    }

    #[test]
    fn ppm_source_shows_the_frame_as_measured_with_a_2_s_hold() {
        let p = Ballistics::default();
        let mut m = Meter::default();
        m.step(&p, Some(-18.5), Some(-17.0), 0.033);
        assert_eq!((m.bar, m.hold), (Some(-18.5), Some(-17.0)));
        // the hold stays for 2 s, then re-latches to the current peak
        for _ in 0..55 {
            m.step(&p, Some(-30.0), Some(-30.0), 0.033);
        }
        assert_eq!(m.hold, Some(-17.0));
        for _ in 0..10 {
            m.step(&p, Some(-30.0), Some(-30.0), 0.033);
        }
        assert_eq!(m.hold, Some(-30.0));
        m.step(&p, None, None, 0.033);
        assert_eq!(m.bar, None);
    }

    #[test]
    fn hold_off_and_the_tick_falling_at_its_own_rate() {
        let mut p = Ballistics::preset("digital_fast").unwrap();
        let mut m = run(&p, &[Some(-6.0)], 0.033);
        assert_eq!(m.hold, Some(-6.0));
        // 1 s hold, then 60 dB/s toward the bar
        for _ in 0..40 {
            m.step(&p, Some(-40.0), Some(-40.0), 0.033);
        }
        assert!(
            m.hold.unwrap() < -6.0 && m.hold.unwrap() >= -40.0,
            "{:?}",
            m.hold
        );
        p.hold = false;
        m.step(&p, Some(-40.0), Some(-40.0), 0.033);
        assert_eq!(m.hold, None);
    }

    #[test]
    fn per_station_settings_inherit_the_default_until_set() {
        let mut m = MeterSettings::default();
        assert!(m.inherits("office") && m.effective("office").preset == "ppm_iec1");
        m.apply(None, &json!({"preset": "vu"})).unwrap();
        assert_eq!(m.effective("eve").preset, "vu");
        m.apply(Some("office"), &json!({"preset": "digital_fast"}))
            .unwrap();
        assert!(!m.inherits("office") && m.inherits("eve"));
        assert_eq!(
            (
                m.effective("office").preset.as_str(),
                m.effective("eve").preset.as_str()
            ),
            ("digital_fast", "vu")
        );
        // a field edit on a station starts from what it uses now
        m.apply(Some("eve"), &json!({"release_db_s": 33.0}))
            .unwrap();
        assert_eq!(
            (
                m.effective("eve").preset.as_str(),
                m.effective("eve").attack_ms
            ),
            ("custom", 300.0)
        );
        m.apply(Some("office"), &json!({"inherit": true})).unwrap();
        assert!(m.inherits("office"));
        // round trip, and the earlier flat file is the default
        assert_eq!(MeterSettings::from_json(&m.to_json()).unwrap(), m);
        let old =
            MeterSettings::from_json(&Ballistics::preset("ppm_bbc").unwrap().to_json()).unwrap();
        assert_eq!(
            (old.default.preset.as_str(), old.stations.len()),
            ("ppm_bbc", 0)
        );
        assert!(m.apply(Some("x"), &json!({"preset": "nope"})).is_err());
    }

    #[test]
    fn json_round_trip_validation_and_custom() {
        let base = Ballistics::default();
        let b = Ballistics::from_json(&json!({"preset": "digital_fast"}), &base).unwrap();
        assert_eq!(b, Ballistics::preset("digital_fast").unwrap());
        let c = Ballistics::from_json(&json!({"release_db_s": 25.0}), &b).unwrap();
        assert_eq!(
            (c.preset.as_str(), c.release_db_s, c.attack_ms),
            ("custom", 25.0, 0.0)
        );
        assert_eq!(Ballistics::from_json(&c.to_json(), &base).unwrap(), c);
        assert!(Ballistics::from_json(&json!({"preset": "nope"}), &base).is_err());
        assert!(Ballistics::from_json(&json!({"release_db_s": 0.0}), &base).is_err());
        assert!(Ballistics::from_json(&json!({"source": "rms"}), &base).is_err());
        assert_eq!(Ballistics::presets_json().as_array().unwrap().len(), 5);
    }

    #[test]
    fn gr_bars_attack_at_once_release_at_their_rate_and_the_tick_waits() {
        let p = Ballistics::default();
        assert_eq!((p.gr_release_db_s, p.gr_hold_ms), (15.0, 1000.0));
        let dt = 1.0 / 30.0;
        let mut m = GrMeter::default();
        m.step(&p, Some(-12.0), dt);
        assert_eq!((m.bar, m.hold), (12.0, Some(12.0)));
        // the reduction ends: 12 dB at 15 dB/s takes 0.8 s, to within a frame
        let mut t = 0.0;
        while m.bar > 0.0 {
            m.step(&p, Some(0.0), dt);
            t += dt;
        }
        assert!((t - 0.8).abs() <= dt + 1e-9, "{t}");
        // the tick waited its 1 s (it was set at the first step, 0.8 s ago), then falls toward the bar
        assert!(m.hold.is_some_and(|h| h > 0.05), "{:?}", m.hold);
        for _ in 0..30 {
            m.step(&p, Some(0.0), dt);
        }
        assert_eq!(m.hold, None);
        // a deeper bite replaces the tick at once; hold off turns the tick off
        let mut m = GrMeter::default();
        m.step(&p, Some(-3.0), dt);
        m.step(&p, Some(-9.0), dt);
        assert_eq!(m.hold, Some(9.0));
        let mut off = p.clone();
        off.gr_hold_ms = 0.0;
        m.step(&off, Some(-9.0), dt);
        assert_eq!(m.hold, None);
        // a frame with no value is no reduction
        let mut m = GrMeter::default();
        m.step(&p, None, dt);
        assert_eq!(m.bar, 0.0);
    }

    #[test]
    fn gr_settings_ride_beside_the_preset_without_turning_it_custom() {
        let mut m = MeterSettings::default();
        m.apply(None, &json!({"gr_release_db_s": 30.0, "gr_hold_ms": 0.0}))
            .unwrap();
        let d = m.effective("office");
        assert_eq!(
            (d.preset.as_str(), d.gr_release_db_s, d.gr_hold_ms),
            ("ppm_iec1", 30.0, 0.0)
        );
        // choosing another preset keeps them
        m.apply(None, &json!({"preset": "vu"})).unwrap();
        let d = m.effective("office");
        assert_eq!(
            (d.preset.as_str(), d.gr_release_db_s, d.gr_hold_ms),
            ("vu", 30.0, 0.0)
        );
        // a station override starts from what the box default uses, and a round trip through JSON is the same
        m.apply(Some("eve"), &json!({"gr_release_db_s": 6.0}))
            .unwrap();
        assert_eq!(m.effective("eve").preset, "vu");
        assert_eq!(m.effective("eve").gr_release_db_s, 6.0);
        let back = MeterSettings::from_json(&m.to_json()).unwrap();
        assert_eq!(back, m);
        assert!(m
            .apply(None, &json!({"gr_release_db_s": 0.0}))
            .unwrap_err()
            .contains("gr_release_db_s"));
    }
}
