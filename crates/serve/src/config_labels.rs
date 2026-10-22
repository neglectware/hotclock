//! Names for the configuration history: the raw keys of the station file (`chain.eq.b2_db`, `processing_preset`)
//! shown the way the page's controls name them (`Equalizer · Band 2 gain: -6.0 dB → -8.5 dB`).
//!
//! One source for the chain: the block schema in `common::chain` (`BLOCKS`: the block's label, each setting's label,
//! unit, resolution and kind), the same schema the Processing page is built from. The loudness controller has its own
//! small table below (the card's fields live in the page, `LOUD_FIELDS` in app.js; the labels are kept in step with
//! it). A key without a known label keeps its raw key.

use crate::config_history::Change;
use common::chain::{Kind, Param, BLOCKS};
use serde_json::{json, Value};

/// A change as the page shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub label: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub raw_key: String,
    /// A whole section that came or went (`before`/`after` are empty).
    pub section: Option<&'static str>,
}

impl Shown {
    /// `label: old → new`, or `label: settings added`.
    pub fn text(&self) -> String {
        if let Some(s) = self.section {
            return format!("{}: settings {s}", self.label);
        }
        format!(
            "{}: {} → {}",
            self.label,
            self.before.as_deref().unwrap_or("(not set)"),
            self.after.as_deref().unwrap_or("(not set)")
        )
    }

    pub fn json(&self) -> Value {
        json!({"key": self.raw_key, "label": self.label, "before": self.before, "after": self.after,
               "section": self.section, "text": self.text()})
    }
}

/// The loudness controller's settings: key, label, unit, decimals.
const LOUDNESS: &[(&str, &str, &str, usize)] = &[
    ("enabled", "on/off", "", 0),
    ("preset", "preset", "", 0),
    ("user_preset", "saved preset", "", 0),
    ("target_lufs", "target", "LUFS", 1),
    ("true_peak_dbtp", "true-peak ceiling", "dBTP", 1),
    ("window_s", "window", "s", 0),
    ("max_slew_db_per_min", "fastest move", "dB/min", 0),
    ("deadband_lu", "dead band", "LU", 0),
    ("gain_min_db", "lowest gain", "dB", 0),
    ("gain_max_db", "highest gain", "dB", 0),
    ("min_valid_s", "program needed", "s", 0),
    ("trim_tau_s", "trim time constant", "s", 0),
    ("trim_range_db", "trim range", "dB", 1),
    ("robust_items", "items compared", "", 0),
    ("item_cap_s", "longest an item counts", "s", 0),
    ("item_memory_s", "item memory", "s", 0),
    ("max_limiter_gr_db", "most limiting", "dB", 1),
    ("peak_allowance_db", "peak allowance", "dB", 1),
    ("within_item", "within an item", "", 0),
    ("gate_categories", "gated categories", "", 0),
];

fn number(x: f64, decimals: usize) -> String {
    let s = format!("{:.*}", decimals, x.abs());
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i.to_string(), Some(f.to_string())),
        None => (s.clone(), None),
    };
    let mut grouped = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let neg = x < 0.0 && s.chars().any(|c| c.is_ascii_digit() && c != '0');
    format!(
        "{}{}{}",
        if neg { "\u{2212}" } else { "" },
        grouped,
        frac.map(|f| format!(".{f}")).unwrap_or_default()
    )
}

/// Decimals of the shortest form of `x` (12.5 gives 1, 12 gives 0).
fn own_decimals(x: f64) -> usize {
    let s = format!("{x}");
    s.split_once('.').map_or(0, |(_, f)| f.len())
}

fn step_decimals(step: f64) -> usize {
    own_decimals(step)
}

fn with_unit(s: String, unit: &str) -> String {
    if unit.is_empty() {
        s
    } else {
        format!("{s} {unit}")
    }
}

fn num_of(v: &toml::Value) -> Option<f64> {
    match v {
        toml::Value::Float(f) => Some(*f),
        toml::Value::Integer(i) => Some(*i as f64),
        _ => None,
    }
}

fn plain(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.replace('_', " "),
        toml::Value::Boolean(b) => (if *b { "on" } else { "off" }).into(),
        toml::Value::Float(_) | toml::Value::Integer(_) => {
            let x = num_of(v).unwrap_or(0.0);
            number(x, own_decimals(x))
        }
        toml::Value::Array(a) => a.iter().map(plain).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

fn fmt_param(v: &toml::Value, p: &Param) -> String {
    let dec = |x: f64| {
        let d = if p.step > 0.0 {
            step_decimals(p.step)
        } else if matches!(p.kind, Kind::Int { .. }) {
            0
        } else {
            own_decimals(x)
        };
        if p.unit.starts_with("dB") && matches!(p.kind, Kind::Float { .. }) {
            d.max(1)
        } else {
            d
        }
    };
    match v {
        toml::Value::Boolean(b) => (if *b { "on" } else { "off" }).into(),
        toml::Value::String(s) => s.replace('_', " "),
        toml::Value::Array(a) => {
            let items: Vec<String> = a
                .iter()
                .map(|x| num_of(x).map_or_else(|| plain(x), |n| number(n, dec(n))))
                .collect();
            with_unit(items.join(", "), p.unit)
        }
        x => match num_of(x) {
            Some(n) => with_unit(number(n, dec(n)), p.unit),
            None => plain(x),
        },
    }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match (c.next(), c.next()) {
        (Some(a), Some(b)) if a.is_uppercase() && b.is_lowercase() => {
            format!("{}{b}{}", a.to_lowercase(), c.as_str())
        }
        _ => s.to_string(),
    }
}

fn section_label(name: &str) -> String {
    let parts: Vec<&str> = name.split('.').collect();
    match parts.as_slice() {
        ["chain"] => "Processing chain".into(),
        ["chain", b] => BLOCKS
            .iter()
            .find(|x| x.key == *b)
            .map_or_else(|| name.to_string(), |x| x.label.to_string()),
        ["loudness"] => "Loudness controller".into(),
        _ => name.to_string(),
    }
}

/// How the page names and formats one change.
pub fn describe(c: &Change) -> Shown {
    let mut sh = Shown {
        label: c.key.clone(),
        before: c.before.clone(),
        after: c.after.clone(),
        raw_key: c.key.clone(),
        section: None,
    };
    if let Some(name) = c.key.strip_prefix('[').and_then(|k| k.strip_suffix(']')) {
        sh.label = section_label(name);
        sh.section = Some(if c.after.is_some() {
            "added"
        } else {
            "removed"
        });
        sh.before = None;
        sh.after = None;
        return sh;
    }
    let (bv, av) = (c.bv.as_ref(), c.av.as_ref());
    let show = |f: &dyn Fn(&toml::Value) -> String| (bv.map(f), av.map(f));
    let parts: Vec<&str> = c.key.split('.').collect();
    match parts.as_slice() {
        ["processing_preset"] => {
            sh.label = "Chain preset".into();
            (sh.before, sh.after) = show(&plain);
        }
        ["chain", "user_preset"] => {
            sh.label = "Chain \u{b7} saved preset".into();
            (sh.before, sh.after) = show(&plain);
        }
        ["chain", block, key] => {
            if let Some(b) = BLOCKS.iter().find(|x| x.key == *block) {
                let key = common::chain::legacy_key(block, key);
                if key == "enabled" {
                    sh.label = b.label.to_string();
                    (sh.before, sh.after) = show(&plain);
                } else if key == "user_preset" {
                    sh.label = format!("{} \u{b7} saved preset", b.label);
                    (sh.before, sh.after) = show(&plain);
                } else if let Some(p) = b.params.iter().find(|p| p.key == key) {
                    sh.label = format!("{} \u{b7} {}", b.label, lower_first(p.label));
                    (sh.before, sh.after) = show(&|v| fmt_param(v, p));
                }
            }
        }
        ["loudness", key] => {
            if let Some((_, label, unit, dec)) = LOUDNESS.iter().find(|x| x.0 == *key) {
                sh.label = format!("Loudness controller \u{b7} {label}");
                let f = |v: &toml::Value| match num_of(v) {
                    Some(n) => with_unit(number(n, (*dec).max(own_decimals(n))), unit),
                    None => plain(v),
                };
                (sh.before, sh.after) = show(&f);
            }
        }
        _ => {}
    }
    sh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_history::diff_text;

    fn one(before: &str, after: &str) -> Shown {
        let c = diff_text(before, after);
        describe(&c[0])
    }

    #[test]
    fn equalizer_values_read_like_the_controls() {
        let s = one("[chain.eq]\nb2_db = -6.0\n", "[chain.eq]\nb2_db = -8.5\n");
        assert_eq!(
            s.text(),
            "Equalizer \u{b7} band 2 gain: \u{2212}6.0 dB → \u{2212}8.5 dB"
        );
        assert_eq!(s.raw_key, "chain.eq.b2_db");
        let s = one("[chain.eq]\nb3_hz = 4070.0\n", "[chain.eq]\nb3_hz = 4124\n");
        assert_eq!(
            s.text(),
            "Equalizer \u{b7} band 3 frequency: 4,070 Hz → 4,124 Hz"
        );
        let s = one("[chain.eq]\nb1_q = 0.71\n", "[chain.eq]\nb1_q = 1.57\n");
        assert_eq!(s.text(), "Equalizer \u{b7} band 1 Q: 0.71 → 1.57");
        let s = one(
            "[chain.eq]\nb1_type = \"low_shelf\"\n",
            "[chain.eq]\nb1_type = \"high_shelf\"\n",
        );
        assert_eq!(
            s.text(),
            "Equalizer \u{b7} band 1 type: low shelf → high shelf"
        );
    }

    #[test]
    fn other_blocks_switches_and_the_chain_preset() {
        assert_eq!(
            one(
                "processing_preset = \"radio\"\n",
                "processing_preset = \"bypass\"\n"
            )
            .text(),
            "Chain preset: radio → bypass"
        );
        assert_eq!(
            one(
                "[chain.agc]\nenabled = false\n",
                "[chain.agc]\nenabled = true\n"
            )
            .text(),
            "Wideband AGC: off → on"
        );
        let s = one(
            "[chain.agc]\ntarget_dbfs = -20.0\n",
            "[chain.agc]\ntarget_dbfs = -18.5\n",
        );
        assert!(
            s.text().starts_with("Wideband AGC \u{b7} ") && s.text().contains("dBFS"),
            "{}",
            s.text()
        );
        let s = one(
            "[chain.multiband]\ncrossovers_hz = [120.0, 500.0]\n",
            "[chain.multiband]\ncrossovers_hz = [120.0, 600.0]\n",
        );
        assert!(
            s.text().contains("120, 500 Hz") && s.text().contains("120, 600 Hz"),
            "{}",
            s.text()
        );
        let s = one(
            "[chain.trim]\ngain_db = 0.0\n",
            "[chain.trim]\ngain_db = 1.5\n",
        );
        assert_eq!(s.text(), "Input trim \u{b7} gain: 0.0 dB → 1.5 dB");
    }

    #[test]
    fn loudness_fields_use_the_cards_names() {
        let s = one(
            "[loudness]\nmax_slew_db_per_min = 2.0\n",
            "[loudness]\nmax_slew_db_per_min = 3.0\n",
        );
        assert_eq!(
            s.text(),
            "Loudness controller \u{b7} fastest move: 2 dB/min → 3 dB/min"
        );
        let s = one(
            "[loudness]\nenabled = false\n",
            "[loudness]\nenabled = true\n",
        );
        assert_eq!(s.text(), "Loudness controller \u{b7} on/off: off → on");
        let s = one(
            "[loudness]\ntarget_lufs = -14.0\n",
            "[loudness]\ntarget_lufs = -16.0\n",
        );
        assert_eq!(
            s.text(),
            "Loudness controller \u{b7} target: \u{2212}14.0 LUFS → \u{2212}16.0 LUFS"
        );
    }

    #[test]
    fn sections_and_unknown_keys() {
        let c = diff_text("name = \"a\"\n", "name = \"a\"\n[chain.eq]\nb2_db = 1.0\n");
        let texts: Vec<String> = c.iter().map(|c| describe(c).text()).collect();
        assert!(
            texts.contains(&"Equalizer: settings added".to_string()),
            "{texts:?}"
        );
        assert!(texts.contains(&"Processing chain: settings added".to_string()));
        let s = one("display_name = \"A\"\n", "display_name = \"B\"\n");
        assert_eq!(s.label, "display_name");
        assert_eq!(s.text(), "display_name: \"A\" → \"B\"");
        let s = one("[chain.eq]\nzz_future = 1\n", "[chain.eq]\nzz_future = 2\n");
        assert_eq!(s.label, "chain.eq.zz_future");
        let s = one("[mystery]\nk = 1\n", "[mystery]\nk = 2\n");
        assert_eq!(s.text(), "mystery.k: 1 → 2");
    }
}
