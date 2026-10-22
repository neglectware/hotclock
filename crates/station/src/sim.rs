//! An audio-free run of the scheduler on a virtual clock: the tests of milestone M1.5 and the 30-day
//! rotation report use it. Each item "plays" at the start the timing model gives it, exactly as the
//! station would start it, and is fed back with [`Scheduler::on_started`].

use crate::catalog::{Catalog, TrackMeta};
use crate::item::Item;
use crate::plan::{bounds_of, plan, PlacedView, PlanInput, TransitionKind};
use crate::sched::{Origin, QEntry, Scheduler, Timing};
use common::transition::{resolve, Chain, PresetBook};
use std::collections::HashMap;
use std::sync::Arc;

/// One play, as the station's as-run log would record it.
#[derive(Debug, Clone, PartialEq)]
pub struct SimPlay {
    pub entry: QEntry,
    pub start_ms: i64,
}

impl SimPlay {
    pub fn item_id(&self) -> i64 {
        self.entry.item_id
    }

    pub fn relaxed(&self) -> bool {
        self.entry.flags.iter().any(|f| f == "rule_relaxed")
    }
}

/// Run until `until_ms` (virtual wall time), or `max_plays`. `hook(sched, cat, plays_so_far, now_ms)` is
/// called before each start (operators act there). The first item starts at `start_ms`.
pub fn run(
    sched: &mut Scheduler,
    cat: &Catalog,
    timing: &dyn Timing,
    start_ms: i64,
    until_ms: i64,
    max_plays: usize,
    mut hook: impl FnMut(&mut Scheduler, &Catalog, usize, i64),
) -> Vec<SimPlay> {
    let mut plays: Vec<SimPlay> = Vec::new();
    let mut now = start_ms;
    sched.fill(now, cat, timing);
    while now < until_ms && plays.len() < max_plays {
        hook(sched, cat, plays.len(), now);
        sched.fill(now, cat, timing);
        let Some(head) = sched.head().cloned() else {
            break;
        };
        let start = match (sched.on_air(), sched.st.on_air_start_ms) {
            (Some(a), Some(s)) => {
                let (ma, mb) = (cat.meta(a.item_id), cat.meta(head.item_id));
                match (ma, mb) {
                    (Some(ma), Some(mb)) => {
                        (s + timing.advance_ms(ma, mb, head.continuity_from_prev)).max(now)
                    }
                    _ => now,
                }
            }
            _ => now,
        };
        now = start;
        if sched.on_started(head.id, start, cat).is_none() {
            break;
        }
        plays.push(SimPlay {
            entry: head,
            start_ms: start,
        });
    }
    plays
}

/// What the virtual-clock driver saw of a timed act (a stop, a fade stop, the end of a program).
#[derive(Debug, Clone, PartialEq)]
pub struct SimAct {
    pub at_ms: i64,
    pub act: crate::timed::TimedAct,
}

/// A run with timed blocks (M3): as [`run`], and the hard fallbacks, the acts and the wake-ups of the
/// scheduler happen at their times. An entry whose tag has `hard_at_ms` starts there when its natural
/// start is later (the station plans a hard-time transition, 2.5); with nothing on air a tagged entry
/// starts at its time. Returns the plays, the acts, and the wall time the run stopped at (never past
/// `until_ms`: an item that would start later is left for the next call).
#[allow(clippy::too_many_arguments)]
pub fn run_timed(
    sched: &mut Scheduler,
    cat: &Catalog,
    timing: &dyn Timing,
    start_ms: i64,
    until_ms: i64,
    max_plays: usize,
    mut hook: impl FnMut(&mut Scheduler, &Catalog, usize, i64),
) -> (Vec<SimPlay>, Vec<SimAct>, i64) {
    let mut plays: Vec<SimPlay> = Vec::new();
    let mut acts: Vec<SimAct> = Vec::new();
    let mut now = start_ms;
    let mut guard = 0u64;
    while now < until_ms && plays.len() < max_plays {
        guard += 1;
        if guard > 50_000_000 {
            break;
        }
        hook(sched, cat, plays.len(), now);
        for a in sched.due_acts(now, 0, cat, timing) {
            if matches!(
                a.kind,
                crate::timed::ActKind::Stop { .. }
                    | crate::timed::ActKind::HardStop { .. }
                    | crate::timed::ActKind::FadeStop { .. }
            ) {
                sched.on_air_ended(cat);
            }
            acts.push(SimAct { at_ms: now, act: a });
        }
        sched.fill(now, cat, timing);
        let wake = sched.next_wake_ms(now, cat);
        let Some(head) = sched.head().cloned() else {
            match wake {
                Some(w) if w <= until_ms => {
                    now = w;
                    continue;
                }
                _ => {
                    now = until_ms;
                    break;
                }
            }
        };
        let hard = head.timed.as_ref().and_then(|t| t.hard_at_ms);
        let natural = match (sched.on_air(), sched.st.on_air_start_ms) {
            (Some(a), Some(s)) => {
                let (ma, mb) = (cat.meta(a.item_id), cat.meta(head.item_id));
                match (ma, mb) {
                    (Some(ma), Some(mb)) => {
                        (s + timing.advance_ms(ma, mb, head.continuity_from_prev)).max(now)
                    }
                    _ => now,
                }
            }
            // Nothing sounding: a tagged entry acts at its time.
            _ => hard.map_or(now, |h| h.max(now)),
        };
        let mut start = natural;
        let mut cut = false;
        if let Some(h) = hard {
            if natural > h && sched.on_air().is_some() {
                start = h.max(now);
                cut = true;
            }
        }
        if let Some(w) = wake {
            if w < start {
                now = w;
                continue;
            }
        }
        if start > until_ms {
            now = until_ms;
            break;
        }
        now = start;
        let mut head = head;
        if cut {
            head.flags.push("hard_time".into());
            if let Some(h) = sched.st.queue.first_mut() {
                h.flags.push("hard_time".into());
            }
        }
        if sched.on_started(head.id, start, cat).is_none() {
            break;
        }
        plays.push(SimPlay {
            entry: head,
            start_ms: start,
        });
    }
    (plays, acts, now)
}

/// The transition planner as the timing model: how long after A starts does B start (2.6), from the real
/// markers of real items.
pub struct PlanTiming {
    pub items: HashMap<i64, Arc<Item>>,
    pub station_preset: String,
    pub presets: PresetBook,
    pub rate: u32,
}

impl PlanTiming {
    pub fn new(items: HashMap<i64, Arc<Item>>, preset: &str) -> Self {
        Self {
            items,
            station_preset: preset.into(),
            presets: PresetBook::builtin(),
            rate: 48_000,
        }
    }
}

impl Timing for PlanTiming {
    fn advance_ms(&self, a: &TrackMeta, b: &TrackMeta, continuity: bool) -> i64 {
        let (Some(ia), Some(ib)) = (self.items.get(&a.item_id), self.items.get(&b.item_id)) else {
            return a.effective_ms;
        };
        fn chain<'a>(preset: &'a str, i: &'a Item) -> Chain<'a> {
            Chain {
                station_preset: preset,
                category_preset: i.category_preset.as_deref(),
                category_patch: Some(&i.category_patch),
                item: Some(&i.transition),
                entry: None,
            }
        }
        let (Ok(pa), Ok(pb)) = (
            resolve(&self.presets, &chain(&self.station_preset, ia)),
            resolve(&self.presets, &chain(&self.station_preset, ib)),
        ) else {
            return a.effective_ms;
        };
        let (bin, bout, fe) = bounds_of(ia, &pa);
        let view = PlacedView {
            item: ia,
            start_frame: 0,
            bound_in_ms: bin,
            bound_out_ms: bout,
            bound_out_is_file_end: fe,
            frames: None,
        };
        let p = plan(&PlanInput {
            rate: self.rate,
            now: 0,
            a: &view,
            b: ib,
            pa: &pa,
            pb: &pb,
            kind: if continuity {
                TransitionKind::Continuity
            } else {
                TransitionKind::Auto
            },
            truncated_fade_s: crate::plan::TRUNCATED_FADE_S,
        });
        (p.s_b as f64 * 1000.0 / f64::from(self.rate)).round() as i64
    }
}

/// Seam check over a played sequence: for every pair of consecutive passes of the deck `key`, does any
/// item among the last `k` plays of the first pass appear among the first `k` of the next? Returns
/// `(pairs checked, violations)`.
pub fn seam_violations(plays: &[SimPlay], key: &str, k: usize) -> (usize, usize) {
    let mut passes: Vec<(u64, Vec<i64>)> = Vec::new();
    for p in plays {
        if p.entry.origin != Origin::Rotation {
            continue;
        }
        let Some(d) = &p.entry.deck else { continue };
        if d.key != key {
            continue;
        }
        match passes.last_mut() {
            Some((g, v)) if *g == d.gen => v.push(p.entry.item_id),
            _ => passes.push((d.gen, vec![p.entry.item_id])),
        }
    }
    let (mut pairs, mut bad) = (0, 0);
    for w in passes.windows(2) {
        let (a, b) = (&w[0].1, &w[1].1);
        pairs += 1;
        let tail: std::collections::HashSet<&i64> = a[a.len().saturating_sub(k)..].iter().collect();
        if b.iter().take(k).any(|i| tail.contains(i)) {
            bad += 1;
        }
    }
    (pairs, bad)
}

/// For each rotation item, the gaps between consecutive starts (ms), in play order.
pub fn spacing_ms(plays: &[SimPlay], only_origin: Origin) -> Vec<i64> {
    let mut last: HashMap<i64, i64> = HashMap::new();
    let mut gaps = Vec::new();
    for p in plays {
        if p.entry.origin != only_origin {
            continue;
        }
        if let Some(prev) = last.insert(p.entry.item_id, p.start_ms) {
            gaps.push(p.start_ms - prev);
        }
    }
    gaps
}

/// The order of each pass of the deck `key`, from the rotation plays (grouped by the deck's pass
/// generation), for the pass-to-pass comparisons.
pub fn pass_orders(plays: &[SimPlay], key: &str) -> Vec<Vec<i64>> {
    let mut passes: Vec<(u64, Vec<i64>)> = Vec::new();
    for p in plays {
        let Some(d) = &p.entry.deck else { continue };
        if p.entry.origin != Origin::Rotation || d.key != key {
            continue;
        }
        match passes.last_mut() {
            Some((g, v)) if *g == d.gen => v.push(p.entry.item_id),
            _ => passes.push((d.gen, vec![p.entry.item_id])),
        }
    }
    passes.into_iter().map(|(_, v)| v).collect()
}

/// Spearman rank correlation between two orders of the same items (items missing from either are
/// left out). `None` when fewer than 3 items are common.
pub fn spearman(a: &[i64], b: &[i64]) -> Option<f64> {
    let rb: HashMap<i64, usize> = b.iter().enumerate().map(|(i, x)| (*x, i)).collect();
    let n = a.iter().filter(|x| rb.contains_key(x)).count();
    if n < 3 {
        return None;
    }
    // Re-rank both within the common items.
    let a2: Vec<i64> = a.iter().copied().filter(|x| rb.contains_key(x)).collect();
    let ra: HashMap<i64, f64> = a2.iter().enumerate().map(|(i, x)| (*x, i as f64)).collect();
    let b2: Vec<i64> = b.iter().copied().filter(|x| ra.contains_key(x)).collect();
    let d2: f64 = b2
        .iter()
        .enumerate()
        .map(|(j, x)| (ra[x] - j as f64).powi(2))
        .sum();
    let n = n as f64;
    Some(1.0 - 6.0 * d2 / (n * (n * n - 1.0)))
}

/// The expected Spearman rank correlation between consecutive passes of `n` units under the seam
/// alone (4.3), derived exactly for the deck's draw: the `k` units that ended pass `p` (its ranks
/// `n - k .. n - 1`) are barred from the first `k` places of pass `p + 1`. The repair treats the
/// places `k .. n - 1` alike and the barred units alike, so each barred unit's expected rank in the
/// new pass is the mean of `k .. n - 1`, `(n + k - 1) / 2`, and each other unit's is the mean of what
/// is left, `(n(n - 1)/2 - k(n + k - 1)/2) / (n - k)`. With `S_T` and `S_N` the old ranks' sums of the
/// two groups, `E[rho] = ((S_T m_T + S_N m_N) / n - mu^2) / sigma^2`, `mu = (n - 1)/2`,
/// `sigma^2 = (n^2 - 1)/12`. For `n = 20, k = 5` it is 6.25 / 33.25 = 0.188; it is 0 for `k = 0`.
pub fn seam_rank_correlation(n: usize, k: usize) -> f64 {
    if n < 2 || k == 0 {
        return 0.0;
    }
    let (n, k) = (n as f64, k as f64);
    let m_t = (n + k - 1.0) / 2.0;
    let m_n = (n * (n - 1.0) / 2.0 - k * m_t) / (n - k);
    let s_t = k * (2.0 * n - k - 1.0) / 2.0;
    let s_n = (n - k) * (n - k - 1.0) / 2.0;
    let mu = (n - 1.0) / 2.0;
    let var = (n * n - 1.0) / 12.0;
    ((s_t * m_t + s_n * m_n) / n - mu * mu) / var
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seam_correlation_formula() {
        assert!((seam_rank_correlation(20, 5) - 6.25 / 33.25).abs() < 1e-12);
        assert_eq!(seam_rank_correlation(20, 0), 0.0);
        assert!((spearman(&[1, 2, 3, 4], &[1, 2, 3, 4]).unwrap() - 1.0).abs() < 1e-12);
        assert!((spearman(&[1, 2, 3, 4], &[4, 3, 2, 1]).unwrap() + 1.0).abs() < 1e-12);
    }
}
