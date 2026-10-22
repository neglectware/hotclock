//! Back-timing (architecture 4.9; milestone M5): a `play` block whose `end` is `natural` fills so that a
//! track ends at the block's end time.
//!
//! At every slot the planner looks at the rest of the deck and searches the chains of one to four items
//! that would carry the fill to the target, using the real transition overlaps (`Timing`), and deals the
//! first item of the best chain. The next slot searches again from the start the previous item really has
//! (so the plan is recomputed after every transition and each recomputation is a row of the block log).
//!
//! * `best_fit`: the chain whose last item ends nearest the target. It is `on_target` within half a second,
//!   `within_tolerance` inside `tolerance_s`, else `fallback`: the fill goes on as well as it can and the
//!   block's fade (or the next block's) closes it at the target.
//! * `exact` (the old `end_at`): the last item is chosen and started so that its cue-out lands on the
//!   target. It starts at a hard time, and the item before it is cut with a 2 s cosine fade when its own
//!   end falls later.
//!
//! Not built: an early exit at a natural low point and a short fade of the closing item. The tolerance and
//! the choice of the closing song close the gap; what remains is the fade.

use crate::catalog::{Catalog, TrackMeta};
use crate::deck::Unit;
use crate::programming::{BackMode, BackTime, Block, PlayBlock};
use crate::sched::{BackStatus, DeckRef, Origin, QEntry, Scheduler, Timing};
use crate::timed::{LogRow, TimedTag};

/// The planner does not look further ahead than this.
pub const HORIZON_MS: i64 = 25 * 60_000;
/// Closing items considered per step, and the work of one search.
const CANDIDATES: usize = 24;
const BUDGET: usize = 400_000;
/// A slot that starts this close to the target is not filled: nothing useful fits.
const NOTHING_FITS_MS: i64 = 8_000;
const ON_TARGET_MS: i64 = 500;

struct Best {
    cost: f64,
    chain: Vec<usize>,
    end: i64,
    hard_at: Option<i64>,
}

struct Search<'a> {
    metas: Vec<&'a TrackMeta>,
    timing: &'a dyn Timing,
    target: i64,
    exact: bool,
    budget: usize,
    best: Option<Best>,
}

impl Search<'_> {
    fn cost(
        &self,
        prev: Option<(&TrackMeta, i64)>,
        start: i64,
        last: &TrackMeta,
    ) -> (f64, i64, Option<i64>) {
        let end = start + last.play_ms;
        if !self.exact {
            return (((end - self.target).abs()) as f64, end, None);
        }
        // The closing item must start at `h` to end on the target.
        let h = self.target - last.play_ms;
        let excess = start - h;
        let cost = if excess >= 0 {
            // The item before it is cut (2 s fade) unless the item before it ends right there; the cut
            // needs room: it cannot start before the fade has run.
            let room = prev.map_or(i64::MAX, |(_, ps)| h - ps);
            if room < 2_000 {
                1e9 + excess as f64
            } else {
                excess as f64
            }
        } else {
            // Starting later than the natural boundary leaves a gap of silence.
            1e6 + (-excess) as f64 * 20.0
        };
        (cost, self.target, Some(h))
    }

    #[allow(clippy::too_many_arguments)]
    fn go(
        &mut self,
        prev: Option<(&TrackMeta, i64)>,
        depth: usize,
        chain: &mut Vec<usize>,
        slot_start: i64,
    ) {
        for i in 0..self.metas.len() {
            if self.budget == 0 {
                return;
            }
            self.budget -= 1;
            if chain.contains(&i) {
                continue;
            }
            let y = self.metas[i];
            let start = match prev {
                Some((p, ps)) => ps + self.timing.advance_ms(p, y, false),
                None => slot_start,
            };
            chain.push(i);
            let (cost, end, hard_at) = self.cost(prev, start, y);
            let better = match &self.best {
                None => true,
                Some(b) => {
                    cost < b.cost - 1.0 || (cost <= b.cost + 1.0 && chain.len() < b.chain.len())
                }
            };
            if better {
                self.best = Some(Best {
                    cost,
                    chain: chain.clone(),
                    end,
                    hard_at,
                });
            }
            // Longer chains only while the item does not already overshoot the target by more than a song.
            if depth > 1 && start + y.effective_ms < self.target {
                self.go(Some((y, start)), depth - 1, chain, slot_start);
            }
            chain.pop();
        }
    }
}

impl Scheduler {
    /// The end time of a `play` block with back-timing: its `until` time, or the next timed flow block's.
    pub(crate) fn back_time_target(
        &self,
        pl: &crate::programming::Playlist,
        fi: usize,
        pb: &PlayBlock,
        since_ms: i64,
        t0: i64,
    ) -> Option<i64> {
        use crate::programming::{Entry, Until};
        match &pb.until {
            Until::At(spec) => crate::program::spec_instant(spec, &self.cfg.zone, t0, since_ms),
            Until::Duration(d) => Some(since_ms + d * 1000),
            _ => pl.entries.iter().skip(fi + 1).find_map(|e| match e {
                Entry::Block(b) if b.enabled && b.is_timed_flow() => {
                    b.timed.as_ref().and_then(|t| {
                        crate::program::spec_instant(&t.spec, &self.cfg.zone, t0, since_ms)
                    })
                }
                _ => None,
            }),
        }
    }

    /// Deal the next entry of a back-timed fill (see the module docs). `None` when the plan says nothing
    /// more fits (the closing item is already dealt), or the source has nothing playable.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn back_time_deal(
        &mut self,
        block: &Block,
        pb: &PlayBlock,
        bt: BackTime,
        entry_idx: usize,
        target: i64,
        queue: &[QEntry],
        slot_start: i64,
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> Option<Vec<QEntry>> {
        let program = self.st.program.clone()?;
        let exact = bt.mode == BackMode::Exact;
        let tol_ms = (bt.tolerance_s * 1000.0) as i64;
        let mode = if exact { "exact" } else { "best_fit" };
        let mut status = BackStatus {
            block_id: block.id,
            name: block.name.clone(),
            target_ms: target,
            planned_end_ms: slot_start,
            state: "planning".into(),
            mode: mode.into(),
            closed: false,
        };
        // Already closed by the previous deal, or nothing useful starts this close to the target.
        if program
            .bt
            .as_ref()
            .is_some_and(|b| b.closed && b.block_id == block.id && b.target_ms == target)
        {
            return None;
        }
        if target - slot_start < NOTHING_FITS_MS {
            if let Some(p) = self.st.program.as_mut() {
                if let Some(b) = p.bt.as_mut() {
                    b.closed = true;
                }
                p.fill = None;
            }
            return None;
        }
        if target - slot_start > HORIZON_MS {
            // Too far to plan: the rotation deals as it always does.
            status.state = "planning".into();
            self.st.bt_display = Some(status.clone());
            if let Some(p) = self.st.program.as_mut() {
                p.bt = Some(status);
            }
            return self.deal_fill(&pb.source, queue, slot_start, cat);
        }
        let (key, pool, pid) = self.source_pool(&pb.source, cat)?;
        let rules_pl = pid.and_then(|p| self.prog.playlists.get(&p)).cloned();
        let k = self
            .rotation_of(
                &pool,
                rules_pl.as_ref().map_or(0, |p| p.no_repeat_minutes),
                cat,
            )
            .k;
        let mut deck = self.st.decks.remove(&key).unwrap_or_default();
        if !deck.prepare(&pool, k, &mut self.st.rng) {
            self.st.decks.insert(key, deck);
            return None;
        }
        let remaining = deck.remaining();
        let chain_recs = self.chain(queue, cat);
        let tail = self.tail(queue, cat);
        // The units a slot at `slot_start` may take under the playlist's rules, relaxing level by level.
        let (eff_ms, sep) = match &rules_pl {
            Some(p) => {
                let r = self.rotation_of(&pool, p.no_repeat_minutes, cat);
                let e = (r.effective_minutes * 60_000.0).round() as i64;
                (
                    if e > 0 {
                        e + crate::sched::PREDICTION_SLACK_MS
                    } else {
                        e
                    },
                    p.separation.clone(),
                )
            }
            None => (0, Default::default()),
        };
        let horizon = slot_start
            - eff_ms
                .max(sep.title_minutes * 60_000)
                .max(sep.artist_minutes * 60_000)
                .max(sep.album_minutes * 60_000);
        let recent: Vec<&crate::sched::PlayRec> = chain_recs
            .recs
            .iter()
            .filter(|r| r.start_ms > horizon - 1 && r.start_ms <= slot_start)
            .collect();
        let mut off_pass: Vec<Unit> = pool
            .iter()
            .map(|(u, _)| u.clone())
            .filter(|u| !remaining.contains(u))
            .collect();
        let last_play = |u: &Unit| -> i64 {
            let Some(first) = Self::unit_first_item(cat, u) else {
                return i64::MIN;
            };
            chain_recs
                .recs
                .iter()
                .rev()
                .find(|r| r.item_id == first)
                .map_or(i64::MIN, |r| r.start_ms)
        };
        off_pass.sort_by_key(&last_play);
        let eligible = |u: &Unit, level: u8| -> bool {
            let Some(first) = Self::unit_first_item(cat, u) else {
                return false;
            };
            let Some(m) = cat.meta(first) else {
                return false;
            };
            let within = |r: &&crate::sched::PlayRec, min: i64| {
                min > 0 && slot_start - r.start_ms < min * 60_000
            };
            if level < 4
                && eff_ms > 0
                && recent
                    .iter()
                    .any(|r| r.item_id == first && slot_start - r.start_ms < eff_ms)
            {
                return false;
            }
            if level < 3
                && recent
                    .iter()
                    .any(|r| r.title_key == m.title_key() && within(r, sep.title_minutes))
            {
                return false;
            }
            if level < 2
                && !m.artist.is_empty()
                && recent.iter().any(|r| {
                    r.artist == crate::catalog::norm(&m.artist) && within(r, sep.artist_minutes)
                })
            {
                return false;
            }
            if level < 1
                && !m.album_key.is_empty()
                && recent
                    .iter()
                    .any(|r| r.album_key == m.album_key && within(r, sep.album_minutes))
            {
                return false;
            }
            true
        };
        let mut chosen: Option<(Best, Vec<Unit>, u8)> = None;
        for level in 0..=4u8 {
            let cands: Vec<(Unit, &TrackMeta)> = remaining
                .iter()
                .chain(off_pass.iter())
                .filter(|u| eligible(u, level))
                .filter_map(|u| match u {
                    Unit::Item(i) => cat.meta(*i).map(|m| (u.clone(), m)),
                    Unit::Album(_) => None,
                })
                .take(CANDIDATES)
                .collect();
            if cands.len() < 2 && level < 4 {
                continue;
            }
            if cands.is_empty() {
                continue;
            }
            let mut s = Search {
                metas: cands.iter().map(|(_, m)| *m).collect(),
                timing,
                target,
                exact,
                budget: BUDGET,
                best: None,
            };
            let mut ch = Vec::new();
            s.go(tail, 4, &mut ch, slot_start);
            if let Some(b) = s.best {
                let units: Vec<Unit> = b.chain.iter().map(|i| cands[*i].0.clone()).collect();
                let good = if exact {
                    b.cost < 1e6
                } else {
                    (b.cost as i64) <= tol_ms
                };
                let take = good || chosen.is_none();
                if take {
                    chosen = Some((b, units, level));
                }
                if good {
                    break;
                }
            }
        }
        let Some((best, units, level)) = chosen else {
            self.st.decks.insert(key, deck);
            return None;
        };
        let first_unit = units[0].clone();
        let Unit::Item(first_id) = first_unit.clone() else {
            self.st.decks.insert(key, deck);
            return None;
        };
        let on_pass = remaining.contains(&first_unit);
        let pick = if on_pass {
            deck.take_unit(&first_unit, level, k)
        } else {
            None
        };
        self.st.decks.insert(key.clone(), deck);
        if level > 0 {
            self.relaxed_picks += 1;
        }
        let closing = best.chain.len() == 1;
        let delta = best.end - target;
        status.planned_end_ms = best.end;
        status.state = if delta.abs() <= ON_TARGET_MS {
            "on_target"
        } else if delta.abs() <= tol_ms {
            "within_tolerance"
        } else {
            "fallback"
        }
        .into();
        status.closed = closing;
        let mut e = QEntry::new(self.take_id(), first_id, Origin::Rotation);
        e.playlist_id = Some(program.playlist_id);
        e.block_id = Some(block.id);
        if let Some(pick) = pick {
            e.deck = Some(DeckRef {
                key,
                gen: pick.gen,
                seq: pick.seq,
            });
        } else {
            e.flags.push("off_pass".into());
        }
        e.flags.push("bt".into());
        if level > 0 {
            e.flags.push("rule_relaxed".into());
        }
        if closing && exact {
            if let Some(h) = best.hard_at {
                e.flags.push("bt_exact".into());
                let _ = entry_idx;
                e.timed = Some(TimedTag {
                    block_id: block.id,
                    run_id: Some(program.run_id),
                    name: block.name.clone(),
                    kind: "back_time".into(),
                    target_ms: target,
                    lo_ms: h,
                    hi_ms: h,
                    mode: crate::programming::BoundaryMode::Hard,
                    hard_at_ms: Some(h),
                    late: false,
                    entry_idx: None,
                    depth: None,
                });
            }
        }
        if closing {
            e.flags.push("bt_final".into());
        }
        // The log: one row per recomputation, keyed by the slot it was made for.
        let row = LogRow {
            run_id: Some(program.run_id),
            block_id: block.id,
            name: block.name.clone(),
            kind: "back_time".into(),
            target_ms: slot_start,
            planned_ms: best.end,
            fired_ms: Some(now_ms.max(slot_start.min(now_ms))),
            how: "planned".into(),
            deviation_ms: Some(delta),
            detail: format!(
                "{{\"mode\":\"{mode}\",\"target\":{target},\"chain\":{},\"state\":\"{}\"}}",
                best.chain.len(),
                status.state
            ),
        };
        self.st.log.push(row);
        self.st.bt_display = Some(status.clone());
        if let Some(p) = self.st.program.as_mut() {
            p.bt = Some(status);
            if closing {
                // Nothing more is dealt from this fill; the closing item plays out.
                p.fill = None;
            }
        }
        Some(vec![e])
    }
}
