//! The processing chain's settings (architecture 6, 9): every block, every parameter, its range and default, the
//! shipped whole-chain presets, and the checks a station file, the API and a user preset all go through.
//!
//! The schema ([`BLOCKS`]) is the one list the rest is built from: the engine reads values by block and key, the
//! settings catalog and the Processing panel are generated from it (`GET /api/v1/chain` serves it), and the Help
//! reference lists it. A parameter added here appears everywhere.
//!
//! A chain is a [`ChainConfig`]: for each block a JSON object holding `enabled` (when the block has a switch) and its
//! parameters. A station file names a shipped preset (`processing_preset`) and may carry a `[chain]` table of keys
//! that differ from it ([`ChainConfig::resolve`]).

use serde_json::{json, Map, Value};

/// What a parameter holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Bool,
    Float {
        min: f64,
        max: f64,
    },
    Int {
        min: i64,
        max: i64,
    },
    Choice(&'static [&'static str]),
    /// A list of numbers, each in `min..=max`. `per_band` lists must have one value per multiband band.
    List {
        min: f64,
        max: f64,
        per_band: bool,
    },
}

/// The default of a parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Def {
    B(bool),
    F(f64),
    I(i64),
    S(&'static str),
    L(&'static [f64]),
}

#[derive(Clone, Copy)]
pub struct Param {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    pub default: Def,
    pub unit: &'static str,
    pub help: &'static str,
    /// The resolution of a `Float` as the Processing page offers it (0: none). The engine and the API take any value
    /// in range, so a setting made before the parametric equalizer (a frequency of 250.5 Hz) is kept as it is.
    pub step: f64,
}

pub struct BlockDef {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    /// The block has an `enabled` switch (every block but the input trim and the output makeup).
    pub switch: bool,
    pub params: &'static [Param],
}

const fn f(
    key: &'static str,
    label: &'static str,
    min: f64,
    max: f64,
    d: f64,
    unit: &'static str,
    help: &'static str,
) -> Param {
    Param {
        key,
        label,
        kind: Kind::Float { min, max },
        default: Def::F(d),
        unit,
        help,
        step: 0.0,
    }
}

/// A float with a resolution: `4087` Hz, `-1.7` dB and `1.57` are kept as typed, whatever the knob's own feel.
#[allow(clippy::too_many_arguments)]
const fn fs(
    key: &'static str,
    label: &'static str,
    min: f64,
    max: f64,
    d: f64,
    step: f64,
    unit: &'static str,
    help: &'static str,
) -> Param {
    Param {
        key,
        label,
        kind: Kind::Float { min, max },
        default: Def::F(d),
        unit,
        help,
        step,
    }
}

const fn int(
    key: &'static str,
    label: &'static str,
    min: i64,
    max: i64,
    d: i64,
    help: &'static str,
) -> Param {
    Param {
        key,
        label,
        kind: Kind::Int { min, max },
        default: Def::I(d),
        unit: "",
        help,
        step: 0.0,
    }
}

const fn list(
    key: &'static str,
    label: &'static str,
    min: f64,
    max: f64,
    d: &'static [f64],
    unit: &'static str,
    help: &'static str,
) -> Param {
    Param {
        key,
        label,
        kind: Kind::List {
            min,
            max,
            per_band: key.as_bytes()[0] != b'c',
        },
        default: Def::L(d),
        unit,
        help,
        step: 0.0,
    }
}

/// The compressor parameters shared by the wideband compressor.
const WB: [Param; 6] = [
    f("threshold_dbfs", "Threshold", -60.0, 0.0, -20.0, "dBFS", "The level above which the compressor turns the program down. A full-scale sine reads 0 dB."),
    f("ratio", "Ratio", 1.0, 20.0, 2.0, ": 1", "How much the level above the threshold is reduced: 2 turns 6 dB over into 3 dB over."),
    f("knee_db", "Knee", 0.0, 24.0, 6.0, "dB", "The width of the soft transition around the threshold; 0 is a hard knee."),
    f("attack_ms", "Attack", 0.5, 500.0, 30.0, "ms", "The time constant of the gain falling when the level rises."),
    f("release_ms", "Release", 10.0, 5000.0, 500.0, "ms", "The time constant of the gain recovering when the level falls."),
    f("makeup_db", "Makeup", -12.0, 24.0, 0.0, "dB", "Gain added after the compression."),
];

/// The band types of the equalizer, in the order the Processing page lists them.
pub const EQ_TYPES: [&str; 7] = [
    "peak",
    "low_shelf",
    "high_shelf",
    "low_pass",
    "high_pass",
    "notch",
    "band_pass",
];
/// The most bands the equalizer has.
pub const EQ_MAX_BANDS: usize = 8;
/// The Q of a shelf that the old fixed equalizer used (RBJ shelf slope 1): the default of a shelf band.
pub const EQ_SHELF_Q: f64 = 1.0 / std::f64::consts::SQRT_2;

/// One band's five parameters: `b<n>_on`, `_type`, `_hz`, `_db`, `_q`.
macro_rules! eq_band {
    ($n:literal, $on:expr, $ty:expr, $hz:expr, $q:expr) => {
        [
            Param {
                key: concat!("b", $n, "_on"),
                label: concat!("Band ", $n),
                kind: Kind::Bool,
                default: Def::B($on),
                unit: "",
                help: concat!("Band ", $n, " is in the signal path. A band that is off does nothing, and keeps its settings."),
                step: 0.0,
            },
            Param {
                key: concat!("b", $n, "_type"),
                label: concat!("Band ", $n, " type"),
                kind: Kind::Choice(&EQ_TYPES),
                default: Def::S($ty),
                unit: "",
                help: "peak (bell), low_shelf, high_shelf, low_pass, high_pass, notch or band_pass. The gain applies to peak and the shelves only.",
                step: 0.0,
            },
            fs(concat!("b", $n, "_hz"), concat!("Band ", $n, " frequency"), 20.0, 20000.0, $hz, 1.0, "Hz", "The centre of a bell, notch or band-pass, the corner of a shelf (its midpoint), or the -3 dB frequency of a low-pass or high-pass (at Q 0.71)."),
            fs(concat!("b", $n, "_db"), concat!("Band ", $n, " gain"), -18.0, 18.0, 0.0, 0.1, "dB", "The boost or cut of a bell or a shelf. Low-pass, high-pass, notch and band-pass have no gain."),
            fs(concat!("b", $n, "_q"), concat!("Band ", $n, " Q"), 0.1, 10.0, $q, 0.01, "", "The width of a bell, notch or band-pass (higher is narrower), and the steepness of a shelf (0.71 is the gentlest without overshoot) or of the corner of a low-pass or high-pass."),
        ]
    };
}

const fn eq_params() -> [Param; 42] {
    let bands: [[Param; 5]; 8] = [
        eq_band!(1, true, "low_shelf", 100.0, EQ_SHELF_Q),
        eq_band!(2, true, "peak", 250.0, 1.0),
        eq_band!(3, true, "peak", 3000.0, 1.0),
        eq_band!(4, true, "high_shelf", 10000.0, EQ_SHELF_Q),
        eq_band!(5, false, "peak", 500.0, 1.0),
        eq_band!(6, false, "peak", 1500.0, 1.0),
        eq_band!(7, false, "peak", 6000.0, 1.0),
        eq_band!(8, false, "peak", 12000.0, 1.0),
    ];
    let mut out = [bands[0][0]; 42];
    out[0] = Param {
        key: "position",
        label: "Position",
        kind: Kind::Choice(&["pre", "post"]),
        default: Def::S("pre"),
        unit: "",
        help: "Before the multiband compressor (pre) or after it (post). The sound is tuned differently in each; the choice is a matter of taste.",
        step: 0.0,
    };
    out[1] = int("bands", "Bands", 1, EQ_MAX_BANDS as i64, 4, "How many bands the equalizer has (1 to 8). Bands above this number do nothing; their settings are kept.");
    let mut b = 0;
    while b < 8 {
        let mut k = 0;
        while k < 5 {
            out[2 + b * 5 + k] = bands[b][k];
            k += 1;
        }
        b += 1;
    }
    out
}

const EQ_PARAMS: [Param; 42] = eq_params();

pub const BLOCKS: &[BlockDef] = &[
    BlockDef {
        key: "trim",
        label: "Input trim",
        help: "A fixed gain at the start of the chain. 0 dB leaves the program as it is.",
        switch: false,
        params: &[f("gain_db", "Gain", -24.0, 24.0, 0.0, "dB", "The trim in dB.")],
    },
    BlockDef {
        key: "highpass",
        label: "Input high-pass",
        help: "Removes subsonic content below the cutoff so it does not use up headroom later in the chain.",
        switch: true,
        params: &[
            f("freq_hz", "Cutoff", 10.0, 200.0, 25.0, "Hz", "The -3 dB frequency."),
            int("order", "Order", 1, 4, 2, "Butterworth order: 1 is 6 dB per octave, 4 is 24 dB per octave."),
        ],
    },
    BlockDef {
        key: "rotator",
        label: "Phase rotator",
        help: "A chain of all-pass filters that makes asymmetric waveforms (voice, mostly) more symmetric, so that the peak stages after it work less. The sound changes little; the peaks change.",
        switch: true,
        params: &[
            f("freq_hz", "Frequency", 40.0, 1000.0, 200.0, "Hz", "The centre of the phase rotation."),
            int("sections", "Sections", 1, 6, 3, "How many all-pass sections are in the chain; more rotates more."),
        ],
    },
    BlockDef {
        key: "stereo",
        label: "Stereo enhancer",
        help: "Changes the width of the stereo image by scaling the difference between the channels.",
        switch: true,
        params: &[f("width", "Width", 0.0, 2.0, 1.25, "", "1 leaves the image alone, 0 is mono, 2 doubles the difference between the channels.")],
    },
    BlockDef {
        key: "agc",
        label: "Wideband AGC",
        help: "A slow gain that holds the program at a target level, riding a fade up (within a limit), freezing in silence, and clamping down quickly when a loud song starts. It comes before the multiband compressor.",
        switch: true,
        params: &[
            f("target_dbfs", "Target", -40.0, -6.0, -20.0, "dBFS RMS", "The K-weighted level the AGC steers the program to."),
            f("max_gain_db", "Max gain", 0.0, 24.0, 12.0, "dB", "The most the AGC turns the program up (for example while it rides a fade)."),
            f("max_reduction_db", "Max reduction", 0.0, 24.0, 9.0, "dB", "The most the AGC turns the program down."),
            f("down_db_per_s", "Down rate", 0.1, 40.0, 3.0, "dB/s", "How fast the gain may fall."),
            f("up_db_per_s", "Up rate", 0.05, 40.0, 2.5, "dB/s", "How fast the gain may rise, also while it rides a fade."),
            f("detector_ms", "Fast detector", 50.0, 2000.0, 400.0, "ms", "The window of the fast level, which sees a fade and an onset. The 3 s program level sets the long-term target."),
            f("program_ms", "Program level", 500.0, 10000.0, 3000.0, "ms", "The window of the slow level that sets the long-term target."),
            f("gate_db", "Gate", -60.0, -10.0, -35.0, "dB re target", "Below this level (relative to the target) the gain holds its value: silence and room tone are not turned up."),
            f("fast_attack_ms", "Fast attack", 0.0, 200.0, 10.0, "ms", "The time the gain takes to fall 95 percent of the way when a loud onset arrives. 0 turns the fast clamp off."),
            f("fast_trigger_db", "Fast trigger", 1.0, 24.0, 6.0, "dB", "The fast clamp acts when the output would be this far over the target."),
        ],
    },
    BlockDef {
        key: "eq",
        label: "Equalizer",
        help: "A parametric equalizer of up to eight bands. Each band is a bell, a low or high shelf, a low-pass, a high-pass, a notch or a band-pass, with a frequency from 20 Hz to 20 kHz in 1 Hz steps, a gain of plus or minus 18 dB in 0.1 dB steps (bell and shelves) and a Q from 0.10 to 10.00 in 0.01 steps. The default is four bands (low shelf, two bells, high shelf), flat at 0 dB.",
        switch: true,
        params: &EQ_PARAMS,
    },
    BlockDef {
        key: "bass",
        label: "Bass enhancer",
        help: "Makes the low end seem fuller on small speakers by adding harmonics of the bass. It comes before the multiband compressor, so the compressor controls what it adds.",
        switch: true,
        params: &[
            f("freq_hz", "Frequency", 40.0, 200.0, 80.0, "Hz", "The top of the bass that is enhanced."),
            f("amount_pct", "Amount", 0.0, 100.0, 30.0, "%", "How much of the added harmonics is mixed in."),
            f("drive", "Drive", 1.0, 10.0, 3.0, "", "How hard the bass is driven into the harmonic generator."),
        ],
    },
    BlockDef {
        key: "multiband",
        label: "Multiband compressor",
        help: "Two to six bands with 4th-order Linkwitz-Riley crossovers that sum flat. Each band has its own threshold, ratio, knee, attack, release and makeup. The number of bands is the number of crossovers plus one; changing it is a swap of the whole block.",
        switch: true,
        params: &[
            list("crossovers_hz", "Crossovers", 30.0, 18000.0, &[120.0, 500.0, 2000.0, 6000.0], "Hz", "The crossover frequencies, ascending. The band count is their number plus one (2 to 6)."),
            list("threshold_dbfs", "Threshold", -60.0, 0.0, &[-26.0, -24.0, -22.0, -22.0, -24.0], "dBFS", "Per band: the level above which the band is turned down. A full-scale sine reads 0 dB."),
            list("ratio", "Ratio", 1.0, 20.0, &[3.0, 3.0, 3.0, 3.0, 2.5], ": 1", "Per band: the compression ratio."),
            list("knee_db", "Knee", 0.0, 24.0, &[6.0, 6.0, 6.0, 6.0, 6.0], "dB", "Per band: the soft knee width."),
            list("attack_ms", "Attack", 0.5, 500.0, &[40.0, 25.0, 12.0, 6.0, 4.0], "ms", "Per band: the attack time constant."),
            list("release_ms", "Release", 10.0, 5000.0, &[400.0, 300.0, 200.0, 150.0, 120.0], "ms", "Per band: the release time constant."),
            list("makeup_db", "Makeup", -12.0, 24.0, &[3.0, 3.0, 3.0, 3.0, 3.0], "dB", "Fixed gain after this band's compressor. Sets how loud the chain's output is; the loudness controller then fine-tunes."),
        ],
    },
    BlockDef {
        key: "wideband",
        label: "Wideband compressor",
        help: "One compressor on the whole program after the multiband, for a slow levelling stage. Off in every shipped preset.",
        switch: true,
        params: &WB,
    },
    BlockDef {
        key: "makeup",
        label: "Output makeup",
        help: "A fixed gain after the compressors and before the clipper, to bring the chain's output to the loudness target so that the loudness controller has little to do. 0 dB leaves the program as it is. A change glides at 3 dB a second.",
        switch: false,
        params: &[f("gain_db", "Gain", -24.0, 24.0, 0.0, "dB", "The makeup in dB. The loudness controller's Move to makeup button sets it for you.")],
    },
    BlockDef {
        key: "clipper",
        label: "Clipper",
        help: "A soft clipper that shaves peaks before the loudness gain and the limiter, so that the limiter works less. The safety limiter still enforces the ceiling.",
        switch: true,
        params: &[
            f("threshold_dbfs", "Threshold", -12.0, 0.0, -1.5, "dBFS", "No sample leaves the clipper above this level."),
            f("softness_db", "Softness", 0.0, 12.0, 3.0, "dB", "How far under the threshold the curve starts to bend; 0 is a hard clip."),
        ],
    },
    BlockDef {
        key: "limiter",
        label: "True-peak limiter",
        help: "The lookahead limiter (5 ms) that keeps the true peak under the ceiling. The loudness controller turns it on by itself when it is enabled.",
        switch: true,
        params: &[f("ceiling_dbtp", "Ceiling", -6.0, -0.5, -1.0, "dBTP", "The true-peak ceiling (4x oversampled detection).")],
    },
];

/// The blocks that have their own switch and presets, in chain order. The loudness controller has its own page
/// of settings and is not a block here.
pub fn block(key: &str) -> Option<&'static BlockDef> {
    BLOCKS.iter().find(|b| b.key == key)
}

fn default_value(d: &Def) -> Value {
    match d {
        Def::B(b) => json!(b),
        Def::F(x) => json!(x),
        Def::I(n) => json!(n),
        Def::S(s) => json!(s),
        Def::L(l) => json!(l),
    }
}

/// The names of the ordered blocks as the meter frame and the API list the stages.
pub const STAGES: [&str; 6] = [
    "agc",
    "multiband",
    "wideband",
    "limiter",
    "clipper",
    "safety_limiter",
];

/// The whole chain's settings. Every block of [`BLOCKS`] is present with every key.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainConfig {
    pub blocks: Map<String, Value>,
}

impl Default for ChainConfig {
    /// Every block off, every parameter at its default (the `bypass` preset).
    fn default() -> Self {
        let mut blocks = Map::new();
        for b in BLOCKS {
            let mut o = Map::new();
            if b.switch {
                o.insert("enabled".into(), json!(false));
            }
            for p in b.params {
                o.insert(p.key.into(), default_value(&p.default));
            }
            blocks.insert(b.key.into(), Value::Object(o));
        }
        Self { blocks }
    }
}

impl ChainConfig {
    fn obj(&self, block: &str) -> &Map<String, Value> {
        static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
        self.blocks
            .get(block)
            .and_then(Value::as_object)
            .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
    }

    /// A true-or-false parameter other than `enabled` (the schema default when absent).
    pub fn flag(&self, block: &str, key: &str) -> bool {
        self.obj(block)
            .get(key)
            .and_then(Value::as_bool)
            .or_else(|| {
                block_param(block, key).and_then(|p| match p.default {
                    Def::B(b) => Some(b),
                    _ => None,
                })
            })
            .unwrap_or(false)
    }

    pub fn enabled(&self, block: &str) -> bool {
        self.obj(block)
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// A number parameter (the schema default when absent, which a validated config never is).
    pub fn num(&self, block: &str, key: &str) -> f64 {
        self.obj(block)
            .get(key)
            .and_then(Value::as_f64)
            .or_else(|| {
                block_param(block, key).and_then(|p| match p.default {
                    Def::F(x) => Some(x),
                    Def::I(n) => Some(n as f64),
                    _ => None,
                })
            })
            .unwrap_or(0.0)
    }

    pub fn text(&self, block: &str, key: &str) -> String {
        self.obj(block)
            .get(key)
            .and_then(Value::as_str)
            .or_else(|| {
                block_param(block, key).and_then(|p| match p.default {
                    Def::S(s) => Some(s),
                    _ => None,
                })
            })
            .unwrap_or("")
            .to_string()
    }

    pub fn list(&self, block: &str, key: &str) -> Vec<f64> {
        self.obj(block)
            .get(key)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default()
    }

    /// The multiband's band count (its crossovers plus one).
    pub fn bands(&self) -> usize {
        self.list("multiband", "crossovers_hz").len() + 1
    }

    pub fn block_json(&self, block: &str) -> Value {
        self.blocks.get(block).cloned().unwrap_or(Value::Null)
    }

    /// The whole chain as one JSON object, block by block.
    pub fn to_json(&self) -> Value {
        Value::Object(self.blocks.clone())
    }

    /// The parameters of a block without its switch (what a per-effect preset holds).
    pub fn params_json(&self, block: &str) -> Value {
        let mut o = self.obj(block).clone();
        o.remove("enabled");
        Value::Object(o)
    }

    /// Set keys of one block from a JSON object; the block as a whole is checked afterwards (so a band count
    /// change must bring its per-band lists along). On an error nothing is changed.
    pub fn apply_block(&mut self, block: &str, obj: &Map<String, Value>) -> Result<(), String> {
        let def = self::block(block).ok_or_else(|| format!("no block `{block}`"))?;
        let mut cur = self.obj(block).clone();
        for (k, v) in obj {
            let k = legacy_key(block, k);
            if k == "enabled" {
                if !def.switch {
                    return Err(format!("`{block}` has no on/off switch"));
                }
                cur.insert(
                    k.to_string(),
                    json!(v.as_bool().ok_or("`enabled` must be true or false")?),
                );
                continue;
            }
            let p = def
                .params
                .iter()
                .find(|p| p.key == k)
                .ok_or_else(|| format!("`{block}` has no setting `{k}`"))?;
            cur.insert(
                k.to_string(),
                check_value(p, v).map_err(|m| format!("`{k}` {m}"))?,
            );
        }
        check_block(def, &cur)?;
        self.blocks.insert(block.into(), Value::Object(cur));
        Ok(())
    }

    /// Replace the settings of a block (a preset of that effect): the keys named, the rest at their defaults.
    /// The switch is kept.
    pub fn set_block_params(
        &mut self,
        block: &str,
        obj: &Map<String, Value>,
    ) -> Result<(), String> {
        let def = self::block(block).ok_or_else(|| format!("no block `{block}`"))?;
        let mut fresh = ChainConfig::default();
        let on = self.enabled(block);
        let mut o = obj.clone();
        if def.switch {
            o.insert("enabled".into(), json!(on));
        }
        fresh.apply_block(block, &o)?;
        self.blocks.insert(block.into(), fresh.block_json(block));
        Ok(())
    }

    /// Merge a whole-chain object (block by block, partial allowed); every block is checked.
    pub fn apply_all(&mut self, v: &Value) -> Result<(), String> {
        let o = v
            .as_object()
            .ok_or("the chain must be a JSON object of blocks")?;
        let mut next = self.clone();
        for (k, b) in o {
            let bo = b
                .as_object()
                .ok_or_else(|| format!("`{k}` must be a JSON object"))?;
            next.apply_block(k, bo)?;
        }
        *self = next;
        Ok(())
    }

    /// The keys of this config that differ from `base`, as `block -> {key: value}` (empty blocks left out).
    pub fn diff(&self, base: &ChainConfig) -> Map<String, Value> {
        let mut out = Map::new();
        for b in BLOCKS {
            let (a, c) = (self.obj(b.key), base.obj(b.key));
            let mut d = Map::new();
            for (k, v) in a {
                if c.get(k) != Some(v) {
                    d.insert(k.clone(), v.clone());
                }
            }
            if !d.is_empty() {
                out.insert(b.key.into(), Value::Object(d));
            }
        }
        out
    }

    /// A station's chain: the shipped preset `preset`, then the `overrides` of its `[chain]` table (`user_preset`
    /// is bookkeeping and ignored here). Errors come back as `(key, message)` with the key spelled
    /// `chain.<block>.<key>`.
    pub fn resolve(
        preset: &str,
        overrides: &Map<String, Value>,
    ) -> Result<ChainConfig, Vec<(String, String)>> {
        let mut c = preset_config(preset).ok_or_else(|| {
            vec![(
                "processing_preset".to_string(),
                format!(
                    "must be one of {}, got `{preset}`",
                    CHAIN_PRESETS.map(|p| p.name).join(", ")
                ),
            )]
        })?;
        let mut errs = vec![];
        for (k, v) in overrides {
            if k == "user_preset" {
                if !v.is_string() {
                    errs.push(("chain.user_preset".into(), "must be text".into()));
                }
                continue;
            }
            let Some(bo) = v.as_object() else {
                errs.push((format!("chain.{k}"), "must be a table".into()));
                continue;
            };
            if block(k).is_none() {
                errs.push((format!("chain.{k}"), "is not a block of the chain".into()));
                continue;
            }
            // Check key by key so the message names the key; the block as a whole after.
            let mut staged = c.clone();
            let mut bad = false;
            for (kk, vv) in bo {
                if kk == "user_preset" {
                    if !vv.is_string() {
                        errs.push((format!("chain.{k}.user_preset"), "must be text".into()));
                        bad = true;
                    }
                    continue;
                }
                let one: Map<String, Value> = [(kk.clone(), vv.clone())].into_iter().collect();
                if let Err(m) = staged.apply_block_unchecked(k, &one) {
                    errs.push((format!("chain.{k}.{kk}"), m));
                    bad = true;
                }
            }
            if bad {
                continue;
            }
            match check_block(block(k).unwrap(), staged.obj(k)) {
                Ok(()) => c = staged,
                Err(m) => errs.push((format!("chain.{k}"), m)),
            }
        }
        if errs.is_empty() {
            Ok(c)
        } else {
            Err(errs)
        }
    }

    /// The preset names a station file's `[chain]` table remembers: the whole chain's, and each block's.
    pub fn bookkeeping(
        overrides: &Map<String, Value>,
    ) -> (String, std::collections::BTreeMap<String, String>) {
        let mut blocks = std::collections::BTreeMap::new();
        for (k, v) in overrides {
            if let Some(n) = v
                .get("user_preset")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty())
            {
                blocks.insert(k.clone(), n.to_string());
            }
        }
        let whole = overrides
            .get("user_preset")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        (whole, blocks)
    }

    /// Set keys without the whole-block check (used while merging a block key by key).
    fn apply_block_unchecked(
        &mut self,
        block_key: &str,
        obj: &Map<String, Value>,
    ) -> Result<(), String> {
        let def = block(block_key).ok_or_else(|| format!("no block `{block_key}`"))?;
        let mut cur = self.obj(block_key).clone();
        for (k, v) in obj {
            let k = legacy_key(block_key, k);
            if k == "enabled" {
                if !def.switch {
                    return Err("this block has no on/off switch".into());
                }
                cur.insert(
                    k.to_string(),
                    json!(v.as_bool().ok_or("must be true or false")?),
                );
                continue;
            }
            let p = def
                .params
                .iter()
                .find(|p| p.key == k)
                .ok_or_else(|| "is not a setting of this block".to_string())?;
            cur.insert(k.to_string(), check_value(p, v)?);
        }
        self.blocks.insert(block_key.into(), Value::Object(cur));
        Ok(())
    }

    /// Check a config read from outside (a whole chain with every block).
    pub fn check(&self) -> Result<(), String> {
        for b in BLOCKS {
            check_block(b, self.obj(b.key)).map_err(|m| format!("{}: {m}", b.key))?;
        }
        Ok(())
    }
}

/// The setting a key of an older release stands for. The fixed four-band equalizer's `low_*`, `mid1_*`, `mid2_*`
/// and `high_*` are bands 1 to 4 of the parametric one (the same types, the same Q for the shelves), so a station
/// file, a user preset or a request that names them still means what it did.
pub fn legacy_key<'a>(block_key: &str, key: &'a str) -> &'a str {
    if block_key != "eq" {
        return key;
    }
    match key {
        "low_hz" => "b1_hz",
        "low_db" => "b1_db",
        "mid1_hz" => "b2_hz",
        "mid1_db" => "b2_db",
        "mid1_q" => "b2_q",
        "mid2_hz" => "b3_hz",
        "mid2_db" => "b3_db",
        "mid2_q" => "b3_q",
        "high_hz" => "b4_hz",
        "high_db" => "b4_db",
        _ => key,
    }
}

/// A block's settings with the keys of older releases renamed (a user preset saved before the change).
pub fn migrate_params(block_key: &str, o: &Map<String, Value>) -> Map<String, Value> {
    o.iter()
        .map(|(k, v)| (legacy_key(block_key, k).to_string(), v.clone()))
        .collect()
}

/// A whole chain's settings with every block migrated.
pub fn migrate_chain(v: &Value) -> Value {
    match v.as_object() {
        Some(o) => Value::Object(
            o.iter()
                .map(|(b, bv)| match bv.as_object() {
                    Some(bo) => (b.clone(), Value::Object(migrate_params(b, bo))),
                    None => (b.clone(), bv.clone()),
                })
                .collect(),
        ),
        None => v.clone(),
    }
}

pub fn block_param(block_key: &str, key: &str) -> Option<&'static Param> {
    block(block_key)?.params.iter().find(|p| p.key == key)
}

/// Check one value against its parameter; returns it normalized (whole numbers as integers for `Int`).
fn check_value(p: &Param, v: &Value) -> Result<Value, String> {
    match p.kind {
        Kind::Bool => v
            .as_bool()
            .map(|b| json!(b))
            .ok_or_else(|| "must be true or false".to_string()),
        Kind::Float { min, max } => match v.as_f64() {
            Some(x) if x.is_finite() && (min..=max).contains(&x) => Ok(json!(x)),
            Some(x) if x.is_finite() => Err(format!("must be between {min} and {max}, got {x}")),
            _ => Err("must be a number".into()),
        },
        Kind::Int { min, max } => {
            let n = v
                .as_i64()
                .or_else(|| v.as_f64().filter(|x| x.fract() == 0.0).map(|x| x as i64));
            match n {
                Some(n) if (min..=max).contains(&n) => Ok(json!(n)),
                Some(n) => Err(format!("must be between {min} and {max}, got {n}")),
                None => Err("must be a whole number".into()),
            }
        }
        Kind::Choice(opts) => match v.as_str() {
            Some(s) if opts.contains(&s) => Ok(json!(s)),
            _ => Err(format!("must be one of {}", opts.join(", "))),
        },
        Kind::List { min, max, .. } => {
            let a = v.as_array().ok_or("must be a list of numbers")?;
            let mut out = vec![];
            for x in a {
                match x.as_f64() {
                    Some(x) if x.is_finite() && (min..=max).contains(&x) => out.push(x),
                    Some(x) if x.is_finite() => {
                        return Err(format!(
                            "every value must be between {min} and {max}, got {x}"
                        ))
                    }
                    _ => return Err("must be a list of numbers".into()),
                }
            }
            Ok(json!(out))
        }
    }
}

/// The checks that look at several keys of a block.
fn check_block(def: &BlockDef, o: &Map<String, Value>) -> Result<(), String> {
    if def.key == "multiband" {
        let list = |k: &str| -> Vec<f64> {
            o.get(k)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_f64).collect())
                .unwrap_or_default()
        };
        let x = list("crossovers_hz");
        if x.is_empty() || x.len() > 5 {
            return Err(format!(
                "`crossovers_hz` must list 1 to 5 frequencies (2 to 6 bands), got {}",
                x.len()
            ));
        }
        if x.windows(2).any(|w| w[1] <= w[0] * 1.05) {
            return Err(
                "`crossovers_hz` must ascend, each at least 5 percent above the one before".into(),
            );
        }
        for p in def.params {
            if matches!(p.kind, Kind::List { per_band: true, .. }) {
                let n = list(p.key).len();
                if n != x.len() + 1 {
                    return Err(format!(
                        "`{}` has {n} values but {} crossovers make {} bands",
                        p.key,
                        x.len(),
                        x.len() + 1
                    ));
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Shipped presets

/// A shipped whole-chain preset.
pub struct ChainPreset {
    pub name: &'static str,
    pub help: &'static str,
}

pub const CHAIN_PRESETS: [ChainPreset; 3] = [
    ChainPreset {
        name: "bypass",
        help: "Nothing in the chain: only the safety limiter (and the loudness controller's gain and the limiter it forces on, when it is enabled). The program is unchanged.",
    },
    ChainPreset {
        name: "gentle",
        help: "A light touch: the AGC within plus or minus 6 dB, a three-band compressor at 2:1, and the limiter. For slow music and ambient programs.",
    },
    ChainPreset {
        name: "radio",
        help: "The broadcast sound: the AGC (rides a fade up to 12 dB, clamps on a loud onset), a five-band compressor at about 3:1 and the limiter.",
    },
];

fn set(c: &mut ChainConfig, block_key: &str, kv: &[(&str, Value)]) {
    let o: Map<String, Value> = kv.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    c.apply_block(block_key, &o)
        .unwrap_or_else(|e| panic!("shipped preset: {e}"));
}

/// The shipped preset by name.
pub fn preset_config(name: &str) -> Option<ChainConfig> {
    let mut c = ChainConfig::default();
    match name {
        "bypass" => {}
        "radio" => {
            set(&mut c, "agc", &[("enabled", json!(true))]);
            set(&mut c, "eq", &[("enabled", json!(true))]);
            set(&mut c, "multiband", &[("enabled", json!(true))]);
            set(&mut c, "limiter", &[("enabled", json!(true))]);
        }
        "gentle" => {
            set(
                &mut c,
                "agc",
                &[
                    ("enabled", json!(true)),
                    ("max_gain_db", json!(6.0)),
                    ("max_reduction_db", json!(6.0)),
                    ("up_db_per_s", json!(1.5)),
                    ("detector_ms", json!(800.0)),
                    ("fast_attack_ms", json!(40.0)),
                ],
            );
            set(&mut c, "eq", &[("enabled", json!(true))]);
            set(
                &mut c,
                "multiband",
                &[
                    ("enabled", json!(true)),
                    ("crossovers_hz", json!([200.0, 4000.0])),
                    ("threshold_dbfs", json!([-24.0, -22.0, -24.0])),
                    ("ratio", json!([2.0, 2.0, 2.0])),
                    ("knee_db", json!([6.0, 6.0, 6.0])),
                    ("attack_ms", json!([30.0, 12.0, 5.0])),
                    ("release_ms", json!([400.0, 200.0, 120.0])),
                    ("makeup_db", json!([2.0, 2.0, 2.0])),
                ],
            );
            set(&mut c, "limiter", &[("enabled", json!(true))]);
        }
        _ => return None,
    }
    Some(c)
}

/// A shipped preset of one effect (block): a name, what it is for, and the block's settings.
pub struct EffectPreset {
    pub name: &'static str,
    pub help: &'static str,
    pub params: Value,
}

/// The shipped presets of one block. Every block lists at least the settings of the whole-chain presets it takes
/// part in; a few blocks add voicings.
pub fn effect_presets(block_key: &str) -> Vec<EffectPreset> {
    let mut out = vec![];
    let Some(def) = block(block_key) else {
        return out;
    };
    let _ = def;
    let mut seen: Vec<(String, Value)> = vec![];
    for (name, help) in [
        ("radio", "As the radio chain preset sets it."),
        ("gentle", "As the gentle chain preset sets it."),
    ] {
        let c = preset_config(name).unwrap();
        let p = c.params_json(block_key);
        // Blocks off in both presets carry only their defaults; show one entry named `default`.
        if !c.enabled(block_key) && block_key != "trim" && block_key != "makeup" {
            continue;
        }
        if seen.iter().any(|(_, v)| *v == p) {
            continue;
        }
        seen.push((name.to_string(), p.clone()));
        out.push(EffectPreset {
            name,
            help,
            params: p,
        });
    }
    let d = ChainConfig::default();
    let default = d.params_json(block_key);
    let extra = |name: &'static str, help: &'static str, kv: &[(&str, Value)]| -> EffectPreset {
        let mut c = d.clone();
        let o: Map<String, Value> = kv.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        c.apply_block(block_key, &o)
            .unwrap_or_else(|e| panic!("effect preset {block_key}/{name}: {e}"));
        EffectPreset {
            name,
            help,
            params: c.params_json(block_key),
        }
    };
    match block_key {
        "eq" => {
            out.push(extra("flat", "Every band at 0 dB.", &[]));
            out.push(extra(
                "warm",
                "A little more low end and a little less top.",
                &[("b1_db", json!(2.5)), ("b4_db", json!(-2.0))],
            ));
            out.push(extra(
                "presence",
                "Lifts the range where voices and snares live.",
                &[("b3_db", json!(2.5)), ("b3_q", json!(0.9))],
            ));
            out.push(extra(
                "smile",
                "More bass and more top, a little less middle.",
                &[
                    ("b1_db", json!(3.0)),
                    ("b2_db", json!(-2.0)),
                    ("b4_db", json!(3.0)),
                ],
            ));
            out.push(extra(
                "TV-era rolloff",
                "The top end of an analog television set: flat to 2 kHz, a dip of 3.6 dB at 2.08 kHz (Q 1.57), 1.7 dB at 4.087 kHz and a high shelf of 6.4 dB at 8.043 kHz. Three bands.",
                &[
                    ("bands", json!(3)),
                    ("b1_type", json!("peak")),
                    ("b1_hz", json!(2080.0)),
                    ("b1_db", json!(-3.6)),
                    ("b1_q", json!(1.57)),
                    ("b2_type", json!("peak")),
                    ("b2_hz", json!(4087.0)),
                    ("b2_db", json!(-1.7)),
                    ("b2_q", json!(1.0)),
                    ("b3_type", json!("high_shelf")),
                    ("b3_q", json!(EQ_SHELF_Q)),
                    ("b3_hz", json!(8043.0)),
                    ("b3_db", json!(-6.4)),
                ],
            ));
        }
        "multiband" => {
            out.push(extra(
                "four band",
                "Four bands at 150 Hz, 1.5 kHz and 5 kHz, ratio 3:1.",
                &[
                    ("crossovers_hz", json!([150.0, 1500.0, 5000.0])),
                    ("threshold_dbfs", json!([-26.0, -23.0, -22.0, -24.0])),
                    ("ratio", json!([3.0, 3.0, 3.0, 2.5])),
                    ("knee_db", json!([6.0, 6.0, 6.0, 6.0])),
                    ("attack_ms", json!([40.0, 18.0, 8.0, 4.0])),
                    ("release_ms", json!([400.0, 250.0, 160.0, 120.0])),
                    ("makeup_db", json!([3.0, 3.0, 3.0, 3.0])),
                ],
            ));
            out.push(extra(
                "two band",
                "Two bands split at 300 Hz at 2.5:1: holds the low end apart from the rest.",
                &[
                    ("crossovers_hz", json!([300.0])),
                    ("threshold_dbfs", json!([-24.0, -22.0])),
                    ("ratio", json!([2.5, 2.5])),
                    ("knee_db", json!([6.0, 6.0])),
                    ("attack_ms", json!([30.0, 10.0])),
                    ("release_ms", json!([350.0, 180.0])),
                    ("makeup_db", json!([2.0, 2.0])),
                ],
            ));
        }
        "wideband" => {
            out.push(extra(
                "slow leveler",
                "A slow 2:1 stage that evens out the program.",
                &[],
            ));
            out.push(extra(
                "firm",
                "A firmer 3:1 stage with a faster release.",
                &[
                    ("ratio", json!(3.0)),
                    ("release_ms", json!(250.0)),
                    ("threshold_dbfs", json!(-22.0)),
                ],
            ));
        }
        "clipper" => {
            out.push(extra(
                "soft",
                "A soft clip at -1.5 dBFS that bends 3 dB early.",
                &[],
            ));
            out.push(extra(
                "firm",
                "A harder clip at -1 dBFS that bends 1 dB early.",
                &[("threshold_dbfs", json!(-1.0)), ("softness_db", json!(1.0))],
            ));
        }
        "highpass" => {
            out.push(extra("subsonic", "12 dB per octave at 25 Hz.", &[]));
            out.push(extra(
                "rumble",
                "24 dB per octave at 40 Hz.",
                &[("freq_hz", json!(40.0)), ("order", json!(4))],
            ));
        }
        "rotator" => {
            out.push(extra("voice", "Three sections at 200 Hz.", &[]));
            out.push(extra(
                "strong",
                "Five sections at 160 Hz.",
                &[("sections", json!(5)), ("freq_hz", json!(160.0))],
            ));
        }
        "stereo" => {
            out.push(extra("subtle", "Width 1.15.", &[("width", json!(1.15))]));
            out.push(extra("wide", "Width 1.5.", &[("width", json!(1.5))]));
            out.push(extra("narrow", "Width 0.7.", &[("width", json!(0.7))]));
        }
        "bass" => {
            out.push(extra(
                "light",
                "A little added bass harmonics.",
                &[("amount_pct", json!(15.0))],
            ));
            out.push(extra(
                "strong",
                "Hip-hop and dance: strong harmonics.",
                &[("amount_pct", json!(60.0)), ("drive", json!(5.0))],
            ));
        }
        "limiter" => {
            out.push(extra(
                "-1 dBTP",
                "The ceiling YouTube and Twitch want.",
                &[],
            ));
            out.push(extra(
                "-2 dBTP",
                "More margin for a low-bit-rate encode.",
                &[("ceiling_dbtp", json!(-2.0))],
            ));
        }
        "trim" | "makeup" => {
            out.push(extra("unity", "0 dB.", &[]));
        }
        _ => {}
    }
    if out.is_empty() {
        out.push(EffectPreset {
            name: "default",
            help: "The block's default settings.",
            params: default,
        });
    }
    // The same name twice (`radio` and `agc` only has one voicing) is dropped.
    let mut names: Vec<&str> = vec![];
    out.retain(|p| {
        if names.contains(&p.name) {
            false
        } else {
            names.push(p.name);
            true
        }
    });
    out
}

/// The schema as JSON for `GET /api/v1/chain` and the UI.
pub fn schema_json() -> Value {
    Value::Array(
        BLOCKS
            .iter()
            .map(|b| {
                json!({
                    "key": b.key, "label": b.label, "help": b.help, "switch": b.switch,
                    "params": b.params.iter().map(|p| {
                        let (kind, extra) = match p.kind {
                            Kind::Bool => ("bool", json!({})),
                            Kind::Float { min, max } => ("float", json!({"min": min, "max": max, "step": p.step})),
                            Kind::Int { min, max } => ("int", json!({"min": min, "max": max})),
                            Kind::Choice(o) => ("choice", json!({"options": o})),
                            Kind::List { min, max, per_band } => ("list", json!({"min": min, "max": max, "per_band": per_band})),
                        };
                        let mut j = json!({"key": p.key, "label": p.label, "kind": kind, "unit": p.unit, "help": p.help, "default": default_value(&p.default)});
                        if let (Some(o), Some(e)) = (j.as_object_mut(), extra.as_object()) {
                            for (k, v) in e { o.insert(k.clone(), v.clone()); }
                        }
                        j
                    }).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_shipped_presets_resolve() {
        ChainConfig::default().check().unwrap();
        for p in &CHAIN_PRESETS {
            let c = preset_config(p.name).unwrap();
            c.check().unwrap();
            assert_eq!(ChainConfig::resolve(p.name, &Map::new()).unwrap(), c);
        }
        let r = preset_config("radio").unwrap();
        assert_eq!(r.bands(), 5);
        assert_eq!(preset_config("gentle").unwrap().bands(), 3);
        assert!(!preset_config("bypass").unwrap().enabled("agc"));
        // The optional blocks are off in the shipped presets.
        for n in ["radio", "gentle"] {
            let c = preset_config(n).unwrap();
            for b in [
                "highpass", "rotator", "stereo", "bass", "wideband", "clipper",
            ] {
                assert!(!c.enabled(b), "{n}/{b}");
            }
        }
    }

    #[test]
    fn a_mismatched_band_array_is_rejected() {
        let mut c = preset_config("radio").unwrap();
        let o: Map<String, Value> = [("crossovers_hz".to_string(), json!([200.0, 4000.0]))]
            .into_iter()
            .collect();
        let e = c.apply_block("multiband", &o).unwrap_err();
        assert!(e.contains("3 bands") || e.contains("bands"), "{e}");
        // A station file carrying the same mismatch names the block.
        let ov: Map<String, Value> =
            serde_json::from_value(json!({"multiband": {"crossovers_hz": [200.0, 4000.0]}}))
                .unwrap();
        let e = ChainConfig::resolve("radio", &ov).unwrap_err();
        assert!(e[0].0.starts_with("chain.multiband"), "{e:?}");
        // The whole change, band count and lists together, is fine.
        let o: Map<String, Value> = serde_json::from_value(json!({
            "crossovers_hz": [200.0, 4000.0], "threshold_dbfs": [-24.0, -22.0, -24.0], "ratio": [2.0, 2.0, 2.0],
            "knee_db": [6.0, 6.0, 6.0], "attack_ms": [30.0, 12.0, 5.0], "release_ms": [400.0, 200.0, 120.0], "makeup_db": [2.0, 2.0, 2.0]
        })).unwrap();
        c.apply_block("multiband", &o).unwrap();
        assert_eq!(c.bands(), 3);
        // Crossovers must ascend.
        let o: Map<String, Value> = [("crossovers_hz".to_string(), json!([500.0, 120.0]))]
            .into_iter()
            .collect();
        assert!(preset_config("gentle")
            .unwrap()
            .apply_block("multiband", &o)
            .is_err());
    }

    #[test]
    fn ranges_and_types_are_enforced_and_name_the_key() {
        let mut c = ChainConfig::default();
        let e = c
            .apply_block(
                "agc",
                &serde_json::from_value(json!({"target_dbfs": 3.0})).unwrap(),
            )
            .unwrap_err();
        assert!(e.contains("target_dbfs"), "{e}");
        assert!(c
            .apply_block("agc", &serde_json::from_value(json!({"nope": 1})).unwrap())
            .is_err());
        assert!(c
            .apply_block(
                "trim",
                &serde_json::from_value(json!({"enabled": true})).unwrap()
            )
            .is_err());
        assert!(c
            .apply_block(
                "eq",
                &serde_json::from_value(json!({"position": "middle"})).unwrap()
            )
            .is_err());
        assert_eq!(
            c.num("agc", "target_dbfs"),
            -20.0,
            "an error changes nothing"
        );
    }

    #[test]
    fn the_diff_against_a_preset_round_trips() {
        let base = preset_config("radio").unwrap();
        let mut c = base.clone();
        c.apply_block(
            "agc",
            &serde_json::from_value(json!({"target_dbfs": -22.0})).unwrap(),
        )
        .unwrap();
        let d = c.diff(&base);
        assert_eq!(d.len(), 1);
        assert_eq!(ChainConfig::resolve("radio", &d).unwrap(), c);
    }

    #[test]
    fn every_block_has_effect_presets_that_pass_the_checks() {
        for b in BLOCKS {
            let ps = effect_presets(b.key);
            assert!(!ps.is_empty(), "{}", b.key);
            for p in ps {
                let mut c = ChainConfig::default();
                c.set_block_params(b.key, p.params.as_object().unwrap())
                    .unwrap_or_else(|e| panic!("{}/{}: {e}", b.key, p.name));
            }
        }
    }

    #[test]
    fn the_equalizer_is_parametric_and_the_old_keys_still_work() {
        let d = ChainConfig::default();
        assert_eq!(d.num("eq", "bands"), 4.0);
        assert_eq!(d.text("eq", "b1_type"), "low_shelf");
        assert_eq!(d.text("eq", "b4_type"), "high_shelf");
        assert!(d.flag("eq", "b4_on") && !d.flag("eq", "b5_on"));
        // The old keys are bands 1 to 4, whichever way they arrive.
        let mut c = ChainConfig::default();
        c.apply_block(
            "eq",
            json!({"low_hz": 60.0, "low_db": -7.5, "mid1_q": 2.5, "mid2_hz": 5200.0, "high_db": 9.0})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(c.num("eq", "b1_hz"), 60.0);
        assert_eq!(c.num("eq", "b1_db"), -7.5);
        assert_eq!(c.num("eq", "b2_q"), 2.5);
        assert_eq!(c.num("eq", "b3_hz"), 5200.0);
        assert_eq!(c.num("eq", "b4_db"), 9.0);
        assert!(c.blocks["eq"].get("low_db").is_none());
        let r = ChainConfig::resolve(
            "radio",
            json!({"eq": {"mid1_db": -2.0}}).as_object().unwrap(),
        )
        .unwrap();
        assert_eq!(r.num("eq", "b2_db"), -2.0);
        let m = migrate_chain(&json!({"eq": {"low_db": 3.0}, "agc": {"target_dbfs": -20.0}}));
        assert_eq!(
            m,
            json!({"eq": {"b1_db": 3.0}, "agc": {"target_dbfs": -20.0}})
        );
        // The band count and each band's ranges.
        for (k, v) in [
            ("bands", json!(9)),
            ("bands", json!(0)),
            ("b1_hz", json!(19.0)),
            ("b8_hz", json!(20001.0)),
            ("b2_db", json!(18.1)),
            ("b3_q", json!(0.09)),
            ("b3_q", json!(10.5)),
            ("b2_type", json!("comb")),
        ] {
            let mut c = ChainConfig::default();
            assert!(
                c.apply_block(
                    "eq",
                    &[(k.to_string(), v.clone())]
                        .into_iter()
                        .collect::<Map<_, _>>()
                )
                .is_err(),
                "{k} {v}"
            );
        }
        // Values are kept as given: a frequency of 4087 Hz, 1.57 Q, -1.7 dB.
        let mut c = ChainConfig::default();
        c.apply_block(
            "eq",
            json!({"b2_hz": 4087.0, "b2_db": -1.7, "b2_q": 1.57})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            (
                c.num("eq", "b2_hz"),
                c.num("eq", "b2_db"),
                c.num("eq", "b2_q")
            ),
            (4087.0, -1.7, 1.57)
        );
        // The schema carries the resolution of each number.
        let sj = schema_json();
        let eq = sj
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["key"] == "eq")
            .unwrap();
        let step = |k: &str| {
            eq["params"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["key"] == k)
                .unwrap()["step"]
                .as_f64()
                .unwrap()
        };
        assert_eq!(
            (step("b1_hz"), step("b1_db"), step("b1_q")),
            (1.0, 0.1, 0.01)
        );
        // The shelf default is the old shelf's Q to the bit, and the shipped TV-era preset resolves.
        assert_eq!(EQ_SHELF_Q, 1.0 / std::f64::consts::SQRT_2);
        assert!(effect_presets("eq")
            .iter()
            .any(|p| p.name == "TV-era rolloff"));
    }
}
