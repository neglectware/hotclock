//! The processing chain's API and its persistence (architecture 6, 7.2): `GET /api/v1/chain`, `PUT
//! /api/v1/chain` and `PUT /api/v1/chain/{stage}`, `POST /api/v1/processing/preset` and `/bypass`.
//!
//! The station keeps one [`ChainState`]: the settings in force, the shipped preset they are measured from, the
//! names of the user presets they came from, and the control end of the engine's chain. A change is checked
//! (`common::chain`), written to the station's config file (`processing_preset` and the `[chain]` tables of the keys
//! that differ from the preset) and only then started on rt-out (a 50 ms crossfade, `engine::chain`).

use crate::http::Response;
use crate::station_api::StationApi;
use common::chain::{preset_config, schema_json, ChainConfig, BLOCKS, CHAIN_PRESETS};
use engine::chain::ChainControl;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A configuration, the shipped preset it is measured from, and the user presets it came from.
#[derive(Clone)]
pub struct Saved {
    pub cfg: ChainConfig,
    pub base: String,
    pub user_preset: String,
    pub block_presets: BTreeMap<String, String>,
}

pub struct ChainApi {
    pub ctl: Arc<ChainControl>,
    state: Mutex<Saved>,
    /// The state `POST /processing/bypass {on: true}` replaced, to come back to.
    stash: Mutex<Option<Saved>>,
    config_path: Option<PathBuf>,
    /// The file's history (every save of the config file is a version).
    history: Option<crate::config_history::ConfigHistory>,
    /// Why the last change could not be written to the config file (the change is live anyway), or `None`.
    persist_warning: Mutex<Option<String>>,
    /// While a "Move to makeup" is gliding (the makeup and the controller's gain move at 3 dB a second), until
    /// when; another move waits for it.
    glide_until: Mutex<Option<std::time::Instant>>,
}

fn bad(msg: &str) -> Response {
    Response::error(400, "bad_request", msg)
}

impl ChainApi {
    /// The station's chain as its file names it.
    pub fn new(
        ctl: Arc<ChainControl>,
        cfg: &common::config::StationConfig,
        config_path: Option<PathBuf>,
    ) -> ChainApi {
        let (user_preset, block_presets) = ChainConfig::bookkeeping(&cfg.chain);
        ChainApi {
            ctl,
            state: Mutex::new(Saved {
                cfg: cfg.chain_config(),
                base: cfg.processing_preset.clone(),
                user_preset,
                block_presets,
            }),
            stash: Mutex::new(None),
            config_path,
            history: None,
            persist_warning: Mutex::new(None),
            glide_until: Mutex::new(None),
        }
    }

    /// Keep the history of the config file's changes (see [`crate::config_history`]).
    pub fn with_history(mut self, h: Option<crate::config_history::ConfigHistory>) -> ChainApi {
        self.history = h;
        self
    }

    /// Start the configured chain on rt-out (the runner calls this once before the first sound).
    pub fn start(&self) -> Result<(), String> {
        let c = self.state.lock().map_err(|_| "poisoned")?.cfg.clone();
        self.ctl.apply(&c)
    }

    pub fn current(&self) -> Saved {
        self.state
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    /// `GET /api/v1/chain`.
    pub fn json(&self, api: &StationApi, with_schema: bool) -> Value {
        let s = self.current();
        let lc = api
            .loudness_cfg
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default();
        let c = &s.cfg;
        let base_cfg = preset_config(&s.base).unwrap_or_default();
        let custom = !c.diff(&base_cfg).is_empty();
        let bands = c.bands();
        let any_on = ["agc", "multiband", "wideband", "clipper", "limiter"]
            .iter()
            .any(|b| c.enabled(b));
        let limiter_on = c.enabled("limiter") || lc.enabled;
        let ceiling = match (c.enabled("limiter"), lc.enabled) {
            (true, true) => c.num("limiter", "ceiling_dbtp").min(lc.true_peak_dbtp),
            (true, false) => c.num("limiter", "ceiling_dbtp"),
            _ => lc.true_peak_dbtp,
        };
        let rev = api
            .meter
            .ctl
            .rev
            .load(std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(u64::from(
                self.ctl
                    .meters
                    .cfg_rev
                    .load(std::sync::atomic::Ordering::Relaxed),
            ))
            & 0xFFFF;
        let on = |b: &str| c.enabled(b);
        let mut v = json!({
            "preset": s.base,
            "user_preset": s.user_preset,
            "custom": custom,
            "bypassed": !any_on && !c.enabled("highpass") && !c.enabled("rotator") && !c.enabled("stereo") && !c.enabled("eq") && !c.enabled("bass") && c.num("trim", "gain_db") == 0.0 && c.num("makeup", "gain_db") == 0.0,
            "bypass_override": self.stash.lock().map(|s| s.is_some()).unwrap_or(false),
            "chain_cfg_rev": rev,
            "latency_ms": 5.0,
            "stages": [
                {"name": "agc", "enabled": on("agc")},
                {"name": "multiband", "enabled": on("multiband")},
                {"name": "wideband", "enabled": on("wideband")},
                {"name": "limiter", "enabled": limiter_on, "ceiling_dbtp": ceiling, "lookahead_ms": 5.0,
                 "note": if lc.enabled && !c.enabled("limiter") {
                    "forced on as the loudness controller's peak stage (the preset has the limiter off)"
                 } else if limiter_on { "on" } else { "off: the true-peak limiter delays the program by 5 ms and does not act" }},
                {"name": "clipper", "enabled": on("clipper")},
                {"name": "safety_limiter", "enabled": true, "note": "always on"},
                {"name": "loudness_controller", "enabled": lc.enabled, "target_lufs": lc.target_lufs},
            ],
            "multiband": {
                "enabled": on("multiband"), "bands": bands, "crossovers_hz": c.list("multiband", "crossovers_hz"),
                "crossovers": c.list("multiband", "crossovers_hz"), "slope_db_per_octave": 24,
            },
            "makeup_db": c.list("multiband", "makeup_db"),
            "blocks": c.to_json(),
            "block_presets": s.block_presets,
            "persist_warning": self.persist_warning.lock().ok().and_then(|w| w.clone()),
            "chain_presets": CHAIN_PRESETS.iter().map(|p| json!({"name": p.name, "help": p.help})).collect::<Vec<_>>(),
        });
        if with_schema {
            v["schema"] = schema_json();
        }
        v
    }

    fn emit(&self, api: &StationApi) {
        let v = self.json(api, false);
        api.emit("chain", &v);
    }

    /// Start `s` and write it to the config file. A failed write does not stop the change: it is live, and
    /// `persist_warning` says (in every chain JSON) that the next start will not have it.
    fn commit(&self, api: &StationApi, s: Saved, source: &str) -> Result<(), Response> {
        let warning = self.config_path.as_ref().and_then(|path| {
            let write = || persist(path, &s);
            let r = match &self.history {
                Some(h) => h.track(source, write),
                None => write(),
            };
            r.err().map(|e| {
                format!("The change is live but could not be saved, so the next start of the station will not have it: {e}")
            })
        });
        if let Some(w) = &warning {
            eprintln!("hotclock: chain: {w}");
        }
        if let Ok(mut pw) = self.persist_warning.lock() {
            *pw = warning;
        }
        self.ctl
            .apply(&s.cfg)
            .map_err(|e| Response::error(503, "busy", &e))?;
        if let Ok(mut st) = self.state.lock() {
            *st = s;
        }
        // The loudness controller and the chain share `chain_cfg_rev`.
        self.emit(api);
        Ok(())
    }

    /// `PUT /api/v1/chain/{stage}`: change one block. The body holds any of its settings, `enabled`, and
    /// `user_preset` (the name of the preset they came from, empty for none).
    pub fn put_block(&self, api: &StationApi, stage: &str, body: &Value) -> Response {
        let Some(obj) = body.as_object() else {
            return bad("the body must be a JSON object");
        };
        if BLOCKS.iter().all(|b| b.key != stage) {
            return Response::error(
                404,
                "not_found",
                &format!(
                    "no block `{stage}`; the blocks are {}",
                    BLOCKS.iter().map(|b| b.key).collect::<Vec<_>>().join(", ")
                ),
            );
        }
        let mut s = self.current();
        let mut params = obj.clone();
        let user = params.remove("user_preset");
        if let Err(e) = s.cfg.apply_block(stage, &params) {
            return bad(&e);
        }
        match user {
            Some(Value::String(n)) if !n.is_empty() => {
                s.block_presets.insert(stage.to_string(), n);
            }
            Some(Value::String(_)) => {
                s.block_presets.remove(stage);
            }
            Some(_) => return bad("`user_preset` must be text"),
            // Changing a setting by hand leaves the name; the UI marks it modified by comparing.
            None => {}
        }
        match self.commit(api, s, &format!("processing: {stage}")) {
            Ok(()) => Response::json(200, &self.json(api, false)),
            Err(r) => r,
        }
    }

    /// `PUT /api/v1/chain`: `{preset}` starts a shipped chain preset; `{chain, user_preset}` starts a saved one (the
    /// whole chain's settings); `{blocks: {stage: {...}}}` changes several blocks in one swap.
    pub fn put_chain(&self, api: &StationApi, body: &Value) -> Response {
        let Some(obj) = body.as_object() else {
            return bad("the body must be a JSON object");
        };
        let mut s = self.current();
        if let Some(p) = obj.get("preset") {
            let Some(name) = p.as_str() else {
                return bad("`preset` must be text");
            };
            let Some(c) = preset_config(name) else {
                return bad(&format!(
                    "unknown preset `{name}`; the shipped presets are {}",
                    CHAIN_PRESETS.map(|p| p.name).join(", ")
                ));
            };
            s = Saved {
                cfg: c,
                base: name.to_string(),
                user_preset: String::new(),
                block_presets: BTreeMap::new(),
            };
        }
        if let Some(c) = obj.get("chain") {
            let mut cfg = ChainConfig::default();
            if let Err(e) = cfg.apply_all(c) {
                return bad(&e);
            }
            s.base = closest_base(&cfg);
            s.cfg = cfg;
            s.user_preset = obj
                .get("user_preset")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            s.block_presets.clear();
        } else if let Some(u) = obj.get("user_preset") {
            match u.as_str() {
                Some(n) => s.user_preset = n.to_string(),
                None => return bad("`user_preset` must be text"),
            }
        }
        if let Some(b) = obj.get("blocks") {
            if let Err(e) = s.cfg.apply_all(b) {
                return bad(&e);
            }
        }
        for k in obj.keys() {
            if !["preset", "chain", "user_preset", "blocks"].contains(&k.as_str()) {
                return bad(&format!(
                    "unknown key `{k}`; use `preset`, `chain` with `user_preset`, or `blocks`"
                ));
            }
        }
        let source = if obj.contains_key("preset") {
            "processing: preset"
        } else if obj.contains_key("chain") {
            "processing: chain"
        } else {
            "processing: blocks"
        };
        match self.commit(api, s, source) {
            Ok(()) => Response::json(200, &self.json(api, false)),
            Err(r) => r,
        }
    }

    /// Start the chain a restored configuration file names, without writing the file (the history's restore has
    /// written it). A bypass by hand is over.
    pub fn restore(
        &self,
        api: &StationApi,
        cfg: &common::config::StationConfig,
    ) -> Result<(), String> {
        let (user_preset, block_presets) = ChainConfig::bookkeeping(&cfg.chain);
        let s = Saved {
            cfg: cfg.chain_config(),
            base: cfg.processing_preset.clone(),
            user_preset,
            block_presets,
        };
        self.ctl.apply(&s.cfg)?;
        if let Ok(mut st) = self.state.lock() {
            *st = s;
        }
        if let Ok(mut stash) = self.stash.lock() {
            *stash = None;
        }
        if let Ok(mut pw) = self.persist_warning.lock() {
            *pw = None;
        }
        self.emit(api);
        Ok(())
    }

    /// `POST /api/v1/loudness/move-to-makeup`: put the loudness controller's steady gain into the chain's output
    /// makeup. The makeup goes up by the gain (to 0.1 dB) while the controller's gain comes down by the same
    /// amount, both at 3 dB a second, so the program level does not move. Saved like any other chain edit.
    pub fn move_gain_to_makeup(&self, api: &StationApi) -> Response {
        let enabled = api
            .loudness_cfg
            .lock()
            .map(|c| c.enabled)
            .unwrap_or_default();
        if !enabled {
            return Response::error(
                409,
                "conflict",
                "the loudness controller is off, so it has no gain to move",
            );
        }
        if self.stash.lock().map(|s| s.is_some()).unwrap_or(false) {
            return Response::error(
                409,
                "conflict",
                "the chain is bypassed by hand; switch it back in first",
            );
        }
        let Some(link) = api.meter.ctl.link.get() else {
            return Response::error(
                503,
                "unavailable",
                "the loudness controller is not running yet",
            );
        };
        let Ok(mut glide) = self.glide_until.lock() else {
            return Response::error(500, "internal", "poisoned");
        };
        if glide.is_some_and(|t| std::time::Instant::now() < t) {
            return Response::error(
                409,
                "conflict",
                "the last move is still gliding; try again in a few seconds",
            );
        }
        let gain = (link.gain_db() * 10.0).round() / 10.0;
        if gain.abs() < 0.1 {
            return bad("the controller's gain is under 0.1 dB, so there is nothing to move");
        }
        let mut s = self.current();
        let before = s.cfg.num("makeup", "gain_db");
        let after = ((before + gain) * 100.0).round() / 100.0;
        let mut o = Map::new();
        o.insert("gain_db".into(), json!(after));
        if let Err(e) = s.cfg.apply_block("makeup", &o) {
            return bad(&format!(
                "the makeup would be {after} dB, which is out of range: {e}"
            ));
        }
        // The chain first (a failed write or a full swap ring changes nothing), then the controller.
        if let Err(r) = self.commit(api, s, "processing: makeup") {
            return r;
        }
        let moved = after - before;
        if let Ok(mut sh) = api.meter.ctl.shift.lock() {
            *sh = Some(sh.unwrap_or(0.0) + moved);
        }
        let glide_s = moved.abs() / engine::chain::MAKEUP_SLEW_DB_PER_S;
        *glide =
            Some(std::time::Instant::now() + std::time::Duration::from_secs_f64(glide_s + 1.0));
        Response::json(
            200,
            &json!({
                "moved_db": moved, "gain_db_before": link.gain_db(),
                "makeup_db_before": before, "makeup_db": after,
                "glide_s": glide_s, "chain": self.json(api, false),
            }),
        )
    }

    /// `POST /api/v1/processing/preset {name}`: a shipped whole-chain preset.
    pub fn post_preset(&self, api: &StationApi, name: &str) -> Response {
        self.put_chain(api, &json!({"preset": name}))
    }

    /// `POST /api/v1/processing/bypass {on}`: the whole chain out of the path (and back), without touching the
    /// config file: the next start returns to the configured chain.
    pub fn post_bypass(&self, api: &StationApi, on: bool) -> Response {
        let Ok(mut stash) = self.stash.lock() else {
            return Response::error(500, "internal", "poisoned");
        };
        if on {
            if stash.is_some() {
                return Response::json(200, &self.json(api, false));
            }
            let cur = self.current();
            let off = Saved {
                cfg: preset_config("bypass").unwrap_or_default(),
                base: "bypass".into(),
                user_preset: String::new(),
                block_presets: BTreeMap::new(),
            };
            if let Err(e) = self.ctl.apply(&off.cfg) {
                return Response::error(503, "busy", &e);
            }
            if let Ok(mut st) = self.state.lock() {
                *st = off;
            }
            *stash = Some(cur);
        } else {
            let Some(back) = stash.take() else {
                return Response::json(200, &self.json(api, false));
            };
            if let Err(e) = self.ctl.apply(&back.cfg) {
                *stash = Some(back);
                return Response::error(503, "busy", &e);
            }
            if let Ok(mut st) = self.state.lock() {
                *st = back;
            }
        }
        drop(stash);
        self.emit(api);
        Response::json(200, &self.json(api, false))
    }
}

/// The shipped preset a configuration is closest to (the fewest differing keys), the base the file names.
pub fn closest_base(cfg: &ChainConfig) -> String {
    CHAIN_PRESETS
        .iter()
        .map(|p| {
            let d = cfg
                .diff(&preset_config(p.name).unwrap_or_default())
                .values()
                .map(|b| b.as_object().map_or(0, Map::len))
                .sum::<usize>();
            (d, p.name)
        })
        .min()
        .map_or("bypass", |(_, n)| n)
        .to_string()
}

fn to_item(v: &Value) -> toml_edit::Item {
    use toml_edit::{value, Array};
    match v {
        Value::Bool(b) => value(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) if n.is_i64() => value(i),
            _ => value(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => value(s.as_str()),
        Value::Array(a) => {
            let mut arr = Array::new();
            for x in a {
                match x {
                    Value::Number(n) => arr.push(n.as_f64().unwrap_or(0.0)),
                    Value::String(s) => arr.push(s.as_str()),
                    Value::Bool(b) => arr.push(*b),
                    _ => {}
                }
            }
            value(arr)
        }
        _ => value(""),
    }
}

/// Write `processing_preset` and the `[chain]` tables into the station's config file, keeping everything else.
pub fn persist(path: &std::path::Path, s: &Saved) -> Result<(), String> {
    use toml_edit::{value, Table};
    let mut doc = crate::settings::read_doc(path)?;
    doc["processing_preset"] = value(s.base.as_str());
    doc.remove("chain");
    let base = preset_config(&s.base).unwrap_or_default();
    let diff = s.cfg.diff(&base);
    if !diff.is_empty() || !s.user_preset.is_empty() || !s.block_presets.is_empty() {
        let mut chain = Table::new();
        chain.set_implicit(true);
        if !s.user_preset.is_empty() {
            chain["user_preset"] = value(s.user_preset.as_str());
        }
        for b in BLOCKS {
            let d = diff.get(b.key).and_then(Value::as_object);
            let n = s.block_presets.get(b.key);
            if d.is_none() && n.is_none() {
                continue;
            }
            let mut t = Table::new();
            if let Some(n) = n {
                t["user_preset"] = value(n.as_str());
            }
            for (k, v) in d.into_iter().flatten() {
                t[k.as_str()] = to_item(v);
            }
            chain[b.key] = toml_edit::Item::Table(t);
        }
        doc["chain"] = toml_edit::Item::Table(chain);
    }
    crate::settings::write_atomic(path, &doc.to_string()).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::config::StationConfig;

    const FILE: &str = "# my station\nname = \"office\"\nlisten = \"0.0.0.0:1\"\ndevice = \"null\"\n\n[stream]\nenabled = false\n\n[loudness]\nenabled = false\n";

    fn write(text: &str) -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("hc-chain-{}-{}", std::process::id(), text.len()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("office.toml");
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn what_is_written_reads_back_as_the_same_chain_and_the_file_keeps_its_other_content() {
        let path = write(FILE);
        let mut cfg = preset_config("radio").unwrap();
        cfg.apply_block("agc", json!({"target_dbfs": -23.5}).as_object().unwrap())
            .unwrap();
        cfg.apply_block(
            "clipper",
            json!({"enabled": true, "threshold_dbfs": -2.0})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        cfg.apply_block(
            "multiband",
            json!({"crossovers_hz": [200.0, 4000.0], "threshold_dbfs": [-24.0, -22.0, -24.0], "ratio": [2.0, 2.0, 2.0], "knee_db": [6.0, 6.0, 6.0],
                "attack_ms": [30.0, 12.0, 5.0], "release_ms": [400.0, 200.0, 120.0], "makeup_db": [2.0, 2.0, 2.0]})
            .as_object()
            .unwrap(),
        )
        .unwrap();
        cfg.apply_block(
            "eq",
            json!({"enabled": true, "position": "post", "low_db": 2.5})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        let s = Saved {
            cfg: cfg.clone(),
            base: "radio".into(),
            user_preset: "Mine".into(),
            block_presets: [("agc".to_string(), "Quiet".to_string())]
                .into_iter()
                .collect(),
        };
        persist(&path, &s).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed = StationConfig::parse(
            &text,
            std::path::Path::new("/etc/hotclock/stations/office.toml"),
        )
        .unwrap();
        assert_eq!(parsed.processing_preset, "radio");
        assert_eq!(parsed.chain_config(), cfg, "{text}");
        let (user, blocks) = ChainConfig::bookkeeping(&parsed.chain);
        assert_eq!(
            (user.as_str(), blocks.get("agc").map(String::as_str)),
            ("Mine", Some("Quiet"))
        );
        // The comment, the other tables and the order of the root keys survive; the root key sits before every table.
        assert!(
            text.starts_with("# my station\nname = \"office\""),
            "{text}"
        );
        assert!(
            text.contains("[stream]\nenabled = false")
                && text.contains("[loudness]\nenabled = false")
        );
        assert!(
            text.find("processing_preset").unwrap() < text.find("[stream]").unwrap(),
            "{text}"
        );
        // Writing the preset itself removes every [chain] table.
        persist(
            &path,
            &Saved {
                cfg: preset_config("gentle").unwrap(),
                base: "gentle".into(),
                user_preset: String::new(),
                block_presets: BTreeMap::new(),
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("[chain"), "{text}");
        assert_eq!(
            StationConfig::parse(
                &text,
                std::path::Path::new("/etc/hotclock/stations/office.toml")
            )
            .unwrap()
            .chain_config(),
            preset_config("gentle").unwrap()
        );
    }

    #[test]
    fn the_closest_shipped_preset_is_the_base() {
        let mut c = preset_config("gentle").unwrap();
        c.apply_block("agc", json!({"target_dbfs": -25.0}).as_object().unwrap())
            .unwrap();
        assert_eq!(closest_base(&c), "gentle");
        assert_eq!(closest_base(&preset_config("radio").unwrap()), "radio");
        assert_eq!(closest_base(&ChainConfig::default()), "bypass");
    }
}
