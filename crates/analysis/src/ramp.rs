//! Plex-compatible loudness ramps (architecture 2.6, 3.2): the `startRamp` and `endRamp` strings
//! (`dB seconds;` points, ascending dB from -51.00), their threshold lookup, and the computation
//! of both from the per-block K-weighted power of the analysis pass.
//!
//! The ramp's level is a **K-weighted loudness relative to a reference**; which window, which
//! time stamp and which reference reproduce Plex's own strings was fitted against the server's
//! saved ramps for the calibration tracks present locally (`ramp_fit`, results in BUILDLOG
//! M1.3). The winner, [`RampSpec::PLEX`]: 100 ms blocks (window = hop = 100 ms, aligned to the
//! start of the audio), 0 dB = the item's integrated loudness, the start ramp stamped at the
//! block's start, the end ramp at the block's end less [`END_SHIFT_S`].

/// The floor point of every ramp.
pub const FLOOR_DB: f64 = -51.0;

/// Fitted constant: Plex's end-ramp seconds are this much smaller than the distance from the
/// end of a 100 ms block to the end of the audio (the fit's median residual was +0.19 s before
/// it; 0.1 s was chosen as the value that keeps both the FLAC and the AAC calibration tracks
/// inside the 0.25 s target).
pub const END_SHIFT_S: f64 = 0.1;

/// Which instant of a sliding window carries its level in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stamp {
    Start,
    Centre,
    End,
}

/// What 0 dB means.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reference {
    /// The item's own gated integrated loudness (`L_int`, or `L_W` inside a cue window).
    Integrated,
    /// A fixed LUFS value.
    Fixed(f64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RampSpec {
    /// Window length in 10 ms blocks.
    pub window_blocks: usize,
    /// Hop in 10 ms blocks.
    pub hop_blocks: usize,
    pub stamp: Stamp,
    pub reference: Reference,
    /// The window is filled with silence before the start of the range (a running loudness
    /// query after each hop, as a streaming meter would give): the first windows are partial.
    /// Time stamps are then always the window end.
    pub pad: bool,
}

impl RampSpec {
    /// The variant the fit selected (see BUILDLOG M1.3 for the table it won). The end ramp uses
    /// the same spec with [`Stamp::End`].
    pub const PLEX: RampSpec = RampSpec {
        window_blocks: 10,
        hop_blocks: 10,
        stamp: Stamp::Start,
        reference: Reference::Integrated,
        pad: false,
    };
}

/// A level curve: `(time_s, level_db)` at each window stamp.
pub type Curve = Vec<(f64, f64)>;

/// The level curve of the per-block K power `kpow` (complete 10 ms blocks) under `spec`, relative to `reference_lufs`. `first`/`last` limit
/// the blocks used (the cue window, as 10 ms block indices; `last` exclusive); times are
/// measured from the start of that range.
pub fn level_curve(
    kpow: &[f64],
    spec: &RampSpec,
    reference_lufs: f64,
    first: usize,
    last: usize,
) -> Curve {
    let last = last.min(kpow.len());
    if last < first + spec.window_blocks || spec.window_blocks == 0 {
        return vec![];
    }
    let w = spec.window_blocks;
    let mut out = vec![];
    if spec.pad {
        let mut end = spec.hop_blocks;
        while first + end <= last {
            let s = (first + end).saturating_sub(w).max(first);
            let sum: f64 = kpow[s..first + end].iter().sum();
            out.push((
                end as f64 * 0.01,
                crate::blocks::lufs(sum / w as f64) - reference_lufs,
            ));
            end += spec.hop_blocks;
        }
        return out;
    }
    let mut i = 0;
    while first + i * spec.hop_blocks + w <= last {
        let s = first + i * spec.hop_blocks;
        let sum: f64 = kpow[s..s + w].iter().sum();
        let level = crate::blocks::lufs(sum / w as f64) - reference_lufs;
        let start_s = (i * spec.hop_blocks) as f64 * 0.01;
        let len_s = w as f64 * 0.01;
        let t = match spec.stamp {
            Stamp::Start => start_s,
            Stamp::Centre => start_s + len_s / 2.0,
            Stamp::End => start_s + len_s,
        };
        out.push((t, level));
        i += 1;
    }
    out
}

/// Seconds from the range start until the level first reaches `db`; `None` if it never does.
pub fn first_reach(curve: &Curve, db: f64) -> Option<f64> {
    curve.iter().find(|(_, l)| *l >= db).map(|(t, _)| *t)
}

/// Seconds before `duration_s` after which the level never again reaches `db` (measured from the
/// stamp of the last window that does); `None` if it never does.
pub fn last_reach_from_end(curve: &Curve, duration_s: f64, db: f64) -> Option<f64> {
    curve
        .iter()
        .rev()
        .find(|(_, l)| *l >= db)
        .map(|(t, _)| (duration_s - t).max(0.0))
}

// ---- Plex strings ----

pub type Ramp = Vec<(f64, f64)>;

/// Parse `dB seconds;dB seconds;` (a trailing `;` is fine). Malformed pieces are skipped.
pub fn parse_ramp(s: &str) -> Ramp {
    s.split(';')
        .filter_map(|piece| {
            let mut it = piece.split_whitespace();
            let db: f64 = it.next()?.parse().ok()?;
            let t: f64 = it.next()?.parse().ok()?;
            Some((db, t))
        })
        .collect()
}

pub fn format_ramp(r: &Ramp) -> String {
    let mut s = String::new();
    for (db, t) in r {
        s.push_str(&format!("{db:.2} {t:.2};"));
    }
    s
}

/// Plex's threshold lookup: walk to the first point with dB at or above `t_db` and interpolate
/// linearly in time from the previous point; `t_db` at or below the first point gives the first
/// point's time; above every point, or an empty ramp, gives 0.
pub fn lookup(ramp: &Ramp, t_db: f64) -> f64 {
    let Some(first) = ramp.first() else {
        return 0.0;
    };
    if t_db <= first.0 {
        return first.1;
    }
    for w in ramp.windows(2) {
        let (d0, s0) = w[0];
        let (d1, s1) = w[1];
        if d1 >= t_db {
            if d1 == d0 {
                return s1;
            }
            return s0 + (s1 - s0) * (t_db - d0) / (d1 - d0);
        }
    }
    0.0
}

// ---- building the strings ----

/// The points of a ramp from the record curve of a walk over the levels: each record is a
/// level above every earlier one, `(level_db, seconds)`. `first_time` is the time of the floor
/// sentinel `(-51.00, first_time)` that starts every ramp.
///
/// Plex writes 4 to 14 points that approximate the curve; Hotclock writes **every record**
/// and, where the level plateaus (a time gap of more than 0.05 s between two records), one
/// extra point 0.01 dB above the earlier record at the later record's time. Plex's lookup
/// interpolates linearly between points, so without that point it would answer with a time
/// somewhere in the middle of the plateau; with it, the lookup returns the time the level
/// really reaches the threshold. Two-decimal dB values that would coincide are dropped.
fn record_points(records: &[(f64, f64)], floor_time: f64) -> Ramp {
    let mut out: Ramp = vec![(FLOOR_DB, floor_time)];
    let mut prev: Option<(f64, f64)> = None;
    for &(db, t) in records {
        if db <= FLOOR_DB + 0.005 {
            continue;
        }
        if let Some((pdb, pt)) = prev {
            if (t - pt).abs() > 0.05 && db - pdb > 0.03 {
                out.push((pdb + 0.01, t));
            }
        }
        let last_db = out.last().map_or(f64::NEG_INFINITY, |p| p.0);
        if db > last_db + 0.005 {
            out.push((db, t));
        }
        prev = Some((db, t));
    }
    out
}

/// `startRamp` from a start-stamped level curve: seconds from the start until the level first
/// reaches each dB.
pub fn build_start_ramp(curve: &Curve) -> Ramp {
    let mut records = vec![];
    let mut m = f64::NEG_INFINITY;
    for &(t, l) in curve {
        if l > m {
            m = l;
            records.push((l, t));
        }
    }
    record_points(&records, 0.0)
}

/// `endRamp` from an end-stamped level curve: seconds before `duration_s` (less `shift_s`)
/// after which the level never again reaches each dB.
pub fn build_end_ramp(curve: &Curve, duration_s: f64, shift_s: f64) -> Ramp {
    let mut records = vec![];
    let mut m = f64::NEG_INFINITY;
    let mut first_x = 0.0;
    for (i, &(t, l)) in curve.iter().rev().enumerate() {
        let x = (duration_s - t - shift_s).max(0.0);
        if i == 0 {
            first_x = x;
        }
        if l > m {
            m = l;
            records.push((l, x));
        }
    }
    record_points(&records, first_x)
}

/// Both ramps of an item over the blocks `[first, last)` (10 ms block indices; the whole file is
/// `0..kpow.len()`), 0 dB at `reference_lufs`. `duration_s` is the length of that range.
pub fn compute_ramps(
    kpow: &[f64],
    reference_lufs: f64,
    first: usize,
    last: usize,
    duration_s: f64,
) -> (Ramp, Ramp) {
    let start = level_curve(kpow, &RampSpec::PLEX, reference_lufs, first, last);
    let end_spec = RampSpec {
        stamp: Stamp::End,
        ..RampSpec::PLEX
    };
    let end = level_curve(kpow, &end_spec, reference_lufs, first, last);
    (
        build_start_ramp(&start),
        build_end_ramp(&end, duration_s, END_SHIFT_S),
    )
}

/// The ramps inside a cue window, from the stored `K10` blocks (architecture 3.3): what the
/// `plexamp` preset reads when an item has a manual cue override. `reference_lufs` is the
/// window loudness `L_W` (or its estimate). 100 ms blocks start at the window's first block.
pub fn window_ramps(
    env: &crate::env::Envelopes,
    reference_lufs: f64,
    c_in_ms: i64,
    c_out_ms: i64,
) -> (Ramp, Ramp) {
    let kpow = env.kpow();
    let first = (c_in_ms.max(0) / 10) as usize;
    let last = ((c_out_ms.max(0) / 10) as usize).min(kpow.len());
    if last <= first {
        return (vec![(FLOOR_DB, 0.0)], vec![(FLOOR_DB, 0.0)]);
    }
    compute_ramps(
        &kpow,
        reference_lufs,
        first,
        last,
        (last - first) as f64 * 0.01,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLKA_END: &str = "-51.00 0.11;-37.22 3.31;-26.65 3.61;-21.12 3.71;-16.47 3.81;-13.40 3.91;-11.33 4.01;-8.43 4.11;-3.34 4.21;0.30 4.31;4.14 4.41;6.02 11.71;";

    #[test]
    fn parse_and_format_round_trip() {
        let r = parse_ramp(POLKA_END);
        assert_eq!(r.len(), 12);
        assert_eq!(r[0], (-51.0, 0.11));
        assert_eq!(format_ramp(&r), POLKA_END);
        assert!(parse_ramp("").is_empty());
        assert_eq!(parse_ramp("garbage;1 2;x y;").len(), 1);
    }

    #[test]
    fn lookup_follows_plexs_rules() {
        let r = parse_ramp(POLKA_END);
        // Exact point.
        assert!((lookup(&r, -21.12) - 3.71).abs() < 1e-9);
        // Interpolated between (-16.47, 3.81) and (-13.40, 3.91).
        let t = lookup(&r, -15.0);
        assert!(t > 3.81 && t < 3.91, "{t}");
        // At or below the first point: the first point's time.
        assert_eq!(lookup(&r, -60.0), 0.11);
        assert_eq!(lookup(&r, -51.0), 0.11);
        // Above every point, or an empty ramp: 0.
        assert_eq!(lookup(&r, 7.0), 0.0);
        assert_eq!(lookup(&vec![], -10.0), 0.0);
    }

    #[test]
    fn a_plateau_is_looked_up_exactly() {
        // Level rises to -20 at 1.0 s, sits there, and reaches -5 at 9.0 s.
        let curve: Curve = vec![
            (0.0, -60.0),
            (0.1, -30.0),
            (1.0, -20.0),
            (5.0, -20.0),
            (9.0, -5.0),
        ];
        let r = build_start_ramp(&curve);
        assert!((lookup(&r, -20.0) - 1.0).abs() < 0.02);
        // -19.9 is first reached at 9.0 s, not somewhere in the middle of the plateau.
        assert!(
            (lookup(&r, -19.9) - 9.0).abs() < 0.05,
            "{}",
            lookup(&r, -19.9)
        );
        assert!((lookup(&r, -5.0) - 9.0).abs() < 1e-9);
    }

    #[test]
    fn ramps_are_ascending_and_start_at_the_floor() {
        let curve: Curve = (0..200)
            .map(|i| {
                (
                    i as f64 * 0.1,
                    -50.0 + (i as f64 * 0.37).sin() * 20.0 + i as f64 * 0.1,
                )
            })
            .collect();
        for r in [
            build_start_ramp(&curve),
            build_end_ramp(&curve, 20.0, END_SHIFT_S),
        ] {
            assert_eq!(r[0].0, FLOOR_DB);
            assert!(r.windows(2).all(|w| w[1].0 > w[0].0), "ascending dB");
            assert!(
                r.windows(2).all(|w| w[1].1 >= w[0].1 - 1e-9),
                "time never goes back"
            );
        }
    }
}
