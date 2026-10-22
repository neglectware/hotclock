//! M1.3 acceptance 10 (first half): the property test of 3.3. 100,000 random combinations of
//! envelope, loudness, profile, policy and overrides, hostile ones included (cue-out before
//! cue-in, overrides past either end of the file, windows under 250 ms, negative values), must
//! give effective markers that satisfy every invariant of 3.1 and 3.3, and the computation itself
//! (before the safety fallback) must never have produced a violating row.

use analysis::effective::{
    check_invariants, compute_violates, effective_markers, EffectiveInput, Overrides,
};
use analysis::env::Envelopes;
use analysis::markers::{FileFacts, Flag, LongIntroPolicy, ProfileParams, MIN_WINDOW_MS};
use std::collections::BTreeSet;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// True with probability 1/n.
    fn chance(&mut self, n: u64) -> bool {
        self.next() % n < 1
    }
    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.f() * (hi - lo + 1) as f64) as i64
    }
    fn pick<T: Copy>(&mut self, v: &[T]) -> T {
        v[(self.next() % v.len() as u64) as usize]
    }
}

/// A random envelope shape over `n` samples: silence, plateaus, ramps, spikes, noise.
fn series(r: &mut Rng, n: usize, lo: f32, hi: f32) -> Vec<f32> {
    let mut v = vec![lo; n];
    match r.next() % 6 {
        0 => {}                                  // all silent
        1 => v.iter_mut().for_each(|x| *x = hi), // all loud
        2 => {
            for x in v.iter_mut() {
                *x = lo + (hi - lo) * r.f() as f32;
            }
        }
        3 => {
            // plateaus
            let mut i = 0;
            while i < n {
                let len = r.range(1, (n as i64 / 3).max(2)) as usize;
                let level = lo + (hi - lo) * r.f() as f32;
                for x in v.iter_mut().skip(i).take(len) {
                    *x = level;
                }
                i += len;
            }
        }
        4 => {
            // ramp up then down
            for (i, x) in v.iter_mut().enumerate() {
                let t = i as f32 / n.max(1) as f32;
                let tri = if t < 0.5 { t * 2.0 } else { (1.0 - t) * 2.0 };
                *x = lo + (hi - lo) * tri;
            }
        }
        _ => {
            // mostly quiet with a few loud bursts
            for _ in 0..r.range(0, 6) {
                let s = r.range(0, n as i64 - 1) as usize;
                let len = r.range(1, 200) as usize;
                for x in v.iter_mut().skip(s).take(len) {
                    *x = hi;
                }
            }
        }
    }
    v
}

fn random_profile(r: &mut Rng) -> ProfileParams {
    let mut p =
        ProfileParams::builtin(r.pick(&["music", "liner", "ambient", "longform", "id"])).unwrap();
    if r.chance(2) {
        let j = serde_json::json!({
            "segue_lu": (r.f() * 40.0 - 40.0),
            "silence_offset_db": r.f() * 60.0,
            "silence_min_db": -(r.f() * 90.0),
            "silence_max_db": -(r.f() * 90.0),
            "long_tail_s": r.f() * 40.0,
            "long_tail_lu": -(r.f() * 40.0),
            "attack_below_db": r.f() * 30.0,
            "long_intro_s": r.f() * 30.0,
            "holdoff": r.chance(2),
            "attack_ignored": r.chance(4),
        });
        p.apply_json(&j);
    }
    p
}

fn wild(r: &mut Rng, file_ms: i64) -> Option<i64> {
    match r.next() % 7 {
        0..=2 => None,
        3 => Some(r.range(-100_000, file_ms + 100_000)),
        4 => Some(r.range(0, file_ms)),
        5 => Some(r.pick(&[0, file_ms, file_ms + 1, -1, i64::MAX / 4, i64::MIN / 4])),
        _ => Some(r.range(0, file_ms)),
    }
}

#[test]
fn hundred_thousand_random_cases_keep_every_invariant() {
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let base: BTreeSet<Flag> = BTreeSet::new();
    let mut inner_violations = 0u64;
    let mut fallbacks = 0u64;
    let mut with_cue_override = 0u64;
    let mut hostile_pairs = 0u64;
    let mut outside_flagged = 0u64;
    const CASES: u64 = 100_000;
    for case in 0..CASES {
        // File length: mostly short (fast), sometimes longer, sometimes just over the minimum.
        let file_ms = match r.next() % 8 {
            0 => r.range(MIN_WINDOW_MS, 600),
            1 => r.range(600, 3_000),
            2..=5 => r.range(3_000, 40_000),
            _ => r.range(40_000, 200_000),
        };
        let n10 = (file_ms / 10) as usize + 1;
        let env = Envelopes {
            e10: series(&mut r, n10, -100.0, 0.0),
            e50: series(&mut r, n10.saturating_sub(4), -100.0, 0.0),
            k200: series(
                &mut r,
                (file_ms as usize / 20).saturating_sub(9),
                -100.0,
                0.0,
            ),
            k10: series(&mut r, n10, -100.0, 0.0),
        };
        let facts = FileFacts {
            file_ms,
            l_int: r.f() * 70.0 - 70.0,
            lra: r.f() * 30.0,
        };
        let profile = random_profile(&mut r);
        let mut ovr = Overrides {
            cue_in: wild(&mut r, file_ms),
            attack: wild(&mut r, file_ms),
            intro: wild(&mut r, file_ms),
            segue: wild(&mut r, file_ms),
            fade_out: wild(&mut r, file_ms),
            cue_out: wild(&mut r, file_ms),
        };
        // Force some targeted hostile pairs: cue-out before cue-in, and windows under 250 ms.
        if r.chance(10) {
            let a = r.range(0, file_ms);
            ovr.cue_in = Some(a);
            ovr.cue_out = Some(match r.next() % 3 {
                0 => a - r.range(0, 5_000),
                1 => a + r.range(0, MIN_WINDOW_MS + 5),
                _ => a,
            });
            hostile_pairs += 1;
        }
        if ovr.cue_in.is_some() || ovr.cue_out.is_some() {
            with_cue_override += 1;
        }
        let exact = if r.chance(4) {
            Some((
                r.range(-10, file_ms),
                r.range(0, file_ms + 10),
                r.f() * 60.0 - 60.0,
            ))
        } else {
            None
        };
        let inp = EffectiveInput {
            facts,
            env: &env,
            profile: &profile,
            ovr,
            policy: if r.chance(2) {
                LongIntroPolicy::Play
            } else {
                LongIntroPolicy::SkipToAttack
            },
            base_flags: &base,
            l_w_exact: exact,
        };
        if let Some(why) = compute_violates(&inp) {
            inner_violations += 1;
            if inner_violations <= 5 {
                eprintln!("case {case}: computation violated an invariant: {why}; file {file_ms}, {ovr:?}, profile {}", profile.name);
            }
        }
        let e = effective_markers(&inp);
        if e.warning.is_some() {
            fallbacks += 1;
        }
        if let Err(why) = check_invariants(&e, file_ms) {
            panic!("case {case}: {why}; file_ms {file_ms}, overrides {ovr:?}, result {e:?}");
        }
        if e.flags.contains(&Flag::OverrideOutsideWindow) {
            outside_flagged += 1;
        }
        // A manual marker inside the window is honored (up to the clamps).
        if let Some(a) = ovr.attack {
            if a >= e.window.0
                && a <= e.window.1 - 50
                && a < e.segue
                && ovr.segue.is_none_or(|s| s > a + 10)
                && !e.flags.contains(&Flag::CueOverrideInvalid)
            {
                assert!(
                    e.attack == a || e.attack == e.cue_in.max(a.min(e.cue_out - 50)),
                    "case {case}: manual attack {a} became {}",
                    e.attack
                );
            }
        }
    }
    println!(
        "{CASES} cases: 0 invariant failures in the results; the computation itself violated an invariant {inner_violations} times (safety fallback used {fallbacks}); \
         {with_cue_override} with a cue override, {hostile_pairs} forced hostile cue pairs, {outside_flagged} flagged override_outside_window"
    );
    assert_eq!(
        inner_violations, 0,
        "the computation must be valid by construction; the fallback is only a safety net"
    );
}
