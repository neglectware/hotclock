//! Which station or stations the screen shows (architecture 11.2b): the box's view setting, the rotation and
//! the pin on a critical alert. Pure: the caller passes the stations and the time.

use crate::model::StationModel;
use crate::scene::Shown;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    Single,
    Split,
    Rotate,
    /// One station's processing: the gain-reduction meters of its chain (11.2c).
    Processing,
}

impl ViewMode {
    pub fn parse(s: &str) -> Option<ViewMode> {
        match s {
            "single" => Some(ViewMode::Single),
            "split" => Some(ViewMode::Split),
            "rotate" => Some(ViewMode::Rotate),
            "processing" => Some(ViewMode::Processing),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            ViewMode::Single => "single",
            ViewMode::Split => "split",
            ViewMode::Rotate => "rotate",
            ViewMode::Processing => "processing",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ViewSettings {
    pub view: ViewMode,
    /// The station `single` shows; `None` is the first configured.
    pub station: Option<String>,
    pub rotate_s: u32,
    pub pin_on_alert: bool,
    /// `rotate` also shows each station's processing view, after its normal one.
    pub rotate_processing: bool,
}

impl Default for ViewSettings {
    fn default() -> Self {
        ViewSettings {
            view: ViewMode::Single,
            station: None,
            rotate_s: 20,
            pin_on_alert: true,
            rotate_processing: false,
        }
    }
}

impl ViewSettings {
    /// From the library's `display` payload: `{view, station, rotate_s, rotate_pin_on_alert}`.
    pub fn from_json(v: &serde_json::Value, base: &ViewSettings) -> ViewSettings {
        ViewSettings {
            view: v["view"]
                .as_str()
                .and_then(ViewMode::parse)
                .unwrap_or(base.view),
            station: match v.get("station") {
                Some(serde_json::Value::String(s)) => Some(s.clone()),
                Some(serde_json::Value::Null) => None,
                _ => base.station.clone(),
            },
            rotate_s: v["rotate_s"]
                .as_u64()
                .map_or(base.rotate_s, |n| n.clamp(3, 600) as u32),
            pin_on_alert: v["rotate_pin_on_alert"]
                .as_bool()
                .unwrap_or(base.pin_on_alert),
            rotate_processing: v["rotate_processing"]
                .as_bool()
                .unwrap_or(base.rotate_processing),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({"view": self.view.name(), "station": self.station, "rotate_s": self.rotate_s, "rotate_pin_on_alert": self.pin_on_alert, "rotate_processing": self.rotate_processing})
    }
}

#[derive(Default)]
pub struct Presenter {
    pub settings: ViewSettings,
    rotate_start_ms: Option<i64>,
    pinned: Option<usize>,
}

impl Presenter {
    pub fn new(settings: ViewSettings) -> Presenter {
        Presenter {
            settings,
            rotate_start_ms: None,
            pinned: None,
        }
    }

    pub fn set(&mut self, s: ViewSettings) {
        if s != self.settings {
            if s.view != self.settings.view
                || s.rotate_s != self.settings.rotate_s
                || s.rotate_processing != self.settings.rotate_processing
            {
                self.rotate_start_ms = None;
            }
            self.settings = s;
        }
    }

    pub fn shown(&mut self, stations: &[StationModel], now_ms: i64) -> Shown {
        let n = stations.len();
        if n == 0 {
            return Shown::Single {
                idx: 0,
                other: None,
                rotate: None,
                pinned: false,
            };
        }
        let first = self
            .settings
            .station
            .as_deref()
            .and_then(|id| stations.iter().position(|s| s.id == id))
            .unwrap_or(0);
        let other_of = |i: usize| (0..n).find(|&j| j != i);
        if self.settings.view == ViewMode::Processing {
            // also on a box with one station: the view was asked for by name
            return Shown::Processing {
                idx: first,
                other: other_of(first),
                rotate: None,
                pinned: false,
            };
        }
        if n == 1 {
            return Shown::Single {
                idx: 0,
                other: None,
                rotate: None,
                pinned: false,
            };
        }
        match self.settings.view {
            ViewMode::Single => Shown::Single {
                idx: first,
                other: other_of(first),
                rotate: None,
                pinned: false,
            },
            ViewMode::Split => Shown::Split { left: 0, right: 1 },
            ViewMode::Processing => unreachable!("handled above"),
            ViewMode::Rotate => {
                // A critical alert on a station pins the view to it until the alert clears.
                let crit = |i: usize| stations[i].critical();
                let pin = if self.settings.pin_on_alert {
                    match self.pinned {
                        Some(p) if crit(p) => Some(p),
                        _ => (0..n).find(|&i| crit(i)),
                    }
                } else {
                    None
                };
                if self.pinned.is_some() && pin.is_none() {
                    // released: the rotation starts over from the station that was pinned
                    self.rotate_start_ms = Some(now_ms);
                }
                self.pinned = pin;
                if let Some(p) = pin {
                    return Shown::Single {
                        idx: p,
                        other: other_of(p),
                        rotate: Some(1.0),
                        pinned: true,
                    };
                }
                let interval = i64::from(self.settings.rotate_s) * 1000;
                let start = *self.rotate_start_ms.get_or_insert(now_ms);
                let el = (now_ms - start).max(0);
                let frac = (el % interval) as f64 / interval as f64;
                if self.settings.rotate_processing {
                    // each station twice in a row: its normal view, then its processing view
                    let slot = (first * 2 + (el / interval) as usize) % (2 * n);
                    let idx = slot / 2;
                    return if slot % 2 == 1 {
                        Shown::Processing {
                            idx,
                            other: other_of(idx),
                            rotate: Some(frac),
                            pinned: false,
                        }
                    } else {
                        Shown::Single {
                            idx,
                            other: other_of(idx),
                            rotate: Some(frac),
                            pinned: false,
                        }
                    };
                }
                let idx = (first + (el / interval) as usize) % n;
                Shown::Single {
                    idx,
                    other: other_of(idx),
                    rotate: Some(frac),
                    pinned: false,
                }
            }
        }
    }

    pub fn is_pinned(&self) -> bool {
        self.pinned.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Alert;

    fn two() -> Vec<StationModel> {
        vec![
            StationModel::new("office", "office"),
            StationModel::new("eve", "eve"),
        ]
    }

    #[test]
    fn one_station_is_always_single() {
        let mut p = Presenter::new(ViewSettings {
            view: ViewMode::Split,
            ..Default::default()
        });
        let one = vec![StationModel::new("a", "a")];
        assert_eq!(
            p.shown(&one, 0),
            Shown::Single {
                idx: 0,
                other: None,
                rotate: None,
                pinned: false
            }
        );
    }

    #[test]
    fn single_picks_the_station_and_names_the_other() {
        let mut p = Presenter::new(ViewSettings {
            station: Some("eve".into()),
            ..Default::default()
        });
        assert_eq!(
            p.shown(&two(), 0),
            Shown::Single {
                idx: 1,
                other: Some(0),
                rotate: None,
                pinned: false
            }
        );
        let mut p = Presenter::new(ViewSettings::default());
        assert_eq!(
            p.shown(&two(), 0),
            Shown::Single {
                idx: 0,
                other: Some(1),
                rotate: None,
                pinned: false
            }
        );
        let mut p = Presenter::new(ViewSettings {
            view: ViewMode::Split,
            ..Default::default()
        });
        assert_eq!(p.shown(&two(), 0), Shown::Split { left: 0, right: 1 });
    }

    #[test]
    fn rotate_alternates_pins_on_a_critical_alert_and_releases() {
        let mut p = Presenter::new(ViewSettings {
            view: ViewMode::Rotate,
            rotate_s: 20,
            ..Default::default()
        });
        let mut st = two();
        let at = |p: &mut Presenter, st: &[StationModel], t: i64| p.shown(st, t);
        assert!(matches!(at(&mut p, &st, 0), Shown::Single { idx: 0, .. }));
        assert!(matches!(
            at(&mut p, &st, 19_999),
            Shown::Single { idx: 0, .. }
        ));
        match at(&mut p, &st, 20_000) {
            Shown::Single {
                idx: 1,
                rotate: Some(f),
                pinned: false,
                ..
            } => assert!(f < 0.01),
            x => panic!("{x:?}"),
        }
        assert!(matches!(
            at(&mut p, &st, 40_000),
            Shown::Single { idx: 0, .. }
        ));
        // a critical alert on the hidden station pins it
        st[1].alerts.push(Alert {
            id: 1,
            code: "silence".into(),
            severity: "critical".into(),
            message: "x".into(),
            raised_ms: None,
        });
        assert!(matches!(
            at(&mut p, &st, 41_000),
            Shown::Single {
                idx: 1,
                pinned: true,
                ..
            }
        ));
        assert!(matches!(
            at(&mut p, &st, 90_000),
            Shown::Single {
                idx: 1,
                pinned: true,
                ..
            }
        ));
        // clearing releases the pin
        st[1].alerts.clear();
        assert!(matches!(
            at(&mut p, &st, 91_000),
            Shown::Single { pinned: false, .. }
        ));
        // a warning does not pin
        st[0].alerts.push(Alert {
            id: 2,
            code: "w".into(),
            severity: "warning".into(),
            message: "x".into(),
            raised_ms: None,
        });
        assert!(matches!(
            at(&mut p, &st, 92_000),
            Shown::Single { pinned: false, .. }
        ));
    }
}
