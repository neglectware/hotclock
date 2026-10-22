//! The block engine's cursor (architecture 4.4, 4.5, 4.9; milestone M5): how a `sequential` playlist is
//! walked, one slot at a time, by the scheduler that deals the queue ahead of the clock.
//!
//! The cursor stands at an entry of the playlist (`seq_pos`). Asked for the next slot (`slot_start`, the
//! planned wall time at which that slot begins), it moves over what takes no time and stops at what does:
//!
//! * an **item** or **album** is dealt; a **`play`** block starts its source filling (a rotation, or a nested
//!   playlist), and either lets the cursor move on at once (`until: next`: the source keeps playing until a
//!   later block takes over) or keeps it there until the end condition is met (`forever`, a time, a
//!   duration, a count);
//! * a **`wait`** holds the cursor until a time, a duration or the operator's Continue; while it holds, the
//!   fill goes on being dealt, so the music continues through a wait;
//! * a **timed flow block** holds the cursor until its time (the timed engine of `timed.rs` acts at the
//!   time and moves the cursor past it);
//! * a **side block** (`osc_out`, `midi_out`, `http_request`, `set_gain`, `set_mute`) leaves an act that the
//!   station carries out at the boundary where the previous step ended;
//! * an in-sequence **`stop`** or **`fade_stop`** leaves such an act too and keeps the cursor from dealing
//!   anything until the audio has really stopped (`hold_until_ms`); the next item dealt then restarts it.
//!
//! The cursor is part of [`ProgramRun`]; every entry dealt carries a snapshot of it (`prog_after`) so that
//! a regeneration of the soft plan rewinds it to the last entry that stays.

use crate::catalog::Catalog;
use crate::programming::{
    Anchor, Block, BlockKind, Entry, Playlist, Programming, Source, StopHow, Trigger, Until,
    WaitUntil,
};
use crate::sched::{Hold, Origin, ProgramRun, QEntry, Scheduler, SeqAct, SeqKind, Timing};
use common::timespec::{TimeSpec, Zone};
use library::playlists::Mode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// The state of one occurrence of a `manual` start block (5.3 `start_arm`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArmRec {
    pub block_id: i64,
    /// The nominal time of the occurrence, ms.
    pub occurrence_ms: i64,
    pub armed: bool,
    pub consumed: bool,
    /// The fallback auto-start fired for it.
    #[serde(default)]
    pub fired: bool,
}

/// The index of the first entry that is not the playlist's `start` block.
pub fn first_entry(pl: &Playlist) -> usize {
    pl.entries
        .iter()
        .position(|e| {
            matches!(
                e,
                Entry::Block(Block {
                    kind: BlockKind::Start(_),
                    ..
                })
            )
        })
        .map_or(0, |i| i + 1)
}

/// The one instant of a time that is not a recurrence, or the first one at or after `since` of a
/// recurrence. `t0` is the level's own start.
pub fn spec_instant(spec: &TimeSpec, zone: &Zone, t0: i64, since: i64) -> Option<i64> {
    if spec.is_recurring() {
        spec.first_after(zone, Some(t0), since - 1, 400 * 86_400_000)
    } else {
        spec.occurrences(zone, Some(t0), i64::MIN / 4, i64::MAX / 4)
            .first()
            .copied()
    }
}

/// Does the playlist walk as a nested level (a cursor of its own) when a `play` block names it?
pub fn nestable(pl: &Playlist) -> bool {
    pl.mode == Mode::Sequential
}

/// What stopped the cursor this round.
enum Step {
    /// The cursor moved over something that takes no time; look again.
    Again,
    /// Deal the active fill (if any); nothing else may be dealt now.
    Hold,
    /// These entries.
    Deal(Vec<QEntry>),
}

impl Scheduler {
    /// A snapshot of the cursor for the entries about to be returned.
    pub(crate) fn snap(&self) -> Option<Box<ProgramRun>> {
        self.st.program.clone().map(Box::new)
    }

    fn bump_epoch(&mut self) {
        if let Some(p) = self.st.program.as_mut() {
            p.epoch += 1;
        }
    }

    /// The level above the current one finished its `play`: the cursor returns to it, past the block.
    fn pop_frame(&mut self) -> bool {
        let Some(p) = self.st.program.as_mut() else {
            return false;
        };
        let Some(parent) = p.stack.pop() else {
            return false;
        };
        p.load_cursor(parent);
        // The nested play is over: the parent moves past its block.
        p.seq_pos += 1;
        p.hold = None;
        p.fill = None;
        true
    }

    /// Unwind to `depth` levels in the stack (0 is the outermost), leaving the cursor at the `play` block of
    /// that level (not past it): the parent's own end condition decides what follows.
    pub(crate) fn unwind_to(&mut self, depth: usize) {
        let Some(p) = self.st.program.as_mut() else {
            return;
        };
        while p.stack.len() > depth {
            let Some(parent) = p.stack.pop() else { break };
            p.load_cursor(parent);
            p.fill = None;
        }
        self.ev_cache = crate::timed::EventCache::default();
    }

    fn take_over(&mut self) {
        if let Some(p) = self.st.program.as_mut() {
            p.fill = None;
        }
    }

    /// Is the end condition of the `play` at the cursor met at `slot_start`?
    fn play_release_due(
        &self,
        pb: &crate::programming::PlayBlock,
        hold: &Hold,
        t0: i64,
        slot_start: i64,
        closed: bool,
    ) -> bool {
        // A back-timed fill whose closing item is dealt is over: what follows takes the next slot.
        if closed && pb.end == crate::programming::EndMode::Natural {
            return true;
        }
        match &pb.until {
            Until::Next | Until::Forever => false,
            Until::Count(n) => hold.count >= *n,
            Until::Duration(d) => slot_start >= hold.since_ms + d * 1000,
            Until::At(spec) => spec_instant(spec, &self.cfg.zone, t0, hold.since_ms)
                .is_some_and(|t| slot_start >= t),
        }
    }

    /// The wall time at which a `play` with an end time ends, when it has one.
    pub(crate) fn play_end_ms(
        &self,
        pb: &crate::programming::PlayBlock,
        hold: &Hold,
        t0: i64,
    ) -> Option<i64> {
        match &pb.until {
            Until::Duration(d) => Some(hold.since_ms + d * 1000),
            Until::At(spec) => spec_instant(spec, &self.cfg.zone, t0, hold.since_ms),
            _ => None,
        }
    }

    /// Record an act of an in-sequence block.
    fn push_seq_act(&mut self, b: &Block, kind: SeqKind, queue: &[QEntry], slot_start: i64) {
        let Some(pr) = self.st.program.as_ref() else {
            return;
        };
        let after = queue
            .last()
            .map(|e| e.id)
            .or_else(|| self.st.on_air.as_ref().map(|e| e.id));
        let visit: u32 = pr.loops.values().sum();
        let a = SeqAct {
            block_id: b.id,
            run_id: pr.run_id,
            name: b.name.clone(),
            kind,
            after_entry: after,
            before_entry: None,
            at_ms: slot_start,
            fixed: false,
            visit,
        };
        self.st.seq_acts.push(a);
    }

    /// The next slot of a `sequential` program: the interpreter of the module docs.
    pub(crate) fn generate_sequential(
        &mut self,
        _pl: &Playlist,
        cat: &Catalog,
        queue: &[QEntry],
        slot_start: i64,
        now_ms: i64,
        timing: &dyn Timing,
    ) -> Option<Vec<QEntry>> {
        let acts_before = self.st.seq_acts.len();
        let mut out = self.walk(cat, queue, slot_start, now_ms, timing)?;
        // The acts made on the way belong between the entry before and the first one dealt now.
        if let Some(first) = out.first() {
            let id = first.id;
            for a in self.st.seq_acts.iter_mut().skip(acts_before) {
                a.before_entry = Some(id);
            }
        }
        let silenced = self.st.program.as_ref().is_some_and(|p| p.silenced);
        let snap = self.snap();
        for (i, e) in out.iter_mut().enumerate() {
            if i == 0 && silenced {
                e.flags.push("seq_start".into());
            }
            e.prog_after = snap.clone();
        }
        if silenced {
            if let Some(p) = self.st.program.as_mut() {
                p.silenced = false;
            }
            for e in &mut out {
                if let Some(s) = e.prog_after.as_mut() {
                    s.silenced = false;
                }
            }
        }
        Some(out)
    }

    fn walk(
        &mut self,
        cat: &Catalog,
        queue: &[QEntry],
        slot_start: i64,
        now_ms: i64,
        timing: &dyn Timing,
    ) -> Option<Vec<QEntry>> {
        let prog: Arc<Programming> = self.prog.clone();
        let mut guard = 0u32;
        loop {
            guard += 1;
            if guard > 5000 {
                tracing::warn!("the cursor made 5000 moves without dealing; holding");
                return self.deal_hold(cat, queue, slot_start, now_ms, timing);
            }
            let pr = self.st.program.clone()?;
            let pl = prog.playlists.get(&pr.playlist_id)?;
            // Silence after an in-sequence stop: nothing is dealt until it is over.
            if let Some(h) = pr.hold_until_ms {
                if slot_start.max(now_ms) < h {
                    return None;
                }
                if let Some(p) = self.st.program.as_mut() {
                    p.hold_until_ms = None;
                }
            }
            // A parent's end condition ends the levels nested in it.
            if !pr.stack.is_empty() {
                if let Some(depth) = self.ancestor_released(&prog, &pr, slot_start) {
                    self.unwind_to(depth);
                    self.finish_play_at_cursor(&prog, slot_start);
                    self.bump_epoch();
                    continue;
                }
            }
            let step = self.step(&prog, pl, &pr, cat, queue, slot_start, now_ms, timing);
            match step {
                Step::Again => continue,
                Step::Hold => return self.deal_hold(cat, queue, slot_start, now_ms, timing),
                Step::Deal(v) => return Some(v),
            }
        }
    }

    /// The index in the stack of the outermost ancestor whose `play` has met its end condition.
    fn ancestor_released(
        &self,
        prog: &Programming,
        pr: &ProgramRun,
        slot_start: i64,
    ) -> Option<usize> {
        for (depth, c) in pr.stack.iter().enumerate() {
            let pl = prog.playlists.get(&c.playlist_id)?;
            let Some(Entry::Block(Block {
                kind: BlockKind::Play(pb),
                ..
            })) = pl.entries.get(c.seq_pos)
            else {
                continue;
            };
            if let Some(h) = &c.hold {
                if self.play_release_due(pb, h, c.t0_ms, slot_start, false) {
                    return Some(depth);
                }
            }
        }
        None
    }

    /// The cursor stands at a `play` whose condition is met: it moves on, and the source stops (the
    /// following block, or the end of the program, takes the next slot).
    fn finish_play_at_cursor(&mut self, prog: &Programming, slot_start: i64) {
        let Some(pr) = self.st.program.clone() else {
            return;
        };
        let mut fade: Option<(f64, i64)> = None;
        if let Some(Entry::Block(b)) = prog
            .playlists
            .get(&pr.playlist_id)
            .and_then(|pl| pl.entries.get(pr.seq_pos))
        {
            if let (BlockKind::Play(pb), Some(h)) = (&b.kind, &pr.hold) {
                let late_natural = pb.end == crate::programming::EndMode::Natural
                    && pr.bt.as_ref().is_some_and(|b| {
                        b.block_id == b_id_of(b_ref(pl_for_fade(prog, &pr), pr.seq_pos))
                            && b.state == "fallback"
                    });
                if pb.end == crate::programming::EndMode::Fade || late_natural {
                    if let Some(t) = self.play_end_ms(pb, h, pr.t0_ms) {
                        fade = Some((pb.fade_s, t));
                    }
                }
                if let Some(t) = fade {
                    // A master-gain fade at the end time, then silence (4.5).
                    let ms = (t.0 * 1000.0).round() as i64;
                    let id = b.id;
                    let name = b.name.clone();
                    let run_id = pr.run_id;
                    let after = self.st.on_air.as_ref().map(|e| e.id);
                    self.st.seq_acts.push(SeqAct {
                        block_id: id,
                        run_id,
                        name,
                        kind: SeqKind::FadeStop { ms },
                        after_entry: after,
                        before_entry: None,
                        at_ms: t.1,
                        fixed: true,
                        visit: 0,
                    });
                }
            }
        }
        if let Some(p) = self.st.program.as_mut() {
            p.seq_pos += 1;
            p.hold = None;
            p.fill = None;
            if let Some((_, t)) = fade {
                // Nothing may be dealt until the fade is over; the next item restarts the audio.
                let ms = fade.map_or(0, |f| (f.0 * 1000.0).round() as i64);
                p.hold_until_ms = Some(t + ms);
                p.silenced = true;
            }
        }
        let _ = slot_start;
    }

    /// Deal the active fill, if there is one (the cursor is holding).
    fn deal_hold(
        &mut self,
        cat: &Catalog,
        queue: &[QEntry],
        slot_start: i64,
        now_ms: i64,
        timing: &dyn Timing,
    ) -> Option<Vec<QEntry>> {
        let pr = self.st.program.clone()?;
        let fi = pr.fill?;
        let prog = self.prog.clone();
        let pl = prog.playlists.get(&pr.playlist_id)?;
        let Some(Entry::Block(Block {
            kind: BlockKind::Play(pb),
            ..
        })) = pl.entries.get(fi)
        else {
            return None;
        };
        let bt_block = match pl.entries.get(fi) {
            Some(Entry::Block(b)) => Some(b),
            _ => None,
        };
        let mut out = match (
            pb.end == crate::programming::EndMode::Natural,
            pb.back_time,
            bt_block,
        ) {
            (true, Some(bt), Some(block)) => {
                let since = pr.hold.as_ref().map_or(slot_start, |h| h.since_ms);
                match self.back_time_target(pl, fi, pb, since, pr.t0_ms) {
                    Some(target) => self.back_time_deal(
                        block, pb, bt, fi, target, queue, slot_start, now_ms, cat, timing,
                    )?,
                    None => self.deal_fill(&pb.source, queue, slot_start, cat)?,
                }
            }
            _ => self.deal_fill(&pb.source, queue, slot_start, cat)?,
        };
        for e in &mut out {
            e.playlist_id = Some(pr.playlist_id);
        }
        // `until: count` counts what was dealt; the last one dealt ends the block.
        let mut done = false;
        if let Some(p) = self.st.program.as_mut() {
            if let (
                Some(h),
                Some(Entry::Block(Block {
                    kind: BlockKind::Play(pb),
                    ..
                })),
            ) = (p.hold.as_mut(), pl.entries.get(p.seq_pos))
            {
                h.count += 1;
                if let Until::Count(n) = pb.until {
                    done = h.count >= n;
                }
            }
        }
        if done {
            if let Some(p) = self.st.program.as_mut() {
                p.seq_pos += 1;
                p.hold = None;
                p.fill = None;
            }
        }
        Some(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn step(
        &mut self,
        prog: &Programming,
        pl: &Playlist,
        pr: &ProgramRun,
        cat: &Catalog,
        queue: &[QEntry],
        slot_start: i64,
        now_ms: i64,
        timing: &dyn Timing,
    ) -> Step {
        let pos = pr.seq_pos;
        // The cursor ran off the end of the playlist.
        if pos >= pl.entries.len() {
            if pr.fill.is_some() {
                // A fill with nothing after it goes on until something else takes over.
                return Step::Hold;
            }
            if !pr.stack.is_empty() {
                self.pop_frame();
                self.bump_epoch();
                return Step::Again;
            }
            return self.program_ended(cat, queue, slot_start, now_ms, timing);
        }
        let entry = &pl.entries[pos];
        let set_pos = |s: &mut Scheduler, to: usize| {
            if let Some(p) = s.st.program.as_mut() {
                p.seq_pos = to;
            }
        };
        match entry {
            Entry::Item { item_id, .. } => {
                set_pos(self, pos + 1);
                if !cat.meta(*item_id).is_some_and(|m| m.playable) {
                    return Step::Again;
                }
                self.take_over();
                let mut e = QEntry::new(self.take_id(), *item_id, Origin::Rotation);
                e.playlist_id = Some(pl.id);
                Step::Deal(vec![e])
            }
            Entry::Album {
                album_key, gapless, ..
            } => {
                set_pos(self, pos + 1);
                let mut v = self.expand_album(album_key, *gapless, Origin::Rotation, cat);
                if v.is_empty() {
                    return Step::Again;
                }
                self.take_over();
                for e in &mut v {
                    e.playlist_id = Some(pl.id);
                }
                Step::Deal(v)
            }
            Entry::Block(b) => {
                if !b.enabled {
                    set_pos(self, pos + 1);
                    return Step::Again;
                }
                // A `play` with an end condition that is being waited for.
                if let (BlockKind::Play(pb), Some(h)) = (&b.kind, &pr.hold) {
                    let closed = pr
                        .bt
                        .as_ref()
                        .is_some_and(|x| x.closed && x.block_id == b.id);
                    if self.play_release_due(pb, h, pr.t0_ms, slot_start, closed) {
                        self.finish_play_at_cursor(prog, slot_start);
                        self.bump_epoch();
                        return Step::Again;
                    }
                    return Step::Hold;
                }
                match &b.kind {
                    BlockKind::InsertEvery(_) | BlockKind::Start(_) => {
                        set_pos(self, pos + 1);
                        return Step::Again;
                    }
                    _ => {}
                }
                if b.timed.is_some() {
                    if b.kind.is_flow() {
                        // Reached early: the cursor holds; the timed engine acts at its time (4.4).
                        return Step::Hold;
                    }
                    // A timed side block never moves the cursor.
                    set_pos(self, pos + 1);
                    return Step::Again;
                }
                debug_assert!(b.trigger != Trigger::Timed);
                self.in_sequence(prog, pl, pr, b, pos, cat, queue, slot_start, now_ms, timing)
            }
        }
    }

    /// An in-sequence block at the cursor.
    #[allow(clippy::too_many_arguments)]
    fn in_sequence(
        &mut self,
        prog: &Programming,
        pl: &Playlist,
        pr: &ProgramRun,
        b: &Block,
        pos: usize,
        cat: &Catalog,
        queue: &[QEntry],
        slot_start: i64,
        now_ms: i64,
        timing: &dyn Timing,
    ) -> Step {
        let advance = |s: &mut Scheduler| {
            if let Some(p) = s.st.program.as_mut() {
                p.seq_pos = pos + 1;
            }
        };
        match &b.kind {
            BlockKind::Insert { source } => {
                advance(self);
                let Some((key, pool, _)) = self.source_pool(source, cat) else {
                    return Step::Again;
                };
                if pool.is_empty() {
                    return Step::Again;
                }
                let chain = self.chain(queue, cat);
                let Some((pick, _)) = self.deal(&key, &pool, None, &chain, slot_start, cat) else {
                    return Step::Again;
                };
                let crate::deck::Unit::Item(id) = pick.unit else {
                    return Step::Again;
                };
                let mut e = QEntry::new(self.take_id(), id, Origin::Insert);
                e.block_id = Some(b.id);
                e.playlist_id = Some(pl.id);
                e.deck = Some(crate::sched::DeckRef {
                    key,
                    gen: pick.gen,
                    seq: pick.seq,
                });
                Step::Deal(vec![e])
            }
            BlockKind::Side(a) => {
                advance(self);
                self.push_seq_act(b, SeqKind::Side(a.clone()), queue, slot_start);
                Step::Again
            }
            BlockKind::SetGain { gain_db, ramp_ms } => {
                advance(self);
                self.push_seq_act(
                    b,
                    SeqKind::Gain {
                        gain_db: *gain_db,
                        ramp_ms: *ramp_ms,
                    },
                    queue,
                    slot_start,
                );
                Step::Again
            }
            BlockKind::SetMute { on, ramp_ms } => {
                advance(self);
                self.push_seq_act(
                    b,
                    SeqKind::Mute {
                        on: *on,
                        ramp_ms: *ramp_ms,
                    },
                    queue,
                    slot_start,
                );
                Step::Again
            }
            BlockKind::Wait(w) => {
                let since = pr.wait_since.unwrap_or(slot_start);
                let done = match w {
                    WaitUntil::Operator => pr.released,
                    WaitUntil::DurationS(d) => slot_start >= since + d * 1000,
                    WaitUntil::At(spec) => spec_instant(spec, &self.cfg.zone, pr.t0_ms, since)
                        .is_none_or(|t| slot_start >= t),
                };
                if let Some(p) = self.st.program.as_mut() {
                    if done {
                        p.seq_pos = pos + 1;
                        p.wait_since = None;
                        p.released = false;
                        self.log_wait_released(b, pr.run_id, slot_start, pr.released);
                    } else {
                        p.wait_since = Some(since);
                    }
                }
                if done {
                    Step::Again
                } else {
                    Step::Hold
                }
            }
            BlockKind::Loop { to, times } => {
                let first = crate::program::first_entry(pl);
                let target = to.map_or(first, |t| t.saturating_sub(1));
                let jumps = pr.loops.get(&pos).copied().unwrap_or(0);
                let again = times.is_none_or(|n| jumps + 1 < n) && target < pos;
                if let Some(p) = self.st.program.as_mut() {
                    if again {
                        p.loops.insert(pos, jumps + 1);
                        // Loops inside the body start counting again on the next pass.
                        p.loops.retain(|k, _| *k >= pos || *k < target);
                        p.seq_pos = target;
                        p.wait_since = None;
                    } else {
                        p.loops.remove(&pos);
                        p.seq_pos = pos + 1;
                    }
                }
                Step::Again
            }
            BlockKind::End => {
                // The playlist ends here.
                if let Some(p) = self.st.program.as_mut() {
                    p.seq_pos = pl.entries.len();
                    p.fill = None;
                    p.hold = None;
                }
                Step::Again
            }
            BlockKind::StartPlaylist { playlist_id, how } => {
                advance(self);
                let Some(np) = prog.playlists.get(playlist_id) else {
                    return Step::Again;
                };
                if !np.enabled || self.playable_units(np, cat).is_empty() {
                    return Step::Again;
                }
                self.start_program_with(*playlist_id, slot_start, "block", None);
                self.take_over();
                if self.end_depth >= 2 {
                    return Step::Hold;
                }
                self.end_depth += 1;
                let v = self.generate_normal(queue, now_ms, cat, timing, true);
                self.end_depth -= 1;
                match v {
                    Some(mut v) => {
                        if *how == crate::programming::StartHow::End {
                            if let Some(first) = v.first_mut() {
                                first.transition = Some(crate::sched::END_TRANSITION.into());
                            }
                        }
                        Step::Deal(v)
                    }
                    None => Step::Hold,
                }
            }
            BlockKind::Stop { how, fade_ms } => {
                advance(self);
                self.take_over();
                match how {
                    StopHow::Now => {
                        self.push_seq_act(
                            b,
                            SeqKind::Stop { fade_ms: *fade_ms },
                            queue,
                            slot_start,
                        );
                        self.silence_until(slot_start + fade_ms);
                    }
                    StopHow::AfterCurrent | StopHow::Boundary => {
                        self.push_seq_act(
                            b,
                            SeqKind::Hold {
                                hard_after_30s: *how == StopHow::Boundary,
                                fade_ms: *fade_ms,
                            },
                            queue,
                            slot_start,
                        );
                        // The item on air plays out; nothing is dealt before it is over.
                        let until = queue
                            .last()
                            .and_then(|e| e.planned_segue_ms)
                            .unwrap_or(slot_start)
                            .max(slot_start);
                        self.silence_until(until + 1);
                    }
                }
                self.bump_epoch();
                Step::Again
            }
            BlockKind::FadeStop { seconds } => {
                advance(self);
                self.take_over();
                let ms = (seconds * 1000.0).round() as i64;
                self.push_seq_act(b, SeqKind::FadeStop { ms }, queue, slot_start);
                self.silence_until(slot_start + ms);
                self.bump_epoch();
                Step::Again
            }
            BlockKind::Play(pb) => self.begin_play(prog, pl, b, pb, pos, cat, slot_start),
            BlockKind::Other(_) | BlockKind::InsertEvery(_) | BlockKind::Start(_) => {
                advance(self);
                Step::Again
            }
        }
    }

    fn silence_until(&mut self, t: i64) {
        if let Some(p) = self.st.program.as_mut() {
            p.hold_until_ms = Some(t);
            p.silenced = true;
        }
    }

    /// An in-sequence `play`: its source starts filling.
    #[allow(clippy::too_many_arguments)]
    fn begin_play(
        &mut self,
        prog: &Programming,
        pl: &Playlist,
        _b: &Block,
        pb: &crate::programming::PlayBlock,
        pos: usize,
        _cat: &Catalog,
        slot_start: i64,
    ) -> Step {
        let _ = pl;
        // A nested playlist walks as a level of its own.
        if let Source::Playlist(child) = &pb.source {
            if let Some(cpl) = prog.playlists.get(child) {
                // A back-timed fill plays its source as a rotation: the planner chooses the songs.
                if nestable(cpl) && pb.end != crate::programming::EndMode::Natural {
                    if let Some(p) = self.st.program.as_mut() {
                        if p.stack.len() >= crate::programming::MAX_NESTING - 1 {
                            tracing::warn!("nesting is too deep; the play is skipped");
                            p.seq_pos = pos + 1;
                            return Step::Again;
                        }
                        p.hold = Some(Hold {
                            since_ms: slot_start,
                            count: 0,
                        });
                        p.fill = None;
                        p.stack.push(p.cursor());
                        let c = ProgramRun::fresh_cursor(*child, slot_start, 0);
                        p.load_cursor(c);
                        p.epoch += 1;
                    }
                    self.ev_cache = crate::timed::EventCache::default();
                    return Step::Again;
                }
            }
        }
        if let Some(p) = self.st.program.as_mut() {
            p.fill = Some(pos);
            p.halted = false;
            match pb.until {
                Until::Next => {
                    p.seq_pos = pos + 1;
                    p.hold = None;
                }
                _ => {
                    p.hold = Some(Hold {
                        since_ms: slot_start,
                        count: 0,
                    });
                }
            }
        }
        Step::Again
    }

    /// The playlist ran off its end (or an `end` block ended it) with no level above it: the program is
    /// over and the station goes on with its scheduled program or its default playlist (4.4).
    fn program_ended(
        &mut self,
        cat: &Catalog,
        queue: &[QEntry],
        slot_start: i64,
        now_ms: i64,
        timing: &dyn Timing,
    ) -> Step {
        let Some(pr) = self.st.program.clone() else {
            return Step::Hold;
        };
        // A playlist that ends at once (nothing playable in it) would start itself again without end.
        if self.end_depth >= 2 {
            return Step::Hold;
        }
        let next = self
            .st
            .scheduled_program
            .or(self.cfg.default_playlist)
            .or(Some(pr.playlist_id));
        let Some(next) = next else {
            return Step::Hold;
        };
        self.start_program_with(next, pr.t0_ms, "ended", None);
        let Some(pl2) = self.prog.playlists.get(&next).cloned() else {
            return Step::Hold;
        };
        if pl2.mode == Mode::Sequential && pl2.entries.is_empty() {
            return Step::Hold;
        }
        self.end_depth += 1;
        let dealt = self.generate_normal(queue, now_ms, cat, timing, true);
        self.end_depth -= 1;
        match dealt {
            Some(v) => Step::Deal(v),
            None => {
                let _ = slot_start;
                Step::Hold
            }
        }
    }

    fn log_wait_released(&mut self, b: &Block, run_id: i64, at_ms: i64, by_hand: bool) {
        let row = crate::timed::LogRow {
            run_id: Some(run_id),
            block_id: b.id,
            name: b.name.clone(),
            kind: "wait".into(),
            target_ms: at_ms,
            planned_ms: at_ms,
            fired_ms: Some(at_ms),
            how: "fired".into(),
            deviation_ms: Some(0),
            detail: if by_hand {
                "released by the operator".into()
            } else {
                "the wait ended".into()
            },
        };
        self.st.log.push(row);
    }

    /// Does the cursor stand at a `wait` for the operator?
    pub fn waiting_for_operator(&self) -> Option<&Block> {
        let p = self.st.program.as_ref()?;
        let pl = self.prog.playlists.get(&p.playlist_id)?;
        match pl.entries.get(p.seq_pos)? {
            Entry::Block(b)
                if b.enabled
                    && b.timed.is_none()
                    && matches!(b.kind, BlockKind::Wait(WaitUntil::Operator)) =>
            {
                Some(b)
            }
            _ => None,
        }
    }

    /// The operator's Continue (4.7): releases the `wait` the cursor stands at. `Err` says why not.
    pub fn continue_program(
        &mut self,
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> Result<String, String> {
        let name = match self.waiting_for_operator() {
            Some(b) => b.name.clone(),
            None => return Err("no wait block is waiting for the operator".into()),
        };
        if let Some(p) = self.st.program.as_mut() {
            p.released = true;
            p.epoch += 1;
        }
        self.times_sig = None;
        // What was dealt ahead while the wait held is dealt again from the cursor's new place.
        self.regenerate(now_ms, cat, timing, false);
        Ok(name)
    }

    /// The next wall time at which the cursor itself has something to do (a wait that ends, the end of a
    /// silence), for the virtual-clock driver.
    pub fn cursor_wake_ms(&self, now_ms: i64) -> Option<i64> {
        let p = self.st.program.as_ref()?;
        let mut best: Option<i64> = None;
        let mut put = |t: i64| {
            let t = t.max(now_ms + 1);
            best = Some(best.map_or(t, |b: i64| b.min(t)));
        };
        if let Some(h) = p.hold_until_ms {
            put(h);
        }
        let pl = self.prog.playlists.get(&p.playlist_id)?;
        if let Some(Entry::Block(b)) = pl.entries.get(p.seq_pos) {
            match &b.kind {
                BlockKind::Wait(WaitUntil::DurationS(d)) if b.timed.is_none() => {
                    if let Some(s) = p.wait_since {
                        put(s + d * 1000);
                    }
                }
                BlockKind::Wait(WaitUntil::At(spec)) if b.timed.is_none() => {
                    if let Some(t) = spec_instant(
                        spec,
                        &self.cfg.zone,
                        p.t0_ms,
                        p.wait_since.unwrap_or(now_ms),
                    ) {
                        put(t);
                    }
                }
                BlockKind::Play(pb) => {
                    if let Some(h) = &p.hold {
                        if let Some(t) = self.play_end_ms(pb, h, p.t0_ms) {
                            put(t);
                        }
                    }
                }
                _ => {}
            }
        }
        best
    }

    // ---- the acts of in-sequence blocks --------------------------------------------------------------------

    /// When `a` acts: the planned start of the entry dealt after it, else the planned end of the one before.
    pub(crate) fn seq_act_time(&self, a: &SeqAct, cat: &Catalog) -> i64 {
        if a.fixed {
            return a.at_ms;
        }
        if let Some(b) = a.before_entry {
            if let Some(e) = self.st.queue.iter().find(|e| e.id == b) {
                if let Some(s) = e.planned_start_ms {
                    return s;
                }
            }
            if self.st.on_air.as_ref().is_some_and(|e| e.id == b) {
                if let Some(s) = self.st.on_air_start_ms {
                    return s;
                }
            }
        }
        if let Some(x) = a.after_entry {
            if let Some(e) = self.st.queue.iter().find(|e| e.id == x) {
                if let Some(s) = e.planned_segue_ms {
                    return s;
                }
            }
            if let (Some(e), Some(s)) = (&self.st.on_air, self.st.on_air_start_ms) {
                if e.id == x {
                    if let Some(m) = cat.meta(e.item_id) {
                        return s + m.effective_ms;
                    }
                }
            }
        }
        a.at_ms
    }

    /// The in-sequence acts waiting for their boundary, with the time each is predicted for.
    pub fn pending_seq_acts(&self, cat: &Catalog) -> Vec<(SeqAct, i64)> {
        self.st
            .seq_acts
            .iter()
            .map(|a| (a.clone(), self.seq_act_time(a, cat)))
            .collect()
    }

    /// What the cursor stands at, when it is a block that takes time: a `wait` (with the wall time it
    /// ends at, when it has one) or a `play` that holds. `None` when it stands at a timed block or none.
    pub fn cursor_block(&self, cat: &Catalog) -> Option<CursorBlock> {
        let _ = cat;
        let p = self.st.program.as_ref()?;
        let pl = self.prog.playlists.get(&p.playlist_id)?;
        let Entry::Block(b) = pl.entries.get(p.seq_pos)? else {
            return None;
        };
        if !b.enabled || b.timed.is_some() {
            return None;
        }
        match &b.kind {
            BlockKind::Wait(w) => {
                let (kind, until) = match w {
                    WaitUntil::Operator => ("operator", None),
                    WaitUntil::DurationS(d) => ("duration", p.wait_since.map(|s| s + d * 1000)),
                    WaitUntil::At(spec) => (
                        "time",
                        spec_instant(
                            spec,
                            &self.cfg.zone,
                            p.t0_ms,
                            p.wait_since.unwrap_or(p.t0_ms),
                        ),
                    ),
                };
                Some(CursorBlock {
                    block_id: b.id,
                    name: b.name.clone(),
                    type_name: "wait",
                    wait_kind: Some(kind),
                    until_ms: until,
                    position: p.seq_pos,
                })
            }
            BlockKind::Play(pb) if p.hold.is_some() => Some(CursorBlock {
                block_id: b.id,
                name: b.name.clone(),
                type_name: "play",
                wait_kind: None,
                until_ms: p
                    .hold
                    .as_ref()
                    .and_then(|h| self.play_end_ms(pb, h, p.t0_ms)),
                position: p.seq_pos,
            }),
            _ => None,
        }
    }

    /// Cursor-act is a stop-type (armed ahead of time).
    pub(crate) fn seq_act_is_stop(a: &SeqAct) -> bool {
        matches!(a.kind, SeqKind::Stop { .. } | SeqKind::FadeStop { .. })
    }

    /// The in-sequence acts that are due: removed from the list, logged, and returned as station acts.
    pub(crate) fn due_seq_acts(
        &mut self,
        now_ms: i64,
        ahead_ms: i64,
        cat: &Catalog,
    ) -> Vec<crate::timed::TimedAct> {
        use crate::timed::{ActKind, TimedAct};
        let mut due: Vec<(i64, usize)> = Vec::new();
        for (i, a) in self.st.seq_acts.iter().enumerate() {
            let t = self.seq_act_time(a, cat);
            let ahead = match &a.kind {
                SeqKind::Side(_) => crate::side::SIDE_AHEAD_MS,
                SeqKind::Hold { .. } => 0,
                _ => ahead_ms.max(0),
            };
            if t - now_ms <= ahead {
                due.push((t, i));
            }
        }
        due.sort();
        let mut out = Vec::new();
        let mut remove: Vec<usize> = Vec::new();
        for (t, i) in due {
            let a = self.st.seq_acts[i].clone();
            remove.push(i);
            let kind = match &a.kind {
                SeqKind::Side(s) => ActKind::Side(s.clone()),
                SeqKind::Gain { gain_db, ramp_ms } => ActKind::Gain {
                    gain_db: *gain_db,
                    ramp_ms: *ramp_ms,
                },
                SeqKind::Mute { on, ramp_ms } => ActKind::Mute {
                    on: *on,
                    ramp_ms: *ramp_ms,
                },
                SeqKind::FadeStop { ms } => ActKind::FadeStop { fade_ms: *ms },
                SeqKind::Stop { fade_ms } => ActKind::Stop { fade_ms: *fade_ms },
                SeqKind::Hold { .. } => ActKind::Hold,
            };
            let type_name = match &a.kind {
                SeqKind::Side(s) => s.type_name(),
                SeqKind::Gain { .. } => "set_gain",
                SeqKind::Mute { .. } => "set_mute",
                SeqKind::FadeStop { .. } => "fade_stop",
                SeqKind::Stop { .. } | SeqKind::Hold { .. } => "stop",
            };
            let late = now_ms - t;
            let row = crate::timed::LogRow {
                run_id: Some(a.run_id),
                block_id: a.block_id,
                name: a.name.clone(),
                kind: type_name.into(),
                target_ms: t,
                planned_ms: t,
                fired_ms: Some(t.max(now_ms.min(t + 2000))),
                how: if late > 2000 {
                    "late_run".into()
                } else {
                    "fired".into()
                },
                deviation_ms: Some(late.clamp(0, 2000)),
                detail: "in sequence".into(),
            };
            self.st.log.push(row);
            out.push(TimedAct {
                block_id: a.block_id,
                name: a.name.clone(),
                target_ms: t,
                kind,
            });
            if let SeqKind::Hold {
                hard_after_30s: true,
                fade_ms,
            } = a.kind
            {
                out.push(TimedAct {
                    block_id: a.block_id,
                    name: a.name.clone(),
                    target_ms: t + 30_000,
                    kind: ActKind::HardStop { fade_ms },
                });
            }
        }
        remove.sort_unstable();
        for i in remove.into_iter().rev() {
            self.st.seq_acts.remove(i);
        }
        out
    }

    /// The soonest time an in-sequence act wants the driver's attention.
    pub(crate) fn seq_act_wake_ms(&self, now_ms: i64, ahead_ms: i64, cat: &Catalog) -> Option<i64> {
        self.st
            .seq_acts
            .iter()
            .map(|a| {
                let t = self.seq_act_time(a, cat);
                let ahead = if Self::seq_act_is_stop(a) {
                    ahead_ms
                } else {
                    0
                };
                (t - ahead).max(now_ms + 1)
            })
            .min()
    }
}

/// What a program start asks for as T0 (7.2 `POST /program/start`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum T0Req {
    /// The moment of the request (the Deck button, the web UI, the API): relative blocks shift with it.
    Now,
    /// The nominal time of the scheduled occurrence ("join in progress").
    Nominal,
    At(i64),
}

/// The in-sequence block the cursor stands at, for the status (`GET /program/state`, the queue).
#[derive(Debug, Clone, PartialEq)]
pub struct CursorBlock {
    pub block_id: i64,
    pub name: String,
    pub type_name: &'static str,
    /// `operator`, `duration` or `time` for a wait.
    pub wait_kind: Option<&'static str>,
    pub until_ms: Option<i64>,
    pub position: usize,
}

/// What `GET /program/state` says about the next scheduled start (4.9).
#[derive(Debug, Clone, PartialEq)]
pub struct Expected {
    pub playlist_id: i64,
    pub block_id: i64,
    pub at_ms: i64,
    pub manual: bool,
    pub armed: bool,
    pub fallback_grace_s: Option<i64>,
}

impl Scheduler {
    fn arm_rec(&self, block_id: i64, occ: i64) -> Option<&ArmRec> {
        self.st
            .arms
            .iter()
            .find(|a| a.block_id == block_id && a.occurrence_ms == occ)
    }

    fn arm_rec_mut(&mut self, block_id: i64, occ: i64, default_armed: bool) -> &mut ArmRec {
        if let Some(i) = self
            .st
            .arms
            .iter()
            .position(|a| a.block_id == block_id && a.occurrence_ms == occ)
        {
            return &mut self.st.arms[i];
        }
        self.st.arms.push(ArmRec {
            block_id,
            occurrence_ms: occ,
            armed: default_armed,
            consumed: false,
            fired: false,
        });
        self.st.arms.last_mut().expect("just pushed")
    }

    /// The scheduled occurrence a start at `t0_ms` resolves to for `T0Req::Nominal`: the latest one at or
    /// before `now_ms` within a day, else the next one.
    pub fn nominal_for(&self, playlist_id: i64, now_ms: i64) -> Option<i64> {
        let occ = crate::preview::nominal_occurrences(
            &self.prog,
            playlist_id,
            &self.cfg.zone,
            now_ms - 86_400_000,
            now_ms + 8 * 86_400_000,
        );
        occ.iter()
            .rev()
            .find(|(_, t)| *t <= now_ms)
            .or_else(|| occ.first())
            .map(|(_, t)| *t)
    }

    /// Resolve a T0 request: the T0, where it came from and the occurrence it belongs to.
    pub fn resolve_t0(
        &self,
        playlist_id: i64,
        req: T0Req,
        now_ms: i64,
    ) -> Result<(i64, &'static str, Option<i64>), String> {
        match req {
            T0Req::Now => Ok((now_ms, "api", None)),
            T0Req::At(t) => Ok((t, "api", None)),
            T0Req::Nominal => match self.nominal_for(playlist_id, now_ms) {
                Some(t) => Ok((t, "nominal", Some(t))),
                None => Err("this playlist has no scheduled occurrence to join".into()),
            },
        }
    }

    /// A hand start consumes the occurrences it belongs to: those whose nominal time lies within
    /// `arm_window_min` before the press, or later the same local day (4.9).
    pub fn consume_occurrences(&mut self, playlist_id: i64, press_ms: i64) {
        let Some(pl) = self.prog.playlists.get(&playlist_id) else {
            return;
        };
        let Some((b, sb)) = pl.start_block() else {
            return;
        };
        if sb.anchor != Anchor::Manual {
            return;
        }
        let zone = self.cfg.zone.clone();
        let day_end = {
            // The end of the local day of the press.
            let d = zone.date_string(press_ms);
            let mut t = press_ms;
            while zone.date_string(t) == d && t < press_ms + 48 * 3_600_000 {
                t += 15 * 60_000;
            }
            t
        };
        let from = press_ms - sb.arm_window_min * 60_000;
        let (bid, default_armed) = (b.id, sb.fallback_grace_s.is_some());
        let occ =
            crate::preview::nominal_occurrences(&self.prog, playlist_id, &zone, from - 1, day_end);
        for (_, t) in occ {
            self.arm_rec_mut(bid, t, default_armed).consumed = true;
        }
        // Old records are not kept for ever.
        let cut = press_ms - 7 * 86_400_000;
        self.st.arms.retain(|a| a.occurrence_ms >= cut);
    }

    /// The next start the station expects: the earliest unconsumed occurrence of a `manual` start block,
    /// counting one that is up to `arm_window_min` overdue, else the next scheduled start of any kind.
    pub fn expected(&self, now_ms: i64) -> Option<Expected> {
        let mut best: Option<Expected> = None;
        for (pid, b, sb) in self.prog.scheduled_starts() {
            let manual = sb.anchor == Anchor::Manual;
            let from = if manual {
                now_ms - sb.arm_window_min * 60_000
            } else {
                now_ms
            };
            for spec in &sb.schedule {
                for t in spec.occurrences(&self.cfg.zone, None, from - 1, now_ms + 8 * 86_400_000) {
                    let rec = self.arm_rec(b.id, t);
                    if rec.is_some_and(|r| r.consumed || r.fired) {
                        continue;
                    }
                    let armed = rec.map_or(sb.fallback_grace_s.is_some(), |r| r.armed);
                    if best.as_ref().is_none_or(|x| t < x.at_ms) {
                        best = Some(Expected {
                            playlist_id: pid,
                            block_id: b.id,
                            at_ms: t,
                            manual,
                            armed: manual && armed && sb.fallback_grace_s.is_some(),
                            fallback_grace_s: sb.fallback_grace_s,
                        });
                    }
                    break;
                }
            }
        }
        best
    }

    /// `arm` and `disarm` (7.2): the fallback auto-start of the next occurrence of a `manual` start block.
    pub fn set_armed(
        &mut self,
        playlist_id: i64,
        occurrence_ms: Option<i64>,
        armed: bool,
        now_ms: i64,
    ) -> Result<(i64, i64), String> {
        let pl = self
            .prog
            .playlists
            .get(&playlist_id)
            .ok_or_else(|| format!("no playlist {playlist_id}"))?;
        let (b, sb) = pl.start_block().ok_or("this playlist has no start block")?;
        if sb.anchor != Anchor::Manual {
            return Err("only a manual start block has a fallback auto-start".into());
        }
        if armed && sb.fallback_grace_s.is_none() {
            return Err(
                "this start block has no fallback start, so there is nothing to arm: nobody starts the show but you. \
                 To use it, set `fallback_start` to `{\"enabled\": true, \"grace_s\": <seconds>}` in the Start block of the playlist"
                    .into(),
            );
        }
        let bid = b.id;
        let occ = match occurrence_ms {
            Some(o) => o,
            None => {
                let e = self
                    .expected(now_ms)
                    .filter(|e| e.playlist_id == playlist_id)
                    .ok_or("there is no upcoming occurrence")?;
                e.at_ms
            }
        };
        let default = sb.fallback_grace_s.is_some();
        self.arm_rec_mut(bid, occ, default).armed = armed;
        Ok((bid, occ))
    }

    /// An occurrence whose fallback auto-start is due: nobody started it by its time plus the grace and it is
    /// armed (4.9). The caller starts the program and calls [`Scheduler::mark_fallback_fired`].
    pub fn fallback_due(&self, now_ms: i64) -> Option<(i64, i64, i64)> {
        for (pid, b, sb) in self.prog.scheduled_starts() {
            let (true, Some(grace)) = (sb.anchor == Anchor::Manual, sb.fallback_grace_s) else {
                continue;
            };
            for spec in &sb.schedule {
                let from = now_ms - 2 * 3_600_000;
                for t in spec.occurrences(&self.cfg.zone, None, from, now_ms) {
                    if now_ms < t + grace * 1000 {
                        continue;
                    }
                    let rec = self.arm_rec(b.id, t);
                    if rec.is_some_and(|r| r.consumed || r.fired || !r.armed) {
                        continue;
                    }
                    // A program of this playlist is already running: it was started some other way.
                    if self.st.program.as_ref().is_some_and(|p| {
                        p.root_playlist() == pid && p.t0_ms >= t - sb.arm_window_min * 60_000
                    }) {
                        continue;
                    }
                    return Some((pid, b.id, t));
                }
            }
        }
        None
    }

    pub fn mark_fallback_fired(&mut self, block_id: i64, occ: i64) {
        let r = self.arm_rec_mut(block_id, occ, true);
        r.fired = true;
        r.consumed = true;
    }

    /// `abort` (4.7): end the program without further blocks. What plays now keeps playing; the station
    /// goes on with its scheduled program or its default playlist at the next boundary.
    pub fn abort_program(
        &mut self,
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> Result<(), String> {
        let pr = self.st.program.clone().ok_or("no program is running")?;
        let next = self
            .st
            .scheduled_program
            .or(self.cfg.default_playlist)
            .unwrap_or(pr.root_playlist());
        if let Some(sb) = self
            .prog
            .playlists
            .get(&pr.root_playlist())
            .filter(|p| p.mode == Mode::Sequential)
        {
            let _ = sb;
        }
        // Blocks the program had armed cannot act any more: the new run has a new id.
        self.start_program_with(next, now_ms, "abort", None);
        self.st.seq_acts.clear();
        self.st.bt_display = None;
        self.st.queue.retain(|e| e.pinned || e.committed);
        let mut q = std::mem::take(&mut self.st.queue);
        self.set_times(&mut q, now_ms, cat, timing);
        self.st.queue = q;
        self.times_sig = None;
        self.fill(now_ms, cat, timing);
        Ok(())
    }
}

impl Scheduler {
    /// The station act that an event of an act kind amounts to, at `target`.
    fn act_for(ev: &crate::timed::Event, target: i64) -> Vec<crate::timed::TimedAct> {
        use crate::timed::{ActKind, EvKind, TimedAct};
        let mk = |kind: ActKind, t: i64| TimedAct {
            block_id: ev.block_id,
            name: ev.name.clone(),
            target_ms: t,
            kind,
        };
        match &ev.kind {
            EvKind::Side(a) => vec![mk(ActKind::Side(a.clone()), target)],
            EvKind::Gain { gain_db, ramp_ms } => vec![mk(
                ActKind::Gain {
                    gain_db: *gain_db,
                    ramp_ms: *ramp_ms,
                },
                target,
            )],
            EvKind::Mute { on, ramp_ms } => vec![mk(
                ActKind::Mute {
                    on: *on,
                    ramp_ms: *ramp_ms,
                },
                target,
            )],
            EvKind::FadeStop { ms } => vec![mk(ActKind::FadeStop { fade_ms: *ms }, target)],
            EvKind::StopNow { fade_ms } => vec![mk(ActKind::Stop { fade_ms: *fade_ms }, target)],
            EvKind::StopBoundary {
                fade_ms,
                hard_after_late,
            } => {
                let mut v = vec![mk(ActKind::Hold, target)];
                if *hard_after_late {
                    v.push(mk(ActKind::HardStop { fade_ms: *fade_ms }, target + 30_000));
                }
                v
            }
            EvKind::End => vec![mk(ActKind::EndProgram, target)],
            _ => vec![],
        }
    }

    /// How far ahead `fire block` looks for a block's next occurrence (a stop after midnight with a day offset is
    /// more than a day away from a T0 just after midnight).
    pub const FIRE_AHEAD_MS: i64 = 49 * 3_600_000;

    /// `fire block` (4.7): act now for a block of the program (or a station block), as if its time had
    /// come. It is logged as fired by hand and does not fire again at its own time. The acts are returned
    /// for the station to carry out at once.
    pub fn fire_block(
        &mut self,
        block_id: i64,
        now_ms: i64,
        cat: &Catalog,
        timing: &dyn Timing,
    ) -> Result<Vec<crate::timed::TimedAct>, String> {
        use crate::timed::EvKind;
        let evs = self.events(now_ms);
        let mut ev = evs
            .iter()
            .filter(|e| e.block_id == block_id && !self.event_logged(e))
            .min_by_key(|e| e.target)
            .cloned();
        if ev.is_none() {
            // A block further ahead than the act window (the stop after midnight of a program started the night
            // before): its next occurrence acts now.
            ev = self
                .build_events(now_ms - 60_000, now_ms + Self::FIRE_AHEAD_MS)
                .into_iter()
                .filter(|e| e.block_id == block_id && !self.event_logged(e))
                .min_by_key(|e| e.target);
        }
        if let Some(ev) = ev {
            if ev.kind.is_act() {
                self.log_event(&ev, "fired", Some(now_ms), "fired by hand");
                if ev.flow {
                    self.settle_act(&ev, now_ms, false, cat, timing);
                }
                return Ok(Self::act_for(&ev, now_ms));
            }
            // A block that puts audio in the queue: it acts at once, as a hard start.
            let mut e2 = ev.clone();
            e2.target = now_ms;
            e2.lo = now_ms;
            e2.hi = now_ms;
            e2.mode = crate::programming::BoundaryMode::Hard;
            if matches!(
                e2.kind,
                EvKind::Insert(_) | EvKind::Play(_) | EvKind::Switch { .. }
            ) {
                self.log_event(&ev, "fired", Some(now_ms), "fired by hand");
                self.force_place(&e2, now_ms, false, now_ms, cat, timing);
                self.bump_epoch();
                return Ok(vec![]);
            }
        }
        // An in-sequence side block, or a standing one: it acts now.
        let b = self
            .find_block(block_id)
            .ok_or("no such block in the running program or the station blocks")?;
        use crate::timed::ActKind;
        let kind = match &b.kind {
            BlockKind::Side(a) => ActKind::Side(a.clone()),
            BlockKind::SetGain { gain_db, ramp_ms } => ActKind::Gain {
                gain_db: *gain_db,
                ramp_ms: *ramp_ms,
            },
            BlockKind::SetMute { on, ramp_ms } => ActKind::Mute {
                on: *on,
                ramp_ms: *ramp_ms,
            },
            _ => return Err(format!(
                "`{}` has nothing left to fire: only a timed block of the running program, or an osc, midi, web, gain or mute block, can be fired by hand",
                b.name
            )),
        };
        let row = crate::timed::LogRow {
            run_id: self.st.program.as_ref().map(|p| p.run_id),
            block_id,
            name: b.name.clone(),
            kind: b.kind.type_name().into(),
            target_ms: now_ms,
            planned_ms: now_ms,
            fired_ms: Some(now_ms),
            how: "fired".into(),
            deviation_ms: Some(0),
            detail: "fired by hand".into(),
        };
        self.st.log.push(row);
        Ok(vec![crate::timed::TimedAct {
            block_id,
            name: b.name,
            target_ms: now_ms,
            kind,
        }])
    }

    fn find_block(&self, block_id: i64) -> Option<Block> {
        let p = self.st.program.as_ref();
        let mut levels: Vec<i64> = p
            .map(|p| p.stack.iter().map(|c| c.playlist_id).collect())
            .unwrap_or_default();
        levels.extend(p.map(|p| p.playlist_id));
        for id in levels {
            if let Some(b) = self
                .prog
                .playlists
                .get(&id)
                .and_then(|pl| pl.blocks().find(|b| b.id == block_id))
            {
                return Some(b.clone());
            }
        }
        self.prog
            .station_blocks
            .iter()
            .find(|b| b.id == block_id)
            .cloned()
    }

    /// `skip next block` (4.7): the cursor passes the block it stands at, or, when it stands at nothing in
    /// sequence, the next timed block of the program is logged `skipped` and the cursor passes it.
    pub fn skip_next_block(&mut self, now_ms: i64) -> Result<String, String> {
        // A wait or a play that holds the cursor.
        if let Some(pr) = self.st.program.clone() {
            if let Some(Entry::Block(b)) = self
                .prog
                .playlists
                .get(&pr.playlist_id)
                .and_then(|pl| pl.entries.get(pr.seq_pos))
            {
                let holds = b.enabled
                    && b.timed.is_none()
                    && (matches!(b.kind, BlockKind::Wait(_))
                        || (matches!(b.kind, BlockKind::Play(_)) && pr.hold.is_some()));
                if holds {
                    let name = b.name.clone();
                    let b = b.clone();
                    self.log_block_by_hand(&b, "skipped", now_ms);
                    if let Some(p) = self.st.program.as_mut() {
                        p.seq_pos += 1;
                        p.hold = None;
                        p.wait_since = None;
                        p.released = false;
                        p.fill = None;
                        p.epoch += 1;
                    }
                    self.times_sig = None;
                    return Ok(name);
                }
            }
        }
        let evs = self.events(now_ms);
        let ev = evs
            .iter()
            .filter(|e| {
                e.origin == crate::timed::EvOrigin::Program && !self.event_logged(e) && e.flow
            })
            .min_by_key(|e| e.target)
            .cloned()
            .ok_or("there is no next block to skip")?;
        self.log_event(&ev, "skipped", Some(now_ms), "skipped by hand");
        self.passed_up_to(&ev);
        self.ev_cache = crate::timed::EventCache::default();
        self.times_sig = None;
        Ok(ev.name)
    }

    fn log_block_by_hand(&mut self, b: &Block, how: &str, now_ms: i64) {
        let row = crate::timed::LogRow {
            run_id: self.st.program.as_ref().map(|p| p.run_id),
            block_id: b.id,
            name: b.name.clone(),
            kind: b.kind.type_name().into(),
            target_ms: now_ms,
            planned_ms: now_ms,
            fired_ms: Some(now_ms),
            how: how.into(),
            deviation_ms: Some(0),
            detail: format!("{how} by hand"),
        };
        self.st.log.push(row);
    }
}

/// What `end-at` does after the back-timed play ends (7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndThen {
    /// The program ends with the play (the station goes on with its scheduled or default playlist).
    End,
    /// Another playlist starts at the end with a normal segue (`then: switch_playlist`).
    Playlist(i64),
}

impl Scheduler {
    /// `POST /end-at` (7.2): a one-off program that plays `playlist_id` and ends with a back-timed track at
    /// `at_ms` (the `exact` mode: the closing item's cue-out lands on the time). Returns the id of the
    /// program it made (negative: it is not in the library).
    pub fn make_end_at(
        &mut self,
        playlist_id: i64,
        at_ms: i64,
        then: EndThen,
    ) -> Result<i64, String> {
        let src = self
            .prog
            .playlists
            .get(&playlist_id)
            .ok_or_else(|| format!("no playlist {playlist_id}"))?;
        let local = format!("{}", self.cfg.zone.civil(at_ms));
        let spec = TimeSpec::date(&local)?;
        let id = self.synthetic.keys().min().copied().unwrap_or(0).min(0) - 1;
        let mut entries = vec![Entry::Block(Block::new(
            id * 10 - 1,
            &format!("Play {} until the end time", src.name),
            BlockKind::Play(crate::programming::PlayBlock {
                source: Source::Playlist(playlist_id),
                until: Until::At(spec),
                end: crate::programming::EndMode::Natural,
                fade_s: crate::programming::DEFAULT_FADE_S,
                back_time: Some(crate::programming::BackTime {
                    mode: crate::programming::BackMode::Exact,
                    tolerance_s: crate::programming::DEFAULT_TOLERANCE_S,
                }),
            }),
            Trigger::Sequence,
            None,
        ))];
        if let EndThen::Playlist(next) = then {
            if !self.prog.playlists.contains_key(&next) {
                return Err(format!("no playlist {next}"));
            }
            entries.push(Entry::Block(Block::new(
                id * 10 - 2,
                "Then",
                BlockKind::StartPlaylist {
                    playlist_id: next,
                    how: crate::programming::StartHow::Segue,
                },
                Trigger::Sequence,
                None,
            )));
        }
        let pl = Playlist {
            id,
            name: format!("End at {}", local.get(11..16).unwrap_or("")),
            mode: Mode::Sequential,
            no_repeat_minutes: 0,
            separation: crate::programming::Separation::default(),
            station_blocks: true,
            enabled: true,
            entries,
            station_id: None,
        };
        self.synthetic.insert(id, pl.clone());
        let mut p = (*self.prog).clone();
        p.playlists.insert(id, pl);
        self.prog = Arc::new(p);
        Ok(id)
    }
}

fn pl_for_fade<'a>(prog: &'a Programming, pr: &ProgramRun) -> Option<&'a Playlist> {
    prog.playlists.get(&pr.playlist_id)
}

fn b_ref(pl: Option<&Playlist>, pos: usize) -> Option<&Block> {
    match pl?.entries.get(pos)? {
        Entry::Block(b) => Some(b),
        _ => None,
    }
}

fn b_id_of(b: Option<&Block>) -> i64 {
    b.map_or(-1, |b| b.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_recurring_time_has_one_instant() {
        let z = Zone::utc();
        let spec = TimeSpec::relative(14 * 60_000 + 40_000);
        assert_eq!(
            spec_instant(&spec, &z, 1_000_000, 0),
            Some(1_000_000 + 880_000)
        );
    }
}
