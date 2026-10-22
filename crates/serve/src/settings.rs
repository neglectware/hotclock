//! The Settings page's back end (architecture 5.1, 9): every key of the global and the station configuration
//! files, with its type, range, default and the way a change takes effect; reading the files, validating a change
//! with the same parser the services use, and writing the file back with its comments (`toml_edit`), atomically,
//! with a `.bak` of what was there. Live changes are passed on to the running station; the rest wait for a restart,
//! which the page can ask for ([`restart_unit`]).

use common::config::{GlobalConfig, StationConfig};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Table};

/// How a changed key reaches the running service.
pub const LIVE: &str = "live";
pub const NEXT_BOUNDARY: &str = "next_boundary";
pub const STATION_RESTART: &str = "station_restart";
pub const LIBRARY_RESTART: &str = "library_restart";
pub const DISPLAY_RESTART: &str = "display_restart";

/// One key of a configuration file.
#[derive(Clone, Copy)]
pub struct Def {
    pub scope: &'static str,
    pub key: &'static str,
    pub kind: &'static str,
    pub min: f64,
    pub max: f64,
    pub options: &'static [&'static str],
    /// The default as JSON text.
    pub default: &'static str,
    pub apply: &'static str,
    pub help: &'static str,
}

const S: &str = "station";
const G: &str = "global";

macro_rules! def {
    ($scope:expr, $key:expr, $kind:expr, $min:expr, $max:expr, $opts:expr, $default:expr, $apply:expr, $help:expr) => {
        Def {
            scope: $scope,
            key: $key,
            kind: $kind,
            min: $min,
            max: $max,
            options: $opts,
            default: $default,
            apply: $apply,
            help: $help,
        }
    };
}

/// The keys written out by hand; [`catalog`] adds the processing chain's, which come from its schema.
const BASE: &[Def] = &[
    // ---- the station file, /etc/hotclock/stations/<name>.toml ----
    def!(S, "name", "readonly", 0.0, 0.0, &[], "null", STATION_RESTART,
        "The station's name; it is also the file name, so it is changed by renaming the file."),
    def!(S, "display_name", "text", 0.0, 0.0, &[], "null", STATION_RESTART,
        "The name shown on the display and in the page. The station's own name is used when empty."),
    def!(S, "listen", "address", 0.0, 0.0, &[], "null", STATION_RESTART,
        "The address this station's API listens on, like 0.0.0.0:8301."),
    def!(S, "device", "device", 0.0, 0.0, &[], "null", STATION_RESTART,
        "The sound card. `null` plays to nowhere in real time (a station that only streams); `file:<path.wav>` writes a WAV as fast as possible."),
    def!(S, "rate", "int", 0.0, 0.0, &["44100", "48000"], "48000", STATION_RESTART,
        "The sample rate of the output."),
    def!(S, "period_frames", "int", 64.0, 16384.0,
        &["64", "128", "256", "512", "1024", "2048", "4096", "8192", "16384"], "1024", STATION_RESTART,
        "Frames in one card period (a power of two). Smaller means less delay and less margin."),
    def!(S, "periods", "int", 2.0, 64.0, &[], "8", STATION_RESTART,
        "Periods in the card's buffer; the buffer is period_frames times periods."),
    def!(S, "formats", "formats", 0.0, 0.0, &["S32_LE", "S24_3LE", "S16_LE"],
        r#"["S32_LE","S24_3LE","S16_LE"]"#, STATION_RESTART,
        "The sample formats to try on the card, in order of preference."),
    def!(S, "target_lufs", "float", -40.0, -5.0, &[], "-18.0", NEXT_BOUNDARY,
        "The loudness every item is brought to, in LUFS. The item on air keeps its gain; the next one planned uses the new target."),
    def!(S, "max_boost_db", "float", 0.0, 30.0, &[], "12.0", NEXT_BOUNDARY,
        "The most gain normalization may add to a quiet item, in dB."),
    def!(S, "peak_allowance_db", "float", 0.0, 12.0, &[], "6.0", NEXT_BOUNDARY,
        "How far into the safety limiter normalization may push a peak, in dB."),
    def!(S, "truncated_fade", "bool", 0.0, 0.0, &[], "true", NEXT_BOUNDARY,
        "Fade the end of an item flagged truncated_fade (its file stops in the middle of its own fade-out) to silence at its cue-out. An item with a fade-out set by hand keeps its own fade."),
    def!(S, "truncated_fade_s", "float", 0.5, 10.0, &[], "2.0", NEXT_BOUNDARY,
        "The length of that automatic fade, in seconds."),
    def!(S, "output_trim_db", "float", -40.0, 6.0, &[], "0.0", LIVE,
        "A gain after the safety limiter, on the sound card and the stream alike, in dB; 0 is unity. Also on the System page. A value set there or here is kept by the station and wins over the file at the next start."),
    def!(S, "default_playlist", "playlist", 0.0, 0.0, &[], "null", STATION_RESTART,
        "The playlist the station plays when nothing else is."),
    def!(S, "transition_preset", "preset", 0.0, 0.0, &[], r#""plexamp""#, NEXT_BOUNDARY,
        "The crossfade style between songs, for everything that does not set its own: basic, plexamp, sam, fm or none."),
    def!(S, "emergency_playlist", "playlist", 0.0, 0.0, &[], "null", STATION_RESTART,
        "The playlist played in safe mode, after long silence and when the queue runs dry."),
    def!(S, "stopped_is_normal", "bool", 0.0, 0.0, &[], "false", STATION_RESTART,
        "For a station that only runs for a show and is stopped the rest of the time: an empty queue is not an emergency. The station stays silent; there is no queue_starved alert and no emergency playlist. Leave it off for a station that runs all day."),
    def!(S, "ram_loop_seconds", "int", 0.0, 600.0, &[], "300", STATION_RESTART,
        "Seconds of the emergency playlist kept in memory, so it plays even when the disk does not."),
    def!(S, "lock_ahead_s", "int", 5.0, 120.0, &[], "20", LIVE,
        "A queue entry this close to its start is committed and is never changed by the scheduler, in seconds."),
    def!(S, "silence_dbfs", "float", -100.0, -20.0, &[], "-50.0", LIVE,
        "The level below which the program counts as silent, in dBFS RMS."),
    def!(S, "silence_s", "int", 1.0, 600.0, &[], "15", LIVE,
        "Seconds of silence before the station acts; each escalation step is one window apart."),
    def!(S, "restart_alert_clear_s", "int", 0.0, 86400.0, &[], "600", LIVE,
        "Seconds the station must be on air and healthy before the `restarted` alert clears itself. 0 keeps the alert until someone acknowledges it."),
    def!(S, "processing_preset", "choice", 0.0, 0.0, &["bypass", "gentle", "radio"], r#""bypass""#, STATION_RESTART,
        "The processing chain's shipped preset: bypass (nothing but the safety limiter), gentle or radio. The `chain.*` keys below are the settings that differ from it. The Processing panel on the System page edits all of this live and keeps your own presets."),
    def!(S, "library", "path", 0.0, 0.0, &[], "null", STATION_RESTART,
        "The library database this station reads; the box's own library when empty."),
    def!(S, "rate_trim_ppm", "float", -100.0, 100.0, &[], "0.0", STATION_RESTART,
        "Trim of the clock that paces a card-less station, in parts per million; positive runs faster."),
    def!(S, "stream.enabled", "bool", 0.0, 0.0, &[], "false", STATION_RESTART,
        "Serve the program as a network stream (lossless FLAC in Ogg) beside the card."),
    def!(S, "stream.mount", "text", 0.0, 0.0, &[], r#""program""#, STATION_RESTART,
        "The stream's name: http://<box>:<port>/stream/<mount>.ogg. Letters, digits, - and _."),
    def!(S, "stream.codec", "choice", 0.0, 0.0, &["flac"], r#""flac""#, STATION_RESTART,
        "The stream's codec. Only flac is built."),
    def!(S, "stream.flac_level", "int", 0.0, 8.0, &[], "5", STATION_RESTART,
        "FLAC compression level, 0 to 8."),
    def!(S, "stream.opus_kbps", "int", 32.0, 512.0, &[], "192", STATION_RESTART,
        "Opus bit rate. Not used: Opus is in the design and not built."),
    def!(S, "stream.burst_ms", "int", 0.0, 5000.0, &[], "1000", STATION_RESTART,
        "Audio sent to a new listener at once so playback starts without waiting, in milliseconds."),
    def!(S, "stream.max_listeners", "int", 1.0, 32.0, &[], "4", STATION_RESTART,
        "The most listeners connected at once."),
    def!(S, "stream.listener_lag_s", "int", 1.0, 60.0, &[], "5", STATION_RESTART,
        "A listener that falls this many seconds behind is disconnected."),
    def!(S, "stream.expect_listener", "bool", 0.0, 0.0, &[], "true", STATION_RESTART,
        "Raise a warning when nobody is connected (the office stream always has one)."),
    def!(S, "stream.no_listener_alert_s", "int", 10.0, 86400.0, &[], "60", STATION_RESTART,
        "Seconds with nobody connected before that warning."),
    def!(S, "stream.send_icy_metadata", "bool", 0.0, 0.0, &[], "false", STATION_RESTART,
        "Only meaningful for AAC mounts, which do not exist yet."),
    def!(S, "loudness.preset", "choice", 0.0, 0.0, &["ambient", "balanced", "aggressive", "custom"], r#""ambient""#, STATION_RESTART,
        "The loudness controller's preset: how it treats the program. Individual keys below override it. The Loudness card on the System page edits all of this live and keeps your own presets."),
    def!(S, "loudness.enabled", "bool", 0.0, 0.0, &[], r#"false"#, STATION_RESTART,
        "Run the loudness controller: a slow gain that lands the stream at the target, under a true-peak ceiling. On for the office, off for Eve."),
    def!(S, "loudness.target_lufs", "float", -24.0, -10.0, &[], r#"-14.0"#, STATION_RESTART,
        "The loudness the output is steered to, LUFS; YouTube and Twitch want about -14."),
    def!(S, "loudness.true_peak_dbtp", "float", -3.0, -0.5, &[], r#"-1.0"#, STATION_RESTART,
        "The true-peak ceiling of the limiter the controller forces on, dBTP."),
    def!(S, "loudness.window_s", "float", 60.0, 900.0, &[], r#"240.0"#, STATION_RESTART,
        "Seconds of recent program the level is judged over (follow mode)."),
    def!(S, "loudness.max_slew_db_per_min", "float", 0.25, 3.0, &[], r#"1.0"#, STATION_RESTART,
        "The fastest the gain may change, dB per minute."),
    def!(S, "loudness.deadband_lu", "float", 0.0, 1.0, &[], r#"0.25"#, STATION_RESTART,
        "No gain change while it is this close to where it should be, LU."),
    def!(S, "loudness.gain_min_db", "float", -24.0, 0.0, &[], r#"-10.0"#, STATION_RESTART,
        "The most the controller turns the program down, dB."),
    def!(S, "loudness.gain_max_db", "float", 0.0, 18.0, &[], r#"8.0"#, STATION_RESTART,
        "The most the controller turns the program up, dB."),
    def!(S, "loudness.min_valid_s", "float", 1.0, 900.0, &[], r#"30.0"#, STATION_RESTART,
        "Seconds of measurable program needed before the gain moves at all."),
    def!(S, "loudness.gate_categories", "list", 0.0, 0.0, &[], r#"["liner", "id", "promo"]"#, STATION_RESTART,
        "Categories (or kinds) left out of the loudness measurement while they play."),
    def!(S, "loudness.trim_tau_s", "float", 60.0, 3600.0, &[], r#"600.0"#, STATION_RESTART,
        "Time constant of the slow final correction, seconds."),
    def!(S, "loudness.trim_range_db", "float", 0.0, 6.0, &[], r#"3.0"#, STATION_RESTART,
        "The most that correction may add or take off, dB."),
    def!(S, "loudness.within_item", "choice", 0.0, 0.0, &["hold", "follow"], r#""hold""#, STATION_RESTART,
        "hold: judge each item as a whole (a quiet passage or a quiet track barely moves the volume); follow: track the loudness continuously."),
    def!(S, "loudness.item_memory_s", "float", 60.0, 3600.0, &[], r#"600.0"#, STATION_RESTART,
        "Hold mode: how long an item remembers its own loudness, seconds."),
    def!(S, "loudness.robust_items", "int", 1.0, 20.0, &[], r#"5"#, STATION_RESTART,
        "Hold mode: the program level is the median of this many latest items."),
    def!(S, "loudness.item_cap_s", "float", 30.0, 1800.0, &[], r#"240.0"#, STATION_RESTART,
        "Hold mode: the most seconds a single item counts for in that median."),
    def!(S, "loudness.peak_allowance_db", "float", 0.0, 12.0, &[], r#"0.0"#, STATION_RESTART,
        "How far normalization may push an item's peaks into the limiter while the controller is on, dB."),
    def!(S, "loudness.max_limiter_gr_db", "float", 0.0, 24.0, &[], r#"5.0"#, STATION_RESTART,
        "The gain is held down so the loudest peak needs at most this much limiting, dB; 24 turns the cap off."),
    // ---- the global file, /etc/hotclock/hotclock.toml ----
    def!(G, "data_dir", "path", 0.0, 0.0, &[], r#""/var/lib/hotclock""#, LIBRARY_RESTART,
        "Where library.db, the media, the inbox, the stations' state and the snapshots live. Restart the library and every station."),
    def!(G, "stations_dir", "path", 0.0, 0.0, &[], r#""/etc/hotclock/stations""#, LIBRARY_RESTART,
        "Where the station files are. This page edits the files in the directory the library service was started with."),
    def!(G, "run_dir", "path", 0.0, 0.0, &[], r#""/run/hotclock""#, LIBRARY_RESTART,
        "Start counters and sockets."),
    def!(G, "time_zone", "text", 0.0, 0.0, &[], r#""America/New_York""#, LIBRARY_RESTART,
        "The time zone of the schedule and of the 02:30 snapshots (an IANA name)."),
    def!(G, "library.listen", "address", 0.0, 0.0, &[], r#""0.0.0.0:8300""#, LIBRARY_RESTART,
        "The address of this page and the library API. Changing it moves the page: open the new address after the restart."),
    def!(G, "brand.display_name", "text", 0.0, 0.0, &[], "null", LIBRARY_RESTART,
        "The product name shown in the page, instead of the built-in one."),
    def!(G, "allowed_hosts", "list", 0.0, 0.0, &[], "[]", LIBRARY_RESTART,
        "Extra names this box answers to in the Host header (a CNAME, a tailnet alias)."),
    def!(G, "secrets_file", "path", 0.0, 0.0, &[], r#""/etc/hotclock/secrets.toml""#, LIBRARY_RESTART,
        "Where the SMTP password lives (mode 0600)."),
    // ---- the status display, the [display] block of the global file ----
    def!(G, "display.mode", "text", 0.0, 0.0, &[], r#""1024x600@60""#, DISPLAY_RESTART,
        "The screen's mode, WIDTHxHEIGHT or WIDTHxHEIGHT@HZ. Set through KMS; the panel's EDID is never trusted for the resolution, and a mode the connector does not list is added."),
    def!(G, "display.diagonal_in", "float", 1.0, 100.0, &[], "7.0", DISPLAY_RESTART,
        "The screen's diagonal in inches: the pixel density, and so the smallest text drawn, follow from it."),
    def!(G, "display.view_distance_cm", "float", 20.0, 1000.0, &[], "70.0", DISPLAY_RESTART,
        "The viewing distance the legibility rule is computed for. Raise it until the text reads from the seat."),
    def!(G, "display.default_view", "choice", 0.0, 0.0, &["single", "split", "rotate", "processing"], r#""single""#, DISPLAY_RESTART,
        "The view used until the library service has said one: single, split, rotate, or processing (one station's gain-reduction meters). The view itself is switched from On Air, the Stream Deck or the display's own selector."),
    def!(G, "display.bar_product", "bool", 0.0, 0.0, &[], "true", LIVE,
        "The split view's top bar on the web display: the product name and version at the left."),
    def!(G, "display.bar_clock", "bool", 0.0, 0.0, &[], "true", LIVE,
        "The split view's top bar on the web display: the clock in the middle."),
    def!(G, "display.bar_date", "bool", 0.0, 0.0, &[], "true", LIVE,
        "The split view's top bar on the web display: the date at the right, like SAT 10 OCT."),
    def!(G, "display.bar_cpu", "bool", 0.0, 0.0, &[], "true", LIVE,
        "The split view's top bar on the web display: how busy the whole box's processor is, like CPU 7%. It is the average over 5 seconds. CPU -- means the box cannot tell."),
    def!(G, "display.bar_network", "bool", 0.0, 0.0, &[], "false", LIVE,
        "The split view's top bar on the web display: a dot at the left. Green: the box has a network route out. Red: it has none. Grey: the box cannot tell."),
    def!(G, "display.bypassed_stages", "choice", 0.0, 0.0, &["show", "hide"], r#""show""#, DISPLAY_RESTART,
        "Full layout: show a bypassed processing stage grey with BYP, or hide it."),
    def!(G, "display.processing_view", "choice", 0.0, 0.0, &["spectrum", "curve", "both"], r#""both""#, DISPLAY_RESTART,
        "Full layout: the spectrum, the multiband gain curve, or both."),
    def!(G, "display.spectrum_source", "choice", 0.0, 0.0, &["pre", "post", "both"], r#""post""#, DISPLAY_RESTART,
        "Full layout: the spectrum before the processing chain, after it, or both."),
    def!(G, "display.curve_range_db", "float", 3.0, 18.0, &[], "12.0", DISPLAY_RESTART,
        "How far down the gain curve hangs, in dB."),
    def!(G, "display.curve_include_makeup", "bool", 0.0, 0.0, &[], "false", DISPLAY_RESTART,
        "Add the bands' makeup gain to the gain curve."),
    def!(G, "display.connector", "text", 0.0, 0.0, &[], "null", DISPLAY_RESTART,
        "The video connector to use, like HDMI-A-1. The first connected one when empty."),
    def!(G, "display.device", "text", 0.0, 0.0, &[], "null", DISPLAY_RESTART,
        "The DRM device, like /dev/dri/card1. The first one with a connected connector when empty."),
    def!(G, "display.rotate_pin_on_alert", "bool", 0.0, 0.0, &[], "true", DISPLAY_RESTART,
        "In the rotate view, a critical alert on a station pins the view to it until it clears."),
    def!(G, "display.rotate_processing", "bool", 0.0, 0.0, &[], "false", DISPLAY_RESTART,
        "In the rotate view, show each station's processing view as well as its normal view (office, office processing, eve, eve processing). Off: the rotation alternates between the stations only."),
    def!(G, "display.proc_range_db", "float", 6.0, 30.0, &[], "18.0", DISPLAY_RESTART,
        "The processing view: how far down the gain-reduction scale goes, in dB. One scale for every bar, with a line every 6 dB."),
    def!(G, "display.mini_processing", "choice", 0.0, 0.0, &["off", "auto", "on"], r#""auto""#, DISPLAY_RESTART,
        "A small strip of the processing meters (gain-reduction bars, AGC) in the single and split views of a compact screen, with small labels for reading up close. auto shows it while the processing chain is on, on always, off never."),
    def!(G, "display.proc_spectrum", "bool", 0.0, 0.0, &[], "false", DISPLAY_RESTART,
        "The processing view: draw the spectrum under the bars where the screen has room. The 7-inch 1024 x 600 panel has none, so it is never drawn there."),
];

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// The processing chain's keys, `chain.<block>.<key>`, from the schema in `common::chain`.
fn chain_defs() -> Vec<Def> {
    use common::chain::{Def as D, Kind, BLOCKS};
    let mut v = vec![];
    let tail = " When the key is not in the file, the preset named by `processing_preset` gives the value.";
    for b in BLOCKS {
        if b.switch {
            v.push(Def {
                scope: S,
                key: leak(format!("chain.{}.enabled", b.key)),
                kind: "bool",
                min: 0.0,
                max: 0.0,
                options: &[],
                default: "false",
                apply: STATION_RESTART,
                help: leak(format!("{}: on or off. {}{tail}", b.label, b.help)),
            });
        }
        for p in b.params {
            let (kind, min, max, options): (&'static str, f64, f64, &'static [&'static str]) =
                match p.kind {
                    Kind::Bool => ("bool", 0.0, 0.0, &[]),
                    Kind::Float { min, max } => ("float", min, max, &[]),
                    Kind::Int { min, max } => ("int", min as f64, max as f64, &[]),
                    Kind::Choice(o) => ("choice", 0.0, 0.0, o),
                    Kind::List { min, max, .. } => ("numlist", min, max, &[]),
                };
            let default = match p.default {
                D::B(x) => serde_json::to_string(&x),
                D::F(x) => serde_json::to_string(&x),
                D::I(x) => serde_json::to_string(&x),
                D::S(x) => serde_json::to_string(&x),
                D::L(x) => serde_json::to_string(x),
            }
            .unwrap_or_else(|_| "null".into());
            let unit = if p.unit.is_empty() {
                String::new()
            } else {
                format!(" ({})", p.unit)
            };
            v.push(Def {
                scope: S,
                key: leak(format!("chain.{}.{}", b.key, p.key)),
                kind,
                min,
                max,
                options,
                default: leak(default),
                apply: STATION_RESTART,
                help: leak(format!("{}{unit}: {}{tail}", b.label, p.help)),
            });
        }
    }
    v
}

/// Every key of both files. The defaults are checked against the parser by a test.
pub fn catalog() -> &'static [Def] {
    static ALL: std::sync::OnceLock<Vec<Def>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        let mut v = BASE.to_vec();
        v.extend(chain_defs());
        v
    })
}

/// Keys of other branches that are not in the parser here yet; the page shows them as a note, and the merge
/// adds them to [`catalog`] (the form is built from the catalog, so a key added there appears by itself).
pub fn pending_keys() -> Value {
    json!({
        "display": [],
        "station": [],
    })
}

fn defs(scope: &str) -> impl Iterator<Item = &'static Def> + '_ {
    catalog().iter().filter(move |d| d.scope == scope)
}

/// The catalog as JSON, for the page.
pub fn catalog_json(scope: &str) -> Value {
    Value::Array(
        defs(scope)
            .map(|d| {
                json!({
                    "key": d.key, "kind": d.kind, "min": d.min, "max": d.max, "options": d.options,
                    "default": serde_json::from_str::<Value>(d.default).unwrap_or(Value::Null),
                    "apply": d.apply, "help": d.help,
                })
            })
            .collect(),
    )
}

// ---- reading ------------------------------------------------------------------------------------------------

fn item_json(i: &Item) -> Value {
    use toml_edit::Value as V;
    match i.as_value() {
        Some(V::String(s)) => json!(s.value()),
        Some(V::Integer(n)) => json!(n.value()),
        Some(V::Float(f)) => json!(f.value()),
        Some(V::Boolean(b)) => json!(b.value()),
        Some(V::Array(a)) => Value::Array(
            a.iter()
                .map(|v| match v {
                    V::String(s) => json!(s.value()),
                    V::Integer(n) => json!(n.value()),
                    V::Float(f) => json!(f.value()),
                    V::Boolean(b) => json!(b.value()),
                    _ => Value::Null,
                })
                .collect(),
        ),
        _ => Value::Null,
    }
}

fn get_item<'a>(doc: &'a DocumentMut, key: &str) -> Option<&'a Item> {
    let mut parts = key.split('.');
    let mut cur: &Item = doc.get(parts.next()?)?;
    for p in parts {
        cur = cur.as_table_like()?.get(p)?;
    }
    Some(cur)
}

/// The table at `path` (every segment but the key's last), created when `create`.
fn table_at<'a>(
    doc: &'a mut DocumentMut,
    path: &[&str],
    create: bool,
) -> Option<&'a mut dyn toml_edit::TableLike> {
    let mut cur: &mut dyn toml_edit::TableLike = doc.as_table_mut();
    for p in path {
        if cur.get(p).is_none() {
            if !create {
                return None;
            }
            let mut t = Table::new();
            t.set_implicit(true);
            cur.insert(p, Item::Table(t));
        }
        cur = cur.get_mut(p)?.as_table_like_mut()?;
    }
    Some(cur)
}

/// The keys the file sets, by their dotted names.
pub fn explicit_values(doc: &DocumentMut, scope: &str) -> Map<String, Value> {
    let mut m = Map::new();
    for d in defs(scope) {
        if let Some(i) = get_item(doc, d.key) {
            m.insert(d.key.to_string(), item_json(i));
        }
    }
    m
}

/// What each key is in effect: the file's value, else the default.
pub fn effective_values(explicit: &Map<String, Value>, scope: &str) -> Map<String, Value> {
    let mut m = Map::new();
    for d in defs(scope) {
        let v = explicit
            .get(d.key)
            .cloned()
            .unwrap_or_else(|| serde_json::from_str(d.default).unwrap_or(Value::Null));
        m.insert(d.key.to_string(), v);
    }
    m
}

pub fn read_doc(path: &Path) -> Result<DocumentMut, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    text.parse::<DocumentMut>()
        .map_err(|e| format!("{}: {}", path.display(), e.message()))
}

// ---- changing -----------------------------------------------------------------------------------------------

/// A key problem the form can show next to the field.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyError {
    pub key: String,
    pub message: String,
}

fn to_toml(d: &Def, v: &Value) -> Result<toml_edit::Value, String> {
    let bad = |what: &str| Err(format!("must be {what}"));
    Ok(match d.kind {
        "int" => match v
            .as_i64()
            .or_else(|| v.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
        {
            Some(n) => n.into(),
            None => return bad("a whole number"),
        },
        "float" => match v.as_f64() {
            Some(f) if f.is_finite() => f.into(),
            _ => return bad("a number"),
        },
        "bool" => match v.as_bool() {
            Some(b) => b.into(),
            None => return bad("on or off"),
        },
        "numlist" => match v.as_array() {
            Some(a) if a.iter().all(|x| x.as_f64().is_some_and(f64::is_finite)) => {
                let mut arr = Array::new();
                for x in a {
                    arr.push(x.as_f64().unwrap_or_default());
                }
                toml_edit::Value::Array(arr)
            }
            _ => return bad("a list of numbers"),
        },
        "formats" | "list" => match v.as_array() {
            Some(a) if a.iter().all(Value::is_string) => {
                let mut arr = Array::new();
                for s in a {
                    arr.push(s.as_str().unwrap_or_default());
                }
                toml_edit::Value::Array(arr)
            }
            _ => return bad("a list of text"),
        },
        _ => match v.as_str() {
            Some(s) => s.into(),
            None => return bad("text"),
        },
    })
}

/// Apply `changes` (dotted key to a value, `null` or an empty text meaning "remove the key, use the default")
/// to the document, keeping every comment, the order and the keys this page does not know.
pub fn apply_changes(
    doc: &mut DocumentMut,
    scope: &str,
    changes: &Map<String, Value>,
) -> Result<(), Vec<KeyError>> {
    let mut errs = vec![];
    for (key, v) in changes {
        let Some(d) = defs(scope).find(|d| d.key == key) else {
            errs.push(KeyError {
                key: key.clone(),
                message: "is not a setting of this file".into(),
            });
            continue;
        };
        if d.kind == "readonly" {
            errs.push(KeyError {
                key: key.clone(),
                message: "cannot be changed here".into(),
            });
            continue;
        }
        let remove = v.is_null()
            || v.as_str()
                .is_some_and(|s| s.trim().is_empty() && d.kind != "text_keep");
        let parts: Vec<&str> = key.split('.').collect();
        let (leaf, path) = parts.split_last().map_or(("", &[][..]), |(l, p)| (*l, p));
        if remove {
            if let Some(tab) = table_at(doc, path, false) {
                tab.remove(leaf);
            }
            // A nested key (`chain.agc.x`) leaves no empty table behind.
            for i in (2..=path.len()).rev() {
                let empty = table_at(doc, &path[..i], false).is_some_and(|t| t.is_empty());
                if !empty {
                    break;
                }
                if let Some(par) = table_at(doc, &path[..i - 1], false) {
                    par.remove(path[i - 1]);
                }
            }
            continue;
        }
        let new = match to_toml(d, v) {
            Ok(n) => n,
            Err(m) => {
                errs.push(KeyError {
                    key: key.clone(),
                    message: m,
                });
                continue;
            }
        };
        let Some(tab) = table_at(doc, path, true) else {
            errs.push(KeyError {
                key: key.clone(),
                message: format!("`{}` is not a table in the file", path.join(".")),
            });
            continue;
        };
        let mut new = new;
        if let Some(old) = tab.get(leaf).and_then(Item::as_value) {
            // Keep the comment on that line.
            *new.decor_mut() = old.decor().clone();
        } else {
            new.decor_mut().set_prefix(" ");
        }
        match tab.get_mut(leaf) {
            // The key keeps its own comments; only the value (and its trailing comment) is replaced.
            Some(it) if it.is_value() => *it = Item::Value(new),
            _ => {
                tab.insert(leaf, Item::Value(new));
            }
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

/// Parse the text with the parser the service itself uses; every problem comes back with its key.
pub fn validate(scope: &str, text: &str, file: &Path) -> Result<(), Vec<KeyError>> {
    let r = if scope == S {
        StationConfig::parse(text, file).map(|_| ())
    } else {
        GlobalConfig::parse(text, file).map(|_| ())
    };
    r.map_err(|e| {
        e.0.into_iter()
            .map(|e| KeyError {
                key: e.key,
                message: e.message,
            })
            .collect()
    })
}

/// The keys whose value differs between two documents, with how each reaches the service.
pub fn changed_keys(
    before: &DocumentMut,
    after: &DocumentMut,
    scope: &str,
) -> Vec<(&'static Def, Value)> {
    let b = explicit_values(before, scope);
    let a = explicit_values(after, scope);
    defs(scope)
        .filter(|d| b.get(d.key) != a.get(d.key))
        .map(|d| {
            (
                d,
                a.get(d.key)
                    .cloned()
                    .unwrap_or_else(|| serde_json::from_str(d.default).unwrap_or(Value::Null)),
            )
        })
        .collect()
}

// ---- writing ------------------------------------------------------------------------------------------------

/// Write `text` over `path` atomically: a temporary file beside it, synced, the previous file copied to
/// `<path>.bak`, then a rename. The file keeps its mode.
pub fn write_atomic(path: &Path, text: &str) -> Result<PathBuf, String> {
    use std::io::Write;
    let dir = path.parent().ok_or("no directory")?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("no file name")?;
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    let mut bak = path.as_os_str().to_owned();
    bak.push(".bak");
    let bak = PathBuf::from(bak);
    let r = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        if let Ok(m) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(&tmp, m.permissions());
            let _ = std::fs::copy(path, &bak);
        }
        std::fs::rename(&tmp, path)?;
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    match r {
        Ok(()) => Ok(bak),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(format!("cannot write {}: {e}", path.display()))
        }
    }
}

/// Whether the service can replace `path` (the file and its directory are writable by it).
pub fn can_write(path: &Path) -> bool {
    use rustix::fs::{access, Access};
    let dir_ok = path
        .parent()
        .is_some_and(|d| access(d, Access::WRITE_OK | Access::EXEC_OK).is_ok());
    let file_ok = !path.exists() || access(path, Access::WRITE_OK).is_ok();
    dir_ok && file_ok
}

// ---- the box's sound cards ------------------------------------------------------------------------------------

/// The playback devices under `/proc/asound`-style `root`, as ALSA names (`hw:CARD=PCH,DEV=0`) with a label.
pub fn list_devices(root: &Path) -> Vec<Value> {
    let mut out = vec![
        json!({"value": "null", "label": "null: plays to nowhere in real time (a station that only streams)"}),
    ];
    let Ok(cards) = std::fs::read_to_string(root.join("cards")) else {
        return out;
    };
    // ` 0 [PCH            ]: HDA-Intel - HDA Intel PCH`, then an indented long name.
    let mut lines = cards.lines().peekable();
    while let Some(l) = lines.next() {
        let l = l.trim_start();
        let Some((num, rest)) = l.split_once(' ') else {
            continue;
        };
        let Ok(n) = num.parse::<u32>() else { continue };
        let Some((id, desc)) = rest
            .trim_start()
            .strip_prefix('[')
            .and_then(|r| r.split_once(']'))
        else {
            continue;
        };
        let id = id.trim();
        let long = lines
            .peek()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let desc = desc.trim_start_matches(':').trim();
        let mut devs: Vec<u32> = std::fs::read_dir(root.join(format!("card{n}")))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                name.strip_prefix("pcm")?.strip_suffix('p')?.parse().ok()
            })
            .collect();
        devs.sort_unstable();
        for dev in devs {
            let pcm = std::fs::read_to_string(root.join(format!("card{n}/pcm{dev}p/info")))
                .ok()
                .and_then(|t| {
                    t.lines()
                        .find_map(|x| x.strip_prefix("name: ").map(str::to_string))
                })
                .unwrap_or_default();
            out.push(json!({
                "value": format!("hw:CARD={id},DEV={dev}"),
                "label": format!("hw:CARD={id},DEV={dev}: {} {} {}", if long.is_empty() { desc } else { &long }, "-", pcm),
            }));
        }
    }
    out
}

// ---- restarting -----------------------------------------------------------------------------------------------

/// The program that restarts units (`HOTCLOCK_SYSTEMCTL` replaces it in tests).
fn systemctl() -> String {
    std::env::var("HOTCLOCK_SYSTEMCTL").unwrap_or_else(|_| "systemctl".into())
}

/// `systemctl restart <unit>`; the polkit rule of the package lets the service user do exactly this for the
/// station units and the library unit.
pub fn restart_unit(unit: &str) -> Result<(), String> {
    unit_action("restart", unit)
}

/// `systemctl <verb> <unit>` for `start`, `stop` and `restart` only: the three verbs the package's polkit rule
/// allows the service user on `hotclock-station@<name>.service` (and `restart` on the library and the display).
/// `enable` and `disable` are not here on purpose: polkit cannot limit them to one unit (see deploy.md,
/// "Stations from the web page"), so a station is started at boot by a generator that reads its file instead.
pub fn unit_action(verb: &str, unit: &str) -> Result<(), String> {
    if !matches!(verb, "start" | "stop" | "restart") {
        return Err(format!("`{verb}` is not a verb this service may use"));
    }
    let out = std::process::Command::new(systemctl())
        .args([verb, unit])
        .output()
        .map_err(|e| format!("cannot run systemctl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "systemctl {verb} {unit} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// `systemctl is-active <unit>`: `active`, `inactive`, `failed`, `activating`... (`None` when systemctl cannot be run).
/// Needs no privilege.
pub fn unit_state(unit: &str) -> Option<String> {
    let out = std::process::Command::new(systemctl())
        .args(["is-active", unit])
        .output()
        .ok()?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Some(if t.is_empty() { "unknown".into() } else { t })
}

/// Whether this box has a service manager the page can ask (systemd, or the fake of a test).
pub fn service_manager_available() -> bool {
    std::env::var("HOTCLOCK_SYSTEMCTL").is_ok() || Path::new("/run/systemd/system").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chain_keys_are_in_the_catalog_with_the_schema_defaults_and_nested_edits_work() {
        let eff = effective_values(&Map::new(), S);
        let d = common::chain::ChainConfig::default();
        let mut n = 0;
        for b in common::chain::BLOCKS {
            if b.switch {
                assert_eq!(eff[&format!("chain.{}.enabled", b.key)], json!(false));
                n += 1;
            }
            for p in b.params {
                assert_eq!(
                    eff[&format!("chain.{}.{}", b.key, p.key)],
                    d.block_json(b.key)[p.key],
                    "{}.{}",
                    b.key,
                    p.key
                );
                n += 1;
            }
        }
        assert!(n >= 50, "{n} chain keys");
        // A nested key is written under [chain.agc], read back by its dotted name, validated by the parser, and
        // removing it leaves no empty table behind.
        let mut doc: DocumentMut = "name = \"x\"\nlisten = \"0.0.0.0:1\"\ndevice = \"null\"\n"
            .parse()
            .unwrap();
        let ch: Map<String, Value> = [
            ("chain.agc.target_dbfs".to_string(), json!(-22.0)),
            ("chain.multiband.enabled".to_string(), json!(true)),
            (
                "chain.multiband.crossovers_hz".to_string(),
                json!([200.0, 4000.0]),
            ),
            ("chain.eq.position".to_string(), json!("post")),
        ]
        .into_iter()
        .collect();
        apply_changes(&mut doc, S, &ch).unwrap();
        let text = doc.to_string();
        assert!(
            text.contains("[chain.agc]") && text.contains("target_dbfs = -22.0"),
            "{text}"
        );
        assert_eq!(
            explicit_values(&doc, S)["chain.multiband.crossovers_hz"],
            json!([200.0, 4000.0])
        );
        // The three-band crossovers without their arrays do not pass the parser, and the key is named.
        let e = validate(S, &text, Path::new("/etc/hotclock/stations/x.toml")).unwrap_err();
        assert!(
            e.iter().any(|k| k.key.starts_with("chain.multiband")),
            "{e:?}"
        );
        let rm: Map<String, Value> = [("chain.agc.target_dbfs".to_string(), Value::Null)]
            .into_iter()
            .collect();
        apply_changes(&mut doc, S, &rm).unwrap();
        assert!(!doc.to_string().contains("[chain.agc]"), "{doc}");
        let bad: Map<String, Value> = [("chain.agc.target_dbfs".to_string(), json!("loud"))]
            .into_iter()
            .collect();
        assert!(apply_changes(&mut doc, S, &bad).is_err());
    }

    /// Prints the `GROUPS` lines of the processing chain for `settings.js` (run when the schema changes).
    #[test]
    #[ignore]
    fn print_chain_groups() {
        for b in common::chain::BLOCKS {
            let mut keys: Vec<String> = vec![];
            if b.switch {
                keys.push(format!("'chain.{}.enabled'", b.key));
            }
            for p in b.params {
                keys.push(format!("'chain.{}.{}'", b.key, p.key));
            }
            println!(
                "  ['Processing chain: {}', [{}]],",
                b.label,
                keys.join(", ")
            );
        }
    }

    const FILE: &str = r#"# my station
name = "office"
listen = "0.0.0.0:8301"   # the API
device = "null"
# loudness
target_lufs = -18.0  # LUFS
custom_unknown = 5
"#;

    #[test]
    fn defaults_in_the_catalog_match_the_parser() {
        let c = StationConfig::parse(
            "name = \"x\"\nlisten = \"0.0.0.0:1\"\ndevice = \"null\"\n",
            Path::new("/etc/hotclock/stations/x.toml"),
        )
        .unwrap();
        let eff = effective_values(&Map::new(), S);
        assert_eq!(eff["rate"], json!(c.rate));
        assert_eq!(eff["period_frames"], json!(c.period_frames));
        assert_eq!(eff["periods"], json!(c.periods));
        assert_eq!(eff["target_lufs"], json!(c.target_lufs));
        assert_eq!(eff["max_boost_db"], json!(c.max_boost_db));
        assert_eq!(eff["output_trim_db"], json!(c.output_trim_db));
        assert_eq!(eff["peak_allowance_db"], json!(c.peak_allowance_db));
        assert_eq!(eff["transition_preset"], json!(c.transition_preset));
        assert_eq!(eff["ram_loop_seconds"], json!(c.ram_loop_seconds));
        assert_eq!(eff["lock_ahead_s"], json!(c.lock_ahead_s));
        assert_eq!(eff["silence_dbfs"], json!(c.silence_dbfs));
        assert_eq!(eff["silence_s"], json!(c.silence_s));
        assert_eq!(eff["restart_alert_clear_s"], json!(c.restart_alert_clear_s));
        assert_eq!(eff["processing_preset"], json!(c.processing_preset));
        assert_eq!(eff["rate_trim_ppm"], json!(c.rate_trim_ppm));
        let st = &c.stream;
        assert_eq!(eff["stream.enabled"], json!(st.enabled));
        assert_eq!(eff["stream.mount"], json!(st.mount));
        assert_eq!(eff["stream.codec"], json!(st.codec));
        assert_eq!(eff["stream.flac_level"], json!(st.flac_level));
        assert_eq!(eff["stream.opus_kbps"], json!(st.opus_kbps));
        assert_eq!(eff["stream.burst_ms"], json!(st.burst_ms));
        assert_eq!(eff["stream.max_listeners"], json!(st.max_listeners));
        assert_eq!(eff["stream.listener_lag_s"], json!(st.listener_lag_s));
        assert_eq!(eff["stream.expect_listener"], json!(st.expect_listener));
        assert_eq!(
            eff["stream.no_listener_alert_s"],
            json!(st.no_listener_alert_s)
        );
        assert_eq!(eff["stream.send_icy_metadata"], json!(st.send_icy_metadata));
        let formats: Vec<&str> = c.formats.iter().map(|f| f.name()).collect();
        assert_eq!(eff["formats"], json!(formats));
        let g = GlobalConfig::default();
        let eg = effective_values(&Map::new(), G);
        assert_eq!(eg["data_dir"], json!(g.data_dir));
        assert_eq!(eg["stations_dir"], json!(g.stations_dir));
        assert_eq!(eg["run_dir"], json!(g.run_dir));
        assert_eq!(eg["time_zone"], json!(g.time_zone));
        assert_eq!(eg["library.listen"], json!(g.library.listen));
        assert_eq!(eg["secrets_file"], json!(g.secrets_file));
        let d = &g.display;
        assert_eq!(eg["display.mode"], json!(d.mode));
        assert_eq!(eg["display.diagonal_in"], json!(d.diagonal_in));
        assert_eq!(eg["display.view_distance_cm"], json!(d.view_distance_cm));
        assert_eq!(eg["display.default_view"], json!(d.default_view));
        assert_eq!(eg["display.bypassed_stages"], json!(d.bypassed_stages));
        assert_eq!(eg["display.processing_view"], json!(d.processing_view));
        assert_eq!(eg["display.spectrum_source"], json!(d.spectrum_source));
        assert_eq!(eg["display.curve_range_db"], json!(d.curve_range_db));
        assert_eq!(
            eg["display.curve_include_makeup"],
            json!(d.curve_include_makeup)
        );
        assert_eq!(eg["display.connector"], json!(d.connector));
        assert_eq!(eg["display.device"], json!(d.device));
        assert_eq!(
            eg["display.rotate_pin_on_alert"],
            json!(d.rotate_pin_on_alert)
        );
        assert_eq!(eg["display.rotate_processing"], json!(d.rotate_processing));
        assert_eq!(eg["display.proc_range_db"], json!(d.proc_range_db));
        assert_eq!(eg["display.proc_spectrum"], json!(d.proc_spectrum));
        assert_eq!(eg["display.mini_processing"], json!(d.mini_processing));
        assert_eq!(eg["display.bar_product"], json!(d.bar_product));
        assert_eq!(eg["display.bar_clock"], json!(d.bar_clock));
        assert_eq!(eg["display.bar_date"], json!(d.bar_date));
        assert_eq!(eg["display.bar_cpu"], json!(d.bar_cpu));
        assert_eq!(eg["display.bar_network"], json!(d.bar_network));
    }

    #[test]
    fn a_change_keeps_comments_order_and_unknown_keys() {
        let mut doc: DocumentMut = FILE.parse().unwrap();
        let before = doc.clone();
        let mut ch = Map::new();
        ch.insert("target_lufs".into(), json!(-16.0));
        ch.insert("stream.enabled".into(), json!(true));
        ch.insert("lock_ahead_s".into(), json!(30));
        apply_changes(&mut doc, S, &ch).unwrap();
        let text = doc.to_string();
        assert!(text.contains("# my station"));
        assert!(text.contains("# the API"));
        assert!(text.contains("# loudness"));
        assert!(text.contains("target_lufs = -16.0  # LUFS"), "{text}");
        assert!(text.contains("custom_unknown = 5"));
        let c = StationConfig::parse(
            &text.replace("custom_unknown = 5\n", ""),
            Path::new("/x/office.toml"),
        )
        .unwrap();
        assert_eq!(c.target_lufs, -16.0);
        assert!(c.stream.enabled);
        assert_eq!(c.lock_ahead_s, 30);
        let keys: Vec<_> = changed_keys(&before, &doc, S)
            .iter()
            .map(|(d, _)| d.key)
            .collect();
        assert_eq!(keys, ["target_lufs", "lock_ahead_s", "stream.enabled"]);
    }

    #[test]
    fn the_display_block_is_edited_in_the_global_file_and_validated_by_the_parser() {
        let text = "data_dir = \"/var/lib/hotclock\"\n# the box\n";
        let mut doc: DocumentMut = text.parse().unwrap();
        let before = doc.clone();
        let mut ch = Map::new();
        ch.insert("display.mode".into(), json!("1920x1080@60"));
        ch.insert("display.diagonal_in".into(), json!(15.6));
        ch.insert("display.default_view".into(), json!("split"));
        ch.insert("display.rotate_pin_on_alert".into(), json!(false));
        apply_changes(&mut doc, G, &ch).unwrap();
        let out = doc.to_string();
        assert!(
            out.contains("# the box") && out.contains("[display]"),
            "{out}"
        );
        let g = GlobalConfig::parse(&out, Path::new("/etc/hotclock/hotclock.toml")).unwrap();
        assert_eq!(
            (
                g.display.mode.as_str(),
                g.display.diagonal_in,
                g.display.default_view.as_str(),
                g.display.rotate_pin_on_alert
            ),
            ("1920x1080@60", 15.6, "split", false)
        );
        let keys = changed_keys(&before, &doc, G);
        assert_eq!(keys.len(), 4);
        assert!(keys.iter().all(|(d, _)| d.apply == DISPLAY_RESTART));
        // the parser refuses a bad value and names the key
        let mut bad: DocumentMut = out.parse().unwrap();
        let mut ch = Map::new();
        ch.insert("display.diagonal_in".into(), json!(0.5));
        apply_changes(&mut bad, G, &ch).ok();
        let e = validate(G, &bad.to_string(), Path::new("h.toml")).unwrap_err();
        assert!(e.iter().any(|k| k.key == "display.diagonal_in"), "{e:?}");
        assert!(pending_keys()["display"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_null_removes_the_key_and_a_wrong_type_is_named() {
        let mut doc: DocumentMut = FILE.parse().unwrap();
        let mut ch = Map::new();
        ch.insert("target_lufs".into(), Value::Null);
        apply_changes(&mut doc, S, &ch).unwrap();
        assert!(!doc.to_string().contains("target_lufs"));
        let mut ch = Map::new();
        ch.insert("periods".into(), json!("many"));
        ch.insert("nonsense".into(), json!(1));
        ch.insert("name".into(), json!("other"));
        let e = apply_changes(&mut doc, S, &ch).unwrap_err();
        assert_eq!(e.len(), 3);
        assert!(e.iter().any(|k| k.key == "periods"));
    }

    #[test]
    fn the_parser_rejects_what_the_form_cannot() {
        let mut doc: DocumentMut = FILE.replace("custom_unknown = 5\n", "").parse().unwrap();
        let mut ch = Map::new();
        ch.insert("target_lufs".into(), json!(-3.0));
        ch.insert("period_frames".into(), json!(1000));
        apply_changes(&mut doc, S, &ch).unwrap();
        let e = validate(S, &doc.to_string(), Path::new("/x/office.toml")).unwrap_err();
        let keys: Vec<_> = e.iter().map(|k| k.key.as_str()).collect();
        assert!(
            keys.contains(&"target_lufs") && keys.contains(&"period_frames"),
            "{keys:?}"
        );
    }

    #[test]
    fn write_keeps_a_backup_and_leaves_no_temporary_file() {
        let dir = std::env::temp_dir().join(format!("hc-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("office.toml");
        std::fs::write(&p, "a = 1\n").unwrap();
        let bak = write_atomic(&p, "a = 2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a = 2\n");
        assert_eq!(std::fs::read_to_string(bak).unwrap(), "a = 1\n");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        assert!(can_write(&p));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn devices_come_from_the_proc_tree() {
        let dir = std::env::temp_dir().join(format!("hc-asound-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("card0/pcm0p")).unwrap();
        std::fs::create_dir_all(dir.join("card0/pcm0c")).unwrap();
        std::fs::create_dir_all(dir.join("card0/pcm3p")).unwrap();
        std::fs::write(dir.join("cards"), " 0 [PCH            ]: HDA-Intel - HDA Intel PCH\n                      HDA Intel PCH at 0xdf1c0000\n").unwrap();
        std::fs::write(
            dir.join("card0/pcm0p/info"),
            "card: 0\nname: ALC3246 Analog\n",
        )
        .unwrap();
        let d = list_devices(&dir);
        let vals: Vec<_> = d
            .iter()
            .map(|v| v["value"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(vals, ["null", "hw:CARD=PCH,DEV=0", "hw:CARD=PCH,DEV=3"]);
        assert!(d[1]["label"].as_str().unwrap().contains("ALC3246"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
