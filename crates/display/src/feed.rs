//! Scenario feeds for the headless renderer: a JSON file (or a built-in demo) describing what the stations say,
//! turned into the same `StationModel`s the live display builds from WebSockets. A feed can carry real recorded
//! meter frames (`frames_file`, concatenated 220-byte frames) or synthetic ones described by their fields.

use crate::frame::{Frame, FRAME_LEN};
use crate::model::{iso_ms, parse_iso_ms, Chain, StationModel};
use crate::view::{ViewMode, ViewSettings};
use serde_json::{json, Value};
use std::path::Path;

pub struct Feed {
    pub now_ms: i64,
    pub tz_min: i64,
    pub clock: String,
    pub stations: Vec<StationModel>,
    pub settings: ViewSettings,
    /// Milliseconds into the rotation interval, for `rotate` screenshots.
    pub rotate_elapsed_ms: i64,
    /// `(url, image)`: artwork the feed supplies.
    pub art: Vec<(String, image::RgbaImage)>,
    /// The top bar's answer of the library service (`GET /api/v1/display/bar`), when the feed says one.
    pub bar: Option<Value>,
}

/// A deterministic image for a URL: a diagonal gradient whose hue follows the URL, with a ring.
pub fn synthetic_art(url: &str, size: u32) -> image::RgbaImage {
    let h = url
        .bytes()
        .fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
    let (r0, g0, b0) = (
        60 + (h & 0x7f) as u8,
        60 + ((h >> 7) & 0x7f) as u8,
        60 + ((h >> 14) & 0x7f) as u8,
    );
    image::RgbaImage::from_fn(size, size, |x, y| {
        let t = (x + y) as f32 / (2 * size) as f32;
        let (cx, cy) = (x as f32 - size as f32 / 2.0, y as f32 - size as f32 / 2.0);
        let d = (cx * cx + cy * cy).sqrt() / size as f32;
        let ring = if (d - 0.3).abs() < 0.03 { 1.4 } else { 1.0 };
        let f = |c: u8, k: f32| {
            ((f32::from(c) * (0.6 + 0.8 * (if k > 0.5 { 1.0 - t } else { t })) * ring).min(255.0))
                as u8
        };
        image::Rgba([f(r0, 0.0), f(g0, 1.0), f(b0, 0.0), 255])
    })
}

/// A synthetic spectrum: a floor, a gentle tilt, and an optional tone `(hz, dBFS)` in its band.
pub fn synth_spectrum(tone: Option<(f64, f64)>, floor: f64) -> [f64; 64] {
    let mut a = [floor; 64];
    for (i, v) in a.iter_mut().enumerate() {
        *v = floor - 0.12 * i as f64 + 4.0 * ((i as f64) * 0.55).sin();
    }
    if let Some((hz, db)) = tone {
        let b = (64.0 * (hz / 20.0).log10() / 3.0).floor().clamp(0.0, 63.0) as usize;
        a[b] = db;
        if b > 0 {
            a[b - 1] = a[b - 1].max(db - 30.0);
        }
        if b < 63 {
            a[b + 1] = a[b + 1].max(db - 30.0);
        }
    }
    a
}

fn opt(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn pair(v: &Value) -> [Option<f64>; 2] {
    [opt(&v[0]), opt(&v[1])]
}

/// A synthetic frame from its description.
pub fn frame_from_spec(s: &Value) -> Frame {
    let mut f = Frame::silent();
    f.flags = s["flags"]
        .as_u64()
        .unwrap_or(if s["bypassed"].as_bool().unwrap_or(true) {
            1
        } else {
            0
        }) as u8;
    f.ppm = pair(&s["ppm"]);
    f.peak = if s["peak"].is_null() {
        f.ppm
    } else {
        pair(&s["peak"])
    };
    f.momentary = opt(&s["momentary"]);
    f.short_term = opt(&s["short_term"]);
    f.integrated = opt(&s["integrated"]);
    f.window = opt(&s["window"]);
    f.target = opt(&s["target"]);
    f.gain_db = s["gain_db"].as_f64().unwrap_or(0.0);
    f.loudness_state = s["loudness_state"].as_u64().unwrap_or(0) as u8;
    f.stage_mask = s["stage_mask"].as_u64().unwrap_or(0) as u8;
    f.band_count = s["band_count"].as_u64().unwrap_or(0) as u8;
    f.chain_cfg_rev = s["chain_cfg_rev"].as_u64().unwrap_or(0) as u16;
    if let Some(a) = s["band_gr"].as_array() {
        for (i, v) in a.iter().take(6).enumerate() {
            f.band_gr[i] = v.as_f64();
        }
    }
    for (i, name) in crate::frame::STAGES.iter().enumerate() {
        let m = &s["stages"][*name];
        if m.is_object() {
            f.stages[i].input = opt(&m["in"]);
            f.stages[i].output = opt(&m["out"]);
            f.stages[i].gr = opt(&m["gr"]);
        }
    }
    let tone = |k: &str| {
        s[k].as_object().map(|o| {
            (
                o["hz"].as_f64().unwrap_or(1000.0),
                o["db"].as_f64().unwrap_or(-20.0),
            )
        })
    };
    f.post = synth_spectrum(tone("post_tone"), s["floor"].as_f64().unwrap_or(-72.0));
    f.pre = synth_spectrum(
        tone("pre_tone").or(tone("post_tone")),
        s["floor"].as_f64().unwrap_or(-72.0) + 3.0,
    );
    f
}

impl Feed {
    pub fn from_json(v: &Value, base: &Path) -> Result<Feed, String> {
        let now_ms = v["at"]
            .as_str()
            .and_then(parse_iso_ms)
            .ok_or("feed: `at` must be an RFC 3339 time")?;
        let tz_min = v["tz_min"].as_i64().unwrap_or(0);
        let clock = v["clock"].as_str().map(str::to_string).unwrap_or_else(|| {
            let m = (now_ms.div_euclid(1000) + tz_min * 60).rem_euclid(86_400);
            format!("{:02}:{:02}:{:02}", m / 3600, m / 60 % 60, m % 60)
        });
        let mut stations = vec![];
        let mut art = vec![];
        for sv in v["stations"]
            .as_array()
            .ok_or("feed: `stations` must be a list")?
        {
            let id = sv["id"].as_str().ok_or("feed: a station needs an `id`")?;
            let mut m = StationModel::new(id, sv["display_name"].as_str().unwrap_or(id));
            m.set_connected(sv["connected"].as_bool().unwrap_or(true));
            if sv["snapshot"].is_object() {
                m.apply_json(&json!({"type": "snapshot", "data": sv["snapshot"]}), now_ms);
            }
            if let Some(h) = sv.get("history") {
                m.set_history(h);
            }
            if let Some(l) = sv.get("loudness") {
                m.set_loudness(l);
            }
            if let Some(c) = sv.get("chain") {
                m.chain = Chain::from_json(c);
                m.chain_rev_seen = m.chain.as_ref().map(|c| c.rev);
            }
            for e in sv["events"].as_array().cloned().unwrap_or_default() {
                m.apply_json(&e, now_ms);
            }
            m.set_connected(sv["connected"].as_bool().unwrap_or(true));
            // frames: a recorded file, or a synthetic one; the last ones before `now` set the dynamics
            let mut frames: Vec<Frame> = vec![];
            if let Some(f) = sv["frames_file"].as_str() {
                let bytes = std::fs::read(base.join(f)).map_err(|e| format!("{f}: {e}"))?;
                frames.extend(bytes.chunks_exact(FRAME_LEN).filter_map(Frame::parse));
            }
            if sv["frame"].is_object() {
                frames.push(frame_from_spec(&sv["frame"]));
            }
            if let Some(a) = sv["frames"].as_array() {
                frames.extend(a.iter().map(frame_from_spec));
            }
            let n = frames.len() as i64;
            for (i, fr) in frames.into_iter().enumerate() {
                let t = now_ms - (n - 1 - i as i64) * 33;
                m.dynamics.update(Some(&fr), t);
                m.apply_frame(fr, t);
            }
            if sv["art"].as_str() == Some("synthetic") {
                let mut urls = vec![];
                if let Some(n) = &m.now {
                    urls.extend(n.art_url.clone());
                }
                urls.extend(m.queue.iter().filter_map(|e| e.art_url.clone()));
                for u in urls {
                    for size in [600u32, 144] {
                        let key = crate::scene::sized(&u, size);
                        if !art
                            .iter()
                            .any(|(k, _): &(String, image::RgbaImage)| *k == key)
                        {
                            art.push((key.clone(), synthetic_art(&key, size)));
                        }
                    }
                }
            }
            stations.push(m);
        }
        let mut settings = ViewSettings::default();
        if let Some(x) = v["view"].as_str().and_then(ViewMode::parse) {
            settings.view = x;
        }
        settings.station = v["station"].as_str().map(str::to_string);
        if let Some(n) = v["rotate_s"].as_u64() {
            settings.rotate_s = n as u32;
        }
        if let Some(b) = v["rotate_processing"].as_bool() {
            settings.rotate_processing = b;
        }
        Ok(Feed {
            now_ms,
            tz_min,
            clock,
            stations,
            settings,
            rotate_elapsed_ms: v["rotate_elapsed_ms"].as_i64().unwrap_or(0),
            art,
            bar: v.get("bar").cloned(),
        })
    }
}

// ---- the built-in demos ----------------------------------------------------------------------------------------------

const AT: &str = "2026-10-06T20:21:14.000Z";

fn at_ms() -> i64 {
    parse_iso_ms(AT).expect("AT")
}

fn track(
    id: i64,
    title: &str,
    artist: &str,
    album: &str,
    art: bool,
    start_off_s: i64,
    origin: &str,
) -> Value {
    json!({"kind": "track", "item_id": id, "title": title, "artist": artist, "album": album, "category": "music",
        "art_url": if art { json!(format!("/api/v1/items/{id}/art?size=144")) } else { Value::Null },
        "origin": origin, "pinned": false, "committed": false,
        "planned_start": iso_ms(at_ms() + start_off_s * 1000)})
}

fn block(kind: &str, name: &str, in_s: i64) -> Value {
    json!({"kind": "block", "block_type": kind, "name": name, "in_ms": in_s * 1000, "origin": "block"})
}

/// One station of the demo: `title` is the item on air.
#[allow(clippy::too_many_arguments)]
fn station(
    id: &str,
    name: &str,
    title: &str,
    artist: &str,
    album: &str,
    art: bool,
    remaining_s: i64,
    segue_in_s: i64,
    elapsed_s: i64,
    queue: Vec<Value>,
    alerts: Vec<Value>,
    gapless: bool,
    frame: Value,
) -> Value {
    let now = at_ms();
    let item = 11 + id.len() as i64;
    json!({
        "id": id, "display_name": name, "connected": true, "art": "synthetic",
        "snapshot": {
            "state": "playing", "mode": "auto", "hold": false, "safe_mode": false, "display_name": name,
            "now": {"entry_id": 51, "item_id": item, "title": title, "artist": artist, "album": album, "category": "music",
                    "art_url": if art { json!(format!("/api/v1/items/{item}/art?size=144")) } else { Value::Null },
                    "started_at": iso_ms(now - elapsed_s * 1000), "segue_at": iso_ms(now + segue_in_s * 1000),
                    "ends_at": iso_ms(now + remaining_s * 1000), "continuity_next": gapless, "origin": "rotation"},
            "queue": queue, "alerts": alerts,
        },
        "history": {"rows": [
            {"title": "From Silence (RC1)", "artist": "Demo Artists", "category": "music", "started_at": iso_ms(now - 1_100_000), "ended_at": iso_ms(now - 840_000), "end_reason": "next", "deviation_ms": Value::Null},
            {"title": "Top of hour ID", "artist": "", "category": "id", "started_at": iso_ms(now - 840_000), "ended_at": iso_ms(now - 832_000), "end_reason": "end", "deviation_ms": 1200},
            {"title": "Longing (Demo v1)", "artist": "Demo Artists", "category": "music", "started_at": iso_ms(now - 832_000), "ended_at": iso_ms(now - 410_000), "end_reason": "next", "deviation_ms": Value::Null},
            {"title": "The Canal (Demo v1)", "artist": "Second Artist", "category": "music", "started_at": iso_ms(now - 410_000), "ended_at": iso_ms(now - 250_000), "end_reason": "cut", "deviation_ms": Value::Null},
            {"title": "Station liner 4", "artist": "", "category": "liner", "started_at": iso_ms(now - 250_000), "ended_at": iso_ms(now - 244_000), "end_reason": "end", "deviation_ms": Value::Null},
            {"title": "Spectrotemporal", "artist": "Second Artist", "category": "music", "started_at": iso_ms(now - 244_000), "ended_at": iso_ms(now - 230_000), "end_reason": "error", "deviation_ms": Value::Null},
        ]},
        "frame": frame,
    })
}

fn frame_live() -> Value {
    json!({"ppm": [-18.5, -19.2], "peak": [-17.0, -17.8], "momentary": -14.6, "short_term": -14.2, "integrated": -14.0, "window": -14.1,
           "target": -14.0, "gain_db": 1.2, "loudness_state": 2, "bypassed": true, "post_tone": {"hz": 1000.0, "db": -20.0}})
}

fn frame_chain(gr: [f64; 5]) -> Value {
    json!({"ppm": [-16.0, -16.8], "peak": [-12.0, -12.5], "momentary": -14.4, "short_term": -14.1, "integrated": -14.0, "window": -14.0,
           "target": -14.0, "gain_db": -0.6, "loudness_state": 2, "bypassed": false, "stage_mask": 0b11111, "band_count": 5, "chain_cfg_rev": 3,
           "band_gr": gr,
           "stages": {"agc": {"in": -18.0, "out": -16.0, "gr": 2.4}, "multiband": {"in": -16.0, "out": -17.2, "gr": -4.2},
                      "wideband": {"in": -17.2, "out": -17.9, "gr": -0.7}, "limiter": {"in": -17.9, "out": -18.3, "gr": -0.4},
                      "clipper": {"in": -18.3, "out": -18.3, "gr": -0.1}},
           "post_tone": {"hz": 1000.0, "db": -22.0}, "pre_tone": {"hz": 1000.0, "db": -18.0}})
}

fn chain_cfg(bands: usize) -> Value {
    let xo: Vec<f64> = if bands == 3 {
        vec![200.0, 3000.0]
    } else {
        vec![120.0, 500.0, 2000.0, 6000.0]
    };
    json!({"bypassed": false, "chain_cfg_rev": 3, "makeup_db": 0.0, "preset": "radio",
        "multiband": {"bands": bands, "crossovers_hz": xo, "slope_db_per_octave": 24},
        "stages": [{"name": "agc", "enabled": true}, {"name": "multiband", "enabled": true}, {"name": "wideband", "enabled": true},
                   {"name": "limiter", "enabled": true, "ceiling_dbtp": -1.0}, {"name": "clipper", "enabled": true}, {"name": "safety_limiter", "enabled": true, "note": "always on"}]})
}

/// A chain-5 frame at `k` times the reductions of `frame_chain`, with the AGC's gate frozen and the clipper acting
/// (flags 2 and 16), so a run of them leaves bars part way down and peak ticks above them.
fn frame_proc(k: f64) -> Value {
    let mut f = frame_chain([-3.0 * k, -6.0 * k, -9.0 * k, -6.0 * k, -3.0 * k]);
    f["flags"] = json!(2 | 16);
    f["stages"]["agc"] = json!({"in": -22.0, "out": -19.0, "gr": 2.4});
    f["stages"]["wideband"] = json!({"in": -17.2, "out": -18.4, "gr": -1.2 * k});
    f["stages"]["limiter"] = json!({"in": -18.4, "out": -19.4, "gr": -2.0 * k});
    f["stages"]["clipper"] = json!({"in": -19.4, "out": -19.4, "gr": -0.5 * k});
    f["ppm"] = json!([-9.5, -10.4]);
    f["peak"] = json!([-8.0, -9.0]);
    f
}

/// The built-in feeds: `both` (two stations, live frames, the chain bypassed as in M1), `chain5`, `chain3`, `proc`
/// (the processing view's: five bands, the AGC gate and the clipper lit),
/// `alert` (a critical alert on the second station), `untagged`, `longtitle`, `gapless`, `disconnected`.
pub fn demo(name: &str) -> Option<Value> {
    let office_q = vec![
        track(
            23,
            "January Man (Ambient Version) (Demo v1)",
            "Second Artist",
            "Spectrotemporal (Pre-Release)",
            true,
            23,
            "rotation",
        ),
        block("id", "Top of hour ID", 41),
        track(
            10,
            "From Silence (RC1)",
            "Demo Artists",
            "Drift, Volume 1",
            true,
            960,
            "rotation",
        ),
        track(
            12,
            "Longing (Demo v1)",
            "Demo Artists",
            "Drift, Volume 1",
            true,
            1770,
            "liner",
        ),
        track(
            18,
            "The Canal (Demo v1)",
            "Second Artist",
            "In Progress",
            true,
            2760,
            "rotation",
        ),
        track(
            20,
            "Interlude",
            "Second Artist",
            "In Progress",
            false,
            2960,
            "manual",
        ),
    ];
    let ita_q = vec![
        track(
            31,
            "Late Show Walk-On",
            "Ensemble",
            "the evening show",
            true,
            200,
            "rotation",
        ),
        track(
            32,
            "Cold Open Sting",
            "Demo Artists",
            "the evening show",
            true,
            400,
            "rotation",
        ),
        track(
            33,
            "Credits Bed",
            "Demo Artists",
            "the evening show",
            true,
            700,
            "rotation",
        ),
    ];
    let warn = json!({"id": 4, "code": "restarted", "severity": "warning", "message": "the station restarted; 26.0 s without sound", "raised_at": iso_ms(at_ms() - 480_000)});
    let crit = json!({"id": 9, "code": "silence", "severity": "critical", "message": "no sound for 20 s on the program output", "raised_at": iso_ms(at_ms() - 35_000)});
    let mut office = station(
        "office",
        "Office",
        "But I Do Want To Know You (RC1)",
        "Demo Artists",
        "Drift, Volume 1 (Pre-Release)",
        true,
        38,
        23,
        251,
        office_q.clone(),
        vec![warn],
        false,
        frame_live(),
    );
    let mut eve = station(
        "eve",
        "Eve",
        "the evening show: Cold Open",
        "Demo Artists",
        "the evening show",
        true,
        222,
        200,
        72,
        ita_q,
        vec![],
        false,
        frame_live(),
    );
    let mut view = "single";
    match name {
        "both" => {}
        "chain5" | "chain3" => {
            let (b, gr) = if name == "chain5" {
                (5, [-3.0, -6.0, -9.0, -6.0, -3.0])
            } else {
                (3, [-4.0, -8.0, -2.0, 0.0, 0.0])
            };
            let mut fr = frame_chain(gr);
            if b == 3 {
                fr["band_count"] = json!(3);
            }
            office["frame"] = fr;
            office["chain"] = chain_cfg(b);
        }
        "proc" => {
            // the processing view: five bands, a history that leaves the bars part way down with ticks above
            office["frame"] = Value::Null;
            office["frames"] = json!((0..24)
                .map(|i| frame_proc(if i < 8 { 1.6 } else { 1.0 }))
                .collect::<Vec<_>>());
            office["chain"] = chain_cfg(5);
            view = "processing";
        }
        "alert" => {
            eve["snapshot"]["alerts"] = json!([crit]);
        }
        "untagged" => {
            office["snapshot"]["now"]["art_url"] = Value::Null;
            office["snapshot"]["now"]["album"] = json!("");
            office["snapshot"]["now"]["category"] = json!("");
        }
        "longtitle" => {
            let long = "Symphony of the Unquiet Hours in Seven Movements, Op. 12 (Unabridged Demo)";
            office["snapshot"]["now"]["title"] = json!(long);
            eve["snapshot"]["now"]["title"] = json!(long);
            office["snapshot"]["now"]["artist"] =
                json!("The Ensemble of Rather Long Artist Names and Friends");
            eve["snapshot"]["now"]["artist"] =
                json!("The Ensemble of Rather Long Artist Names and Friends");
        }
        "gapless" => {
            office["snapshot"]["now"]["continuity_next"] = json!(true);
        }
        "disconnected" => {
            eve["connected"] = json!(false);
        }
        _ => return None,
    }
    let _ = &mut view;
    Some(json!({"at": AT, "tz_min": -240, "view": view, "stations": [office, eve]}))
}
