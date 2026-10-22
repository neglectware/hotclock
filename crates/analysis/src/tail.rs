//! A file cut off in the middle of its own fade-out (`truncated_fade`).
//!
//! Many quiet, home-rendered tracks end while their fade is still going: the file stops at, say,
//! -60 dBFS instead of reaching silence. With the gain normalization adds to a quiet track (up to
//! `max_boost_db`) that tail is audible, and then the sound stops dead. The detector reads the last
//! 12 s of the file in 100 ms steps, with the DC offset removed:
//!
//! * the last 500 ms that have sound (trailing digital silence is skipped) are louder than
//!   -70 dBFS RMS, and so is their last 100 ms, which is less than 6 dB under their first 100 ms
//!   (a fade that reaches silence at the very last sample plunges there; a cut one does not), and
//! * the 10 s before that are a fade: the level falls by at least 6 dB, by at least 0.4 dB a
//!   second on a least-squares line, and at least three quarters of its 1 s steps do not rise.
//!
//! A fade that reaches digital silence ends far under -70 dBFS, or plunges in its last 500 ms when
//! it reaches zero exactly at the end; a hard musical ending does not fall. None is flagged.

use crate::blocks::{db, Blocks};

/// How much of the end of the file the detector reads, in 10 ms blocks (12 s).
const TAIL_BLOCKS: usize = 1200;
/// One level step: 100 ms (10 blocks).
const STEP_BLOCKS: usize = 10;
/// The last 500 ms of sound louder than this, dBFS RMS, is not silence.
pub const AUDIBLE_END_DB: f64 = -70.0;
/// Quieter than this is digital silence or dither: such steps at the very end are skipped.
pub const SILENCE_DB: f64 = -90.0;
/// The fade must fall at least this much over the last 10 s, dB.
pub const MIN_DROP_DB: f64 = 6.0;
/// ...and at least this fast on a least-squares line, dB per second.
pub const MAX_SLOPE_DB_S: f64 = -0.4;
/// A last 100 ms this much under the first 100 ms of the last 500 ms is a fade that reached its end.
pub const MAX_END_PLUNGE_DB: f64 = 6.0;

/// What the detector measured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TailReport {
    pub truncated: bool,
    /// The level of the last 500 ms with sound, dBFS RMS (DC removed).
    pub end_db: f64,
    /// How far the last 100 ms of those 500 ms lie under their first 100 ms, dB.
    pub plunge_db: f64,
    /// How far the level falls over the last 10 s with sound, dB.
    pub drop_db: f64,
    /// The least-squares slope of the 1 s levels over those 10 s, dB per second.
    pub slope_db_s: f64,
}

/// The levels of the last 12 s of the file, dBFS RMS in 100 ms steps (the last step ends at the
/// file's end), in time order. The DC offset of each channel over those 12 s is removed; the level
/// of a step is the loudest channel's.
pub fn tail_levels(b: &Blocks) -> Vec<f64> {
    let ch = b.channels.max(1);
    let n = b.block_count();
    let first = n.saturating_sub(TAIL_BLOCKS);
    let frames = |blk: usize| b.block_frames(blk) as f64;
    // The DC of each channel over the tail.
    let mut dc = vec![0.0f64; ch];
    let mut total = 0.0f64;
    for blk in first..n {
        let w = frames(blk);
        total += w;
        for (c, d) in dc.iter_mut().enumerate() {
            *d += w * f64::from(b.mean[blk * ch + c]);
        }
    }
    if total > 0.0 {
        dc.iter_mut().for_each(|d| *d /= total);
    }
    let mut out = vec![];
    let mut end = n;
    while end > first {
        let start = end.saturating_sub(STEP_BLOCKS).max(first);
        let mut loudest = 0.0f64;
        for (c, d) in dc.iter().enumerate() {
            let (mut p, mut s, mut w) = (0.0f64, 0.0f64, 0.0f64);
            for blk in start..end {
                let f = frames(blk);
                p += f * f64::from(b.ms[blk * ch + c]);
                s += f * f64::from(b.mean[blk * ch + c]);
                w += f;
            }
            if w > 0.0 {
                // The mean square about the DC: E[x^2] - 2 dc E[x] + dc^2. Removing the DC only ever
                // lowers a level: digital zero stays zero.
                let ac = (p / w - 2.0 * d * s / w + d * d).clamp(0.0, p / w);
                loudest = loudest.max(ac);
            }
        }
        out.push(db(loudest));
        end = start;
    }
    out.reverse();
    out
}

fn power_mean_db(v: &[f64]) -> f64 {
    if v.is_empty() {
        return -120.0;
    }
    let p: f64 = v.iter().map(|l| 10f64.powf(l / 10.0)).sum::<f64>() / v.len() as f64;
    db(p)
}

/// The detector over 100 ms levels in time order (see the module docs).
pub fn detect(levels: &[f64]) -> TailReport {
    let sound = levels
        .iter()
        .rposition(|&l| l >= SILENCE_DB)
        .map_or(0, |i| i + 1);
    let lv = &levels[..sound];
    let mut r = TailReport {
        truncated: false,
        end_db: power_mean_db(&lv[lv.len().saturating_sub(5)..]),
        plunge_db: 0.0,
        drop_db: 0.0,
        slope_db_s: 0.0,
    };
    // At least 5 s of sound to judge a fade by.
    if lv.len() < 50 {
        return r;
    }
    let last = lv[lv.len() - 1];
    r.plunge_db = lv[lv.len() - 5] - last;
    // 1 s levels over the last (up to) 10 s, ending at the last step with sound.
    let w = &lv[lv.len().saturating_sub(100)..];
    let secs: Vec<f64> = w.rchunks_exact(10).rev().map(power_mean_db).collect();
    let k = secs.len();
    let head = power_mean_db(&secs[..2.min(k)]);
    r.drop_db = head - secs[k - 1];
    let mx = (k as f64 - 1.0) / 2.0;
    let my = secs.iter().sum::<f64>() / k as f64;
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for (i, y) in secs.iter().enumerate() {
        let dx = i as f64 - mx;
        sxy += dx * (y - my);
        sxx += dx * dx;
    }
    r.slope_db_s = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    let steady = secs.windows(2).filter(|p| p[1] <= p[0] + 1.0).count();
    let falling = steady as f64 >= 0.75 * (k - 1) as f64;
    r.truncated = r.end_db > AUDIBLE_END_DB
        && last > AUDIBLE_END_DB
        && r.plunge_db < MAX_END_PLUNGE_DB
        && r.drop_db >= MIN_DROP_DB
        && r.slope_db_s <= MAX_SLOPE_DB_S
        && falling;
    r
}

/// Is the file cut off in the middle of a fade? See the module docs.
pub fn truncated_fade(b: &Blocks) -> TailReport {
    detect(&tail_levels(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::BlockAnalyzer;

    const RATE: u32 = 48_000;

    /// Stereo noise whose RMS follows `level_db(t)` (dBFS; `None` is digital zero), plus `dc` on
    /// every sample that is not zero.
    fn render(secs: f64, dc: f32, level_db: impl Fn(f64) -> Option<f64>) -> Blocks {
        let mut a = BlockAnalyzer::new(RATE, 2);
        let n = (secs * f64::from(RATE)) as usize;
        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        let mut buf = Vec::with_capacity(9600);
        for i in 0..n {
            let t = i as f64 / f64::from(RATE);
            let g = level_db(t).map(|l| 10f64.powf(l / 20.0) * 3f64.sqrt());
            for _ in 0..2 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                // Uniform in [-1, 1): RMS 1/sqrt(3), hence the sqrt(3) above.
                let u = (x >> 11) as f64 / (1u64 << 52) as f64 - 1.0;
                buf.push(match g {
                    Some(g) => (u * g) as f32 + dc,
                    None => 0.0,
                });
            }
            if buf.len() >= 9600 {
                a.push(&buf);
                buf.clear();
            }
        }
        a.push(&buf);
        a.finish()
    }

    /// -20 dBFS for 8 s, then a fade falling linearly in dB to `to` at `fade_end` s.
    fn fade(to: f64, fade_end: f64) -> impl Fn(f64) -> f64 {
        move |t| {
            if t < 8.0 {
                -20.0
            } else {
                -20.0 + (to + 20.0) * ((t - 8.0) / (fade_end - 8.0)).min(1.0)
            }
        }
    }

    #[test]
    fn a_fade_cut_off_at_minus_60_db_is_flagged() {
        let f = fade(-80.0, 24.0); // would reach -80 at 24 s; the file stops at 20 s, at -65
        let r = truncated_fade(&render(20.0, 0.0, |t| Some(f(t))));
        assert!(r.truncated, "{r:?}");
        assert!(r.end_db > -70.0 && r.end_db < -60.0, "{r:?}");
        assert!(r.drop_db > 20.0, "{r:?}");
    }

    #[test]
    fn a_dc_offset_does_not_hide_a_cut_fade() {
        // A DC offset of -40 dBFS: without its removal the level would stop falling at -40.
        let f = fade(-60.0, 20.0);
        let r = truncated_fade(&render(20.0, 0.01, |t| Some(f(t))));
        assert!(r.truncated, "{r:?}");
        assert!(r.end_db < -55.0, "{r:?}");
    }

    #[test]
    fn trailing_digital_silence_after_a_cut_fade_is_skipped() {
        let f = fade(-60.0, 18.0);
        let r = truncated_fade(&render(20.0, 0.0, |t| (t < 18.0).then(|| f(t))));
        assert!(r.truncated, "{r:?}");
    }

    #[test]
    fn a_fade_to_digital_silence_is_not_flagged() {
        // Down to -110 dBFS at 18 s, then zeros: a fade that was rendered to its end.
        let f = fade(-110.0, 18.0);
        let r = truncated_fade(&render(20.0, 0.0, |t| (t < 18.0).then(|| f(t))));
        assert!(!r.truncated, "{r:?}");
        // The same with a DC offset in the file (it is removed, and the zeros are skipped).
        let r = truncated_fade(&render(20.0, 0.001, |t| Some(f(t))));
        assert!(!r.truncated, "{r:?}");
    }

    #[test]
    fn a_linear_fade_that_reaches_zero_at_the_last_sample_is_not_flagged() {
        // The amplitude falls on a straight line from 8 s to 0 at the file's end (20 s): its last 500 ms are
        // still about -45 dBFS on average, but the fade is complete.
        let r = truncated_fade(&render(20.0, 0.0, |t| {
            Some(if t < 8.0 {
                -20.0
            } else {
                -20.0 + 20.0 * ((20.0 - t) / 12.0).max(1e-9).log10()
            })
        }));
        assert!(!r.truncated, "{r:?}");
        assert!(r.end_db > -70.0 && r.plunge_db > 6.0, "{r:?}");
    }

    #[test]
    fn a_hard_ending_at_full_level_is_not_a_fade() {
        let r = truncated_fade(&render(20.0, 0.0, |_| Some(-12.0)));
        assert!(!r.truncated, "{r:?}");
        assert!(r.end_db > -14.0 && r.drop_db.abs() < 1.0, "{r:?}");
    }

    #[test]
    fn a_short_file_and_a_quiet_steady_ending_are_not_flagged() {
        let r = truncated_fade(&render(3.0, 0.0, |t| Some(-20.0 - 10.0 * t)));
        assert!(!r.truncated, "{r:?}");
        // Room tone at -60 dBFS: audible, but not a fade.
        let r = truncated_fade(&render(20.0, 0.0, |_| Some(-60.0)));
        assert!(!r.truncated, "{r:?}");
    }

    #[test]
    fn the_detector_reads_levels() {
        // 12 s falling 2 dB a second from -40 to -64: cut.
        let lv: Vec<f64> = (0..120).map(|i| -40.0 - 0.2 * f64::from(i)).collect();
        let r = detect(&lv);
        assert!(r.truncated, "{r:?}");
        assert!((r.slope_db_s + 2.0).abs() < 0.01, "{r:?}");
        // The same, falling on to -100 and then digital zero: rendered to its end.
        let lv: Vec<f64> = (0..120)
            .map(|i| {
                if i < 110 {
                    -40.0 - 0.55 * f64::from(i)
                } else {
                    -120.0
                }
            })
            .collect();
        assert!(!detect(&lv).truncated);
        assert!(!detect(&[]).truncated);
    }
}
