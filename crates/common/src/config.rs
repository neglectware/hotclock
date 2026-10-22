//! Configuration files (architecture 5.1): `/etc/hotclock/hotclock.toml` and
//! `/etc/hotclock/stations/<name>.toml`. Every error names the file and the key.

use serde::Deserialize;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

pub const DEFAULT_GLOBAL_PATH: &str = "/etc/hotclock/hotclock.toml";

/// Sample formats the output may be opened with, in the order of preference of 1.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Hash)]
pub enum SampleFormat {
    #[serde(rename = "S32_LE")]
    S32Le,
    #[serde(rename = "S24_3LE")]
    S24_3Le,
    #[serde(rename = "S16_LE")]
    S16Le,
}

impl SampleFormat {
    pub const DEFAULT_ORDER: [SampleFormat; 3] = [
        SampleFormat::S32Le,
        SampleFormat::S24_3Le,
        SampleFormat::S16Le,
    ];

    pub fn name(self) -> &'static str {
        match self {
            SampleFormat::S32Le => "S32_LE",
            SampleFormat::S24_3Le => "S24_3LE",
            SampleFormat::S16Le => "S16_LE",
        }
    }

    pub fn bytes_per_sample(self) -> usize {
        match self {
            SampleFormat::S32Le => 4,
            SampleFormat::S24_3Le => 3,
            SampleFormat::S16Le => 2,
        }
    }
}

impl fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One problem in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub file: PathBuf,
    pub key: String,
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.key.is_empty() {
            write!(f, "{}: {}", self.file.display(), self.message)
        } else {
            write!(
                f,
                "{}: key `{}`: {}",
                self.file.display(),
                self.key,
                self.message
            )
        }
    }
}

impl std::error::Error for ConfigError {}

/// All the problems found in one file (validation reports every one it finds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigErrors(pub Vec<ConfigError>);

impl fmt::Display for ConfigErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, e) in self.0.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{e}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigErrors {}

// ---------------------------------------------------------------------------------------------
// Global config

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct Brand {
    /// Overrides `DISPLAY_NAME` (architecture 0).
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct LibraryServiceConfig {
    pub listen: String,
}

impl Default for LibraryServiceConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:8300".into(),
        }
    }
}

/// The `[display]` block of the global config (architecture 11): what the status display needs to know about
/// its screen. Every key has a default; the primary target is the 7-inch 1024 x 600 panel.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct DisplayConfig {
    /// The KMS mode, `WIDTHxHEIGHT` or `WIDTHxHEIGHT@HZ`. Set explicitly through KMS; the panel's EDID is never
    /// trusted for the resolution (11, 7d). A mode the connector does not list is added as a custom mode.
    pub mode: String,
    /// The diagonal of the screen in inches: the pixel density, and so the minimum font size (11.2), follows from it.
    pub diagonal_in: f64,
    /// The viewing distance in centimeters the legibility rule is computed for (11.2).
    pub view_distance_cm: f64,
    /// The view used until the library service has said one: `single`, `split`, `rotate` or `processing` (11.2b).
    pub default_view: String,
    /// A bypassed stage: `show` it grey with `BYP`, or `hide` it and reflow the columns (11.2).
    pub bypassed_stages: String,
    /// The processing view: `spectrum`, `curve` or `both` (11.2a).
    pub processing_view: String,
    /// The spectrum: `post`, `pre` or `both` (11.2a).
    pub spectrum_source: String,
    /// How far down the gain curve hangs, in dB (default 12, up to 18).
    pub curve_range_db: f64,
    /// Add the bands' makeup gain to the curve (it is not by default).
    pub curve_include_makeup: bool,
    /// The connector to use (`HDMI-A-1`); the first connected one when unset.
    pub connector: Option<String>,
    /// The DRM device (`/dev/dri/card1`); the first one with a connected connector when unset.
    pub device: Option<String>,
    /// In `rotate`, a critical alert on a station pins the view to it until it clears (11.2b). A key of the
    /// config rather than a column of the library's `display_view` table: a new column would make the library
    /// database unreadable by the previous package, and `upgrade.sh rollback` must keep working.
    pub rotate_pin_on_alert: bool,
    /// In `rotate`, show each station's processing view as well as its normal one (so with two stations: office,
    /// office processing, eve, eve processing). Off: the rotation alternates between the stations only.
    pub rotate_processing: bool,
    /// The processing view (`processing`): how far down the gain-reduction scale goes, in dB. One scale for every bar.
    pub proc_range_db: f64,
    /// The processing view: draw the spectrum under the bars where the screen has room for it (never on the
    /// 7-inch 1024 x 600 panel, where the 31 px text leaves none).
    pub proc_spectrum: bool,
    /// The small processing strip in the compact `single` and `split` views (11.2c): `off`, `auto` (while the chain
    /// is not fully bypassed) or `on` (always).
    pub mini_processing: String,
    /// The shared top bar of `split` on the web display: the product name and version at the left.
    pub bar_product: bool,
    /// The top bar: the clock in the middle.
    pub bar_clock: bool,
    /// The top bar: the date at the right (`SAT 10 OCT`).
    pub bar_date: bool,
    /// The top bar: the whole box's CPU use at the right (`CPU 7%`), averaged over 5 s.
    pub bar_cpu: bool,
    /// The top bar: a dot at the left, green while the box has a network route out, red when not.
    pub bar_network: bool,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            mode: "1024x600@60".into(),
            diagonal_in: 7.0,
            view_distance_cm: 70.0,
            default_view: "single".into(),
            bypassed_stages: "show".into(),
            processing_view: "both".into(),
            spectrum_source: "post".into(),
            curve_range_db: 12.0,
            curve_include_makeup: false,
            connector: None,
            device: None,
            rotate_pin_on_alert: true,
            rotate_processing: false,
            proc_range_db: 18.0,
            proc_spectrum: false,
            mini_processing: "auto".into(),
            bar_product: true,
            bar_clock: true,
            bar_date: true,
            bar_cpu: true,
            bar_network: false,
        }
    }
}

/// `1024x600` or `1024x600@60` to `(width, height, refresh_hz)`; the refresh defaults to 60.
pub fn parse_mode(s: &str) -> Option<(u32, u32, u32)> {
    let (size, hz) = match s.split_once('@') {
        Some((a, b)) => (a, b.parse::<u32>().ok()?),
        None => (s, 60),
    };
    let (w, h) = size.split_once('x')?;
    let (w, h) = (w.parse::<u32>().ok()?, h.parse::<u32>().ok()?);
    if !(320..=7680).contains(&w) || !(200..=4320).contains(&h) || !(24..=240).contains(&hz) {
        return None;
    }
    Some((w, h, hz))
}

impl DisplayConfig {
    /// Check the values, naming the key; shared by the global config and the display's own tests.
    pub fn validate(&self, file: &Path) -> Vec<ConfigError> {
        let mut errs = Vec::new();
        let mut bad = |key: &str, message: String| {
            errs.push(ConfigError {
                file: file.to_path_buf(),
                key: format!("display.{key}"),
                message,
            })
        };
        if parse_mode(&self.mode).is_none() {
            bad(
                "mode",
                format!("must look like `1024x600@60`, got `{}`", self.mode),
            );
        }
        if !(1.0..=100.0).contains(&self.diagonal_in) {
            bad(
                "diagonal_in",
                format!("must be between 1 and 100 inches, got {}", self.diagonal_in),
            );
        }
        if !(20.0..=1000.0).contains(&self.view_distance_cm) {
            bad(
                "view_distance_cm",
                format!("must be between 20 and 1000, got {}", self.view_distance_cm),
            );
        }
        for (key, v, ok) in [
            (
                "default_view",
                &self.default_view,
                &["single", "split", "rotate", "processing"][..],
            ),
            (
                "bypassed_stages",
                &self.bypassed_stages,
                &["show", "hide"][..],
            ),
            (
                "processing_view",
                &self.processing_view,
                &["spectrum", "curve", "both"][..],
            ),
            (
                "spectrum_source",
                &self.spectrum_source,
                &["pre", "post", "both"][..],
            ),
            (
                "mini_processing",
                &self.mini_processing,
                &["off", "auto", "on"][..],
            ),
        ] {
            if !ok.contains(&v.as_str()) {
                bad(key, format!("must be one of {}, got `{v}`", ok.join(", ")));
            }
        }
        if !(3.0..=18.0).contains(&self.curve_range_db) {
            bad(
                "curve_range_db",
                format!("must be between 3 and 18, got {}", self.curve_range_db),
            );
        }
        if !(6.0..=30.0).contains(&self.proc_range_db) {
            bad(
                "proc_range_db",
                format!("must be between 6 and 30, got {}", self.proc_range_db),
            );
        }
        errs
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct GlobalConfig {
    /// `/var/lib/hotclock`: library.db, media, inbox, stations, backup.
    pub data_dir: PathBuf,
    /// `/etc/hotclock/stations`
    pub stations_dir: PathBuf,
    /// `/run/hotclock`: start counters, sockets.
    pub run_dir: PathBuf,
    /// IANA name; used from M1.5 on (validated there against the tz database).
    pub time_zone: String,
    pub library: LibraryServiceConfig,
    pub brand: Brand,
    /// Extra names this box answers to in the `Host` header (13), besides its host name, `localhost` and
    /// IP addresses: a CNAME, a tailnet alias.
    pub allowed_hosts: Vec<String>,
    /// Where the SMTP password lives (12.1, 13): mode 0600, never in a database or an API answer.
    pub secrets_file: PathBuf,
    /// The status display's screen and views (11).
    pub display: DisplayConfig,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            data_dir: "/var/lib/hotclock".into(),
            stations_dir: "/etc/hotclock/stations".into(),
            run_dir: "/run/hotclock".into(),
            time_zone: "America/New_York".into(),
            library: LibraryServiceConfig::default(),
            brand: Brand::default(),
            allowed_hosts: Vec::new(),
            secrets_file: "/etc/hotclock/secrets.toml".into(),
            display: DisplayConfig::default(),
        }
    }
}

impl GlobalConfig {
    pub fn library_db_path(&self) -> PathBuf {
        self.data_dir.join("library.db")
    }
    pub fn media_dir(&self) -> PathBuf {
        self.data_dir.join("media")
    }
    pub fn inbox_dir(&self) -> PathBuf {
        self.data_dir.join("inbox")
    }
    pub fn upload_tmp_dir(&self) -> PathBuf {
        self.data_dir.join("upload-tmp")
    }
    pub fn backup_dir(&self) -> PathBuf {
        self.data_dir.join("backup")
    }
    pub fn station_state_dir(&self, station: &str) -> PathBuf {
        self.data_dir.join("stations").join(station)
    }

    pub fn parse(text: &str, file: &Path) -> Result<Self, ConfigErrors> {
        let cfg: GlobalConfig = deserialize(text, file)?;
        let mut errs = Vec::new();
        let mut bad = |key: &str, message: String| {
            errs.push(ConfigError {
                file: file.to_path_buf(),
                key: key.into(),
                message,
            })
        };
        for (key, p) in [
            ("data_dir", &cfg.data_dir),
            ("stations_dir", &cfg.stations_dir),
            ("run_dir", &cfg.run_dir),
        ] {
            if !p.is_absolute() {
                bad(
                    key,
                    format!("must be an absolute path, got `{}`", p.display()),
                );
            }
        }
        if cfg.library.listen.parse::<SocketAddr>().is_err() {
            bad(
                "library.listen",
                format!(
                    "must be an address like `0.0.0.0:8300`, got `{}`",
                    cfg.library.listen
                ),
            );
        }
        if cfg.time_zone.trim().is_empty() {
            bad("time_zone", "must not be empty".into());
        }
        if let Some(n) = &cfg.brand.display_name {
            if n.trim().is_empty() {
                bad("brand.display_name", "must not be empty when set".into());
            }
        }
        errs.extend(cfg.display.validate(file));
        if errs.is_empty() {
            Ok(cfg)
        } else {
            Err(ConfigErrors(errs))
        }
    }

    pub fn load(path: &Path) -> Result<Self, ConfigErrors> {
        let text = read(path)?;
        Self::parse(&text, path)
    }

    /// Load `path` if it exists, otherwise the defaults (a development machine has no
    /// `/etc/hotclock`). A file that exists and is wrong is still an error.
    pub fn load_or_default(path: &Path) -> Result<Self, ConfigErrors> {
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self::default())
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Station config

/// The range of `output_trim_db`, dB.
pub const OUTPUT_TRIM_MIN_DB: f64 = -40.0;
pub const OUTPUT_TRIM_MAX_DB: f64 = 6.0;

/// Transition presets of architecture 2.6. Validated by name here; the model itself is M1.4a.
pub const TRANSITION_PRESETS: [&str; 5] = ["basic", "plexamp", "sam", "fm", "none"];

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StationConfig {
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    pub listen: String,
    /// An ALSA device name (`hw:CARD=PCH,DEV=0`), `null` (paced, discards audio) or
    /// `file:<path.wav>` (writes a WAV as fast as possible).
    pub device: String,
    #[serde(default = "d_rate")]
    pub rate: u32,
    #[serde(default = "d_period")]
    pub period_frames: u32,
    #[serde(default = "d_periods")]
    pub periods: u32,
    #[serde(default = "d_formats")]
    pub formats: Vec<SampleFormat>,
    #[serde(default = "d_target_lufs")]
    pub target_lufs: f64,
    /// The most gain normalization may add to a quiet item (3.6), dB.
    #[serde(default = "d_max_boost")]
    pub max_boost_db: f64,
    /// How far into the safety limiter normalization may push a peak (3.6), dB.
    #[serde(default = "d_peak_allowance")]
    pub peak_allowance_db: f64,
    /// Fade the end of an item flagged `truncated_fade` (its file stops in the middle of its own fade) to
    /// silence at its cue-out, when nothing else fades it and it has no manual fade-out.
    #[serde(default = "d_true")]
    pub truncated_fade: bool,
    /// The length of that fade, seconds (0.5 to 10).
    #[serde(default = "d_truncated_fade_s")]
    pub truncated_fade_s: f64,
    /// A gain for the whole output, dB (2.4): after the safety limiter and the master gain, on the sound card
    /// and the network stream alike. It replaces `alsamixer` for matching a card's analog stage to the next
    /// device. -40 to +6, default 0. Changeable at run time (web page, API); a value set there is kept
    /// in the station's `state.db` and wins over this key until it is cleared.
    #[serde(default)]
    pub output_trim_db: f64,
    #[serde(default)]
    pub default_playlist: Option<String>,
    #[serde(default = "d_preset")]
    pub transition_preset: String,
    #[serde(default)]
    pub emergency_playlist: Option<String>,
    /// A part-time station (one that runs for a show and is stopped the rest of the time): an empty queue is
    /// never starvation, so there is no `queue_starved` alert and no emergency playlist; the station is silent.
    #[serde(default)]
    pub stopped_is_normal: bool,
    #[serde(default = "d_ram_loop")]
    pub ram_loop_seconds: u32,
    #[serde(default = "d_lock_ahead")]
    pub lock_ahead_s: u32,
    #[serde(default = "d_silence_dbfs")]
    pub silence_dbfs: f64,
    #[serde(default = "d_silence_s")]
    pub silence_s: u32,
    /// Seconds on air and healthy after which the `restarted` alert clears itself (0: never).
    #[serde(default = "d_restart_alert_clear_s")]
    pub restart_alert_clear_s: u32,
    #[serde(default = "d_processing")]
    pub processing_preset: String,
    #[serde(default)]
    pub library: Option<PathBuf>,
    /// The network stream output (1.3a). Off unless `[stream]` says `enabled = true`.
    #[serde(default)]
    pub stream: StreamConfig,
    /// Trim, in parts per million, of the clock that paces a card-less station (`device = "null"`) or
    /// `file-paced:`; positive runs the station faster (1.3a). 0 unless the drift soak measured an offset.
    #[serde(default)]
    pub rate_trim_ppm: f64,
    /// The loudness controller (architecture 6.1). Off unless `[loudness]` says `enabled = true`.
    #[serde(default)]
    pub loudness: LoudnessConfig,
    /// The processing chain's settings that differ from the preset `processing_preset` names (architecture 6):
    /// one table per block, `[chain.agc]` and so on. [`StationConfig::chain_config`] resolves them.
    #[serde(default)]
    pub chain: serde_json::Map<String, serde_json::Value>,
}

/// The settings a loudness preset fixes (the rest of `[loudness]` is the station's own).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoudnessPreset {
    pub name: &'static str,
    pub help: &'static str,
    pub within_item: &'static str,
    pub item_memory_s: f64,
    pub robust_items: u32,
    pub item_cap_s: f64,
    pub peak_allowance_db: f64,
    pub max_limiter_gr_db: f64,
    pub max_slew_db_per_min: f64,
    pub deadband_lu: f64,
}

/// The loudness presets (M6a), the way an Optimod has its factory programs: a station picks one, individual
/// settings override it (then the preset reads `custom`). `ambient` is the office's.
pub const LOUDNESS_PRESETS: [LoudnessPreset; 3] = [
    LoudnessPreset {
        name: "ambient",
        help: "Slow music, long quiet passages (the office): the level is judged per item, so a quiet passage is left alone, and nothing is pushed into the limiter",
        within_item: "hold",
        item_memory_s: 600.0,
        robust_items: 5,
        item_cap_s: 240.0,
        peak_allowance_db: 0.0,
        max_limiter_gr_db: 5.0,
        max_slew_db_per_min: 1.0,
        deadband_lu: 0.25,
    },
    LoudnessPreset {
        name: "balanced",
        help: "Mixed programming: per-item judgment with a shorter memory, a little peak allowance for the limiter",
        within_item: "hold",
        item_memory_s: 300.0,
        robust_items: 3,
        item_cap_s: 180.0,
        peak_allowance_db: 2.0,
        max_limiter_gr_db: 8.0,
        max_slew_db_per_min: 1.0,
        deadband_lu: 0.25,
    },
    LoudnessPreset {
        name: "aggressive",
        help: "Top-40 style: the gain follows the loudness continuously and faster, and the limiter is allowed to work hard",
        within_item: "follow",
        item_memory_s: 600.0,
        robust_items: 1,
        item_cap_s: 240.0,
        peak_allowance_db: 6.0,
        max_limiter_gr_db: 24.0,
        max_slew_db_per_min: 2.0,
        deadband_lu: 0.15,
    },
];

pub fn loudness_preset(name: &str) -> Option<&'static LoudnessPreset> {
    LOUDNESS_PRESETS.iter().find(|p| p.name == name)
}

/// The `[loudness]` block as the file spells it: the preset, then whatever overrides it.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
struct RawLoudness {
    preset: String,
    user_preset: String,
    enabled: bool,
    target_lufs: f64,
    true_peak_dbtp: f64,
    window_s: f64,
    max_slew_db_per_min: Option<f64>,
    deadband_lu: Option<f64>,
    gain_min_db: f64,
    gain_max_db: f64,
    min_valid_s: f64,
    gate_categories: Vec<String>,
    trim_tau_s: f64,
    trim_range_db: f64,
    within_item: Option<String>,
    item_memory_s: Option<f64>,
    robust_items: Option<u32>,
    item_cap_s: Option<f64>,
    peak_allowance_db: Option<f64>,
    max_limiter_gr_db: Option<f64>,
}

impl Default for RawLoudness {
    fn default() -> Self {
        let d = LoudnessConfig::default();
        Self {
            preset: d.preset,
            user_preset: String::new(),
            enabled: d.enabled,
            target_lufs: d.target_lufs,
            true_peak_dbtp: d.true_peak_dbtp,
            window_s: d.window_s,
            max_slew_db_per_min: None,
            deadband_lu: None,
            gain_min_db: d.gain_min_db,
            gain_max_db: d.gain_max_db,
            min_valid_s: d.min_valid_s,
            gate_categories: d.gate_categories,
            trim_tau_s: d.trim_tau_s,
            trim_range_db: d.trim_range_db,
            within_item: None,
            item_memory_s: None,
            robust_items: None,
            item_cap_s: None,
            peak_allowance_db: None,
            max_limiter_gr_db: None,
        }
    }
}

impl TryFrom<RawLoudness> for LoudnessConfig {
    type Error = String;
    fn try_from(r: RawLoudness) -> Result<Self, String> {
        // `custom` as a name starts from `ambient` and keeps whatever the file says.
        let base = if r.preset == "custom" {
            "ambient"
        } else {
            r.preset.as_str()
        };
        let p = loudness_preset(base).ok_or_else(|| {
            format!(
                "preset must be one of {} or custom, got `{}`",
                LOUDNESS_PRESETS.map(|p| p.name).join(", "),
                r.preset
            )
        })?;
        let mut c = LoudnessConfig {
            preset: r.preset.clone(),
            user_preset: r.user_preset,
            enabled: r.enabled,
            target_lufs: r.target_lufs,
            true_peak_dbtp: r.true_peak_dbtp,
            window_s: r.window_s,
            max_slew_db_per_min: p.max_slew_db_per_min,
            deadband_lu: p.deadband_lu,
            gain_min_db: r.gain_min_db,
            gain_max_db: r.gain_max_db,
            min_valid_s: r.min_valid_s,
            gate_categories: r.gate_categories,
            trim_tau_s: r.trim_tau_s,
            trim_range_db: r.trim_range_db,
            within_item: p.within_item.to_string(),
            item_memory_s: p.item_memory_s,
            robust_items: p.robust_items,
            item_cap_s: p.item_cap_s,
            peak_allowance_db: p.peak_allowance_db,
            max_limiter_gr_db: p.max_limiter_gr_db,
        };
        if let Some(v) = r.max_slew_db_per_min {
            c.max_slew_db_per_min = v;
        }
        if let Some(v) = r.deadband_lu {
            c.deadband_lu = v;
        }
        if let Some(v) = r.within_item {
            c.within_item = v;
        }
        if let Some(v) = r.item_memory_s {
            c.item_memory_s = v;
        }
        if let Some(v) = r.robust_items {
            c.robust_items = v;
        }
        if let Some(v) = r.item_cap_s {
            c.item_cap_s = v;
        }
        if let Some(v) = r.peak_allowance_db {
            c.peak_allowance_db = v;
        }
        if let Some(v) = r.max_limiter_gr_db {
            c.max_limiter_gr_db = v;
        }
        c.mark_custom();
        Ok(c)
    }
}

/// The `[loudness]` block of a station (architecture 6.1).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(try_from = "RawLoudness")]
pub struct LoudnessConfig {
    /// `ambient`, `balanced`, `aggressive` or `custom` (a preset with settings of its own on top).
    pub preset: String,
    /// The name of the user preset (library `processing_presets`) the settings were last taken from, or empty.
    pub user_preset: String,
    /// `hold` (judge the level per item) or `follow` (track the loudness continuously).
    pub within_item: String,
    /// How long, seconds, the running level of an item remembers (hold only).
    pub item_memory_s: f64,
    /// `hold`: how many of the latest items the program level is taken over (their median), and the most
    /// seconds each counts for.
    pub robust_items: u32,
    pub item_cap_s: f64,
    /// How far, dB, normalization may push an item's peaks into the limiter while the controller is on.
    pub peak_allowance_db: f64,
    /// The gain is held down so that the loudest peak of the window needs at most this many dB of true-peak
    /// limiting (24 or more: no cap).
    pub max_limiter_gr_db: f64,
    /// On for the office stream, off for Eve (set by ear into the Wing).
    pub enabled: bool,
    /// Integrated loudness the station's output is steered to, LUFS.
    pub target_lufs: f64,
    /// True-peak ceiling of the lookahead limiter, dBTP.
    pub true_peak_dbtp: f64,
    /// The sliding measurement window, seconds.
    pub window_s: f64,
    /// The fastest the gain may move, dB per minute.
    pub max_slew_db_per_min: f64,
    /// No movement while the gain is within this of its target, LU.
    pub deadband_lu: f64,
    pub gain_min_db: f64,
    pub gain_max_db: f64,
    /// Seconds of gated program the window needs before the gain moves at all.
    pub min_valid_s: f64,
    /// Categories (or category kinds) left out of the measurement while they are audible.
    pub gate_categories: Vec<String>,
    /// Time constant and range of the slow output trim.
    pub trim_tau_s: f64,
    pub trim_range_db: f64,
}

impl Default for LoudnessConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target_lufs: -14.0,
            true_peak_dbtp: -1.0,
            window_s: 240.0,
            max_slew_db_per_min: 1.0,
            deadband_lu: 0.25,
            gain_min_db: -10.0,
            gain_max_db: 8.0,
            min_valid_s: 30.0,
            gate_categories: vec!["liner".into(), "id".into(), "promo".into()],
            trim_tau_s: 600.0,
            trim_range_db: 3.0,
            preset: "ambient".into(),
            user_preset: String::new(),
            within_item: "hold".into(),
            item_memory_s: 600.0,
            robust_items: 5,
            item_cap_s: 240.0,
            peak_allowance_db: 0.0,
            max_limiter_gr_db: 5.0,
        }
    }
}

impl LoudnessConfig {
    /// The settings a user preset of the loudness effect holds: every parameter but the on/off switch and the
    /// bookkeeping names.
    pub fn params_json(&self) -> serde_json::Value {
        serde_json::json!({
            "target_lufs": self.target_lufs, "true_peak_dbtp": self.true_peak_dbtp, "window_s": self.window_s,
            "max_slew_db_per_min": self.max_slew_db_per_min, "deadband_lu": self.deadband_lu,
            "gain_min_db": self.gain_min_db, "gain_max_db": self.gain_max_db, "min_valid_s": self.min_valid_s,
            "gate_categories": self.gate_categories, "trim_tau_s": self.trim_tau_s,
            "trim_range_db": self.trim_range_db, "within_item": self.within_item,
            "item_memory_s": self.item_memory_s, "robust_items": self.robust_items, "item_cap_s": self.item_cap_s, "peak_allowance_db": self.peak_allowance_db,
            "max_limiter_gr_db": self.max_limiter_gr_db,
        })
    }

    /// Set the named keys (the names of [`params_json`](Self::params_json), plus `enabled`) from a JSON
    /// object; an unknown key or a wrong type is an error naming it. Does not range-check (see [`check`](Self::check)).
    pub fn apply_params(
        &mut self,
        obj: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        for (k, v) in obj {
            let num = || v.as_f64();
            let ok = match k.as_str() {
                "enabled" => v.as_bool().map(|b| self.enabled = b).is_some(),
                "target_lufs" => num().map(|x| self.target_lufs = x).is_some(),
                "true_peak_dbtp" => num().map(|x| self.true_peak_dbtp = x).is_some(),
                "window_s" => num().map(|x| self.window_s = x).is_some(),
                "max_slew_db_per_min" => num().map(|x| self.max_slew_db_per_min = x).is_some(),
                "deadband_lu" => num().map(|x| self.deadband_lu = x).is_some(),
                "gain_min_db" => num().map(|x| self.gain_min_db = x).is_some(),
                "gain_max_db" => num().map(|x| self.gain_max_db = x).is_some(),
                "min_valid_s" => num().map(|x| self.min_valid_s = x).is_some(),
                "trim_tau_s" => num().map(|x| self.trim_tau_s = x).is_some(),
                "trim_range_db" => num().map(|x| self.trim_range_db = x).is_some(),
                "item_memory_s" => num().map(|x| self.item_memory_s = x).is_some(),
                "robust_items" => v.as_u64().map(|x| self.robust_items = x as u32).is_some(),
                "item_cap_s" => num().map(|x| self.item_cap_s = x).is_some(),
                "peak_allowance_db" => num().map(|x| self.peak_allowance_db = x).is_some(),
                "max_limiter_gr_db" => num().map(|x| self.max_limiter_gr_db = x).is_some(),
                "within_item" => v
                    .as_str()
                    .map(|x| self.within_item = x.to_string())
                    .is_some(),
                "user_preset" => v
                    .as_str()
                    .map(|x| self.user_preset = x.to_string())
                    .is_some(),
                "gate_categories" => v
                    .as_array()
                    .filter(|a| a.iter().all(serde_json::Value::is_string))
                    .map(|a| {
                        self.gate_categories = a
                            .iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .is_some(),
                _ => return Err(format!("unknown key `{k}`")),
            };
            if !ok {
                return Err(format!("`{k}` has the wrong type"));
            }
        }
        Ok(())
    }

    /// Put the preset's settings in place of the five it controls (a station picking a preset).
    pub fn apply_preset(&mut self, name: &str) -> bool {
        let Some(p) = loudness_preset(name) else {
            return false;
        };
        self.preset = name.to_string();
        self.within_item = p.within_item.to_string();
        self.item_memory_s = p.item_memory_s;
        self.robust_items = p.robust_items;
        self.item_cap_s = p.item_cap_s;
        self.peak_allowance_db = p.peak_allowance_db;
        self.max_limiter_gr_db = p.max_limiter_gr_db;
        self.max_slew_db_per_min = p.max_slew_db_per_min;
        self.deadband_lu = p.deadband_lu;
        true
    }

    /// A preset whose settings no longer match it is `custom`.
    pub fn mark_custom(&mut self) {
        if let Some(p) = loudness_preset(&self.preset) {
            if self.within_item != p.within_item
                || self.item_memory_s != p.item_memory_s
                || self.robust_items != p.robust_items
                || self.item_cap_s != p.item_cap_s
                || self.peak_allowance_db != p.peak_allowance_db
                || self.max_limiter_gr_db != p.max_limiter_gr_db
                || self.max_slew_db_per_min != p.max_slew_db_per_min
                || self.deadband_lu != p.deadband_lu
            {
                self.preset = "custom".into();
            }
        }
    }

    /// The ranges of 6.1; each failure names the key.
    pub fn check(&self) -> Vec<(&'static str, String)> {
        let mut e = vec![];
        let mut chk = |key: &'static str, ok: bool, range: &str, got: f64| {
            if !ok || !got.is_finite() {
                e.push((key, format!("must be {range}, got {got}")));
            }
        };
        chk(
            "target_lufs",
            (-24.0..=-10.0).contains(&self.target_lufs),
            "between -24 and -10",
            self.target_lufs,
        );
        chk(
            "true_peak_dbtp",
            (-3.0..=-0.5).contains(&self.true_peak_dbtp),
            "between -3 and -0.5",
            self.true_peak_dbtp,
        );
        chk(
            "window_s",
            (60.0..=900.0).contains(&self.window_s),
            "between 60 and 900",
            self.window_s,
        );
        chk(
            "max_slew_db_per_min",
            (0.25..=3.0).contains(&self.max_slew_db_per_min),
            "between 0.25 and 3",
            self.max_slew_db_per_min,
        );
        chk(
            "deadband_lu",
            (0.0..=1.0).contains(&self.deadband_lu),
            "between 0 and 1",
            self.deadband_lu,
        );
        chk(
            "gain_min_db",
            (-24.0..=0.0).contains(&self.gain_min_db),
            "between -24 and 0",
            self.gain_min_db,
        );
        chk(
            "gain_max_db",
            (0.0..=18.0).contains(&self.gain_max_db),
            "between 0 and 18",
            self.gain_max_db,
        );
        chk(
            "min_valid_s",
            self.min_valid_s >= 1.0 && self.min_valid_s <= self.window_s,
            "between 1 and window_s",
            self.min_valid_s,
        );
        chk(
            "trim_tau_s",
            (60.0..=3600.0).contains(&self.trim_tau_s),
            "between 60 and 3600",
            self.trim_tau_s,
        );
        chk(
            "trim_range_db",
            (0.0..=6.0).contains(&self.trim_range_db),
            "between 0 and 6",
            self.trim_range_db,
        );
        chk(
            "item_memory_s",
            (60.0..=3600.0).contains(&self.item_memory_s),
            "between 60 and 3600",
            self.item_memory_s,
        );
        chk(
            "robust_items",
            (1.0..=20.0).contains(&f64::from(self.robust_items)),
            "between 1 and 20",
            f64::from(self.robust_items),
        );
        chk(
            "item_cap_s",
            (30.0..=1800.0).contains(&self.item_cap_s),
            "between 30 and 1800",
            self.item_cap_s,
        );
        chk(
            "max_limiter_gr_db",
            (0.0..=24.0).contains(&self.max_limiter_gr_db),
            "between 0 and 24",
            self.max_limiter_gr_db,
        );
        chk(
            "peak_allowance_db",
            (0.0..=12.0).contains(&self.peak_allowance_db),
            "between 0 and 12",
            self.peak_allowance_db,
        );
        if self.within_item != "hold" && self.within_item != "follow" {
            e.push((
                "within_item",
                format!("must be hold or follow, got `{}`", self.within_item),
            ));
        }
        if self.preset != "custom" && loudness_preset(&self.preset).is_none() {
            e.push(("preset", format!("unknown preset `{}`", self.preset)));
        }
        e
    }

    /// The block as TOML text, `[loudness]` header included.
    pub fn to_toml(&self) -> String {
        let cats = self
            .gate_categories
            .iter()
            .map(|c| format!("\"{}\"", c.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "[loudness]\npreset = \"{}\"\nuser_preset = \"{}\"\nwithin_item = \"{}\"\nitem_memory_s = {}\nrobust_items = {}\nitem_cap_s = {}\npeak_allowance_db = {}\nmax_limiter_gr_db = {}\nenabled = {}\ntarget_lufs = {}\ntrue_peak_dbtp = {}\nwindow_s = {}\nmax_slew_db_per_min = {}\ndeadband_lu = {}\ngain_min_db = {}\ngain_max_db = {}\nmin_valid_s = {}\ngate_categories = [{cats}]\ntrim_tau_s = {}\ntrim_range_db = {}\n",
            self.preset, self.user_preset.replace('"', ""), self.within_item, self.item_memory_s, self.robust_items, self.item_cap_s, self.peak_allowance_db, self.max_limiter_gr_db, self.enabled, self.target_lufs, self.true_peak_dbtp, self.window_s, self.max_slew_db_per_min,
            self.deadband_lu, self.gain_min_db, self.gain_max_db, self.min_valid_s, self.trim_tau_s, self.trim_range_db
        )
    }
}

/// Replace (or append) the `[loudness]` block of a station config file, keeping everything else, comments
/// included. Written to a temporary file in the same directory and renamed over the original.
pub fn write_loudness_block(path: &Path, l: &LoudnessConfig) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let block = l.to_toml();
    let mut out = String::with_capacity(text.len() + block.len());
    let mut lines = text.split_inclusive('\n').peekable();
    let mut done = false;
    while let Some(line) = lines.next() {
        if line.trim_end() == "[loudness]" && !done {
            // Skip the old block: until the next table header at the start of a line.
            while let Some(next) = lines.peek() {
                if next.starts_with('[') {
                    break;
                }
                lines.next();
            }
            out.push_str(&block);
            if lines.peek().is_some() {
                out.push('\n');
            }
            done = true;
        } else {
            out.push_str(line);
        }
    }
    if !done {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(&block);
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, path)
}

/// The `[stream]` block of a station (architecture 1.3a).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StreamConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Served at `http://<box>:<api port>/stream/<mount>.ogg`.
    #[serde(default = "d_mount")]
    pub mount: String,
    /// `flac` (Ogg FLAC, lossless). `opus` is in the design and not built.
    #[serde(default = "d_codec")]
    pub codec: String,
    #[serde(default = "d_flac_level")]
    pub flac_level: u8,
    #[serde(default = "d_opus_kbps")]
    pub opus_kbps: u32,
    #[serde(default = "d_burst_ms")]
    pub burst_ms: u32,
    #[serde(default = "d_max_listeners")]
    pub max_listeners: u32,
    #[serde(default = "d_listener_lag")]
    pub listener_lag_s: u32,
    #[serde(default = "d_true")]
    pub expect_listener: bool,
    /// Seconds with nobody connected before `stream_no_listener` (a warning) is raised, when
    /// `expect_listener` is set.
    #[serde(default = "d_no_listener_alert")]
    pub no_listener_alert_s: u32,
    /// Only meaningful for AAC mounts, which do not exist yet: Ogg never carries ICY metadata.
    #[serde(default)]
    pub send_icy_metadata: bool,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mount: d_mount(),
            codec: d_codec(),
            flac_level: d_flac_level(),
            opus_kbps: d_opus_kbps(),
            burst_ms: d_burst_ms(),
            max_listeners: d_max_listeners(),
            listener_lag_s: d_listener_lag(),
            expect_listener: true,
            no_listener_alert_s: d_no_listener_alert(),
            send_icy_metadata: false,
        }
    }
}

fn d_no_listener_alert() -> u32 {
    60
}
fn d_mount() -> String {
    "program".into()
}
fn d_codec() -> String {
    "flac".into()
}
fn d_flac_level() -> u8 {
    5
}
fn d_opus_kbps() -> u32 {
    192
}
fn d_burst_ms() -> u32 {
    1000
}
fn d_max_listeners() -> u32 {
    4
}
fn d_listener_lag() -> u32 {
    5
}
fn d_true() -> bool {
    true
}

fn d_rate() -> u32 {
    48_000
}
fn d_period() -> u32 {
    1024
}
fn d_periods() -> u32 {
    8
}
fn d_formats() -> Vec<SampleFormat> {
    SampleFormat::DEFAULT_ORDER.to_vec()
}
fn d_target_lufs() -> f64 {
    -18.0
}
fn d_max_boost() -> f64 {
    12.0
}
fn d_peak_allowance() -> f64 {
    6.0
}
fn d_truncated_fade_s() -> f64 {
    2.0
}
fn d_preset() -> String {
    "plexamp".into()
}
fn d_ram_loop() -> u32 {
    300
}
fn d_lock_ahead() -> u32 {
    20
}
fn d_silence_dbfs() -> f64 {
    -50.0
}
fn d_silence_s() -> u32 {
    15
}
fn d_restart_alert_clear_s() -> u32 {
    600
}
fn d_processing() -> String {
    "bypass".into()
}

impl StationConfig {
    pub fn parse(text: &str, file: &Path) -> Result<Self, ConfigErrors> {
        let cfg: StationConfig = deserialize(text, file)?;
        let mut errs = Vec::new();
        let mut bad = |key: &str, message: String| {
            errs.push(ConfigError {
                file: file.to_path_buf(),
                key: key.into(),
                message,
            })
        };

        let name_ok = !cfg.name.is_empty()
            && cfg
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
            && !cfg.name.starts_with(['-', '_']);
        if !name_ok {
            bad(
                "name",
                format!(
                    "must be lowercase letters, digits, `-` and `_`, starting with a letter or digit, got `{}`",
                    cfg.name
                ),
            );
        } else if cfg.name == "test" {
            bad(
                "name",
                "`test` is reserved for `hotclock station test`".into(),
            );
        } else if let Some(stem) = file.file_stem().and_then(|s| s.to_str()) {
            if stem != cfg.name {
                bad(
                    "name",
                    format!("must equal the file name `{stem}`, got `{}`", cfg.name),
                );
            }
        }
        if cfg.listen.parse::<SocketAddr>().is_err() {
            bad(
                "listen",
                format!(
                    "must be an address like `0.0.0.0:8301`, got `{}`",
                    cfg.listen
                ),
            );
        }
        if cfg.device.trim().is_empty() {
            bad("device", "must not be empty".into());
        } else if cfg.device.trim() == "none" {
            bad(
                "device",
                "`none` is not a device; a station with no sound card says `device = \"null\"` and serves its program through `[stream]`".into(),
            );
        } else if let Some(p) = cfg.device.strip_prefix("file:") {
            if p.is_empty() {
                bad("device", "`file:` needs a path".into());
            }
        }
        if !(-100.0..=100.0).contains(&cfg.rate_trim_ppm) {
            bad(
                "rate_trim_ppm",
                format!("must be between -100 and 100, got {}", cfg.rate_trim_ppm),
            );
        }
        let st = &cfg.stream;
        if st.enabled {
            let mount_ok = !st.mount.is_empty()
                && st.mount.len() <= 64
                && st
                    .mount
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !mount_ok {
                bad(
                    "stream.mount",
                    format!(
                        "must be 1 to 64 letters, digits, `-` and `_`, got `{}`",
                        st.mount
                    ),
                );
            }
            match st.codec.as_str() {
                "flac" => {}
                "opus" => bad(
                    "stream.codec",
                    "`opus` is in the design but not built (it would need libopus); use `flac`"
                        .into(),
                ),
                other => bad("stream.codec", format!("must be `flac`, got `{other}`")),
            }
            if st.flac_level > 8 {
                bad(
                    "stream.flac_level",
                    format!("must be 0 to 8, got {}", st.flac_level),
                );
            }
            if st.burst_ms > 5000 {
                bad(
                    "stream.burst_ms",
                    format!("must be 0 to 5000, got {}", st.burst_ms),
                );
            }
            if !(1..=32).contains(&st.max_listeners) {
                bad(
                    "stream.max_listeners",
                    format!("must be 1 to 32, got {}", st.max_listeners),
                );
            }
            if !(1..=60).contains(&st.listener_lag_s) {
                bad(
                    "stream.listener_lag_s",
                    format!("must be 1 to 60, got {}", st.listener_lag_s),
                );
            }
            if !(10..=86_400).contains(&st.no_listener_alert_s) {
                bad(
                    "stream.no_listener_alert_s",
                    format!("must be 10 to 86400, got {}", st.no_listener_alert_s),
                );
            }
            if cfg.device.starts_with("file:") {
                bad(
                    "stream.enabled",
                    "needs a paced device: `file:` runs as fast as it can, so the stream would too (use `file-paced:`, `null` or a card)".into(),
                );
            }
        }
        if !matches!(cfg.rate, 44_100 | 48_000) {
            bad("rate", format!("must be 44100 or 48000, got {}", cfg.rate));
        }
        if !(64..=16_384).contains(&cfg.period_frames) || !cfg.period_frames.is_power_of_two() {
            bad(
                "period_frames",
                format!(
                    "must be a power of two from 64 to 16384, got {}",
                    cfg.period_frames
                ),
            );
        }
        if !(2..=64).contains(&cfg.periods) {
            bad("periods", format!("must be 2 to 64, got {}", cfg.periods));
        }
        if cfg.formats.is_empty() {
            bad("formats", "must list at least one format".into());
        }
        if !(OUTPUT_TRIM_MIN_DB..=OUTPUT_TRIM_MAX_DB).contains(&cfg.output_trim_db) {
            bad(
                "output_trim_db",
                format!(
                    "must be between {OUTPUT_TRIM_MIN_DB} and +{OUTPUT_TRIM_MAX_DB}, got {}",
                    cfg.output_trim_db
                ),
            );
        }
        if !(-40.0..=-5.0).contains(&cfg.target_lufs) {
            bad(
                "target_lufs",
                format!("must be between -40 and -5, got {}", cfg.target_lufs),
            );
        }
        if !(0.0..=30.0).contains(&cfg.max_boost_db) {
            bad(
                "max_boost_db",
                format!("must be between 0 and 30, got {}", cfg.max_boost_db),
            );
        }
        if !(0.0..=12.0).contains(&cfg.peak_allowance_db) {
            bad(
                "peak_allowance_db",
                format!("must be between 0 and 12, got {}", cfg.peak_allowance_db),
            );
        }
        if !(0.5..=10.0).contains(&cfg.truncated_fade_s) {
            bad(
                "truncated_fade_s",
                format!("must be between 0.5 and 10, got {}", cfg.truncated_fade_s),
            );
        }
        if !TRANSITION_PRESETS.contains(&cfg.transition_preset.as_str()) {
            bad(
                "transition_preset",
                format!(
                    "must be one of {}, got `{}`",
                    TRANSITION_PRESETS.join(", "),
                    cfg.transition_preset
                ),
            );
        }
        if cfg.ram_loop_seconds > 600 {
            bad(
                "ram_loop_seconds",
                format!("must be 0 to 600, got {}", cfg.ram_loop_seconds),
            );
        }
        if !(5..=120).contains(&cfg.lock_ahead_s) {
            bad(
                "lock_ahead_s",
                format!("must be 5 to 120, got {}", cfg.lock_ahead_s),
            );
        }
        if !(-100.0..=-20.0).contains(&cfg.silence_dbfs) {
            bad(
                "silence_dbfs",
                format!("must be between -100 and -20, got {}", cfg.silence_dbfs),
            );
        }
        if cfg.silence_s == 0 || cfg.silence_s > 600 {
            bad(
                "silence_s",
                format!("must be 1 to 600, got {}", cfg.silence_s),
            );
        }
        if cfg.restart_alert_clear_s > 86_400 {
            bad(
                "restart_alert_clear_s",
                format!("must be 0 to 86400, got {}", cfg.restart_alert_clear_s),
            );
        }
        if cfg.processing_preset.trim().is_empty() {
            bad("processing_preset", "must not be empty".into());
        }
        for (k, m) in cfg.loudness.check() {
            bad(&format!("loudness.{k}"), m);
        }
        if cfg.processing_preset.trim().is_empty() {
            // reported above
        } else if let Err(list) =
            crate::chain::ChainConfig::resolve(&cfg.processing_preset, &cfg.chain)
        {
            for (k, m) in list {
                bad(&k, m);
            }
        }
        if errs.is_empty() {
            Ok(cfg)
        } else {
            Err(ConfigErrors(errs))
        }
    }

    pub fn load(path: &Path) -> Result<Self, ConfigErrors> {
        let text = read(path)?;
        Self::parse(&text, path)
    }

    /// The station's processing chain: the shipped preset and the `[chain]` keys over it.
    pub fn chain_config(&self) -> crate::chain::ChainConfig {
        crate::chain::ChainConfig::resolve(&self.processing_preset, &self.chain).unwrap_or_default()
    }
}

fn read(path: &Path) -> Result<String, ConfigErrors> {
    std::fs::read_to_string(path).map_err(|e| {
        ConfigErrors(vec![ConfigError {
            file: path.to_path_buf(),
            key: String::new(),
            message: format!("cannot read file: {e}"),
        }])
    })
}

/// TOML into `T`, turning serde errors into `file + key + message`.
fn deserialize<T: for<'de> Deserialize<'de>>(text: &str, file: &Path) -> Result<T, ConfigErrors> {
    let de = toml::Deserializer::parse(text).map_err(|e| {
        ConfigErrors(vec![ConfigError {
            file: file.to_path_buf(),
            key: String::new(),
            message: format!("syntax error: {}", first_line(e.message())),
        }])
    })?;
    serde_path_to_error::deserialize(de).map_err(|e| {
        let path = e.path().to_string();
        let key = if path == "." { String::new() } else { path };
        ConfigErrors(vec![ConfigError {
            file: file.to_path_buf(),
            key,
            message: first_line(e.inner().message()),
        }])
    })
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFFICE: &str = r#"
name = "office"
display_name = "Office stream"
listen = "0.0.0.0:8301"
device = "hw:CARD=PCH,DEV=0"
rate = 48000
period_frames = 1024
periods = 8
formats = ["S32_LE", "S24_3LE", "S16_LE"]
target_lufs = -18.0
default_playlist = "Office originals"
transition_preset = "plexamp"
emergency_playlist = "Office emergency"
ram_loop_seconds = 300
lock_ahead_s = 20
silence_dbfs = -50.0
silence_s = 15
processing_preset = "bypass"
library = "/var/lib/hotclock/library.db"
"#;

    fn office_path() -> PathBuf {
        PathBuf::from("/etc/hotclock/stations/office.toml")
    }

    #[test]
    fn display_block_defaults_parses_and_names_the_key() {
        let d = GlobalConfig::parse("", Path::new("/etc/hotclock/hotclock.toml")).unwrap();
        assert_eq!(d.display.mode, "1024x600@60");
        assert_eq!(d.display.diagonal_in, 7.0);
        assert_eq!(d.display.view_distance_cm, 70.0);
        assert!(!d.display.rotate_processing && !d.display.proc_spectrum);
        assert_eq!(d.display.proc_range_db, 18.0);
        let c = GlobalConfig::parse(
            "[display]\nmode = \"1920x1080\"\ndiagonal_in = 15.6\nprocessing_view = \"curve\"\n",
            Path::new("h.toml"),
        )
        .unwrap();
        assert_eq!(parse_mode(&c.display.mode), Some((1920, 1080, 60)));
        assert_eq!(c.display.diagonal_in, 15.6);
        for (line, key) in [
            ("mode = \"wide\"", "display.mode"),
            ("diagonal_in = 0", "display.diagonal_in"),
            ("default_view = \"grid\"", "display.default_view"),
            ("curve_range_db = 40", "display.curve_range_db"),
            ("proc_range_db = 3", "display.proc_range_db"),
            ("mini_processing = \"some\"", "display.mini_processing"),
        ] {
            let e = GlobalConfig::parse(&format!("[display]\n{line}\n"), Path::new("h.toml"))
                .unwrap_err();
            assert!(e.to_string().contains(key), "{key}: {e}");
        }
        assert_eq!(parse_mode("800x480@50"), Some((800, 480, 50)));
        assert_eq!(parse_mode("1024x600@"), None);
    }

    #[test]
    fn architecture_example_parses() {
        let c = StationConfig::parse(OFFICE, &office_path()).unwrap();
        assert_eq!(c.name, "office");
        assert_eq!(c.period_frames, 1024);
        assert_eq!(c.formats, SampleFormat::DEFAULT_ORDER.to_vec());
    }

    const STREAM_ONLY: &str = r#"
name = "office"
listen = "0.0.0.0:8301"
device = "null"
rate_trim_ppm = -3.5
[stream]
enabled = true
mount = "program"
flac_level = 5
max_listeners = 4
"#;

    #[test]
    fn a_stream_only_station_parses_and_defaults_are_the_designs() {
        let c = StationConfig::parse(STREAM_ONLY, &office_path()).unwrap();
        assert!(c.stream.enabled);
        assert_eq!(c.stream.codec, "flac");
        assert_eq!(c.stream.burst_ms, 1000);
        assert_eq!(c.stream.listener_lag_s, 5);
        assert!(c.stream.expect_listener);
        assert_eq!(c.stream.no_listener_alert_s, 60);
        assert_eq!(c.rate_trim_ppm, -3.5);
        // No [stream] block: off.
        let d = StationConfig::parse(OFFICE, &office_path()).unwrap();
        assert!(!d.stream.enabled);
        assert_eq!(d.rate_trim_ppm, 0.0);
    }

    #[test]
    fn output_trim_defaults_to_zero_and_is_range_checked() {
        let d = StationConfig::parse(OFFICE, &office_path()).unwrap();
        assert_eq!(d.output_trim_db, 0.0);
        for (v, ok) in [
            ("-6.5", true),
            ("-40", true),
            ("6", true),
            ("6.5", false),
            ("-41", false),
        ] {
            let t = format!("{OFFICE}\noutput_trim_db = {v}\n");
            let r = StationConfig::parse(&t, &office_path());
            assert_eq!(r.is_ok(), ok, "{v}: {r:?}");
            if let Ok(c) = r {
                assert_eq!(c.output_trim_db, v.parse::<f64>().unwrap());
            } else {
                assert!(format!("{r:?}").contains("output_trim_db"));
            }
        }
    }

    #[test]
    fn stream_errors_name_the_key() {
        let cases = [
            ("codec = \"opus\"", "stream.codec", "not built"),
            ("flac_level = 9", "stream.flac_level", "0 to 8"),
            ("max_listeners = 0", "stream.max_listeners", "1 to 32"),
            ("mount = \"a/b\"", "stream.mount", "letters"),
            ("listener_lag_s = 0", "stream.listener_lag_s", "1 to 60"),
            (
                "no_listener_alert_s = 5",
                "stream.no_listener_alert_s",
                "10 to 86400",
            ),
        ];
        for (line, key, msg) in cases {
            let k = line.split(' ').next().unwrap();
            let t = if STREAM_ONLY.contains(k) {
                let from = STREAM_ONLY.find(k).unwrap();
                let to = from + STREAM_ONLY[from..].find('\n').unwrap();
                format!("{}{line}{}", &STREAM_ONLY[..from], &STREAM_ONLY[to..])
            } else {
                format!("{STREAM_ONLY}{line}\n")
            };
            let e = StationConfig::parse(&t, &office_path()).unwrap_err();
            assert_eq!(e.0[0].key, key, "{t}");
            assert!(e.0[0].message.contains(msg), "{:?}", e.0[0]);
        }
        let t = STREAM_ONLY.replace("\"null\"", "\"none\"");
        let e = StationConfig::parse(&t, &office_path()).unwrap_err();
        assert_eq!(e.0[0].key, "device");
        assert!(e.0[0].message.contains("null"), "{:?}", e.0[0]);
        let t = STREAM_ONLY.replace("\"null\"", "\"file:/tmp/x.wav\"");
        let e = StationConfig::parse(&t, &office_path()).unwrap_err();
        assert_eq!(e.0[0].key, "stream.enabled");
        let t = STREAM_ONLY.replace("-3.5", "250");
        let e = StationConfig::parse(&t, &office_path()).unwrap_err();
        assert_eq!(e.0[0].key, "rate_trim_ppm");
        // A disabled block is not validated.
        let t = STREAM_ONLY.replace("enabled = true", "enabled = false") + "codec = \"opus\"\n";
        assert!(StationConfig::parse(&t, &office_path()).is_ok());
    }

    #[test]
    fn minimal_station_gets_defaults() {
        let t = "name = \"eve\"\nlisten = \"0.0.0.0:8302\"\ndevice = \"hw:CARD=Device,DEV=0\"\n";
        let c = StationConfig::parse(t, Path::new("/x/eve.toml")).unwrap();
        assert_eq!(c.rate, 48_000);
        assert_eq!(c.transition_preset, "plexamp");
        assert_eq!(c.silence_s, 15);
        assert_eq!(c.restart_alert_clear_s, 600);
    }

    #[test]
    fn errors_name_file_and_key() {
        let t = OFFICE.replace("rate = 48000", "rate = 12345");
        let e = StationConfig::parse(&t, &office_path())
            .unwrap_err()
            .to_string();
        assert!(e.contains("/etc/hotclock/stations/office.toml"), "{e}");
        assert!(e.contains("key `rate`"), "{e}");
        assert!(e.contains("12345"), "{e}");
    }

    #[test]
    fn all_validation_errors_are_reported() {
        let t = OFFICE
            .replace("periods = 8", "periods = 1")
            .replace("\"plexamp\"", "\"nonsense\"");
        let e = StationConfig::parse(&t, &office_path()).unwrap_err();
        let keys: Vec<_> = e.0.iter().map(|x| x.key.as_str()).collect();
        assert_eq!(keys, ["periods", "transition_preset"]);
    }

    #[test]
    fn type_error_names_key() {
        let t = OFFICE.replace("rate = 48000", "rate = \"fast\"");
        let e = StationConfig::parse(&t, &office_path()).unwrap_err();
        assert_eq!(e.0[0].key, "rate");
        assert_eq!(e.0[0].file, office_path());
    }

    #[test]
    fn unknown_key_is_an_error_naming_it() {
        let t = format!("{OFFICE}\nperiod_farmes = 3\n");
        let e = StationConfig::parse(&t, &office_path())
            .unwrap_err()
            .to_string();
        assert!(e.contains("period_farmes"), "{e}");
    }

    #[test]
    fn bad_format_name_names_key() {
        let t = OFFICE.replace("\"S24_3LE\"", "\"S24_LE\"");
        let e = StationConfig::parse(&t, &office_path()).unwrap_err();
        assert!(e.0[0].key.starts_with("formats"), "{:?}", e.0[0]);
    }

    #[test]
    fn syntax_error_names_file() {
        let e = StationConfig::parse("name = ", &office_path())
            .unwrap_err()
            .to_string();
        assert!(e.contains("office.toml"), "{e}");
        assert!(e.contains("syntax"), "{e}");
    }

    #[test]
    fn name_must_match_file_and_not_be_test() {
        let e =
            StationConfig::parse(OFFICE, Path::new("/etc/hotclock/stations/eve.toml")).unwrap_err();
        assert_eq!(e.0[0].key, "name");
        let t = OFFICE.replace("\"office\"", "\"test\"");
        let e = StationConfig::parse(&t, Path::new("/x/test.toml")).unwrap_err();
        assert!(e.0[0].message.contains("reserved"));
    }

    #[test]
    fn missing_required_key() {
        let e = StationConfig::parse("name = \"office\"\n", &office_path()).unwrap_err();
        assert!(e.0[0].message.contains("missing field"), "{:?}", e.0[0]);
        assert!(e.0[0].message.contains("listen") || e.0[0].message.contains("device"));
    }

    #[test]
    fn global_defaults_and_errors() {
        let g = GlobalConfig::parse("", Path::new("/etc/hotclock/hotclock.toml")).unwrap();
        assert_eq!(g.library.listen, "0.0.0.0:8300");
        assert_eq!(
            g.library_db_path(),
            PathBuf::from("/var/lib/hotclock/library.db")
        );
        let e = GlobalConfig::parse(
            "data_dir = \"relative\"\n[brand]\ndisplay_name = \" \"\n",
            Path::new("/etc/hotclock/hotclock.toml"),
        )
        .unwrap_err();
        let keys: Vec<_> = e.0.iter().map(|x| x.key.as_str()).collect();
        assert_eq!(keys, ["data_dir", "brand.display_name"]);
    }

    #[test]
    fn unreadable_file_names_file() {
        let e = GlobalConfig::load(Path::new("/nonexistent/hotclock.toml")).unwrap_err();
        assert!(e.to_string().contains("/nonexistent/hotclock.toml"));
    }
}

#[cfg(test)]
mod loudness_tests {
    use super::*;

    const BASE: &str = "name = \"office\"\nlisten = \"0.0.0.0:8301\"\ndevice = \"null\"\n";

    #[test]
    fn the_loudness_block_parses_with_defaults_and_rejects_out_of_range_values() {
        let p = PathBuf::from("/etc/hotclock/stations/office.toml");
        let c = StationConfig::parse(BASE, &p).unwrap();
        assert!(!c.loudness.enabled);
        assert_eq!(c.loudness.target_lufs, -14.0);
        assert_eq!(c.loudness.gate_categories, ["liner", "id", "promo"]);
        let c = StationConfig::parse(
            &format!("{BASE}[loudness]\nenabled = true\ntarget_lufs = -16.5\n"),
            &p,
        )
        .unwrap();
        assert!(c.loudness.enabled && c.loudness.target_lufs == -16.5);
        let e = StationConfig::parse(
            &format!("{BASE}[loudness]\ntarget_lufs = -5\ndeadband_lu = 2\n"),
            &p,
        )
        .unwrap_err();
        let keys: Vec<_> = e.0.iter().map(|x| x.key.as_str()).collect();
        assert!(
            keys.contains(&"loudness.target_lufs") && keys.contains(&"loudness.deadband_lu"),
            "{keys:?}"
        );
        let e = StationConfig::parse(&format!("{BASE}[loudness]\nbogus = 1\n"), &p).unwrap_err();
        assert!(
            e.0[0].key.contains("bogus") || e.0[0].message.contains("bogus"),
            "{:?}",
            e.0[0]
        );
    }

    #[test]
    fn writing_the_block_keeps_the_rest_of_the_file_and_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("hc-loud-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("office.toml");
        let src = format!("# my comment\n{BASE}[stream]\nenabled = false\n\n[loudness]\nenabled = false\ntarget_lufs = -14\n\n[other_later]\n");
        std::fs::write(&path, &src).unwrap();
        let l = LoudnessConfig {
            enabled: true,
            target_lufs: -16.0,
            ..LoudnessConfig::default()
        };
        write_loudness_block(&path, &l).unwrap();
        let t = std::fs::read_to_string(&path).unwrap();
        assert!(t.starts_with("# my comment\n"));
        assert!(t.contains("[stream]\nenabled = false\n"));
        assert!(t.contains("[other_later]"));
        assert_eq!(t.matches("[loudness]").count(), 1);
        let again = StationConfig::parse(&t.replace("[other_later]\n", ""), &path).unwrap();
        assert!(again.loudness.enabled && again.loudness.target_lufs == -16.0);
        write_loudness_block(&path, &l).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), t);
        // No block yet: appended.
        std::fs::write(&path, BASE).unwrap();
        write_loudness_block(&path, &l).unwrap();
        let t = std::fs::read_to_string(&path).unwrap();
        assert!(StationConfig::parse(&t, &path).unwrap().loudness.enabled);
        std::fs::remove_dir_all(&dir).ok();
    }
}
