//! Gain curves and ramp segments (architecture 2.2). Plain data and arithmetic: nothing here
//! allocates, locks or logs, so it is safe to call from the rt-out thread.

/// The shape of a ramp between two gains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Curve {
    /// Linear in amplitude (declick ramps).
    Linear,
    /// Raised cosine in amplitude: smooth at both ends (fade-outs).
    Cosine,
    /// A sine/cosine pair: a rising ramp follows `sin`, a falling one `cos`, so a rising and a
    /// falling ramp over the same frames sum to constant power (the rare true crossfade).
    EqualPower,
    /// Linear in decibels (ducking, the `log` fade of the transition model, 2.2 and 2.6):
    /// `from * (to / from)^x` between two gains that are both above -60 dB of each other. A ramp
    /// to (or from) zero is linear in dB over the 60 dB below (or above) the other end, and the
    /// end value is exactly `to` (a last step of 60 dB, inaudible).
    DbLinear,
}

/// `-60 dB` in amplitude: the floor of the dB-linear curve.
const DB_FLOOR: f64 = 1e-3;

impl Curve {
    /// The gain at position `x` in `[0, 1]` of a ramp from `from` to `to`: exactly `from` at 0 and
    /// `to` at 1.
    #[inline]
    pub fn gain(self, from: f32, to: f32, x: f64) -> f64 {
        let (f, t) = (f64::from(from), f64::from(to));
        if x <= 0.0 {
            return f;
        }
        if x >= 1.0 {
            return t;
        }
        match self {
            Curve::Linear => f + (t - f) * x,
            Curve::Cosine => f + (t - f) * (0.5 - 0.5 * (std::f64::consts::PI * x).cos()),
            Curve::EqualPower => {
                if t >= f {
                    f + (t - f) * (std::f64::consts::FRAC_PI_2 * x).sin()
                } else {
                    // to + (from - to) * cos(x pi/2)
                    t + (f - t) * (std::f64::consts::FRAC_PI_2 * x).cos()
                }
            }
            Curve::DbLinear => {
                if f == t {
                    f
                } else if f > 0.0 && t > f * DB_FLOOR && t > 0.0 && f > t * DB_FLOOR {
                    f * (t / f).powf(x)
                } else if f > t {
                    // Falling towards zero (or 60 dB or more): dB-linear down to the floor.
                    f * (DB_FLOOR).powf(x)
                } else {
                    // Rising from zero (or from 60 dB or more below): the mirror image.
                    t * (DB_FLOOR).powf(1.0 - x)
                }
            }
        }
    }
}

/// A ramp from `from` to `to` over the frames `[start, end)` of a timeline. The gain at
/// `start` is `from`; at `end` and afterwards it is `to`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub start: u64,
    pub end: u64,
    pub from: f32,
    pub to: f32,
    pub curve: Curve,
}

impl Segment {
    /// The gain at `frame`, which must lie in `[start, end)`.
    #[inline]
    pub fn value(&self, frame: u64) -> f32 {
        let len = self.end.saturating_sub(self.start).max(1) as f64;
        let x = ((frame.saturating_sub(self.start)) as f64 / len).clamp(0.0, 1.0);
        self.curve.gain(self.from, self.to, x) as f32
    }
}

/// Up to [`Envelope::CAPACITY`] segments in time order. Before the first segment the gain is
/// `base`; between segments and after the last it holds the `to` of the segment before.
#[derive(Debug, Clone, Copy)]
pub struct Envelope {
    segs: [Segment; Envelope::CAPACITY],
    n: usize,
    base: f32,
}

const EMPTY_SEGMENT: Segment = Segment {
    start: 0,
    end: 0,
    from: 1.0,
    to: 1.0,
    curve: Curve::Linear,
};

impl Default for Envelope {
    fn default() -> Self {
        Self::new()
    }
}

impl Envelope {
    pub const CAPACITY: usize = 4;

    pub const fn new() -> Self {
        Self {
            segs: [EMPTY_SEGMENT; Self::CAPACITY],
            n: 0,
            base: 1.0,
        }
    }

    /// No segments and unity gain: the voice can skip envelope arithmetic.
    #[inline]
    pub fn is_unity(&self) -> bool {
        self.n == 0 && self.base == 1.0
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Add a segment, keeping the list ordered by start frame. When the list is full, segments
    /// that ended before `now` are folded into the base level to make room. Returns false if
    /// there is still no room (the segment is not added).
    pub fn push(&mut self, seg: Segment, now: u64) -> bool {
        if self.n == Self::CAPACITY {
            self.retire(now);
        }
        if self.n == Self::CAPACITY {
            return false;
        }
        let mut i = self.n;
        while i > 0 && self.segs[i - 1].start > seg.start {
            self.segs[i] = self.segs[i - 1];
            i -= 1;
        }
        self.segs[i] = seg;
        self.n += 1;
        true
    }

    /// A segment that changes the gain overlaps the frames `[f0, f1)`.
    pub fn ramping_in(&self, f0: u64, f1: u64) -> bool {
        self.segs[..self.n]
            .iter()
            .any(|s| s.start < f1 && s.end > f0 && s.from != s.to)
    }

    /// Drop the segments that ended at or before `now`, keeping the level they left behind.
    fn retire(&mut self, now: u64) {
        let mut keep = 0;
        while keep < self.n && self.segs[keep].end <= now {
            self.base = self.segs[keep].to;
            keep += 1;
        }
        if keep > 0 {
            self.segs.copy_within(keep..self.n, 0);
            self.n -= keep;
        }
    }

    /// The gain at `frame`.
    #[inline]
    pub fn value_at(&self, frame: u64) -> f32 {
        let mut held = self.base;
        for s in &self.segs[..self.n] {
            if frame < s.start {
                break;
            }
            if frame < s.end {
                return s.value(frame);
            }
            held = s.to;
        }
        held
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: u64, end: u64, from: f32, to: f32, curve: Curve) -> Segment {
        Segment {
            start,
            end,
            from,
            to,
            curve,
        }
    }

    #[test]
    fn endpoints_are_exact() {
        for c in [
            Curve::Linear,
            Curve::Cosine,
            Curve::EqualPower,
            Curve::DbLinear,
        ] {
            for (from, to) in [(1.0, 0.0), (0.0, 1.0), (1.0, 0.25)] {
                let mut e = Envelope::new();
                assert!(e.push(seg(100, 200, from, to, c), 0));
                assert_eq!(e.value_at(99), 1.0);
                assert!((e.value_at(100) - from).abs() < 1e-6, "{c:?}");
                assert!((e.value_at(200) - to).abs() < 1e-6, "{c:?}");
                assert!((e.value_at(10_000) - to).abs() < 1e-6, "{c:?}");
            }
        }
    }

    #[test]
    fn equal_power_pair_has_constant_power() {
        let down = seg(0, 1000, 1.0, 0.0, Curve::EqualPower);
        let up = seg(0, 1000, 0.0, 1.0, Curve::EqualPower);
        for f in (0..1000).step_by(7) {
            let p = down.value(f).powi(2) + up.value(f).powi(2);
            assert!((p - 1.0).abs() < 1e-5, "frame {f}: {p}");
        }
    }

    #[test]
    fn db_linear_is_a_straight_line_in_decibels() {
        // Between two audible gains: -6 dB to -26 dB over 1000 frames.
        let (from, to) = (0.5f32, 0.05f32);
        let s = seg(0, 1000, from, to, Curve::DbLinear);
        for f in (0..=1000).step_by(50) {
            let db = 20.0 * f64::from(s.value(f)).log10();
            let want = 20.0 * f64::from(from).log10()
                + (20.0 * f64::from(to).log10() - 20.0 * f64::from(from).log10()) * f as f64
                    / 1000.0;
            assert!((db - want).abs() < 1e-4, "frame {f}: {db} vs {want}");
        }
        // To silence: linear in dB down to -60 dB below the start, then exactly zero.
        let s = seg(0, 1000, 1.0, 0.0, Curve::DbLinear);
        for f in (0..1000).step_by(100) {
            let db = 20.0 * f64::from(s.value(f)).log10();
            assert!(
                (db + 60.0 * f as f64 / 1000.0).abs() < 1e-4,
                "frame {f}: {db}"
            );
        }
        assert_eq!(s.value(1000), 0.0);
        // Monotonic both ways.
        let up = seg(0, 1000, 0.0, 1.0, Curve::DbLinear);
        let mut prev = -1.0;
        for f in 0..=1000 {
            let v = up.value(f);
            assert!(v >= prev);
            prev = v;
        }
    }

    #[test]
    fn cosine_is_smooth_and_monotonic() {
        let s = seg(0, 480, 1.0, 0.0, Curve::Cosine);
        let mut prev = s.value(0);
        for f in 1..480 {
            let v = s.value(f);
            assert!(v <= prev);
            prev = v;
        }
        assert!((s.value(240) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn segments_hold_between_and_full_list_retires_old_ones() {
        let mut e = Envelope::new();
        assert!(e.push(seg(0, 10, 1.0, 0.5, Curve::Linear), 0));
        assert!(e.push(seg(20, 30, 0.5, 0.25, Curve::Linear), 0));
        assert_eq!(e.value_at(15), 0.5);
        assert_eq!(e.value_at(40), 0.25);
        assert!(e.push(seg(40, 50, 0.25, 1.0, Curve::Linear), 0));
        assert!(e.push(seg(60, 70, 1.0, 0.0, Curve::Linear), 0));
        assert!(
            !e.push(seg(80, 90, 0.0, 1.0, Curve::Linear), 0),
            "full, nothing has ended"
        );
        assert!(
            e.push(seg(80, 90, 0.0, 1.0, Curve::Linear), 35),
            "two ended by frame 35"
        );
        assert_eq!(
            e.value_at(15),
            0.25,
            "before the retained segments, the folded base"
        );
        assert_eq!(e.value_at(55), 1.0);
    }
}
