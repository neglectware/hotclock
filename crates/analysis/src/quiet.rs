//! The longest quiet stretch inside an item's play window (architecture 1.5, program silence).
//!
//! The station's silence detector skips an item after 15 s below -50 dBFS RMS. A real piece can be that
//! quiet for that long on purpose (the office set's "From Silence (RC1)" opens that way), and the owner's rule is
//! that nobody should have to find and flag such items by hand. So the analysis measures, for every item,
//! the longest stretch between cue-in and cue-out whose level stays below a threshold, and the detector
//! gives each item at least that much (plus a margin) before it calls the item dead.
//!
//! The level the detector sees is the file's level plus the station's gain for the item (normalization can
//! move a file by 10 dB or more either way), and the library does not know the station's gain. So the
//! stretch is stored for a ladder of file-level thresholds (a [`QuietProfile`]), and the station reads it at
//! `silence threshold - gain` (plus a slack for the measure, below).
//!
//! The measure is `e50` (50 ms RMS, the louder channel, hop 10 ms). The mixer's statistic is the mean
//! square of both channels over one output block (about 21 ms), so it reads a quiet passage as quiet at
//! least as often as `e50` does: a 50 ms window is not lifted by a click that lifts one 21 ms block, and the
//! louder channel is never quieter than the two-channel mean. Where they differ the profile overstates the
//! quiet stretch (a longer window, never a false skip).

use serde::{Deserialize, Serialize};

/// The ladder: file-level thresholds from `FROM_DB` up in `STEP_DB` steps.
pub const FROM_DB: f32 = -90.0;
pub const STEP_DB: f32 = 2.0;
pub const LEVELS: usize = 36; // -90 .. -20 dBFS

/// The longest stretch below each threshold of the ladder, ms, over two windows: the cue window (what a
/// `bounds = cue` preset plays) and the whole file (what `bounds = file` plays, the `plexamp` preset's
/// choice: the quiet before the cue-in and after the cue-out is heard then).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QuietProfile {
    /// `cue[k]`: the longest stretch inside `[cue_in, cue_out)` whose `e50` stays below
    /// `FROM_DB + k * STEP_DB`.
    pub cue: Vec<i64>,
    /// The same over the whole file.
    pub file: Vec<i64>,
}

fn ladder(e50: &[f32], from_ms: i64, to_ms: i64) -> Vec<i64> {
    let first = (from_ms.max(0) / 10) as usize;
    // The last window that ends inside `[from, to)`.
    let last = ((to_ms - 50).max(0) / 10) as usize;
    let span: &[f32] = if first < e50.len() && first <= last {
        &e50[first..=last.min(e50.len() - 1)]
    } else {
        &[]
    };
    (0..LEVELS)
        .map(|k| {
            let thr = FROM_DB + k as f32 * STEP_DB;
            let (mut run, mut best) = (0i64, 0i64);
            for &v in span {
                if v < thr {
                    run += 1;
                    best = best.max(run);
                } else {
                    run = 0;
                }
            }
            // `best` consecutive windows starting 10 ms apart cover `10 (best - 1) + 50` ms.
            if best == 0 {
                0
            } else {
                (10 * (best - 1) + 50).min((to_ms - from_ms).max(0))
            }
        })
        .collect()
}

fn read(v: &[i64], thr_db: f64) -> i64 {
    if v.is_empty() {
        return 0;
    }
    let k = ((thr_db - f64::from(FROM_DB)) / f64::from(STEP_DB)).ceil();
    let k = if k.is_nan() { 0.0 } else { k.max(0.0) } as usize;
    v[k.min(v.len() - 1)]
}

impl QuietProfile {
    /// Measure it from the `e50` envelope (sample `i` covers `[10 i, 10 i + 50)` ms).
    pub fn measure(e50: &[f32], cue_in_ms: i64, cue_out_ms: i64, file_ms: i64) -> QuietProfile {
        QuietProfile {
            cue: ladder(e50, cue_in_ms, cue_out_ms),
            file: ladder(e50, 0, file_ms.max(cue_out_ms)),
        }
    }

    /// The longest stretch below `thr_db` (file level), ms, inside the cue window or (`whole_file`) the
    /// file. Rounded up to the next rung of the ladder, so it never understates; above the top rung, the
    /// top rung's value (a threshold that loud is not a silence threshold).
    pub fn longest_below(&self, thr_db: f64, whole_file: bool) -> i64 {
        read(if whole_file { &self.file } else { &self.cue }, thr_db)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// `None` for an empty or unreadable value (an item analysed before schema 4).
    pub fn from_json(s: &str) -> Option<QuietProfile> {
        serde_json::from_str::<QuietProfile>(s)
            .ok()
            .filter(|p| !p.cue.is_empty() && !p.file.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_stretch_is_measured_inside_the_play_window_only() {
        // 10 s of -30 dB, 12 s of -60 dB, 10 s of -30 dB, then 20 s of -80 dB outside the cue-out.
        let mut e = vec![-30.0f32; 1000];
        e.extend(vec![-60.0f32; 1200]);
        e.extend(vec![-30.0f32; 1000]);
        e.extend(vec![-80.0f32; 2000]);
        let p = QuietProfile::measure(&e, 0, 32_000, 52_000);
        assert_eq!(p.longest_below(-50.0, false), 12_040);
        assert_eq!(
            p.longest_below(-70.0, false),
            0,
            "nothing is below -70 inside the window"
        );
        // A threshold between rungs reads the rung above it (never less).
        assert_eq!(p.longest_below(-59.5, false), 12_040);
        assert_eq!(p.longest_below(-61.0, false), 0);
        // The whole file: the tail after the cue-out counts.
        assert_eq!(p.longest_below(-50.0, true), 20_000);
        assert_eq!(p.longest_below(-70.0, true), 20_000);
        let back = QuietProfile::from_json(&p.to_json()).unwrap();
        assert_eq!(back, p);
        assert!(QuietProfile::from_json("").is_none());
    }
}
