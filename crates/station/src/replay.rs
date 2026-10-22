//! The Plexamp replay (M1.4a, `plexamp` preset): the logged transitions of the `evemusic` player
//! (`plexamp-calibration/pairs.csv`, 801 of them) replayed through Hotclock's own ramps and the
//! planner's `ramp_overlap` rule, for every pair whose two tracks both exist in the local test sets.
//! The logged overlap is what Plexamp actually did; the replayed one is what Hotclock's `plexamp`
//! preset would do with the same gains, and the difference is the error of the whole chain:
//! ramp computation, threshold rule and the 15 s cap.

use crate::item::{Item, ItemBuild};
use crate::plan::{plan, PlacedView, PlanInput, TransitionKind};
use analysis::ramp_fit::{csv_line, load_plex_tracks, match_local};
use common::transition::{resolve, Chain, PresetBook};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ReplayRow {
    pub a_title: String,
    pub b_title: String,
    pub set: &'static str,
    pub logged_ms: f64,
    pub mine_ms: f64,
    /// Plexamp's own model column of the log (the fit of M1.3), for reference.
    pub model_ms: Option<f64>,
    pub capped: bool,
}

impl ReplayRow {
    pub fn error_ms(&self) -> f64 {
        self.mine_ms - self.logged_ms
    }
}

#[derive(Debug, Clone, Default)]
pub struct ReplayReport {
    pub pairs_total: usize,
    pub tracks_local: usize,
    pub rows: Vec<ReplayRow>,
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let i = ((sorted.len() as f64 * p).ceil() as usize).clamp(1, sorted.len());
    sorted[i - 1]
}

impl ReplayReport {
    /// `(median, p90, max)` of the absolute error, milliseconds.
    pub fn abs_error_stats(&self) -> Option<(f64, f64, f64)> {
        if self.rows.is_empty() {
            return None;
        }
        let mut e: Vec<f64> = self.rows.iter().map(|r| r.error_ms().abs()).collect();
        e.sort_by(f64::total_cmp);
        Some((pct(&e, 0.5), pct(&e, 0.9), *e.last().unwrap()))
    }

    pub fn format(&self) -> String {
        let mut s = format!(
            "Plexamp replay: {} logged pairs; {} of them have both tracks in the local test sets\n",
            self.pairs_total,
            self.rows.len()
        );
        for r in &self.rows {
            s.push_str(&format!(
                "  [{}] {} -> {}: logged {:.0} ms, ours {:.0} ms (error {:+.0} ms){}\n",
                r.set,
                r.a_title,
                r.b_title,
                r.logged_ms,
                r.mine_ms,
                r.error_ms(),
                if r.capped { ", capped at 15 s" } else { "" }
            ));
        }
        match self.abs_error_stats() {
            Some((m, p90, max)) => s.push_str(&format!(
                "  overlap error |own - logged|: median {m:.0} ms, p90 {p90:.0} ms, max {max:.0} ms over {} pairs (target: median under 250 ms)\n",
                self.rows.len()
            )),
            None => s.push_str("  no pair has both tracks locally\n"),
        }
        s
    }
}

/// Replay every logged pair whose tracks are both local. `root` is the test data directory
/// (`HOTCLOCK_TESTDATA`). `None` when the calibration data is missing.
pub fn plexamp_replay(root: &Path, rate: u32) -> Option<ReplayReport> {
    let calib = root.join("plexamp-calibration");
    let tracks = load_plex_tracks(&calib.join("tracks-evemusic-2026-09-29.json"));
    if tracks.is_empty() {
        return None;
    }
    let duration = |p: &Path| {
        let mut n = 0u64;
        let mut r = 0u32;
        let _ = analysis::decode::decode_file(p, |s, ch, sr| {
            r = sr;
            n += (s.len() / ch) as u64;
        });
        n as f64 / f64::from(r.max(1))
    };
    let matched = match_local(&tracks, root, &duration);
    let local: HashMap<String, (String, PathBuf, &'static str)> = matched
        .into_iter()
        .map(|(t, p, s)| (t.rating_key.clone(), (t.title.clone(), p, s)))
        .collect();
    let text = std::fs::read_to_string(calib.join("pairs.csv")).ok()?;
    let mut lines = text.lines();
    let header = csv_line(lines.next()?);
    let col = |n: &str| header.iter().position(|h| h == n);
    let (ak, bk, ag, bg, lo) = (
        col("A_ratingKey")?,
        col("B_ratingKey")?,
        col("A_albumGain")?,
        col("B_albumGain")?,
        col("logged_overlap_ms")?,
    );
    let model = col("model_overlap_ms");
    let book = PresetBook::builtin();
    let p = resolve(
        &book,
        &Chain {
            station_preset: "plexamp",
            ..Chain::default()
        },
    )
    .ok()?;
    let mut items: HashMap<(String, i64), Item> = HashMap::new();
    let mut report = ReplayReport {
        tracks_local: local.len(),
        ..ReplayReport::default()
    };
    for l in lines {
        let f = csv_line(l);
        // Every column read below must exist (a short or malformed row used to panic when the
        // rating-key columns came after the gain and overlap columns).
        if f.len() <= ak.max(bk).max(ag).max(bg).max(lo) {
            continue;
        }
        report.pairs_total += 1;
        let (Some(a), Some(b)) = (local.get(&f[ak]), local.get(&f[bk])) else {
            continue;
        };
        let (Ok(ga), Ok(gb), Ok(logged)) = (
            f[ag].parse::<f64>(),
            f[bg].parse::<f64>(),
            f[lo].parse::<f64>(),
        ) else {
            continue;
        };
        let mut load = |key: &str, t: &(String, PathBuf, &'static str), g: f64| -> Option<Item> {
            let k = (key.to_string(), (g * 1000.0).round() as i64);
            if let Some(i) = items.get(&k) {
                return Some(i.clone());
            }
            let it = Item::analyze(
                &t.1,
                &ItemBuild {
                    name: t.0.clone(),
                    category: "music".into(),
                    params: serde_json::Value::Null,
                    album_gain_db: Some(g),
                    ..ItemBuild::default()
                },
            )
            .ok()?;
            items.insert(k, it.clone());
            Some(it)
        };
        let (Some(ia), Some(ib)) = (load(&f[ak], a, ga), load(&f[bk], b, gb)) else {
            continue;
        };
        let view = PlacedView {
            item: &ia,
            start_frame: 0,
            bound_in_ms: 0,
            bound_out_ms: ia.file_ms,
            bound_out_is_file_end: true,
            frames: None,
        };
        let pl = plan(&PlanInput {
            rate,
            now: 0,
            a: &view,
            b: &ib,
            pa: &p,
            pb: &p,
            kind: TransitionKind::Auto,
            truncated_fade_s: crate::plan::TRUNCATED_FADE_S,
        });
        let Some(ramp) = pl.ramp else { continue };
        report.rows.push(ReplayRow {
            a_title: a.0.clone(),
            b_title: b.0.clone(),
            set: a.2,
            logged_ms: logged,
            mine_ms: ramp.overlap_s * 1000.0,
            model_ms: model.and_then(|m| f.get(m)).and_then(|v| v.parse().ok()),
            capped: ramp.capped,
        });
    }
    Some(report)
}
