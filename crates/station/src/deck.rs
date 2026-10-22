//! Rotation decks (architecture 4.3): continuous re-randomization. A deck is a persisted sequence of
//! **passes**, each a fresh permutation of the playlist's playable units, without replacement inside
//! a pass. The next pass is drawn only when a soft entry needs it, so nothing on air, committed or
//! pinned is ever touched, and the seam between two passes is constrained: no unit among the last `K`
//! picks may appear among the first `K` of the new pass, with `K` derived from the no-repeat window,
//! the playlist size and the mean item length ([`seam_k`]).
//!
//! A deck can be rewound to any earlier pick ([`Deck::rewind_to`]): queue regeneration takes the soft
//! entries back and picks again, and a reshuffle redraws what is left of the current pass
//! ([`Deck::reshuffle_rest`]).

use crate::rng::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// One thing a deck deals: a single item, or an album block (one unit; expanded into its tracks in
/// order when it comes up, 3.7).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Unit {
    Item(i64),
    Album(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pass {
    pub gen: u64,
    /// The sequence number of this pass's first pick.
    pub start_seq: u64,
    pub order: Vec<Unit>,
    pub cursor: usize,
    /// The seed the permutation was drawn from (logged, 4.3).
    pub seed: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Deck {
    pub passes: Vec<Pass>,
    pub next_seq: u64,
    pub next_gen: u64,
    /// The last picks in order, `(sequence number, unit)`: the seam looks at their tail.
    pub picked: Vec<(u64, Unit)>,
    /// The highest pick that has started to play: a regeneration never rewinds past it.
    #[serde(default)]
    pub hard_seq: Option<u64>,
}

/// The share of one pass (the playlist's total length) the no-repeat window may take: the owner's rule
/// (2026-09-29) is no repeat within a configurable time "but not so long that the playlist can't
/// randomize a little over time". A window close to a whole pass leaves each pick only the few
/// units that have not played for that long, so the order of one pass nearly repeats the previous
/// one; a window longer than the pass cannot be kept at all (the office's 120 minutes over 18
/// tracks of 7.2 minutes gave 1,968 relaxed plays in 30 days). The window a playlist enforces is
/// therefore `min(requested, WINDOW_SHARE_OF_PASS * n * m, (n - 2) * m)`: the setting stays what the
/// user set (`no_repeat_minutes`, never rejected or rewritten), and this computed maximum is used
/// instead when the playlist cannot honor it, with the reason ([`Rotation::reason`]). The value is chosen from the 30-day
/// office simulation (BUILDLOG, review of M1.5): relaxed plays about zero, and consecutive passes
/// still well apart in order.
pub const WINDOW_SHARE_OF_PASS: f64 = 0.65;

/// The seam's `K` is at most `floor(n / 3)` (the design had `floor(n / 2)`). At `n / 2` the units
/// barred from the first `K` places of a pass are exactly the second half of the previous pass, so
/// the first half of every pass is the first half of the one before, as a set, forever: the order
/// shuffles inside each half and never across. On the office set the rank correlation between a
/// pass and the pass 100 later stayed at 0.75 (review of M1.5). With `K <= n / 3` it is about 0
/// after 5 passes; the pick-time check still keeps the effective window (the seam only makes it
/// rarely needed), with no relaxed play in the 30-day simulation.
pub const SEAM_MAX_DIVISOR: usize = 3;

/// What the seam needs for a playlist of `n` playable units and mean unit length `mean_ms`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rotation {
    pub n: usize,
    pub mean_ms: f64,
    /// What the playlist asks for, `no_repeat_minutes`.
    pub requested_minutes: f64,
    /// The window the rotation enforces: the requested one, or `share * n * m` when that is shorter
    /// (the auto-cap above). Both the seam and the pick-time check use it.
    pub effective_minutes: f64,
    /// `W = ceil(effective / m)`: units that fit in the window.
    pub window_items: usize,
    /// The seam constraint `K = clamp(W - 1, 0, floor(n / 3))` ([`SEAM_MAX_DIVISOR`]).
    pub k: usize,
    /// The requested window was cut to fit the playlist (`effective < requested`): the form says so.
    pub capped: bool,
    /// About how far apart a track is kept, minutes: the effective window (the form's figure).
    pub achievable_minutes: f64,
    /// What the seam alone guarantees, `(K + 1) * m`; the pick-time check keeps the rest.
    pub seam_minutes: f64,
}

impl Rotation {
    /// Why the effective window is shorter than the configured one, for the form, the API and the log:
    /// `capped: 18 tracks, 129 min total`. `None` when the configured window is used as set.
    pub fn reason(&self) -> Option<String> {
        self.capped.then(|| {
            format!(
                "capped: {} tracks, {:.0} min total",
                self.n,
                self.n as f64 * self.mean_ms / 60_000.0
            )
        })
    }
}

/// `K` and the numbers behind the playlist form's "this playlist can keep a track about X minutes
/// apart; the window asks for Y" (4.3), with the default share of a pass.
pub fn rotation(n: usize, mean_ms: f64, no_repeat_minutes: i64) -> Rotation {
    rotation_with(n, mean_ms, no_repeat_minutes, WINDOW_SHARE_OF_PASS)
}

/// As [`rotation`], with the share of a pass given (the simulation that chose it sweeps it).
pub fn rotation_with(n: usize, mean_ms: f64, no_repeat_minutes: i64, share: f64) -> Rotation {
    let requested = no_repeat_minutes.max(0) as f64;
    let mean = mean_ms.max(1.0);
    let pass_minutes = n as f64 * mean / 60_000.0;
    // And never so long that fewer than two units are left to choose from at a pick: a 3-item
    // playlist whose window covers two items can only ever play one fixed cycle.
    let two_left_minutes = n.saturating_sub(2) as f64 * mean / 60_000.0;
    let effective = requested.min(share * pass_minutes).min(two_left_minutes);
    let w = if effective <= 0.0 {
        0
    } else {
        (effective * 60_000.0 / mean).ceil() as usize
    };
    let k = w.saturating_sub(1).min(n / SEAM_MAX_DIVISOR);
    Rotation {
        n,
        mean_ms: mean,
        requested_minutes: requested,
        effective_minutes: effective,
        window_items: w,
        k,
        capped: effective < requested,
        achievable_minutes: effective,
        seam_minutes: (k as f64 + 1.0) * mean / 60_000.0,
    }
}

/// Draw a permutation of `pool` (weighted shuffle, Efraimidis-Spirakis keys `u^(1/w)`), then repair
/// the seam: each of the first `k` places holding a unit in `barred` swaps with a random unit after
/// place `k` that is not barred. A repair always exists while `k <= n / 2` and `|barred| <= k` (the
/// rotation keeps `k <= n / 3`).
pub fn draw_order(
    pool: &[(Unit, f64)],
    barred: &HashSet<Unit>,
    k: usize,
    rng: &mut Rng,
) -> Vec<Unit> {
    let mut keyed: Vec<(f64, &Unit)> = pool
        .iter()
        .filter(|(_, w)| *w > 0.0)
        .map(|(u, w)| (rng.unit().powf(1.0 / w), u))
        .collect();
    keyed.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut order: Vec<Unit> = keyed.into_iter().map(|(_, u)| u.clone()).collect();
    let k = k.min(order.len());
    for i in 0..k {
        if !barred.contains(&order[i]) {
            continue;
        }
        let candidates: Vec<usize> = (k..order.len())
            .filter(|&j| !barred.contains(&order[j]))
            .collect();
        if candidates.is_empty() {
            break;
        }
        let j = candidates[rng.below(candidates.len())];
        order.swap(i, j);
    }
    order
}

/// A pick.
#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    pub unit: Unit,
    pub seq: u64,
    pub gen: u64,
    /// The relaxation level the pick needed: 0 none; see [`Rules`].
    pub relaxed: u8,
    /// No unit passed even the loosest rule and the one that played longest ago was taken (4.3).
    pub fallback: bool,
}

/// The pick-time check the scheduler supplies. `eligible(unit, level)` says whether `unit` may play
/// now with the rules at relaxation `level`: 0 all rules (item window, title, artist, album), 1 without
/// the album rule, 2 also without the artist rule, 3 also without the title rule, 4 no rule at all.
pub struct Rules<'a> {
    pub eligible: &'a dyn Fn(&Unit, u8) -> bool,
    /// When did this unit last play (start, ms), for the longest-ago fallback; `i64::MIN` if never.
    pub last_played: &'a dyn Fn(&Unit) -> i64,
}

pub const MAX_LEVEL: u8 = 4;

impl Deck {
    /// The pass the next pick comes from.
    fn current(&self) -> Option<&Pass> {
        self.passes.last()
    }

    /// Remaining units of the current pass (a copy), in order.
    pub fn remaining(&self) -> Vec<Unit> {
        self.current()
            .map(|p| p.order[p.cursor.min(p.order.len())..].to_vec())
            .unwrap_or_default()
    }

    /// Bring the deck up to date with `pool` (the playable units now): units that vanished leave the rest
    /// of the pass, new ones join it at random places, and a finished pass is followed by a fresh one
    /// (`k` is the seam). `false` only when the pool is empty.
    pub fn prepare(&mut self, pool: &[(Unit, f64)], k: usize, rng: &mut Rng) -> bool {
        let playable: HashSet<&Unit> = pool
            .iter()
            .filter(|(_, w)| *w > 0.0)
            .map(|(u, _)| u)
            .collect();
        if playable.is_empty() {
            return false;
        }
        // Drop units that are no longer playable from what is left of the pass.
        if let Some(p) = self.passes.last_mut() {
            let mut kept: Vec<Unit> = p.order[..p.cursor.min(p.order.len())].to_vec();
            let done = kept.len();
            kept.extend(
                p.order[done..]
                    .iter()
                    .filter(|u| playable.contains(u))
                    .cloned(),
            );
            p.order = kept;
        }
        // Units newly in the pool join the rest of the pass at random places.
        if let Some(p) = self.passes.last_mut() {
            let have: HashSet<&Unit> = p.order.iter().collect();
            let fresh: Vec<Unit> = pool
                .iter()
                .filter(|(u, w)| *w > 0.0 && !have.contains(u))
                .map(|(u, _)| u.clone())
                .collect();
            let cursor = p.cursor;
            for u in fresh {
                let span = p.order.len() - cursor;
                let at = cursor + rng.below(span + 1);
                p.order.insert(at, u);
            }
        }
        if self.current().is_none_or(|p| p.cursor >= p.order.len()) {
            self.new_pass(pool, k, rng);
        }
        true
    }

    /// Deal the unit at `order[j]` of the current pass (`j` at or after the cursor): it is swapped to the
    /// cursor, which moves on. `relaxed` and `fallback` describe how it passed the rules.
    pub fn take(&mut self, j: usize, relaxed: u8, fallback: bool, k: usize) -> Option<Pick> {
        let pass = self.passes.last_mut()?;
        if j < pass.cursor || j >= pass.order.len() {
            return None;
        }
        pass.order.swap(pass.cursor, j);
        let unit = pass.order[pass.cursor].clone();
        pass.cursor += 1;
        let seq = self.next_seq;
        self.next_seq += 1;
        let gen = pass.gen;
        self.picked.push((seq, unit.clone()));
        let keep = (k + 64).max(256);
        if self.picked.len() > 2 * keep {
            let cut = self.picked.len() - keep;
            self.picked.drain(..cut);
        }
        Some(Pick {
            unit,
            seq,
            gen,
            relaxed,
            fallback,
        })
    }

    /// Deal a particular unit of the rest of the pass (a best-fit choice, 4.6), if it is there.
    pub fn take_unit(&mut self, unit: &Unit, relaxed: u8, k: usize) -> Option<Pick> {
        let pass = self.passes.last()?;
        let j = (pass.cursor..pass.order.len()).find(|&j| &pass.order[j] == unit)?;
        self.take(j, relaxed, false, k)
    }

    /// Deal the next unit. `pool` is the playable units now (units that vanished from it are dropped
    /// from the current pass), `k` the seam. `None` only when the pool is empty.
    pub fn pick(
        &mut self,
        pool: &[(Unit, f64)],
        k: usize,
        rules: &Rules,
        rng: &mut Rng,
    ) -> Option<Pick> {
        if !self.prepare(pool, k, rng) {
            return None;
        }
        let pass = self.passes.last()?;
        // The first unit from the cursor that passes the rules at the lowest level that has one.
        let mut choice: Option<(usize, u8)> = None;
        'levels: for level in 0..=MAX_LEVEL {
            for j in pass.cursor..pass.order.len() {
                if (rules.eligible)(&pass.order[j], level) {
                    choice = Some((j, level));
                    break 'levels;
                }
            }
        }
        let (j, level, fallback) = match choice {
            Some((j, l)) if l < MAX_LEVEL => (j, l, false),
            _ => {
                // Nothing passes even the loosest rule set: the one that played longest ago.
                let j = (pass.cursor..pass.order.len())
                    .min_by_key(|&j| (rules.last_played)(&pass.order[j]))
                    .unwrap_or(pass.cursor);
                (j, MAX_LEVEL, true)
            }
        };
        self.take(j, level, fallback, k)
    }

    fn new_pass(&mut self, pool: &[(Unit, f64)], k: usize, rng: &mut Rng) {
        let n_playable = pool.iter().filter(|(_, w)| *w > 0.0).count();
        let k = k.min(n_playable / 2);
        let barred: HashSet<Unit> = self
            .picked
            .iter()
            .rev()
            .take(k)
            .map(|(_, u)| u.clone())
            .collect();
        let seed = rng.next_u64();
        let mut r = Rng::new(seed);
        let order = draw_order(pool, &barred, k, &mut r);
        let gen = self.next_gen;
        self.next_gen += 1;
        tracing::debug!(gen, seed, units = order.len(), "deck: new pass drawn");
        self.passes.push(Pass {
            gen,
            start_seq: self.next_seq,
            order,
            cursor: 0,
            seed,
        });
        // A rewind never goes before the highest pick that has started (`hard_seq`), so a pass can
        // go once the pass after it began at or before that pick. (Keeping only the last three
        // passes lost the pass on air when the plan was more than two passes deep, a small
        // playlist of short items, and the rewind then dealt that pass's units a second time.)
        while self.passes.len() > 2 && self.hard_seq.is_some_and(|h| self.passes[1].start_seq <= h)
        {
            self.passes.remove(0);
        }
    }

    /// Take back every pick after `seq` (`None`: every pick): the soft entries they made are being
    /// regenerated. Passes drawn after `seq` disappear and the pass that holds it goes back to just
    /// after it.
    pub fn rewind_to(&mut self, seq: Option<u64>) {
        let keep_until = seq.map_or(0, |s| s + 1);
        self.picked.retain(|(s, _)| *s < keep_until);
        while self.passes.len() > 1
            && self
                .passes
                .last()
                .is_some_and(|p| p.start_seq >= keep_until)
        {
            self.passes.pop();
        }
        if let Some(p) = self.passes.last_mut() {
            p.cursor = if p.start_seq >= keep_until {
                0
            } else {
                ((keep_until - p.start_seq) as usize).min(p.order.len())
            };
        }
        self.next_seq = keep_until;
    }

    /// Reshuffle: redraw what is left of the current pass (the picks up to the cursor stay), with the
    /// units for which `recent` is true moved to the end (they played within the window).
    pub fn reshuffle_rest(&mut self, rng: &mut Rng, recent: &dyn Fn(&Unit) -> bool) {
        let Some(p) = self.passes.last_mut() else {
            return;
        };
        let cursor = p.cursor.min(p.order.len());
        let mut rest: Vec<Unit> = p.order.split_off(cursor);
        let seed = rng.next_u64();
        Rng::new(seed).shuffle(&mut rest);
        // Stable partition: units not played within the window first.
        let (old, fresh): (Vec<Unit>, Vec<Unit>) = rest.into_iter().partition(|u| !recent(u));
        p.order.extend(old);
        p.order.extend(fresh);
        p.seed = seed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(n: i64) -> Vec<(Unit, f64)> {
        (1..=n).map(|i| (Unit::Item(i), 1.0)).collect()
    }

    #[allow(clippy::type_complexity)]
    fn open_rules() -> (Box<dyn Fn(&Unit, u8) -> bool>, Box<dyn Fn(&Unit) -> i64>) {
        (Box::new(|_, _| true), Box::new(|_| i64::MIN))
    }

    #[test]
    fn k_follows_the_formula_and_the_caps() {
        // 20 items of 5 min, 30 minutes: W = 6, K = 5.
        let r = rotation(20, 300_000.0, 30);
        assert_eq!((r.window_items, r.k, r.capped), (6, 5, false));
        // 120 minutes over a 100-minute pass: the window is cut to 65 minutes (W = 13), K capped at
        // floor(20 / 3) = 6; the seam alone keeps (6 + 1) * 5 = 35 min, the pick-time check the rest.
        let r = rotation(20, 300_000.0, 120);
        assert_eq!((r.window_items, r.k, r.capped), (13, 6, true));
        assert!((r.achievable_minutes - 65.0).abs() < 1e-9);
        assert!((r.seam_minutes - 35.0).abs() < 1e-9);
        // The office example of 4.3: 18 items of 6 minutes, 120 minutes: cut to 70.2, W = 12, K = 6.
        let r = rotation(18, 360_000.0, 120);
        assert_eq!((r.window_items, r.k, r.capped), (12, 6, true));
        // Zero window: only the deck.
        assert_eq!(rotation(20, 300_000.0, 0).k, 0);
        assert_eq!(rotation(1, 300_000.0, 120).k, 0);
    }

    #[test]
    fn a_pass_deals_every_unit_once_and_the_next_pass_starts_after_it() {
        let mut deck = Deck::default();
        let mut rng = Rng::new(1);
        let (e, l) = open_rules();
        let rules = Rules {
            eligible: &*e,
            last_played: &*l,
        };
        let p = pool(6);
        let mut first: Vec<Unit> = (0..6)
            .map(|_| deck.pick(&p, 0, &rules, &mut rng).unwrap().unit)
            .collect();
        first.sort();
        assert_eq!(first, p.iter().map(|x| x.0.clone()).collect::<Vec<_>>());
        let g0 = deck.passes.last().unwrap().gen;
        let next = deck.pick(&p, 0, &rules, &mut rng).unwrap();
        assert_ne!(next.gen, g0, "the seventh pick starts a new pass");
    }

    #[test]
    fn the_seam_holds_for_every_pass() {
        let mut deck = Deck::default();
        let mut rng = Rng::new(9);
        let (e, l) = open_rules();
        let rules = Rules {
            eligible: &*e,
            last_played: &*l,
        };
        let n = 20;
        let k = 5;
        let p = pool(n);
        let mut seq = Vec::new();
        for _ in 0..(n as usize * 300) {
            seq.push(deck.pick(&p, k, &rules, &mut rng).unwrap().unit);
        }
        for pass in 1..300 {
            let last_k: HashSet<&Unit> = seq[pass * 20 - k..pass * 20].iter().collect();
            for u in &seq[pass * 20..pass * 20 + k] {
                assert!(!last_k.contains(u), "seam broken at pass {pass}");
            }
        }
        // Any two plays are at least K + 1 apart.
        let mut last: std::collections::HashMap<&Unit, usize> = Default::default();
        for (i, u) in seq.iter().enumerate() {
            if let Some(&j) = last.get(u) {
                assert!(i - j > k, "{u:?} again after {}", i - j);
            }
            last.insert(u, i);
        }
    }

    #[test]
    fn a_rewind_takes_soft_picks_back_and_a_new_pick_can_differ() {
        let mut deck = Deck::default();
        let mut rng = Rng::new(4);
        let (e, l) = open_rules();
        let rules = Rules {
            eligible: &*e,
            last_played: &*l,
        };
        let p = pool(10);
        let picks: Vec<Pick> = (0..6)
            .map(|_| deck.pick(&p, 2, &rules, &mut rng).unwrap())
            .collect();
        let keep = picks[2].seq;
        deck.rewind_to(Some(keep));
        assert_eq!(deck.picked.len(), 3);
        assert_eq!(deck.remaining().len(), 7);
        // The units after the rewind are exactly the 7 not yet dealt.
        let mut dealt: HashSet<Unit> = picks[..3].iter().map(|x| x.unit.clone()).collect();
        deck.reshuffle_rest(&mut rng, &|_| false);
        for _ in 0..7 {
            let u = deck.pick(&p, 2, &rules, &mut rng).unwrap().unit;
            assert!(dealt.insert(u), "a unit was dealt twice in one pass");
        }
        assert_eq!(dealt.len(), 10);
    }

    #[test]
    fn rewinding_across_a_seam_returns_to_the_earlier_pass() {
        let mut deck = Deck::default();
        let mut rng = Rng::new(5);
        let (e, l) = open_rules();
        let rules = Rules {
            eligible: &*e,
            last_played: &*l,
        };
        let p = pool(4);
        let picks: Vec<Pick> = (0..7)
            .map(|_| deck.pick(&p, 1, &rules, &mut rng).unwrap())
            .collect();
        // Rewind to the 3rd pick of the first pass.
        deck.rewind_to(Some(picks[2].seq));
        assert_eq!(deck.passes.last().unwrap().gen, picks[2].gen);
        assert_eq!(deck.remaining().len(), 1);
        let again = deck.pick(&p, 1, &rules, &mut rng).unwrap();
        assert_eq!(
            again.unit, picks[3].unit,
            "the rest of the pass was not reshuffled"
        );
        assert_eq!(again.seq, picks[3].seq);
    }

    #[test]
    fn a_unit_that_fails_the_rules_is_passed_over_and_the_lowest_level_wins() {
        let mut deck = Deck::default();
        let mut rng = Rng::new(2);
        // Unit 1 is never eligible at levels below 4; everything else is.
        let e = |u: &Unit, level: u8| *u != Unit::Item(1) || level >= 4;
        let l = |_: &Unit| i64::MIN;
        let rules = Rules {
            eligible: &e,
            last_played: &l,
        };
        let p = pool(5);
        let picks: Vec<Pick> = (0..5)
            .map(|_| deck.pick(&p, 0, &rules, &mut rng).unwrap())
            .collect();
        assert_eq!(
            picks[4].unit,
            Unit::Item(1),
            "it goes last, when only it is left"
        );
        assert!(picks[..4].iter().all(|x| x.relaxed == 0));
        assert!(
            picks[4].fallback,
            "no rule set passes it: longest-ago fallback"
        );
    }

    #[test]
    fn a_reshuffle_moves_recent_units_to_the_end() {
        let mut deck = Deck::default();
        let mut rng = Rng::new(6);
        let (e, l) = open_rules();
        let rules = Rules {
            eligible: &*e,
            last_played: &*l,
        };
        let p = pool(10);
        deck.pick(&p, 0, &rules, &mut rng).unwrap();
        let recent = |u: &Unit| matches!(u, Unit::Item(i) if *i <= 3);
        deck.reshuffle_rest(&mut rng, &recent);
        let rest = deck.remaining();
        let first_recent = rest.iter().position(&recent);
        let last_fresh = rest.iter().rposition(|u| !recent(u));
        if let (Some(fr), Some(lf)) = (first_recent, last_fresh) {
            assert!(
                fr > lf,
                "recent units must all come after the others: {rest:?}"
            );
        }
    }
}

#[cfg(test)]
mod review_m15 {
    use super::*;

    /// A queue many passes deep (a small playlist of short items, 30 minutes of plan) and a rewind to
    /// the pick on air, which lies several passes back: the rest of that pass must be dealt, never a
    /// unit that pass already dealt.
    #[test]
    fn a_rewind_several_passes_back_deals_the_rest_of_that_pass() {
        let mut deck = Deck::default();
        let mut rng = Rng::new(11);
        let e = |_: &Unit, _: u8| true;
        let l = |_: &Unit| i64::MIN;
        let rules = Rules {
            eligible: &e,
            last_played: &l,
        };
        let p: Vec<(Unit, f64)> = (1..=4).map(|i| (Unit::Item(i), 1.0)).collect();
        let picks: Vec<Pick> = (0..24)
            .map(|_| deck.pick(&p, 0, &rules, &mut rng).unwrap())
            .collect();
        // Pick 1 (the second of the first pass) started; everything after it is taken back.
        deck.hard_seq = Some(picks[1].seq);
        deck.rewind_to(Some(picks[1].seq));
        let again: Vec<Unit> = (0..2)
            .map(|_| deck.pick(&p, 0, &rules, &mut rng).unwrap().unit)
            .collect();
        let mut first_pass: Vec<Unit> = picks[..2].iter().map(|x| x.unit.clone()).collect();
        first_pass.extend(again);
        first_pass.sort();
        first_pass.dedup();
        assert_eq!(first_pass.len(), 4, "a unit was dealt twice in one pass");
    }
}
