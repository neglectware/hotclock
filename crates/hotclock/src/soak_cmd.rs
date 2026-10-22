//! `hotclock soak` (M1.8 acceptance 4): the accelerated soak. The real station (conductor, scheduler,
//! mixer, decoders, `state.db`, library sync, the nightly snapshot) runs on a virtual clock as fast as the
//! CPU allows, on the office set, while a seeded random schedule of operator actions and fault injections
//! is applied. Everything the design says must hold over a month is measured:
//!
//! * no panic and no step error;
//! * no dead air: a run of 50 ms or more below -60 dBFS (peak, both channels) that no operator action or
//!   injected fault explains;
//! * memory flat: the process's RSS, thread and file-descriptor counts every virtual hour;
//! * the as-run log complete: every play in `state.db`, and no gap between one item's end and the next
//!   one's start that is over 50 ms outside the explained windows;
//! * disk growth of the station's data directory, and the nightly snapshots (7 kept).
//!
//! Output (in `--out`): `samples.csv` (one line per virtual hour), `events.jsonl` (every action, fault,
//! finding and restart), `summary.json` and `summary.md`. The work directory holds the library, the media
//! store and `state.db`; it goes when the run ends (`--keep` keeps it, and `--resume` continues a run that
//! stopped for the wall-time limit).

use crate::explain::{Explainer, RowView};
use crate::procstats;
use clap::Args;
use engine::render::{render_tone, ToneRender};
use engine::wav::WavFormat;
use library::playlists::{Mode, NewBlock, NewPlaylist};
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use serde_json::{json, Value};
use station::offline::Offline;
use station::runtime::{Op, Station, StationOptions};
use station::sched::{At, SwitchHow};
use station::state::FaultMode;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const RATE: u32 = 48_000;
const T0: i64 = 1_800_000_000_000;

#[derive(Args)]
pub struct SoakArgs {
    /// Virtual days to run.
    #[arg(long, default_value_t = 30.0)]
    days: f64,
    /// The office test set (default: `$HOTCLOCK_TESTDATA/office`). Never mixed with another set.
    #[arg(long)]
    set: Option<PathBuf>,
    /// Work directory (default: a new one under the temp directory).
    #[arg(long)]
    work_dir: Option<PathBuf>,
    /// Where the report goes.
    #[arg(long, default_value = "soak-out")]
    out: PathBuf,
    #[arg(long, default_value_t = 2026)]
    seed: u64,
    /// Operator actions per virtual day (random times).
    #[arg(long, default_value_t = 24.0)]
    ops_per_day: f64,
    /// Fault injections per virtual day (random times, random kinds).
    #[arg(long, default_value_t = 6.0)]
    faults_per_day: f64,
    /// Stop after this many seconds of wall time (the run can be continued with `--resume`).
    #[arg(long)]
    wall_limit_s: Option<f64>,
    /// Keep the work directory at the end.
    #[arg(long)]
    keep: bool,
    /// Continue the run in `--work-dir` where it stopped.
    #[arg(long)]
    resume: bool,
    /// Do not run: judge the finished (or stopped) run in `--work-dir` again with the current rules, and write a
    /// new `summary.json` (the samples and the saved state of the run are read as they are).
    #[arg(long)]
    report_only: bool,
    /// Only these operator actions, comma separated (`next,cut,fade_next,...`): to look at one at a time.
    #[arg(long)]
    only_ops: Option<String>,
}

// ---- small helpers ---------------------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }
    fn pick(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    /// An exponential wait with the given mean.
    fn exp(&mut self, mean: f64) -> f64 {
        -mean * (1.0 - self.unit()).max(1e-12).ln()
    }
    fn weighted(&mut self, w: &[f64]) -> usize {
        let total: f64 = w.iter().sum();
        let mut x = self.unit() * total;
        for (i, v) in w.iter().enumerate() {
            if x < *v {
                return i;
            }
            x -= v;
        }
        w.len() - 1
    }
}

/// Runs of 50 ms or more with the peak of both channels under -60 dBFS, in wall time.
#[derive(Default)]
struct DeadAir {
    run_start: Option<u64>,
    /// `(start wall ms, length ms)`.
    runs: Vec<(i64, i64)>,
    frames: u64,
}

impl DeadAir {
    fn feed(&mut self, origin_ms: i64, t: u64, block: &[f32]) {
        for (i, f) in block.chunks_exact(2).enumerate() {
            let quiet = f[0].abs().max(f[1].abs()) < 1e-3;
            let frame = t + i as u64;
            match (quiet, self.run_start) {
                (true, None) => self.run_start = Some(frame),
                (false, Some(s)) => {
                    self.close(origin_ms, s, frame);
                }
                _ => {}
            }
        }
        self.frames += (block.len() / 2) as u64;
    }
    fn close(&mut self, origin_ms: i64, start: u64, end: u64) {
        self.run_start = None;
        let len = end - start;
        if len >= 2_400 {
            self.runs.push((
                origin_ms + (start * 1000 / u64::from(RATE)) as i64,
                (len * 1000 / u64::from(RATE)) as i64,
            ));
        }
    }
}

/// A clock step or a device outage: the dead-air runs recorded before it, the newest as-run id then, the rows on
/// air (their start is in the old wall time, their end in the new), and how far the wall time moved (ms).
#[derive(Clone, Debug, PartialEq)]
struct Shift {
    runs_before: usize,
    max_id: i64,
    on_air: Vec<i64>,
    ms: i64,
}

/// A hold or an assist: `(rows on air, when it began, when it was released + 5 s, what)`.
#[derive(Clone, Debug)]
struct Wait {
    rows: Vec<i64>,
    from: i64,
    to: i64,
    why: &'static str,
}

/// The view of the as-run rows for the stretch of wall time after the first `epoch` shifts (`RowView`).
fn row_view(shifts: &[Shift], epoch: usize) -> RowView {
    let mut v = RowView {
        widen_ms: shifts.iter().map(|s| s.ms.abs()).sum(),
        ..RowView::default()
    };
    if epoch > 0 {
        v.lo = shifts[epoch - 1].max_id;
        v.carried = shifts[epoch - 1].on_air.clone();
    }
    if let Some(next) = shifts.get(epoch) {
        v.hi = next.max_id;
    }
    for (k, sh) in shifts.iter().enumerate() {
        for id in &sh.on_air {
            let m = if k < epoch {
                &mut v.start_shift
            } else {
                &mut v.end_shift
            };
            *m.entry(*id).or_default() += sh.ms;
        }
    }
    v
}

/// The stretch a dead-air run belongs to: how many shifts came before it was recorded.
fn epoch_of_run(shifts: &[Shift], run_index: usize) -> usize {
    shifts.iter().filter(|s| s.runs_before <= run_index).count()
}

/// How far the wall time moved between the end of as-run row `p` and the start of row `r` (ms): the shifts that
/// came after `p` ended (`p` was not on air at them) and before `r` started.
fn shift_between(shifts: &[Shift], p: i64, r: i64) -> i64 {
    shifts
        .iter()
        .filter(|s| s.max_id >= p && s.max_id < r && !s.on_air.contains(&p))
        .map(|s| s.ms)
        .sum()
}

struct World {
    data: PathBuf,
    music: Vec<i64>,
    liners: Vec<i64>,
    office: i64,
    office_b: i64,
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    v.sort();
    for p in v {
        if p.is_dir() {
            files(&p, out);
        } else if p
            .extension()
            .is_some_and(|e| matches!(e.to_str(), Some("flac" | "mp3" | "m4a" | "wav")))
        {
            out.push(p);
        }
    }
}

fn tone(dir: &Path, name: &str, hz: f64, secs: f64) -> Result<PathBuf, String> {
    let p = dir.join(name);
    render_tone(&ToneRender {
        out: p.clone(),
        rate: RATE,
        seconds: secs,
        hz,
        level_dbfs: -20.0,
        format: WavFormat::F32,
    })
    .map_err(|e| e.to_string())?;
    Ok(p)
}

fn import(lib: &Library, p: &Path, category: &str) -> Result<i64, String> {
    match import_file(
        lib,
        p,
        &ImportOptions {
            category: Some(category.into()),
            source: "soak".into(),
        },
    ) {
        ImportOutcome::Done { item_id, .. } | ImportOutcome::Duplicate { item_id } => Ok(item_id),
        o => Err(format!("{}: {o:?}", p.display())),
    }
}

/// The library of the run: the real office set as `music`, three liners, an emergency playlist of two long
/// tones, the office playlist (a shuffle over the set with a liner every 4) and a second one to switch to.
fn build_world(work: &Path, set: &Path) -> Result<World, String> {
    let data = work.join("data");
    let lib = Library::open(Paths::from_data_dir(&data)).map_err(|e| e.to_string())?;
    let mut fl = vec![];
    files(set, &mut fl);
    if fl.is_empty() {
        return Err(format!("no audio files under {}", set.display()));
    }
    let mut music = vec![];
    for f in &fl {
        music.push(import(&lib, f, "music")?);
    }
    let src = work.join("src");
    std::fs::create_dir_all(&src).map_err(|e| e.to_string())?;
    let mut liners = vec![];
    for i in 0..3 {
        let p = tone(
            &src,
            &format!("liner{i}.wav"),
            900.0 + 120.0 * f64::from(i),
            8.0,
        )?;
        liners.push(import(&lib, &p, "liner")?);
    }
    let mut emerg = vec![];
    for i in 0..2 {
        let p = tone(
            &src,
            &format!("emerg{i}.wav"),
            500.0 + 100.0 * f64::from(i),
            90.0,
        )?;
        emerg.push(import(&lib, &p, "music")?);
    }
    let cid = lib
        .category_id("liner")
        .map_err(|e| e.to_string())?
        .ok_or("no liner category")?;
    let mk = |name: &str, ids: &[i64]| -> Result<i64, String> {
        let mut p = NewPlaylist::shuffle(name);
        p.no_repeat_minutes = 60;
        let pl = lib.create_playlist(&p).map_err(|e| e.to_string())?;
        for i in ids {
            lib.add_item_to_playlist(pl, *i, 1.0)
                .map_err(|e| e.to_string())?;
        }
        lib.add_block_to_playlist(
            pl,
            &NewBlock::insert_every("liners", 4, json!({"category_id": cid})),
        )
        .map_err(|e| e.to_string())?;
        Ok(pl)
    };
    let office = mk("Office", &music)?;
    let half: Vec<i64> = music.iter().copied().step_by(2).collect();
    let office_b = mk("Office B", &half)?;
    let mut p = NewPlaylist::shuffle("Emergency");
    p.mode = Mode::Sequential;
    let e = lib.create_playlist(&p).map_err(|e| e.to_string())?;
    for i in &emerg {
        lib.add_item_to_playlist(e, *i, 1.0)
            .map_err(|e| e.to_string())?;
    }
    Ok(World {
        data,
        music,
        liners,
        office,
        office_b,
    })
}

// ---- the run ----------------------------------------------------------------------------------------------

struct Soak {
    args: SoakArgs,
    w: World,
    work: PathBuf,
    off: Option<Offline>,
    rng: Rng,
    /// Virtual seconds since the start of the run.
    vs: f64,
    /// The virtual second at which the current station process started (frames count from it).
    seg_base_s: f64,
    dead: Arc<Mutex<DeadAir>>,
    origin: Arc<AtomicI64>,
    /// Operator actions waiting for their time: `(virtual second, op, what)`.
    pending: Vec<(f64, Op, &'static str)>,
    /// Wall-time windows (ms) in which silence is explained by an action or a fault.
    windows: Vec<(i64, i64, &'static str)>,
    /// Faults that move the wall clock under the item on air (a clock step, a device outage), in order.
    shifts: Vec<Shift>,
    /// Holds and assists: the silence they may leave starts when the rows then on air end, which is known only
    /// from the as-run log (`resolve_waits`).
    waits: Vec<Wait>,
    /// Open windows, closed when their action ends: name to start.
    next_op: f64,
    next_fault: f64,
    /// Items whose file was damaged: `(item id, from wall ms, to wall ms)`. What plays of them meanwhile is
    /// the design's failure handling, not dead air.
    bad: Vec<(i64, i64, i64)>,
    /// Files damaged and waiting to be restored: `(restore at virtual s, item id, backup path, live path)`.
    damaged: Vec<(f64, i64, PathBuf, PathBuf)>,
    /// Device, media and database faults that end at a virtual second.
    ends: Vec<(f64, &'static str)>,
    counts: std::collections::BTreeMap<String, u64>,
    events: std::fs::File,
    csv: std::fs::File,
    as_run_total: u64,
    restarts: u64,
    last_sample_h: i64,
    errors: Vec<String>,
    first_sample: Option<procstats::ProcSample>,
    t_wall0: Instant,
    wall_before: f64,
    imports: u64,
}

impl Soak {
    fn opts(&self) -> StationOptions {
        let mut o = StationOptions::new("office", self.w.data.join("media"));
        o.default_playlist = Some("Office".into());
        o.emergency_playlist = Some("Emergency".into());
        o.seed = self.args.seed;
        o.backup_dir = Some(self.w.data.join("backup"));
        o
    }

    fn state_path(&self) -> PathBuf {
        self.w.data.join("stations").join("office").join("state.db")
    }

    fn wall_now(&self) -> i64 {
        self.off
            .as_ref()
            .map_or(self.origin.load(Ordering::Relaxed), |o| o.wall_of(o.t))
    }

    fn log(&mut self, kind: &str, detail: Value) {
        let mut v = json!({"vs": (self.vs * 10.0).round() / 10.0, "wall_ms": self.wall_now(), "kind": kind});
        if let (Some(o), Some(d)) = (v.as_object_mut(), detail.as_object()) {
            for (k, x) in d {
                o.insert(k.clone(), x.clone());
            }
        }
        let _ = writeln!(self.events, "{v}");
    }

    fn count(&mut self, k: &str) {
        *self.counts.entry(k.into()).or_default() += 1;
    }

    fn open_station(&mut self, wall_ms: i64) -> Result<(), String> {
        let (st, mixer) = Station::open(
            self.opts(),
            Some(self.state_path()),
            Some(self.w.data.join("library.db")),
            wall_ms,
        )?;
        let mut off = Offline::new(st, mixer, wall_ms);
        off.keep_samples = false;
        self.origin.store(wall_ms, Ordering::Relaxed);
        let (dead, origin) = (self.dead.clone(), self.origin.clone());
        off.sink = Some(Box::new(move |t, b| {
            if let Ok(mut d) = dead.lock() {
                d.feed(origin.load(Ordering::Relaxed), t, b);
            }
        }));
        off.start()?;
        self.off = Some(off);
        self.window(wall_ms, wall_ms + 4_000, "start");
        Ok(())
    }

    fn close_station(&mut self) {
        if let Some(mut off) = self.off.take() {
            self.as_run_total += off.station.as_run_total;
            off.station.flush();
            // A kill: the run of quiet frames does not continue into the next process.
            if let Ok(mut d) = self.dead.lock() {
                d.run_start = None;
            }
        }
    }

    /// Did a play of a damaged file end in an error just before wall time `at`?
    fn failed_damaged_play_ended_at(&self, at: i64) -> bool {
        let Ok(db) = rusqlite::Connection::open_with_flags(
            self.state_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) else {
            return false;
        };
        let row: Option<(Option<i64>, Option<String>)> = db
            .query_row(
                "SELECT item_id, end_reason FROM as_run WHERE ended_ms BETWEEN ?1 AND ?2 AND origin != 'restart' ORDER BY ended_ms DESC LIMIT 1",
                [at - 400, at + 50],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        match row {
            Some((Some(item), Some(reason))) => {
                reason == "error" && self.bad.iter().any(|d| d.0 == item)
            }
            _ => false,
        }
    }

    fn window(&mut self, from: i64, to: i64, why: &'static str) {
        self.windows.push((from, to, why));
    }

    /// The as-run rows on air now and the newest id written.
    fn on_air(&self) -> (Vec<i64>, i64) {
        self.off.as_ref().map_or((vec![], 0), |o| {
            let rows = &o.station.as_run;
            (
                rows.iter()
                    .filter(|r| r.ended_ms.is_none())
                    .map(|r| r.id)
                    .collect(),
                rows.iter().map(|r| r.id).max().unwrap_or(0),
            )
        })
    }

    /// Note a fault that moves the wall clock by `ms` under the rows on air (call before moving it).
    fn note_shift(&mut self, ms: i64) {
        let (on_air, newest) = self.on_air();
        // The newest id: in memory, or written by an earlier process of the run.
        let written: i64 = rusqlite::Connection::open_with_flags(
            self.state_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .ok()
        .and_then(|db| {
            db.query_row("SELECT COALESCE(MAX(id), 0) FROM as_run", [], |r| r.get(0))
                .ok()
        })
        .unwrap_or(0);
        let max_id = newest
            .max(written)
            .max(self.shifts.last().map_or(0, |s| s.max_id));
        let runs_before = self.dead.lock().map(|d| d.runs.len()).unwrap_or(0);
        self.shifts.push(Shift {
            runs_before,
            max_id,
            on_air,
            ms,
        });
    }

    /// Turn each hold and assist into the window of silence it may leave: from the end of the rows that were on
    /// air (less 0.5 s for a fade) to its release. Nothing when they played past the release (the station then
    /// goes on as usual). The rest of the item on air is checked like any other stretch.
    fn resolve_waits(&mut self) {
        let db = rusqlite::Connection::open_with_flags(
            self.state_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .ok();
        for w in std::mem::take(&mut self.waits) {
            let ended: Option<i64> = db.as_ref().and_then(|db| {
                w.rows
                    .iter()
                    .filter_map(|id| {
                        db.query_row("SELECT ended_ms FROM as_run WHERE id = ?1", [id], |r| {
                            r.get::<_, Option<i64>>(0)
                        })
                        .ok()
                        .flatten()
                    })
                    .max()
            });
            match ended {
                Some(e) if e < w.to => self.windows.push((e.max(w.from) - 500, w.to, w.why)),
                Some(_) => {}
                None => self.windows.push((w.from, w.to, w.why)),
            }
        }
    }

    // ---- operator actions ------------------------------------------------------------------------------

    fn queue_ids(&self) -> Vec<(i64, bool)> {
        self.off
            .as_ref()
            .map(|o| {
                o.station
                    .sched
                    .st
                    .queue
                    .iter()
                    .map(|e| (e.id, e.committed || e.pinned))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn make_op(&mut self) {
        let now = self.wall_now();
        let kinds = [
            ("next", 3.0),
            ("fade_next", 2.0),
            ("cut", 0.5),
            ("reshuffle", 1.5),
            ("insert", 3.0),
            ("remove", 2.0),
            ("move", 2.0),
            ("hold", 1.5),
            ("assist", 1.0),
            ("stop", 1.0),
            ("cart", 1.5),
            ("switch", 1.0),
        ];
        let allowed: Vec<&(&str, f64)> = match &self.args.only_ops {
            Some(list) => kinds
                .iter()
                .filter(|k| list.split(',').any(|o| o == k.0))
                .collect(),
            None => kinds.iter().collect(),
        };
        let k = allowed[self
            .rng
            .weighted(&allowed.iter().map(|k| k.1).collect::<Vec<_>>())]
        .0;
        let vs = self.vs;
        let free: Vec<i64> = self
            .queue_ids()
            .into_iter()
            .filter(|(_, locked)| !locked)
            .map(|(i, _)| i)
            .collect();
        let item = |s: &mut Self| -> i64 {
            if s.rng.unit() < 0.5 {
                s.w.liners[s.rng.pick(s.w.liners.len())]
            } else {
                s.w.music[s.rng.pick(s.w.music.len())]
            }
        };
        match k {
            "next" => self.pending.push((vs, Op::Next, "next")),
            "fade_next" => self.pending.push((vs, Op::FadeNext(None), "fade_next")),
            "cut" => self.pending.push((vs, Op::Cut, "cut")),
            "reshuffle" => self.pending.push((vs, Op::Reshuffle, "reshuffle")),
            "insert" => {
                let at = [At::Next, At::End][self.rng.pick(2)];
                let item_id = item(self);
                self.pending
                    .push((vs, Op::Insert { item_id, at }, "insert"));
            }
            "remove" if !free.is_empty() => {
                let entry_id = free[self.rng.pick(free.len())];
                self.pending.push((vs, Op::Remove { entry_id }, "remove"));
            }
            "move" if free.len() >= 2 => {
                let entry_id = free[self.rng.pick(free.len())];
                let to = self.rng.pick(free.len());
                self.pending.push((vs, Op::Move { entry_id, to }, "move"));
            }
            "hold" => {
                let d = self.rng.range(1.0, 45.0);
                self.pending.push((vs, Op::Hold, "hold"));
                self.pending.push((vs + d, Op::Resume, "resume"));
                // The item on air plays to its end; the silence, if any, lasts from that end until the resume.
                let rows = self.on_air().0;
                self.waits.push(Wait {
                    rows,
                    from: now,
                    to: now + ((d + 5.0) * 1000.0) as i64,
                    why: "hold",
                });
            }
            "assist" => {
                let d = self.rng.range(20.0, 120.0);
                self.pending.push((vs, Op::Assist(true), "assist_on"));
                self.pending.push((vs + d, Op::Assist(false), "assist_off"));
                let rows = self.on_air().0;
                self.waits.push(Wait {
                    rows,
                    from: now,
                    to: now + ((d + 5.0) * 1000.0) as i64,
                    why: "assist",
                });
            }
            "stop" => {
                let d = self.rng.range(3.0, 60.0);
                self.pending
                    .push((vs, Op::Stop { fade_s: Some(2.0) }, "stop"));
                self.pending.push((vs + d, Op::Resume, "resume"));
                self.window(now, now + ((d + 5.0) * 1000.0) as i64, "stop");
            }
            "cart" => {
                let item_id = self.w.liners[self.rng.pick(self.w.liners.len())];
                self.pending.push((vs, Op::FireCart { item_id }, "cart"));
            }
            "switch" => {
                let playlist_id = if self.rng.unit() < 0.5 {
                    self.w.office_b
                } else {
                    self.w.office
                };
                let how = [SwitchHow::Now, SwitchHow::Segue, SwitchHow::End][self.rng.pick(3)];
                self.pending
                    .push((vs, Op::Switch { playlist_id, how }, "switch"));
            }
            _ => self.pending.push((vs, Op::Next, "next")),
        }
        self.pending
            .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    }

    /// Render virtual second `vs`, handing the operator actions that have come due to the renderer, which
    /// applies each at its first block. Actions it could not apply (a load in flight) are tried again in the
    /// next second.
    fn step_second(&mut self) -> Result<(), String> {
        let vs = self.vs;
        let due: Vec<(f64, Op, &'static str)> = {
            let (d, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending)
                .into_iter()
                .partition(|p| p.0 <= vs);
            self.pending = rest;
            d
        };
        let t_rel = self.vs - self.seg_base_s;
        let ops: Vec<(f64, Op)> = due.iter().map(|d| (0.0, d.1.clone())).collect();
        let off = self.off.as_mut().ok_or_else(|| "no station".to_string())?;
        let before = off.op_log.len();
        off.run_until(t_rel, &ops)?;
        let applied = off.op_log.len() - before;
        let refused = off.op_log[before..].iter().filter(|r| r.2.is_err()).count() as u64;
        off.op_log.clear();
        let now_wall = self.wall_now();
        for (i, d) in due.iter().enumerate() {
            if i < applied {
                self.count(&format!("op_{}", d.2));
                // A manual transition (next, fade, cut, switch, and the resume or assist release that starts
                // the next item) may leave a short silence of its own: measured and listed apart, not called
                // dead air. The actions that do not end the item on air (insert, remove, move, reshuffle, a
                // cart, a hold, assist on) get no window: a silence next to one of them is checked as any other.
                if matches!(
                    d.2,
                    "next" | "fade_next" | "cut" | "switch" | "resume" | "assist_off" | "stop"
                ) {
                    self.windows.push((now_wall - 100, now_wall + 2_500, "op"));
                }
            } else {
                self.pending.push((self.vs + 1.0, d.1.clone(), d.2));
            }
        }
        if refused > 0 {
            *self.counts.entry("ops_refused".into()).or_default() += refused;
        }
        let names: Vec<&str> = due.iter().take(applied).map(|d| d.2).collect();
        if !names.is_empty() {
            self.log("ops", json!({"ops": names, "refused": refused}));
        }
        Ok(())
    }

    // ---- faults -----------------------------------------------------------------------------------------

    fn make_fault(&mut self) -> Result<(), String> {
        let kinds = [
            ("restart", 1.0),
            ("device_lost", 1.0),
            ("xrun", 1.5),
            ("media_fail", 0.7),
            ("db_fault", 0.7),
            ("clock_step", 0.6),
            ("bad_file", 1.0),
            ("import", 0.3),
        ];
        let k = kinds[self
            .rng
            .weighted(&kinds.iter().map(|k| k.1).collect::<Vec<_>>())]
        .0;
        let now = self.wall_now();
        match k {
            "restart" => {
                let gap = self.rng.range(2.0, 5.0);
                self.close_station();
                self.restarts += 1;
                self.vs += gap;
                self.seg_base_s = self.vs;
                let wall = now + (gap * 1000.0) as i64;
                self.window(now, wall + 8_000, "restart");
                self.open_station(wall)?;
                self.log("fault", json!({"fault": "restart", "gap_s": gap}));
            }
            "device_lost" => {
                let long = self.rng.unit() < 0.3;
                let outage = if long {
                    self.rng.range(30.0, 90.0)
                } else {
                    self.rng.range(1.0, 8.0)
                };
                self.note_shift((outage * 1000.0) as i64);
                let off = self.off.as_mut().ok_or("no station")?;
                let frozen = off.t;
                off.station.on_device_lost();
                let wall0 = off.wall_of(frozen);
                let steps = (outage * 50.0) as i64;
                for k in 0..steps {
                    let mixer = &mut off.mixer;
                    let mut send = |c: engine::rt::Command| {
                        use engine::source::Source;
                        mixer.command(&c.kind)
                    };
                    off.station.step(frozen, wall0 + k * 20, &mut send)?;
                }
                off.wall_origin_ms += (outage * 1000.0) as i64;
                self.origin.store(off.wall_origin_ms, Ordering::Relaxed);
                off.station.on_device_restored((outage * 1000.0) as u32);
                self.vs += outage;
                self.seg_base_s += outage;
                // The item on air resumes where it was after the outage: its start is moved on by the outage when a
                // later silence in it is placed in its file (`note_shift`), so only the outage and the recovery are a
                // window.
                self.window(now, now + ((outage + 6.0) * 1000.0) as i64, "device_lost");
                self.log("fault", json!({"fault": "device_lost", "outage_s": outage}));
            }
            "xrun" => {
                if let Some(o) = self.off.as_mut() {
                    o.station.on_xrun();
                }
                self.log("fault", json!({"fault": "xrun"}));
            }
            "media_fail" => {
                let d = self.rng.range(3.0, 20.0);
                if let Some(o) = self.off.as_mut() {
                    o.station
                        .media_faults()
                        .fail_all
                        .store(true, Ordering::SeqCst);
                }
                self.ends.push((self.vs + d, "media_ok"));
                self.window(now, now + ((d + 6.0) * 1000.0) as i64, "media_fail");
                self.log("fault", json!({"fault": "media_fail", "s": d}));
            }
            "db_fault" => {
                let d = self.rng.range(20.0, 90.0);
                let mode = if self.rng.unit() < 0.5 {
                    FaultMode::ReadOnly
                } else {
                    FaultMode::Full
                };
                if let Some(o) = self.off.as_ref() {
                    o.station.set_state_fault(mode);
                }
                self.ends.push((self.vs + d, "db_ok"));
                self.log(
                    "fault",
                    json!({"fault": "db_fault", "mode": format!("{mode:?}"), "s": d}),
                );
            }
            "clock_step" => {
                let step_ms: i64 = if self.rng.unit() < 0.5 {
                    600_000
                } else {
                    -600_000
                };
                self.note_shift(step_ms);
                if let Some(o) = self.off.as_mut() {
                    o.wall_origin_ms += step_ms;
                    self.origin.store(o.wall_origin_ms, Ordering::Relaxed);
                }
                // The rows on air started in the old wall time: `note_shift` brings them into the new one, so only
                // the moment of the step is a window, in both wall times.
                self.window(now - 5_000, now + 5_000, "clock_step");
                self.window(now + step_ms - 5_000, now + step_ms + 5_000, "clock_step");
                self.log("fault", json!({"fault": "clock_step", "step_ms": step_ms}));
            }
            "bad_file" => {
                let id = self.w.music[self.rng.pick(self.w.music.len())];
                if self.damaged.iter().any(|d| d.1 == id) {
                    return Ok(());
                }
                let lib = Library::open_worker(Paths::from_data_dir(&self.w.data))
                    .map_err(|e| e.to_string())?;
                let sp: String = lib
                    .conn()
                    .query_row("SELECT store_path FROM items WHERE id = ?1", [id], |r| {
                        r.get(0)
                    })
                    .map_err(|e| e.to_string())?;
                let live = self.w.data.join("media").join(sp);
                let bak = self.work.join("bad").join(format!("{id}.bak"));
                std::fs::create_dir_all(bak.parent().unwrap_or(Path::new("."))).ok();
                std::fs::copy(&live, &bak).map_err(|e| e.to_string())?;
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&live, std::fs::Permissions::from_mode(0o644))
                    .map_err(|e| e.to_string())?;
                let len = std::fs::metadata(&live).map_err(|e| e.to_string())?.len();
                let how = self.rng.pick(3);
                let f = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&live)
                    .map_err(|e| e.to_string())?;
                match how {
                    0 => f.set_len(len * 4 / 10).map_err(|e| e.to_string())?,
                    1 => {
                        // Zero the middle half: the file is intact but part of it decodes to nothing or noise.
                        use std::os::unix::fs::FileExt;
                        let z = vec![0u8; (len / 2) as usize];
                        f.write_at(&z, len / 4).map_err(|e| e.to_string())?;
                    }
                    _ => {
                        use std::os::unix::fs::FileExt;
                        f.write_at(&[0xFFu8; 4096], 0).map_err(|e| e.to_string())?;
                    }
                }
                let restore = self.vs + self.rng.range(600.0, 1800.0);
                self.bad.push((
                    id,
                    now,
                    now + ((restore - self.vs) * 1000.0) as i64 + 60_000,
                ));
                self.damaged.push((restore, id, bak, live));
                let how_s = ["truncate", "zero_middle", "garbage_head"][how];
                self.log(
                    "fault",
                    json!({"fault": "bad_file", "item": id, "how": how_s}),
                );
            }
            "import" => {
                self.imports += 1;
                let src = self.work.join("src");
                let p = tone(
                    &src,
                    &format!("new{}.wav", self.imports),
                    300.0 + 7.0 * self.imports as f64,
                    30.0,
                )?;
                let lib = Library::open_worker(Paths::from_data_dir(&self.w.data))
                    .map_err(|e| e.to_string())?;
                let id = import(&lib, &p, "music")?;
                lib.add_item_to_playlist(self.w.office, id, 1.0)
                    .map_err(|e| e.to_string())?;
                self.w.music.push(id);
                self.log("fault", json!({"fault": "import", "item": id}));
            }
            _ => {}
        }
        self.count(&format!("fault_{k}"));
        Ok(())
    }

    fn run_ends(&mut self) {
        let vs = self.vs;
        let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.ends)
            .into_iter()
            .partition(|e| e.0 <= vs);
        self.ends = rest;
        for (_, what) in due {
            match what {
                "media_ok" => {
                    if let Some(o) = self.off.as_ref() {
                        o.station
                            .media_faults()
                            .fail_all
                            .store(false, Ordering::SeqCst)
                    }
                }
                "db_ok" => {
                    if let Some(o) = self.off.as_ref() {
                        o.station.set_state_fault(FaultMode::None)
                    }
                }
                _ => {}
            }
        }
        let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.damaged)
            .into_iter()
            .partition(|d| d.0 <= vs);
        self.damaged = rest;
        for (_, id, bak, live) in due {
            if std::fs::copy(&bak, &live).is_ok() {
                if let Some(o) = self.off.as_mut() {
                    o.station.release_item(id);
                }
                let _ = std::fs::remove_file(&bak);
                self.log("restore", json!({"item": id}));
            }
        }
    }

    // ---- sampling and the report -------------------------------------------------------------------------

    fn sample(&mut self, force: bool) {
        let hour = (self.vs / 3600.0).floor() as i64;
        if !force && hour == self.last_sample_h {
            return;
        }
        self.last_sample_h = hour;
        let pid = std::process::id();
        let p = procstats::sample(pid).unwrap_or_default();
        if self.first_sample.is_none() && self.vs >= 2.0 * 3600.0 {
            self.first_sample = Some(p);
        }
        let (as_run_mem, queue, alerts, placed) = self
            .off
            .as_ref()
            .map(|o| {
                (
                    o.station.as_run.len(),
                    o.station.sched.st.queue.len(),
                    o.station.alerts().active().len(),
                    o.station.cond.placed.len(),
                )
            })
            .unwrap_or((0, 0, 0, 0));
        let total_rows =
            self.as_run_total + self.off.as_ref().map_or(0, |o| o.station.as_run_total);
        let disk = procstats::dir_bytes(&self.w.data.join("stations"))
            + procstats::dir_bytes(&self.w.data.join("backup"));
        let dead_runs = self.dead.lock().map(|d| d.runs.len()).unwrap_or(0);
        let wall = self.wall_before + self.t_wall0.elapsed().as_secs_f64();
        let _ = writeln!(
            self.csv,
            "{:.3},{:.0},{},{},{},{},{:.1},{},{},{},{},{},{},{},{},{},{},{}",
            self.vs / 86_400.0,
            wall,
            p.rss_kb,
            p.mem_kb,
            p.threads,
            p.fds,
            p.cpu_s,
            total_rows,
            as_run_mem,
            queue,
            alerts,
            placed,
            disk,
            dead_runs,
            self.restarts,
            self.counts.get("ops_refused").copied().unwrap_or(0),
            procstats::live_heap_bytes().map_or(-1, |b| (b / 1024) as i64),
            procstats::sqlite_bytes().map_or(-1, |b| (b / 1024) as i64)
        );
        let _ = self.csv.flush();
        if hour % 24 == 0 {
            println!(
                "day {:>5.1}  wall {:>6.0} s  mem {:>6} kB  threads {:>3}  fds {:>3}  as-run {:>6}  state {:>7} kB  dead-air runs {}  restarts {}",
                self.vs / 86_400.0,
                wall,
                p.mem_kb,
                p.threads,
                p.fds,
                total_rows,
                disk / 1024,
                dead_runs,
                self.restarts
            );
        }
        self.save_state();
    }

    fn save_state(&self) {
        let v = json!({
            "vs": self.vs, "seg_base_s": self.seg_base_s, "wall_ms": self.wall_now(),
            "wall_used_s": self.wall_before + self.t_wall0.elapsed().as_secs_f64(),
            "rng": self.rng.0, "as_run_total": self.as_run_total
                + self.off.as_ref().map_or(0, |o| o.station.as_run_total),
            "restarts": self.restarts, "imports": self.imports,
            "next_op": self.next_op, "next_fault": self.next_fault,
            "counts": self.counts,
            "music": self.w.music, "liners": self.w.liners, "office": self.w.office, "office_b": self.w.office_b,
            "windows": self.windows.iter().map(|w| json!([w.0, w.1, w.2])).collect::<Vec<_>>(),
            "dead_runs": self.dead.lock().map(|d| d.runs.iter().map(|r| json!([r.0, r.1])).collect::<Vec<_>>()).unwrap_or_default(),
            "dead_frames": self.dead.lock().map(|d| d.frames).unwrap_or(0),
            "bad": self.bad.iter().map(|b| json!([b.0, b.1, b.2])).collect::<Vec<_>>(),
            "shifts": self.shifts.iter().map(|x| json!([x.runs_before, x.max_id, x.on_air, x.ms])).collect::<Vec<_>>(),
            "waits": self.waits.iter().map(|w| json!([w.rows, w.from, w.to, w.why])).collect::<Vec<_>>(),
        });
        let _ = std::fs::write(self.work.join("soak.json"), v.to_string());
    }
}

/// `(plays, unended rows, worst unexplained gap ms, the gaps, recovery gaps after a failed damaged file)`.
type AsRunCheck = (u64, u64, i64, Vec<Value>, Vec<i64>);

/// Every play in `state.db`: the count, and the gaps between one play's end and the next one's start that
/// are over 50 ms and no window explains.
fn as_run_check(
    db: &Path,
    windows: &[(i64, i64, &'static str)],
    bad: &[(i64, i64, i64)],
    shifts: &[Shift],
) -> Result<AsRunCheck, String> {
    let c = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let mut q = c
        .prepare(
            "SELECT id, origin, started_ms, ended_ms, end_reason, title, item_id FROM as_run ORDER BY id",
        )
        .map_err(|e| e.to_string())?;
    type Row = (
        i64,
        String,
        i64,
        Option<i64>,
        Option<String>,
        String,
        Option<i64>,
    );
    let rows: Vec<Row> = q
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let total = rows.len() as u64;
    let mut unended = 0u64;
    let mut worst = 0i64;
    let mut findings = vec![];
    // Gaps after an item whose damaged file failed on air: the failure handling's own recovery, listed apart.
    let mut recovery = vec![];
    let mut prev: Option<&Row> = None;
    let last_id = rows.last().map(|r| r.0);
    for r in &rows {
        if r.3.is_none() && Some(r.0) != last_id {
            unended += 1;
        }
        if r.1 == "restart" || r.1 == "cart" {
            continue;
        }
        if let Some(p) = prev {
            if let Some(end) = p.3 {
                // In one wall time: less any clock step or outage between the two.
                let gap = r.2 - end - shift_between(shifts, p.0, r.0);
                // A fault or a hold/stop/assist explains a gap of any length; an operator action only one
                // of up to 3 s (a manual transition's own silence, as for the dead-air runs): a remove or a
                // reshuffle next to a boundary must not excuse a long hole.
                let explained = windows
                    .iter()
                    .any(|w| end <= w.1 && r.2 >= w.0 && (w.2 != "op" || gap <= 3_000));
                let failed_bad = p.4.as_deref() == Some("error")
                    && p.6.is_some_and(|it| {
                        bad.iter().any(|d| d.0 == it && end >= d.1 && end <= d.2)
                    });
                if gap > 50 && failed_bad {
                    recovery.push(gap);
                } else if gap > 50 && !explained {
                    worst = worst.max(gap);
                    if findings.len() < 50 {
                        findings.push(json!({"after_id": p.0, "after": p.5, "before_id": r.0,
                            "before": r.5, "gap_ms": gap, "end_reason": p.4}));
                    }
                }
            }
        }
        prev = Some(r);
    }
    Ok((total, unended, worst, findings, recovery))
}

/// The dead-air runs that nothing explains: not a window (an action or a fault), not a damaged file that was
/// on air, not the source's own quiet (`explain.rs`).
fn unexplained_runs(
    s: &Soak,
    runs: &[(i64, i64)],
    ex: &mut Explainer,
    op_runs: &mut Vec<i64>,
    recovery: &mut Vec<i64>,
) -> Vec<Value> {
    let mut bad = vec![];
    for (i, (start, len)) in runs.iter().enumerate() {
        let view = row_view(&s.shifts, epoch_of_run(&s.shifts, i));
        let (a, b) = (*start, start + len);
        let hit: Vec<&(i64, i64, &str)> =
            s.windows.iter().filter(|w| a <= w.1 && b >= w.0).collect();
        if !hit.is_empty() && hit.iter().any(|w| w.2 != "op") {
            continue;
        }
        if !hit.is_empty() {
            // Only operator-action windows cover it. The music's own quiet is not the operator's doing;
            // what is left and is short is the manual transition's own silence (listed apart): `next` leaves
            // 55 ms, `fade_next` and `switch` 110 ms with tones, and a fade of the outgoing item under a
            // quiet start of the next up to about 3 s (the 2 s fade of 4.7 plus the quiet). Longer is dead air.
            if ex.unexplained(a, *len, &s.bad, &view).is_none() {
                continue;
            }
            if *len <= 3_000 {
                op_runs.push(*len);
                continue;
            }
        }
        if let Some(why) = ex.unexplained(a, *len, &s.bad, &view) {
            // A play of a damaged file that failed on air, and the next item starting a moment later: the
            // failure handling's own recovery (listed with the as-run gaps), not dead air.
            if *len <= 500 && s.failed_damaged_play_ended_at(a) {
                recovery.push(*len);
                continue;
            }
            bad.push(
                json!({"wall_ms": a, "ms": len, "day": (a - T0) as f64 / 86_400_000.0, "why": why}),
            );
        }
    }
    bad
}

pub fn run(a: SoakArgs) -> ExitCode {
    match run_inner(a) {
        Ok(ok) => {
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("soak: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(a: SoakArgs) -> Result<bool, String> {
    let set = a
        .set
        .clone()
        .or_else(|| std::env::var_os("HOTCLOCK_TESTDATA").map(|r| PathBuf::from(r).join("office")))
        .ok_or("give --set DIR (the office set) or set HOTCLOCK_TESTDATA")?;
    let work = a.work_dir.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("hotclock-soak-{}", std::process::id()))
    });
    std::fs::create_dir_all(&a.out).map_err(|e| format!("{}: {e}", a.out.display()))?;
    let resuming = (a.resume || a.report_only) && work.join("soak.json").exists();
    let (w, saved): (World, Option<Value>) = if resuming {
        let s: Value = serde_json::from_str(
            &std::fs::read_to_string(work.join("soak.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let ids = |k: &str| -> Vec<i64> {
            s[k].as_array()
                .map(|a| a.iter().filter_map(Value::as_i64).collect())
                .unwrap_or_default()
        };
        (
            World {
                data: work.join("data"),
                music: ids("music"),
                liners: ids("liners"),
                office: s["office"].as_i64().unwrap_or(1),
                office_b: s["office_b"].as_i64().unwrap_or(2),
            },
            Some(s),
        )
    } else {
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        println!("importing {} ...", set.display());
        let t = Instant::now();
        let w = build_world(&work, &set)?;
        println!(
            "library ready: {} music items, {} liners, in {:.0} s",
            w.music.len(),
            w.liners.len(),
            t.elapsed().as_secs_f64()
        );
        (w, None)
    };
    let csv_path = a.out.join("samples.csv");
    let fresh_csv = !csv_path.exists() || !resuming;
    let mut csv = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&csv_path)
        .map_err(|e| e.to_string())?;
    if fresh_csv {
        writeln!(
            csv,
            "day,wall_s,rss_kb,mem_kb,threads,fds,cpu_s,as_run_rows,as_run_in_memory,queue,alerts,voices,state_bytes,dead_air_runs,restarts,ops_refused,live_heap_kb,sqlite_kb"
        )
        .map_err(|e| e.to_string())?;
    }
    let events = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(a.out.join("events.jsonl"))
        .map_err(|e| e.to_string())?;
    let mut s = Soak {
        w,
        work: work.clone(),
        off: None,
        rng: Rng(saved
            .as_ref()
            .and_then(|s| s["rng"].as_u64())
            .unwrap_or(a.seed | 1)),
        vs: saved.as_ref().and_then(|s| s["vs"].as_f64()).unwrap_or(0.0),
        seg_base_s: 0.0,
        dead: Arc::new(Mutex::new(DeadAir::default())),
        origin: Arc::new(AtomicI64::new(T0)),
        pending: vec![],
        windows: vec![],
        shifts: vec![],
        waits: vec![],
        next_op: 0.0,
        next_fault: 0.0,
        damaged: vec![],
        bad: vec![],
        ends: vec![],
        counts: Default::default(),
        events,
        csv,
        as_run_total: saved
            .as_ref()
            .and_then(|s| s["as_run_total"].as_u64())
            .unwrap_or(0),
        restarts: saved
            .as_ref()
            .and_then(|s| s["restarts"].as_u64())
            .unwrap_or(0),
        last_sample_h: -1,
        errors: vec![],
        first_sample: None,
        t_wall0: Instant::now(),
        wall_before: saved
            .as_ref()
            .and_then(|s| s["wall_used_s"].as_f64())
            .unwrap_or(0.0),
        imports: saved
            .as_ref()
            .and_then(|s| s["imports"].as_u64())
            .unwrap_or(0),
        args: a,
    };
    if let Some(sv) = &saved {
        s.next_op = sv["next_op"].as_f64().unwrap_or(0.0);
        s.next_fault = sv["next_fault"].as_f64().unwrap_or(0.0);
        if let Some(c) = sv["counts"].as_object() {
            for (k, v) in c {
                s.counts.insert(k.clone(), v.as_u64().unwrap_or(0));
            }
        }
        for w in sv["windows"].as_array().into_iter().flatten() {
            let why: &'static str = match w[2].as_str() {
                Some("hold") => "hold",
                Some("assist") => "assist",
                Some("stop") => "stop",
                Some("restart") => "restart",
                Some("device_lost") => "device_lost",
                Some("media_fail") => "media_fail",
                Some("clock_step") => "clock_step",
                Some("start") => "start",
                Some("op") => "op",
                _ => "other",
            };
            s.windows
                .push((w[0].as_i64().unwrap_or(0), w[1].as_i64().unwrap_or(0), why));
        }
        for x in sv["shifts"].as_array().into_iter().flatten() {
            s.shifts.push(Shift {
                runs_before: x[0].as_u64().unwrap_or(0) as usize,
                max_id: x[1].as_i64().unwrap_or(0),
                on_air: x[2]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_i64)
                    .collect(),
                ms: x[3].as_i64().unwrap_or(0),
            });
        }
        for w in sv["waits"].as_array().into_iter().flatten() {
            s.waits.push(Wait {
                rows: w[0]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_i64)
                    .collect(),
                from: w[1].as_i64().unwrap_or(0),
                to: w[2].as_i64().unwrap_or(0),
                why: if w[3].as_str() == Some("assist") {
                    "assist"
                } else {
                    "hold"
                },
            });
        }
        for b in sv["bad"].as_array().into_iter().flatten() {
            s.bad.push((
                b[0].as_i64().unwrap_or(0),
                b[1].as_i64().unwrap_or(0),
                b[2].as_i64().unwrap_or(0),
            ));
        }
        if let Ok(mut d) = s.dead.lock() {
            d.frames = sv["dead_frames"].as_u64().unwrap_or(0);
            for r in sv["dead_runs"].as_array().into_iter().flatten() {
                d.runs
                    .push((r[0].as_i64().unwrap_or(0), r[1].as_i64().unwrap_or(0)));
            }
        }
    }
    let start_wall = saved
        .as_ref()
        .and_then(|sv| sv["wall_ms"].as_i64())
        .map_or(T0, |w| w + 3_000);
    if resuming && !s.args.report_only {
        s.vs += 3.0;
    }
    s.seg_base_s = s.vs;
    if !s.args.report_only {
        s.open_station(start_wall)?;
    }
    if resuming && !s.args.report_only {
        s.restarts += 1;
        s.log(
            "resume",
            json!({"note": "the run was stopped and continued"}),
        );
    }
    let end_s = s.args.days * 86_400.0;
    let mean_op = 86_400.0 / s.args.ops_per_day.max(1e-9);
    let mean_fault = 86_400.0 / s.args.faults_per_day.max(1e-9);
    if s.next_op == 0.0 {
        s.next_op = s.vs + s.rng.exp(mean_op);
        s.next_fault = s.vs + s.rng.exp(mean_fault);
    }
    let mut stopped_early = false;
    let mut failure: Option<String> = None;
    while s.vs < end_s && !s.args.report_only {
        if let Some(l) = s.args.wall_limit_s {
            if s.t_wall0.elapsed().as_secs_f64() >= l {
                stopped_early = true;
                break;
            }
        }
        s.vs += 1.0;
        if let Err(e) = s.step_second() {
            failure = Some(format!("step at virtual {:.0} s: {e}", s.vs));
            s.errors.push(failure.clone().unwrap_or_default());
            break;
        }
        s.run_ends();
        if s.vs >= s.next_op {
            s.make_op();
            s.next_op = s.vs + s.rng.exp(mean_op);
        }
        if s.vs >= s.next_fault {
            if let Err(e) = s.make_fault() {
                failure = Some(format!("fault injection: {e}"));
                break;
            }
            s.next_fault = s.vs + s.rng.exp(mean_fault);
        }
        s.sample(false);
    }
    if !s.args.report_only {
        s.sample(true);
    }
    let vs_done = s.vs;
    let wall_used = s.wall_before + s.t_wall0.elapsed().as_secs_f64();
    // The last of everything on disk, then the reports.
    let (alerts_by_code, last_snapshot): (Vec<(String, usize)>, Option<Value>) = s
        .off
        .as_ref()
        .map(|o| {
            let codes = [
                "silence",
                "item_missing",
                "item_truncated",
                "item_silent",
                "item_decode_failed",
                "item_quarantined",
                "decode_slow",
                "media_store_failing",
                "queue_starved",
                "device_lost",
                "xrun_burst",
                "restarted",
                "db_unavailable",
                "disk_full",
                "clock_jump",
                "backup_check_failed",
                "playlist_depleted",
                "emergency_failed",
            ];
            (
                codes
                    .iter()
                    .map(|c| ((*c).to_string(), o.station.alerts().count_raised(c)))
                    .collect(),
                o.station
                    .health()
                    .snapshot
                    .map(|i| json!({"date": i.date, "check": i.check, "bytes": i.bytes})),
            )
        })
        .unwrap_or_default();
    s.close_station();
    if !s.args.report_only {
        s.save_state();
    }
    s.resolve_waits();
    let (rows, unended, worst_gap, gap_findings, recovery_gaps) = as_run_check(
        &s.state_path(),
        &s.windows,
        &s.bad,
        &s.shifts,
    )
    .unwrap_or((0, 0, 0, vec![], vec![]));
    let dead_runs: Vec<(i64, i64)> = s.dead.lock().map(|d| d.runs.clone()).unwrap_or_default();
    let mut ex = Explainer::new(&s.state_path(), &s.w.data);
    let mut op_runs: Vec<i64> = vec![];
    let mut recovery_runs: Vec<i64> = vec![];
    let unexplained: Vec<Value> =
        unexplained_runs(&s, &dead_runs, &mut ex, &mut op_runs, &mut recovery_runs);
    let snapshots = std::fs::read_dir(s.w.data.join("backup"))
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".db"))
                .count()
        })
        .unwrap_or(0);
    let csv_text = std::fs::read_to_string(s.args.out.join("samples.csv")).unwrap_or_default();
    let series: Vec<Vec<f64>> = csv_text
        .lines()
        .skip(1)
        .map(|l| l.split(',').filter_map(|x| x.parse().ok()).collect())
        .filter(|v: &Vec<f64>| v.len() >= 16)
        .collect();
    // Memory flat: the medians of the first and of the last half day after virtual day 1 (a single sample of a
    // process that frees and allocates moves by megabytes). `mem` is the physical footprint on macOS and RssAnon
    // on Linux; RSS is reported beside it (on macOS it counts pages the allocator has freed and not returned).
    let after: Vec<&Vec<f64>> = series.iter().filter(|r| r[0] >= 1.0).collect();
    let med = |v: Vec<f64>| -> f64 {
        let mut v = v;
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    };
    let window = |col: usize, from: f64, to: f64| -> f64 {
        med(after
            .iter()
            .filter(|r| r[0] >= from && r[0] < to)
            .map(|r| r[col])
            .collect())
    };
    let last_day = after.last().map_or(1.0, |r| r[0]);
    let growth = |col: usize| -> (f64, f64, f64) {
        let (f, l) = (
            window(col, 1.0, 1.5),
            window(col, last_day - 0.5, last_day + 1.0),
        );
        (f, l, if f > 0.0 { (l - f) / f * 100.0 } else { 0.0 })
    };
    let (mem_first, mem_last, mem_growth_pct) = growth(3);
    // The plateau: the median of the last five days against the five days before them. The first figure is the
    // design's wording (flat after the first virtual day); on the real set memory keeps rising for about ten
    // days as the allocator's working set settles, so this one is what says whether it is still rising.
    let plateau = |col: usize| -> f64 {
        let (a, b, c) = (last_day - 10.0, last_day - 5.0, last_day + 1.0);
        let (e, l) = (window(col, a, b), window(col, b, c));
        if e > 0.0 {
            (l - e) / e * 100.0
        } else {
            0.0
        }
    };
    let mem_plateau_pct = if last_day >= 12.0 {
        plateau(3)
    } else {
        mem_growth_pct
    };
    let (rss_first, rss_last, rss_growth_pct) = growth(2);
    // The live heap (built with `alloc-count`): what the process holds, whatever the allocator keeps resident.
    let (heap_first, heap_last, heap_growth_pct) =
        if after.iter().all(|r| r.len() > 16 && r[16] >= 0.0) {
            growth(16)
        } else {
            (-1.0, -1.0, 0.0)
        };
    let mem_max = after.iter().map(|r| r[3]).fold(0.0, f64::max);
    let rss_max = after.iter().map(|r| r[2]).fold(0.0, f64::max);
    let (threads_first, threads_last) = (
        after.first().map_or(0.0, |r| r[4]),
        after.last().map_or(0.0, |r| r[4]),
    );
    let (fds_first, fds_last) = (
        after.first().map_or(0.0, |r| r[5]),
        after.last().map_or(0.0, |r| r[5]),
    );
    let summary = json!({
        "virtual_days": vs_done / 86_400.0, "requested_days": s.args.days, "stopped_early": stopped_early,
        "wall_s": wall_used, "speed_x": vs_done / wall_used.max(1e-9),
        "seed": s.args.seed, "failure": failure, "errors": s.errors,
        "actions": s.counts, "restarts": s.restarts,
        "as_run": {"rows_in_state_db": rows, "rows_written_by_the_station": s.as_run_total,
                   "unended_rows": unended, "gaps_over_50ms_unexplained": gap_findings.len(), "worst_gap_ms": worst_gap,
                   "recovery_gaps_after_a_failed_damaged_file_ms": recovery_gaps,
                   "findings": gap_findings},
        "dead_air": {"runs_over_50ms": dead_runs.len(), "unexplained": unexplained.len(), "findings": unexplained,
                     "after_a_failed_damaged_file_ms": recovery_runs,
                     "after_operator_actions": {"runs": op_runs.len(), "longest_ms": op_runs.iter().max().copied().unwrap_or(0), "lengths_ms": op_runs},
                     "frames_checked": s.dead.lock().map(|d| d.frames).unwrap_or(0)},
        "memory": {"mem_kb_day1": mem_first, "mem_kb_last": mem_last, "mem_kb_max": mem_max, "growth_pct_after_day1": mem_growth_pct,
                   "growth_pct_last_5_days_vs_5_before": mem_plateau_pct,
                   "live_heap_kb_day1": heap_first, "live_heap_kb_last": heap_last, "live_heap_growth_pct_after_day1": heap_growth_pct,
                   "rss_kb_day1": rss_first, "rss_kb_last": rss_last, "rss_kb_max": rss_max, "rss_growth_pct_after_day1": rss_growth_pct,
                   "threads_day1": threads_first, "threads_last": threads_last, "fds_day1": fds_first, "fds_last": fds_last},
        "snapshots_kept": snapshots, "last_snapshot": last_snapshot,
        "alerts_raised": alerts_by_code.iter().filter(|c| c.1 > 0).map(|c| json!({c.0.clone(): c.1})).collect::<Vec<_>>(),
        "state_bytes": procstats::dir_bytes(&s.w.data.join("stations")) + procstats::dir_bytes(&s.w.data.join("backup")),
    });
    let _ = std::fs::write(
        s.args.out.join("summary.json"),
        serde_json::to_string_pretty(&summary).unwrap_or_default(),
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).unwrap_or_default()
    );
    let ok = failure.is_none()
        && unexplained.is_empty()
        && gap_findings.is_empty()
        && mem_plateau_pct < 5.0
        && !stopped_early;
    if !s.args.keep && !stopped_early && !s.args.report_only {
        let _ = std::fs::remove_dir_all(&work);
    }
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(runs_before: usize, max_id: i64, on_air: &[i64], ms: i64) -> Shift {
        Shift {
            runs_before,
            max_id,
            on_air: on_air.to_vec(),
            ms,
        }
    }

    /// Row 5 is on air across a 10-minute step back and then a 30 s outage; row 6 starts after the step, is on
    /// air at the outage and ends after it; row 7 starts after both. Each stretch sees only its own rows, with
    /// their times in its own wall time.
    #[test]
    fn a_clock_step_and_an_outage_move_the_rows_on_air_into_the_new_wall_time() {
        let shifts = vec![sh(10, 5, &[5], -600_000), sh(20, 6, &[6], 30_000)];
        let v0 = row_view(&shifts, 0);
        assert_eq!((v0.lo, v0.hi), (i64::MIN, 5));
        // Before the step row 5's end (written after both faults) is brought back by both.
        assert_eq!(v0.end_shift.get(&5), Some(&-600_000));
        assert_eq!(v0.start_shift.get(&5), None);
        let v1 = row_view(&shifts, 1);
        assert_eq!((v1.lo, v1.hi, v1.carried.clone()), (5, 6, vec![5]));
        assert_eq!(v1.start_shift.get(&5), Some(&-600_000));
        assert_eq!(v1.end_shift.get(&6), Some(&30_000));
        let v2 = row_view(&shifts, 2);
        assert_eq!((v2.lo, v2.hi, v2.carried.clone()), (6, i64::MAX, vec![6]));
        assert_eq!(v2.start_shift.get(&6), Some(&30_000));
        assert_eq!(v2.widen_ms, 630_000);
        // A run recorded before the first fault is in stretch 0, one after both in stretch 2.
        assert_eq!(epoch_of_run(&shifts, 9), 0);
        assert_eq!(epoch_of_run(&shifts, 10), 1);
        assert_eq!(epoch_of_run(&shifts, 25), 2);
        // Between rows that ended and started on either side of a fault nobody was on air for, the wall time
        // moved by it; across a fault the earlier row was on air for, it did not (its end is in the new time).
        let idle = vec![sh(0, 8, &[], 600_000)];
        assert_eq!(shift_between(&idle, 8, 9), 600_000);
        assert_eq!(shift_between(&idle, 7, 8), 0);
        assert_eq!(shift_between(&shifts, 5, 6), 0);
        assert_eq!(shift_between(&shifts, 6, 7), 0);
    }
}
