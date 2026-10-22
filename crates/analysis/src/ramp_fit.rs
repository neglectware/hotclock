//! The ramp fit (milestone M1.3, "Plexamp ramp fit"): Plex's saved `startRamp`/`endRamp`
//! strings for the calibration tracks that exist in the local test sets, against Hotclock's own
//! computation from the same audio. Test data lives outside the repository; nothing here is
//! reachable without it.

use crate::blocks::{integrated_from_blocks, BlockAnalyzer, Blocks};
use crate::census::AUDIO_EXTENSIONS;
use crate::decode::decode_file;
use crate::ramp::*;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct PlexTrack {
    pub rating_key: String,
    pub file: String,
    pub title: String,
    pub duration_ms: f64,
    pub gain: f64,
    pub album_gain: f64,
    pub loudness: f64,
    pub start_ramp: Ramp,
    pub end_ramp: Ramp,
}

fn f(v: &Value, k: &str) -> Option<f64> {
    match v.get(k)? {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// `tracks-evemusic-2026-09-29.json`: rating key to the full metadata response.
pub fn load_plex_tracks(json: &Path) -> Vec<PlexTrack> {
    let text = std::fs::read_to_string(json).unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let mut out = vec![];
    if let Some(map) = v.as_object() {
        for (rk, resp) in map {
            let m = &resp["MediaContainer"]["Metadata"][0];
            let part = &m["Media"][0]["Part"][0];
            let s = &part["Stream"][0];
            let (Some(gain), Some(loud)) = (f(s, "gain"), f(s, "loudness")) else {
                continue;
            };
            out.push(PlexTrack {
                rating_key: rk.clone(),
                file: part["file"].as_str().unwrap_or("").to_string(),
                title: m["title"].as_str().unwrap_or("").to_string(),
                duration_ms: f(part, "duration")
                    .or_else(|| f(m, "duration"))
                    .unwrap_or(0.0),
                gain,
                album_gain: f(s, "albumGain").unwrap_or(gain),
                loudness: loud,
                start_ramp: parse_ramp(s["startRamp"].as_str().unwrap_or("")),
                end_ramp: parse_ramp(s["endRamp"].as_str().unwrap_or("")),
            });
        }
    }
    out.sort_by(|a, b| a.rating_key.cmp(&b.rating_key));
    out
}

pub fn norm_title(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    // Strip a leading track number such as `02-08 ` or `06 `.
    let mut chars = stem.chars().peekable();
    let mut consumed = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() || c == '-' || c == '.' {
            consumed.push(c);
            chars.next();
        } else {
            break;
        }
    }
    let rest: String = if consumed.is_empty() {
        stem.to_string()
    } else {
        chars.collect::<String>().trim_start().to_string()
    };
    rest.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        {
            out.push(p);
        }
    }
}

/// A local file for each Plex track that has one: the same normalized title (track number and
/// punctuation ignored) and a duration within 1.5 s. Each of the three test sets is searched on
/// its own and never mixed with the others; a track matching in two sets is ambiguous and
/// skipped.
pub fn match_local(
    tracks: &[PlexTrack],
    testdata: &Path,
    durations: &dyn Fn(&Path) -> f64,
) -> Vec<(PlexTrack, PathBuf, &'static str)> {
    let mut out = vec![];
    for t in tracks {
        let key = norm_title(&t.file);
        let mut found: Vec<(PathBuf, &'static str)> = vec![];
        for set in ["office", "preshow", "elevator"] {
            let mut files = vec![];
            walk(&testdata.join(set), &mut files);
            for p in files {
                if norm_title(p.file_name().and_then(|n| n.to_str()).unwrap_or("")) == key
                    && (durations(&p) * 1000.0 - t.duration_ms).abs() < 1500.0
                {
                    found.push((p, set));
                }
            }
        }
        if found.len() == 1 {
            let (p, s) = found.remove(0);
            out.push((t.clone(), p, s));
        }
    }
    out
}

/// A decoded track ready for evaluation.
pub struct Prepared {
    pub track: PlexTrack,
    pub path: PathBuf,
    pub set: &'static str,
    pub blocks: Blocks,
    pub l_int: f64,
    pub duration_s: f64,
}

pub fn prepare(track: PlexTrack, path: PathBuf, set: &'static str) -> Option<Prepared> {
    let mut ba: Option<BlockAnalyzer> = None;
    let info = decode_file(&path, |s, ch, rate| {
        ba.get_or_insert_with(|| BlockAnalyzer::new(rate, ch))
            .push(s);
    })
    .ok()?;
    let blocks = ba?.finish();
    let l_int = integrated_from_blocks(&blocks.kpow[..blocks.full_blocks()])?;
    Some(Prepared {
        track,
        path,
        set,
        blocks,
        l_int,
        duration_s: info.frames as f64 / f64::from(info.sample_rate),
    })
}

pub fn reference_of(p: &Prepared, r: Reference) -> f64 {
    match r {
        Reference::Integrated => p.l_int,
        Reference::Fixed(v) => v,
    }
}

/// Threshold on the ramp scale for this track: `-16 - g` (g = album gain).
pub fn threshold(p: &Prepared) -> f64 {
    -16.0 - p.track.album_gain
}

/// Error of one variant's true crossing times against Plex's lookups at the threshold:
/// `(start_errors_s, end_errors_s)`, one entry per track that has both a Plex value and a crossing.
/// Plex's lookup at `T` is the interpolated value of its own string.
pub fn errors_at_threshold(
    preps: &[Prepared],
    spec: &RampSpec,
    use_plex_loudness: bool,
) -> (Vec<f64>, Vec<f64>) {
    let mut se = vec![];
    let mut ee = vec![];
    for p in preps {
        let t = threshold(p);
        let refl = if use_plex_loudness {
            p.track.loudness
        } else {
            reference_of(p, spec.reference)
        };
        let curve = level_curve(
            &p.blocks.kpow[..p.blocks.full_blocks()],
            spec,
            refl,
            0,
            usize::MAX,
        );
        let plex_s = lookup(&p.track.start_ramp, t);
        let plex_e = lookup(&p.track.end_ramp, t);
        se.push(first_reach(&curve, t).unwrap_or(0.0) - plex_s);
        ee.push(last_reach_from_end(&curve, p.duration_s, t).unwrap_or(0.0) - plex_e);
    }
    (se, ee)
}

/// Error at every Plex point: my crossing time at that point's dB minus the point's seconds.
pub fn errors_at_points(
    preps: &[Prepared],
    spec: &RampSpec,
    use_plex_loudness: bool,
) -> (Vec<f64>, Vec<f64>) {
    let mut se = vec![];
    let mut ee = vec![];
    for p in preps {
        let refl = if use_plex_loudness {
            p.track.loudness
        } else {
            reference_of(p, spec.reference)
        };
        let curve = level_curve(
            &p.blocks.kpow[..p.blocks.full_blocks()],
            spec,
            refl,
            0,
            usize::MAX,
        );
        for &(db, s) in p.track.start_ramp.iter().skip(1) {
            se.push(first_reach(&curve, db).unwrap_or(f64::NAN) - s);
        }
        for &(db, s) in p.track.end_ramp.iter().skip(1) {
            ee.push(last_reach_from_end(&curve, p.duration_s, db).unwrap_or(f64::NAN) - s);
        }
    }
    (se, ee)
}

pub fn rms(v: &[f64]) -> f64 {
    let v: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
    (v.iter().map(|x| x * x).sum::<f64>() / v.len().max(1) as f64).sqrt()
}

/// The `q` quantile (0..1) of the absolute values.
pub fn abs_quantile(v: &[f64], q: f64) -> f64 {
    let mut a: Vec<f64> = v
        .iter()
        .copied()
        .filter(|x| x.is_finite())
        .map(f64::abs)
        .collect();
    a.sort_by(|x, y| x.partial_cmp(y).unwrap());
    if a.is_empty() {
        return f64::NAN;
    }
    a[((a.len() - 1) as f64 * q).round() as usize]
}

pub fn median(v: &[f64]) -> f64 {
    let mut a: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
    a.sort_by(|x, y| x.partial_cmp(y).unwrap());
    if a.is_empty() {
        f64::NAN
    } else {
        a[a.len() / 2]
    }
}

// ---------------------------------------------------------------------------------------------
// The report

/// A ramp thinned to points at least `step_db` apart (what a sparser writer such as Plex's
/// would give); the last point is always kept.
pub fn thin(r: &Ramp, step_db: f64) -> Ramp {
    let mut out: Ramp = vec![];
    for (i, &(db, t)) in r.iter().enumerate() {
        let last = i + 1 == r.len();
        if out.is_empty() || db >= out.last().map_or(0.0, |p| p.0) + step_db || last {
            out.push((db, t));
        }
    }
    out
}

/// Error statistics of one set of crossing-time errors (seconds), against the 0.25 s target.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub n: usize,
    pub within: usize,
    pub median: f64,
    pub rms: f64,
    pub p90: f64,
    pub p95: f64,
    pub max: f64,
}

pub const TARGET_S: f64 = 0.25;

pub fn stats(v: &[f64]) -> Stats {
    let f: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
    Stats {
        n: f.len(),
        within: f.iter().filter(|x| x.abs() <= TARGET_S + 1e-9).count(),
        median: median(&f),
        rms: rms(&f),
        p90: abs_quantile(&f, 0.9),
        p95: abs_quantile(&f, 0.95),
        max: abs_quantile(&f, 1.0),
    }
}

/// One variant of the computation, and its errors at `T = -16 - g` against Plex's own lookups.
#[derive(Debug, Clone)]
pub struct VariantRow {
    pub label: String,
    pub start: Stats,
    pub end: Stats,
}

/// A variant: window (10 ms blocks), reference offset in dB (added to `L_int`), whether the
/// reference is Plex's own `loudness`, the end shift, the grid thinning (0 = every record), and
/// whether the meter is a running one (silence-padded, end-stamped both ways).
#[derive(Debug, Clone, Copy)]
pub struct Variant {
    pub window: usize,
    pub ref_offset_db: f64,
    pub plex_loudness: bool,
    pub end_shift_s: f64,
    pub thin_db: f64,
    pub running: bool,
}

impl Variant {
    pub const CHOSEN: Variant = Variant {
        window: 10,
        ref_offset_db: 0.0,
        plex_loudness: false,
        end_shift_s: END_SHIFT_S,
        thin_db: 0.0,
        running: false,
    };
}

/// My two ramps for a track under a variant.
pub fn ramps_for(p: &Prepared, v: &Variant) -> (Ramp, Ramp) {
    let kpow = &p.blocks.kpow[..p.blocks.full_blocks()];
    let refl = if v.plex_loudness {
        p.track.loudness
    } else {
        p.l_int
    } + v.ref_offset_db;
    let base = RampSpec {
        window_blocks: v.window,
        hop_blocks: 10,
        stamp: Stamp::Start,
        reference: Reference::Fixed(0.0),
        pad: v.running,
    };
    let cs = level_curve(kpow, &base, refl, 0, usize::MAX);
    let ce = level_curve(
        kpow,
        &RampSpec {
            stamp: Stamp::End,
            ..base
        },
        refl,
        0,
        usize::MAX,
    );
    let mut rs = build_start_ramp(&cs);
    let mut re = build_end_ramp(&ce, p.duration_s, v.end_shift_s);
    if v.thin_db > 0.0 {
        rs = thin(&rs, v.thin_db);
        re = thin(&re, v.thin_db);
    }
    (rs, re)
}

/// Errors of a variant at the threshold, per track: `(start, end)`.
pub fn threshold_errors(preps: &[Prepared], v: &Variant) -> (Vec<f64>, Vec<f64>) {
    let mut se = vec![];
    let mut ee = vec![];
    for p in preps {
        let t = threshold(p);
        let (rs, re) = ramps_for(p, v);
        se.push(lookup(&rs, t) - lookup(&p.track.start_ramp, t));
        ee.push(lookup(&re, t) - lookup(&p.track.end_ramp, t));
    }
    (se, ee)
}

/// One pair of `pairs.csv` replayed through Hotclock's ramps.
#[derive(Debug, Clone)]
pub struct PairRow {
    pub logged_ms: f64,
    pub mine_ms: f64,
}

/// Minimal CSV: quoted fields with `""` escapes.
pub fn csv_line(l: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut q = false;
    let mut it = l.chars().peekable();
    while let Some(c) = it.next() {
        match (c, q) {
            ('"', true) if it.peek() == Some(&'"') => {
                cur.push('"');
                it.next();
            }
            ('"', _) => q = !q,
            (',', false) => out.push(std::mem::take(&mut cur)),
            (c, _) => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Replay every logged pair whose two tracks are both present locally: the overlap
/// `min(15, t_A + t_B)` from Hotclock's ramps against the logged overlap.
pub fn replay_pairs(preps: &[Prepared], pairs_csv: &Path, v: &Variant) -> (usize, Vec<PairRow>) {
    let text = std::fs::read_to_string(pairs_csv).unwrap_or_default();
    let mut lines = text.lines();
    let header = csv_line(lines.next().unwrap_or(""));
    let col = |n: &str| header.iter().position(|h| h == n);
    let (Some(ak), Some(bk), Some(ag), Some(bg), Some(lo)) = (
        col("A_ratingKey"),
        col("B_ratingKey"),
        col("A_albumGain"),
        col("B_albumGain"),
        col("logged_overlap_ms"),
    ) else {
        return (0, vec![]);
    };
    let by_key: std::collections::HashMap<&str, (Ramp, Ramp)> = preps
        .iter()
        .map(|p| (p.track.rating_key.as_str(), ramps_for(p, v)))
        .collect();
    let mut total = 0;
    let mut rows = vec![];
    for l in lines {
        let f = csv_line(l);
        if f.len() <= lo.max(bg) {
            continue;
        }
        total += 1;
        let (Some((_, a_end)), Some((b_start, _))) =
            (by_key.get(f[ak].as_str()), by_key.get(f[bk].as_str()))
        else {
            continue;
        };
        let (Ok(ga), Ok(gb), Ok(logged)) = (
            f[ag].parse::<f64>(),
            f[bg].parse::<f64>(),
            f[lo].parse::<f64>(),
        ) else {
            continue;
        };
        let ta = lookup(a_end, -16.0 - ga);
        let tb = lookup(b_start, -16.0 - gb);
        rows.push(PairRow {
            logged_ms: logged,
            mine_ms: (ta + tb).min(15.0) * 1000.0,
        });
    }
    (total, rows)
}

#[derive(Debug, Clone)]
pub struct TrackRow {
    pub set: &'static str,
    pub title: String,
    pub l_int: f64,
    pub plex_loudness: f64,
    pub threshold: f64,
    pub start_err: f64,
    pub end_err: f64,
}

pub struct Report {
    pub plex_tracks: usize,
    pub matched: usize,
    pub by_set: Vec<(&'static str, usize)>,
    pub max_loudness_diff: f64,
    pub tracks: Vec<TrackRow>,
    pub variants: Vec<VariantRow>,
    pub chosen_start: Stats,
    pub chosen_end: Stats,
    pub chosen_both: Stats,
    pub pairs_total: usize,
    pub pairs: Vec<PairRow>,
}

fn label(v: &Variant) -> String {
    format!(
        "W {:>4} ms{}{}{}{}",
        v.window * 10,
        if v.running { ", running (padded)" } else { "" },
        if v.plex_loudness {
            ", 0 dB = Plex loudness"
        } else if v.ref_offset_db != 0.0 {
            ""
        } else {
            ", 0 dB = own L_int"
        },
        if v.ref_offset_db != 0.0 {
            format!(", ref {:+.1} dB", v.ref_offset_db)
        } else {
            String::new()
        },
        if v.thin_db > 0.0 {
            format!(", grid thinned to {} dB", v.thin_db)
        } else {
            String::new()
        },
    ) + &if (v.end_shift_s - END_SHIFT_S).abs() > 1e-9 {
        format!(", end shift {} s", v.end_shift_s)
    } else {
        String::new()
    }
}

/// Run the whole fit against `HOTCLOCK_TESTDATA` (`root`). `None` when the calibration data or
/// no matching local file exists.
pub fn run(root: &Path) -> Option<Report> {
    let calib = root.join("plexamp-calibration");
    let tracks = load_plex_tracks(&calib.join("tracks-evemusic-2026-09-29.json"));
    if tracks.is_empty() {
        return None;
    }
    let duration = |p: &Path| {
        let mut n = 0u64;
        let mut rate = 0u32;
        let _ = decode_file(p, |s, ch, r| {
            rate = r;
            n += (s.len() / ch) as u64;
        });
        n as f64 / f64::from(rate.max(1))
    };
    let matched = match_local(&tracks, root, &duration);
    let preps: Vec<Prepared> = matched
        .into_iter()
        .filter_map(|(t, p, s)| prepare(t, p, s))
        .collect();
    if preps.is_empty() {
        return None;
    }
    let mut by_set = vec![];
    for set in ["office", "preshow", "elevator"] {
        by_set.push((set, preps.iter().filter(|p| p.set == set).count()));
    }
    let base = Variant::CHOSEN;
    let mut vs: Vec<Variant> = vec![];
    for w in [5usize, 10, 15, 20, 30, 40, 100, 300] {
        vs.push(Variant { window: w, ..base });
    }
    vs.push(Variant {
        plex_loudness: true,
        ..base
    });
    vs.push(Variant {
        ref_offset_db: -1.0,
        ..base
    });
    vs.push(Variant {
        ref_offset_db: 1.0,
        ..base
    });
    vs.push(Variant {
        end_shift_s: 0.0,
        ..base
    });
    vs.push(Variant {
        end_shift_s: 0.2,
        ..base
    });
    vs.push(Variant {
        thin_db: 1.0,
        ..base
    });
    vs.push(Variant {
        thin_db: 3.0,
        ..base
    });
    vs.push(Variant {
        window: 40,
        running: true,
        ..base
    });
    vs.push(Variant {
        window: 10,
        running: true,
        ..base
    });
    let variants: Vec<VariantRow> = vs
        .iter()
        .map(|v| {
            let (s, e) = threshold_errors(&preps, v);
            VariantRow {
                label: label(v),
                start: stats(&s),
                end: stats(&e),
            }
        })
        .collect();
    let (s, e) = threshold_errors(&preps, &base);
    let mut rows = vec![];
    for (i, p) in preps.iter().enumerate() {
        rows.push(TrackRow {
            set: p.set,
            title: p.track.title.clone(),
            l_int: p.l_int,
            plex_loudness: p.track.loudness,
            threshold: threshold(p),
            start_err: s[i],
            end_err: e[i],
        });
    }
    let both: Vec<f64> = s.iter().chain(e.iter()).copied().collect();
    let (pairs_total, pairs) = replay_pairs(&preps, &calib.join("pairs.csv"), &base);
    Some(Report {
        plex_tracks: tracks.len(),
        matched: preps.len(),
        by_set,
        max_loudness_diff: preps
            .iter()
            .map(|p| (p.l_int - p.track.loudness).abs())
            .fold(0.0, f64::max),
        tracks: rows,
        variants,
        chosen_start: stats(&s),
        chosen_end: stats(&e),
        chosen_both: stats(&both),
        pairs_total,
        pairs,
    })
}

fn fmt_stats(s: &Stats) -> String {
    format!(
        "n {:>2} within {:>2}  median {:+.3}  rms {:.3}  p90 {:.3}  p95 {:.3}  max {:.3}",
        s.n, s.within, s.median, s.rms, s.p90, s.p95, s.max
    )
}

pub fn format_report(r: &Report) -> String {
    let mut o = String::new();
    o.push_str(&format!(
        "Plex tracks with saved ramps: {}. Matched to local files: {} ({}). Largest |own L_int - Plex loudness|: {:.2} dB.\n\n",
        r.plex_tracks,
        r.matched,
        r.by_set.iter().map(|(s, n)| format!("{s} {n}")).collect::<Vec<_>>().join(", "),
        r.max_loudness_diff
    ));
    o.push_str("Variants (crossing time at T = -16 - g of our ramp string vs Plex's own lookup, seconds):\n");
    for v in &r.variants {
        o.push_str(&format!(
            "  {:<58}\n      start: {}\n      end:   {}\n",
            v.label,
            fmt_stats(&v.start),
            fmt_stats(&v.end)
        ));
    }
    o.push_str(&format!(
        "\nChosen variant, target |error| <= {TARGET_S} s:\n  start: {}\n  end:   {}\n  both:  {}  ({:.1} percent within)\n",
        fmt_stats(&r.chosen_start),
        fmt_stats(&r.chosen_end),
        fmt_stats(&r.chosen_both),
        100.0 * r.chosen_both.within as f64 / r.chosen_both.n.max(1) as f64
    ));
    o.push_str("\nPer track (set, title, own L_int, Plex loudness, T, start error, end error):\n");
    for t in &r.tracks {
        o.push_str(&format!(
            "  {:<8} {:<30} {:7.2} {:7.2} {:7.2}  {:+.3} {:+.3}\n",
            t.set, t.title, t.l_int, t.plex_loudness, t.threshold, t.start_err, t.end_err
        ));
    }
    let errs: Vec<f64> = r
        .pairs
        .iter()
        .map(|p| (p.mine_ms - p.logged_ms) / 1000.0)
        .collect();
    let st = stats(&errs);
    o.push_str(&format!(
        "\nPairs replay (pairs.csv: {} logged; {} with both tracks local): overlap error (own - logged), seconds:\n  {}\n",
        r.pairs_total,
        r.pairs.len(),
        fmt_stats(&st)
    ));
    o
}
