//! Is a stretch of silence in the output dead air, or the music being quiet? (M1.8, for both soaks.)
//!
//! A run is explained when every moment of it has an item on air (the as-run log says which, from when to
//! when, and where in its file each play began, `bound_in_ms`) and each of those items has a quiet stretch of
//! its own, as long as the run, within 350 ms of where the run falls in the item's file. The office set is
//! ambient music with long quiet passages, so the source has to be consulted; a gap between two items, or a
//! muted passage of ordinary program, is not explained by it.

use library::{Library, Paths};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Alignment tolerance, ms.
pub const TOLERANCE_MS: f64 = 350.0;

/// The level in the source under which a play is silent at the output (judged at -60 dBFS peak): -60 less the
/// gain the play had (its as-run row says, `gain_db=`), 1 dB more for rounding, and never above -40 dBFS (the
/// bound for rows written before the gain was recorded: the loudness gain goes down to about -20 dB on loud
/// masters). A passage at -45 dBFS in a file boosted by 12 dB is heard at -33 dBFS: silence there is dead air.
pub fn quiet_threshold_dbfs(gain_db: Option<f64>) -> f64 {
    gain_db.map_or(-40.0, |g| (-59.0 - g).min(-40.0))
}

/// The quiet stretches of an item's source file: `(start ms, end ms)` where both channels stay under
/// `thr_dbfs` peak for 40 ms or more.
pub fn quiet_map(path: &Path, thr_dbfs: f64) -> Vec<(f64, f64)> {
    let thr = 10f32.powf(thr_dbfs as f32 / 20.0);
    let mut out = vec![];
    let mut frame: u64 = 0;
    let mut start: Option<u64> = None;
    let mut rate = 48_000u32;
    let close = |s: u64, e: u64, rate: u32, out: &mut Vec<(f64, f64)>| {
        let ms = |f: u64| f as f64 * 1000.0 / f64::from(rate);
        if ms(e - s) >= 40.0 {
            out.push((ms(s), ms(e)));
        }
    };
    let r = analysis::decode::decode_file(path, |smp, ch, sr| {
        rate = sr;
        for f in smp.chunks_exact(ch) {
            let peak = f.iter().fold(0.0f32, |m, x| m.max(x.abs()));
            match (peak < thr, start) {
                (true, None) => start = Some(frame),
                (false, Some(s)) => {
                    close(s, frame, sr, &mut out);
                    start = None;
                }
                _ => {}
            }
            frame += 1;
        }
    });
    if let (Ok(_), Some(s)) = (r, start) {
        close(s, frame, rate, &mut out);
    }
    out
}

/// What the cable between a station's output and `loopcheck`'s input does to the program, so a loopcheck gap
/// can be judged against what the recording would have carried. `loopcheck` calls a 10 ms window quiet when
/// its RMS (louder channel) is under `threshold_db` *as recorded*; the source is quiet at a place when, put
/// through the play's gain and this model, it is under `threshold_db + margin_db` the same way. Without the
/// model the source was judged on its full-band peak at -59 dBFS less the gain, which a loop that loses level
/// or bass cannot meet: Box A's rear jack is a mic input (a 3rd-order high-pass near 790 Hz, -4.5 dB above
/// it), so the rests of a funk track, carried by bass that never reaches the recording, read as 50 to 210 ms
/// "dropouts" while the source never went under -40 dBFS (BUILDLOG, "Loopcheck gaps on Box A").
#[derive(Clone, Debug, PartialEq)]
pub struct LoopModel {
    /// The loop's gain in its passband, dB (station output dBFS to recorded dBFS).
    pub gain_db: f64,
    /// The loop's high-pass corner (Butterworth), Hz; 0 for none.
    pub highpass_hz: f64,
    pub highpass_order: u32,
    /// loopcheck's threshold, dBFS RMS in 10 ms windows.
    pub threshold_db: f64,
    /// The model's allowance, dB: a source predicted this far above the threshold still counts as quiet.
    pub margin_db: f64,
}

/// The default allowance for the loop model's error (the Box A fit is within 1.2 dB rms, 90 Hz to 16 kHz).
pub const LOOP_MARGIN_DB: f64 = 3.0;

impl LoopModel {
    /// A flat, unity loop (a line input).
    pub fn flat(threshold_db: f64) -> Self {
        Self {
            gain_db: 0.0,
            highpass_hz: 0.0,
            highpass_order: 0,
            threshold_db,
            margin_db: LOOP_MARGIN_DB,
        }
    }

    /// `GAIN_DB[,HIGHPASS_HZ[,ORDER]]`, e.g. `-4.5,790,3` (order 2 when left out).
    pub fn parse(spec: &str, threshold_db: f64, margin_db: f64) -> Result<Self, String> {
        let bad = || format!("loop model `{spec}`: expected GAIN_DB[,HIGHPASS_HZ[,ORDER]]");
        let mut it = spec.split(',').map(str::trim);
        let gain_db: f64 = it.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
        let highpass_hz: f64 = match it.next() {
            Some(s) => s.parse().map_err(|_| bad())?,
            None => 0.0,
        };
        let highpass_order: u32 = match it.next() {
            Some(s) => s.parse().map_err(|_| bad())?,
            None if highpass_hz > 0.0 => 2,
            None => 0,
        };
        if it.next().is_some() || !(0.0..20_000.0).contains(&highpass_hz) || highpass_order > 8 {
            return Err(bad());
        }
        Ok(Self {
            gain_db,
            highpass_hz,
            highpass_order: if highpass_hz > 0.0 {
                highpass_order.max(1)
            } else {
                0
            },
            threshold_db,
            margin_db,
        })
    }

    pub fn describe(&self) -> String {
        let hp = if self.highpass_hz > 0.0 {
            format!(
                ", high-pass {:.0} Hz order {}",
                self.highpass_hz, self.highpass_order
            )
        } else {
            String::new()
        };
        format!(
            "loop gain {:+.1} dB{hp}; quiet under {:.1} dBFS RMS predicted (threshold {:.1} + margin {:.1})",
            self.gain_db,
            self.threshold_db + self.margin_db,
            self.threshold_db,
            self.margin_db
        )
    }

    fn sections(&self, rate: u32) -> Vec<Section> {
        let n = self.highpass_order;
        if self.highpass_hz <= 0.0 || n == 0 {
            return vec![];
        }
        let fs = f64::from(rate);
        let fc = self.highpass_hz.min(fs * 0.45);
        let mut v = vec![];
        // Butterworth: pole pairs at angles k*pi/n (odd n, plus one real pole) or (2k-1)*pi/(2n) (even n).
        for k in 1..=n / 2 {
            let ang = if n % 2 == 1 {
                std::f64::consts::PI * f64::from(k) / f64::from(n)
            } else {
                std::f64::consts::PI * f64::from(2 * k - 1) / f64::from(2 * n)
            };
            let q = 1.0 / (2.0 * ang.cos());
            let w0 = std::f64::consts::TAU * fc / fs;
            let (s, c) = w0.sin_cos();
            let alpha = s / (2.0 * q);
            let a0 = 1.0 + alpha;
            v.push(Section {
                b: [(1.0 + c) / 2.0 / a0, -(1.0 + c) / a0, (1.0 + c) / 2.0 / a0],
                a: [-2.0 * c / a0, (1.0 - alpha) / a0],
            });
        }
        if n % 2 == 1 {
            let k = (std::f64::consts::PI * fc / fs).tan();
            v.push(Section {
                b: [1.0 / (1.0 + k), -1.0 / (1.0 + k), 0.0],
                a: [(k - 1.0) / (k + 1.0), 0.0],
            });
        }
        v
    }
}

/// One filter section (normalised biquad; a first-order one has `b[2] = a[1] = 0`).
#[derive(Clone, Copy, Debug)]
struct Section {
    b: [f64; 3],
    a: [f64; 2],
}

/// The level a loop would record, fed the source frame by frame: 10 ms windows at 5 ms steps, RMS of the louder
/// channel after the play's gain and the loop model; the stretches where every window is quiet.
pub struct LoopLevel {
    model: LoopModel,
    scale: f64,
    rate: u32,
    channels: usize,
    sections: Vec<Section>,
    /// Per channel, per section: `[x1, x2, y1, y2]`.
    state: Vec<Vec<[f64; 4]>>,
    half: usize,
    fill: usize,
    acc: Vec<f64>,
    prev: Option<Vec<f64>>,
    /// Index of the next window (each starts 5 ms after the last).
    win: u64,
    quiet_from: Option<u64>,
    pub stretches: Vec<(f64, f64)>,
}

impl LoopLevel {
    /// `play_gain_db` is the play's gain (the as-run row's `gain_db`).
    pub fn new(model: &LoopModel, play_gain_db: f64) -> Self {
        Self {
            model: model.clone(),
            scale: 10f64.powf((play_gain_db + model.gain_db) / 20.0),
            rate: 0,
            channels: 0,
            sections: vec![],
            state: vec![],
            half: 0,
            fill: 0,
            acc: vec![],
            prev: None,
            win: 0,
            quiet_from: None,
            stretches: vec![],
        }
    }

    /// Interleaved frames of `channels` channels at `rate` (fixed for one file).
    pub fn feed(&mut self, smp: &[f32], channels: usize, rate: u32) {
        if channels == 0 {
            return;
        }
        if self.rate != rate || self.channels != channels {
            self.rate = rate;
            self.channels = channels;
            self.sections = self.model.sections(rate);
            self.state = vec![vec![[0.0; 4]; self.sections.len()]; channels];
            self.half = (rate as usize * 5 / 1000).max(1);
            self.fill = 0;
            self.acc = vec![0.0; channels];
        }
        for f in smp.chunks_exact(channels) {
            for (ch, &x) in f.iter().enumerate() {
                let mut y = f64::from(x) * self.scale;
                for (s, st) in self.sections.iter().zip(self.state[ch].iter_mut()) {
                    let out = s.b[0] * y + s.b[1] * st[0] + s.b[2] * st[1]
                        - s.a[0] * st[2]
                        - s.a[1] * st[3];
                    st[1] = st[0];
                    st[0] = y;
                    st[3] = st[2];
                    st[2] = out;
                    y = out;
                }
                self.acc[ch] += y * y;
            }
            self.fill += 1;
            if self.fill == self.half {
                let cur = std::mem::replace(&mut self.acc, vec![0.0; self.channels]);
                if let Some(p) = self.prev.take() {
                    let ms = p
                        .iter()
                        .zip(&cur)
                        .map(|(a, b)| (a + b) / (2 * self.half) as f64)
                        .fold(0.0, f64::max);
                    self.window(ms);
                }
                self.prev = Some(cur);
                self.fill = 0;
            }
        }
    }

    fn window(&mut self, ms: f64) {
        let lim = 10f64.powf((self.model.threshold_db + self.model.margin_db) / 10.0);
        let i = self.win;
        self.win += 1;
        match (ms < lim, self.quiet_from) {
            (true, None) => self.quiet_from = Some(i),
            (false, Some(s)) => {
                self.close(s, i - 1);
                self.quiet_from = None;
            }
            _ => {}
        }
    }

    fn close(&mut self, first: u64, last: u64) {
        let (a, b) = (first as f64 * 5.0, last as f64 * 5.0 + 10.0);
        if b - a >= 20.0 {
            self.stretches.push((a, b));
        }
    }

    /// The quiet stretches, `(start ms, end ms)` into the file.
    pub fn finish(mut self) -> Vec<(f64, f64)> {
        if let Some(s) = self.quiet_from.take() {
            if self.win > 0 {
                self.close(s, self.win - 1);
            }
        }
        self.stretches
    }
}

/// [`LoopLevel`] over a whole file.
pub fn loop_quiet_map(path: &Path, play_gain_db: f64, model: &LoopModel) -> Vec<(f64, f64)> {
    let mut l = LoopLevel::new(model, play_gain_db);
    let _ = analysis::decode::decode_file(path, |smp, ch, sr| l.feed(smp, ch, sr));
    l.finish()
}

/// Which as-run rows exist in one stretch of wall time, and how to bring their times into it. A clock step, and
/// a device outage (the renderer's origin moves on by the outage while the item on air waits), move the wall
/// clock under the rows on air: their start was written in the old wall time and their end in the new one. A
/// stretch between two such faults sees the rows whose ids are in `(lo, hi]` and the rows `carried` over the
/// fault that opened it; a row's start is moved on by `start_shift` and its end back by `end_shift`, so
/// where a silence falls in a file is exact again and nothing needs hiding.
#[derive(Clone, Debug)]
pub struct RowView {
    pub lo: i64,
    pub hi: i64,
    pub carried: Vec<i64>,
    pub start_shift: HashMap<i64, i64>,
    pub end_shift: HashMap<i64, i64>,
    /// The sum of every shift's size (ms): how far the stored times may be from this stretch's.
    pub widen_ms: i64,
}

impl Default for RowView {
    fn default() -> Self {
        Self {
            lo: i64::MIN,
            hi: i64::MAX,
            carried: vec![],
            start_shift: HashMap::new(),
            end_shift: HashMap::new(),
            widen_ms: 0,
        }
    }
}

pub struct Explainer {
    state_db: PathBuf,
    data_dir: PathBuf,
    /// Quiet maps per `(item, threshold in tenths of a dB)` (with a loop model: per item and play gain).
    cache: HashMap<(i64, i64), Vec<(f64, f64)>>,
    /// Judge the source as the loop records it (loopcheck gaps), not at the output.
    loop_model: Option<LoopModel>,
}

type V = (
    Option<i64>,
    i64,
    i64,
    String,
    Option<f64>,
    String,
    i64,
    bool,
);

impl Explainer {
    /// `state_db` is the station's `state.db`; `data_dir` holds `library.db` and `media/`.
    pub fn new(state_db: &Path, data_dir: &Path) -> Self {
        Self {
            state_db: state_db.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
            cache: HashMap::new(),
            loop_model: None,
        }
    }

    /// Judge silences found in a loop recording: a source is quiet where, through the play's gain and `model`,
    /// it would record under loopcheck's threshold (plus the model's margin).
    pub fn with_loop(mut self, model: LoopModel) -> Self {
        self.loop_model = Some(model);
        self
    }

    /// `None` when the run `[start_ms, start_ms + len_ms)` is explained, else why not. `damaged` lists
    /// `(item id, from, to)` for files that were damaged on purpose in that time.
    pub fn unexplained(
        &mut self,
        start_ms: i64,
        len_ms: i64,
        damaged: &[(i64, i64, i64)],
        view: &RowView,
    ) -> Option<String> {
        let db = match Connection::open_with_flags(&self.state_db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        {
            Ok(d) => d,
            Err(e) => return Some(format!("cannot read {}: {e}", self.state_db.display())),
        };
        let (a, b) = (start_ms, start_ms + len_ms);
        let mut q = match db.prepare(
            "SELECT a.item_id, a.started_ms, a.ended_ms, a.title,
                    CAST(json_extract(t.markers, '$.cue_in') AS REAL), a.flags, a.id
             FROM as_run a LEFT JOIN track_cache t ON t.item_id = a.item_id
             WHERE a.origin != 'restart' AND a.id > ?3 AND a.id <= ?4
               AND a.started_ms <= ?2 + ?5 AND COALESCE(a.ended_ms, a.started_ms + 3600000) >= ?1 - ?5",
        ) {
            Ok(q) => q,
            Err(e) => return Some(format!("as_run query: {e}")),
        };
        let first = view
            .carried
            .iter()
            .map(|c| c - 1)
            .min()
            .unwrap_or(view.lo)
            .min(view.lo);
        let voices: Vec<V> = q
            .query_map([a - 100, b + 100, first, view.hi, view.widen_ms], |r| {
                let (started, ended): (i64, Option<i64>) = (r.get(1)?, r.get(2)?);
                Ok((
                    r.get(0)?,
                    started,
                    ended.unwrap_or(i64::MAX),
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    ended.is_some(),
                ))
            })
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
        // Into this stretch's wall time, and only the rows that exist in it.
        let voices: Vec<V> = voices
            .into_iter()
            .filter(|v| v.6 > view.lo || view.carried.contains(&v.6))
            .map(|mut v| {
                v.1 += view.start_shift.get(&v.6).copied().unwrap_or(0);
                v.2 = if v.7 {
                    v.2 - view.end_shift.get(&v.6).copied().unwrap_or(0)
                } else {
                    v.1 + 3_600_000
                };
                v
            })
            .filter(|v| v.1 <= b + 100 && v.2 >= a - 100)
            .collect();
        if voices.is_empty() {
            return Some("no item on air".into());
        }
        // Every moment of the run needs an item on air.
        // A play of a file damaged on purpose that ends (in an error) is followed by the failure handling's
        // recovery, a gap of about 0.1 s before the next item (listed with the as-run recovery gaps): up to 0.5 s
        // after such a play counts as covered. The next item is still checked on its own.
        let is_damaged = |v: &V| {
            v.0.is_some_and(|it| damaged.iter().any(|d| d.0 == it && a <= d.2 && b >= d.1))
        };
        let mut cover: Vec<(i64, i64)> = voices
            .iter()
            .map(|v| {
                let end = if v.7 && is_damaged(v) { v.2 + 500 } else { v.2 };
                (v.1.max(a), end.min(b))
            })
            .collect();
        cover.sort();
        let mut reach = a;
        for (from, to) in &cover {
            if *from > reach + 100 {
                return Some(format!(
                    "no item on air from {:.2} s to {:.2} s of the run",
                    (reach - a) as f64 / 1000.0,
                    (*from - a) as f64 / 1000.0
                ));
            }
            reach = reach.max(*to);
        }
        if reach < b - 100 {
            return Some(format!(
                "no item on air for the last {:.2} s of the run",
                (b - reach) as f64 / 1000.0
            ));
        }
        let lib = Library::open_worker(Paths::from_data_dir(&self.data_dir)).ok();
        for (item, started, end, title, cue, flags, _, _) in &voices {
            let Some(item) = item else { continue };
            if damaged.iter().any(|d| d.0 == *item && a <= d.2 && b >= d.1) {
                continue;
            }
            let (oa, ob) = ((*started).max(a), (*end).min(b));
            if ob - oa < 50 {
                continue;
            }
            let gain = flags
                .split("gain_db=")
                .nth(1)
                .and_then(|t| {
                    t.split(|c: char| !(c.is_ascii_digit() || c == '-' || c == '.'))
                        .next()
                })
                .and_then(|t| t.parse::<f64>().ok());
            let thr = quiet_threshold_dbfs(gain);
            let media = self.data_dir.join("media");
            let lm = self.loop_model.clone();
            // With a loop model the key is the play's gain (a row without one is taken at 0 dB).
            let key = match &lm {
                Some(_) => (gain.unwrap_or(0.0) * 10.0).round() as i64,
                None => (thr * 10.0).round() as i64,
            };
            let quiet = self.cache.entry((*item, key)).or_insert_with(|| {
                let path: Option<String> = lib.as_ref().and_then(|l| {
                    l.conn()
                        .query_row("SELECT store_path FROM items WHERE id = ?1", [item], |r| {
                            r.get(0)
                        })
                        .ok()
                });
                path.map(|p| match &lm {
                    Some(m) => loop_quiet_map(&media.join(p), gain.unwrap_or(0.0), m),
                    None => quiet_map(&media.join(p), thr),
                })
                .unwrap_or_default()
            });
            let flag = |k: &str| -> Option<f64> {
                flags
                    .split(k)
                    .nth(1)
                    .and_then(|t| t.split(|c: char| !c.is_ascii_digit()).next())
                    .and_then(|t| t.parse::<f64>().ok())
            };
            // Where in the file this play began: the as-run row says (`bound_in_ms`), and a resumed play
            // picked up at `resumed_from_ms`; failing both, the item's cue-in.
            let base = flag("resumed_from_ms=")
                .or_else(|| flag("bound_in_ms="))
                .or(*cue)
                .unwrap_or(0.0);
            let pos_a = base + (oa - started) as f64;
            let pos_b = pos_a + (ob - oa) as f64;
            let ok = quiet.iter().any(|(qa, qb)| {
                *qb - *qa >= (ob - oa) as f64 - 50.0
                    && *qa <= pos_a + TOLERANCE_MS
                    && *qb >= pos_b - TOLERANCE_MS
            });
            if !ok {
                return Some(format!(
                    "`{title}` (item {item}) is audible at {:.1} s into its file",
                    pos_a / 1000.0
                ));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::wav::{WavFormat, WavWriter};
    use library::{import_file, ImportOptions, ImportOutcome};

    fn write(p: &Path, f: impl Fn(usize) -> f32, secs: usize) {
        let mut w = WavWriter::create(p, 48_000, 2, WavFormat::F32).unwrap();
        let mut b = Vec::new();
        for i in 0..secs * 48_000 {
            let x = f(i);
            b.extend_from_slice(&x.to_le_bytes());
            b.extend_from_slice(&x.to_le_bytes());
        }
        w.write_bytes(&b).unwrap();
        w.finish().unwrap();
    }

    /// A tone that is muted from 4 s to 6 s (a quiet passage of the source) and one that never is: a silent run
    /// at that place in the first is the music's own, in the second it is dead air; a run where no item is on
    /// air is dead air either way.
    #[test]
    fn a_muted_passage_of_program_is_dead_air_and_a_quiet_passage_of_the_source_is_not() {
        let d = std::env::temp_dir().join(format!("hotclock-explain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let lib = Library::open(Paths::from_data_dir(&d.join("data"))).unwrap();
        let tone = |i: usize| 0.1 * (i as f32 * 0.05).sin();
        write(
            &d.join("quiet.wav"),
            |i| {
                if (192_000..288_000).contains(&i) {
                    0.0
                } else {
                    tone(i)
                }
            },
            10,
        );
        write(&d.join("loud.wav"), tone, 10);
        let imp = |n: &str| match import_file(
            &lib,
            &d.join(n),
            &ImportOptions {
                category: Some("music".into()),
                source: "t".into(),
            },
        ) {
            ImportOutcome::Done { item_id, .. } => item_id,
            o => panic!("{o:?}"),
        };
        let (q, l) = (imp("quiet.wav"), imp("loud.wav"));
        let state = d.join("state.db");
        let c = Connection::open(&state).unwrap();
        c.execute_batch(
            "CREATE TABLE as_run (id INTEGER PRIMARY KEY, item_id INTEGER, title TEXT, origin TEXT, started_ms INTEGER, ended_ms INTEGER, flags TEXT);
             CREATE TABLE track_cache (item_id INTEGER PRIMARY KEY, markers TEXT);",
        )
        .unwrap();
        let t0 = 1_000_000i64;
        // The quiet item plays from t0 to t0 + 10 s, the loud one from t0 + 20 s to t0 + 30 s.
        for (id, item, title, s) in [(1, q, "quiet", t0), (2, l, "loud", t0 + 20_000)] {
            c.execute(
                "INSERT INTO as_run VALUES (?1, ?2, ?3, 'rotation', ?4, ?5, '[\"bound_in_ms=0\"]')",
                rusqlite::params![id, item, title, s, s + 10_000],
            )
            .unwrap();
        }
        drop(c);
        let mut ex = Explainer::new(&state, &d.join("data"));
        let all = RowView::default();
        // 4.0 to 6.0 s into the quiet item: its own quiet.
        assert_eq!(ex.unexplained(t0 + 4_100, 1_800, &[], &all), None);
        // The same time into the loud item: the source is audible there.
        let why = ex
            .unexplained(t0 + 20_000 + 4_100, 1_800, &[], &all)
            .expect("dead air");
        assert!(why.contains("loud") && why.contains("audible"), "{why}");
        // Between the two items nothing is on air.
        let why = ex
            .unexplained(t0 + 12_000, 3_000, &[], &all)
            .expect("dead air");
        assert!(why.contains("no item on air"), "{why}");
        // A damaged file on air explains its own silence.
        assert_eq!(
            ex.unexplained(t0 + 20_000 + 4_100, 1_800, &[(l, t0, t0 + 60_000)], &all),
            None
        );
        // The quiet item again as row 3, started in a wall time 10 minutes ahead of the one it ended in (a clock
        // step back while it played): only with the row moved into the new wall time is its own quiet found.
        let c = Connection::open(&state).unwrap();
        let t1 = t0 + 100_000;
        c.execute(
            "INSERT INTO as_run VALUES (3, ?1, 'quiet', 'rotation', ?2, ?3, '[\"bound_in_ms=0\"]')",
            rusqlite::params![q, t1 + 600_000, t1 + 10_000],
        )
        .unwrap();
        drop(c);
        let moved = RowView {
            lo: 2,
            carried: vec![3],
            start_shift: [(3, -600_000)].into_iter().collect(),
            widen_ms: 600_000,
            ..RowView::default()
        };
        assert_eq!(ex.unexplained(t1 + 4_100, 1_800, &[], &moved), None);
        let why = ex
            .unexplained(t1 + 4_100, 1_800, &[], &all)
            .expect("not placed");
        assert!(why.contains("no item on air"), "{why}");
        // A passage at -45 dBFS: silent at the output when the play's gain is -20 dB (a loud master), heard at
        // -33 dBFS when it is +12 (a quiet one), so a silence there is dead air.
        write(
            &d.join("soft.wav"),
            |i| {
                if (192_000..288_000).contains(&i) {
                    0.0056 * (i as f32 * 0.05).sin()
                } else {
                    tone(i)
                }
            },
            10,
        );
        let soft = imp("soft.wav");
        let t2 = t0 + 200_000;
        let c = Connection::open(&state).unwrap();
        for (id, g, start) in [(4, -20.0, t2), (5, 12.0, t2 + 20_000)] {
            c.execute(
                "INSERT INTO as_run VALUES (?1, ?2, 'soft', 'rotation', ?3, ?4, ?5)",
                rusqlite::params![
                    id,
                    soft,
                    start,
                    start + 10_000,
                    format!("[\"bound_in_ms=0\",\"gain_db={g:.1}\"]")
                ],
            )
            .unwrap();
        }
        drop(c);
        assert_eq!(ex.unexplained(t2 + 4_100, 1_800, &[], &all), None);
        let why = ex
            .unexplained(t2 + 20_000 + 4_100, 1_800, &[], &all)
            .expect("heard at -33 dBFS");
        assert!(why.contains("audible"), "{why}");
        // A damaged file that failed on air, the recovery's 110 ms, then the next item: the silence through the
        // gap is the failure handling's, and without the damage it is dead air.
        let t3 = t0 + 300_000;
        let c = Connection::open(&state).unwrap();
        for (id, item, s0, e0) in [(6, l, t3, t3 + 10_000), (7, q, t3 + 10_110, t3 + 20_110)] {
            c.execute(
                "INSERT INTO as_run VALUES (?1, ?2, 't', 'rotation', ?3, ?4, '[\"bound_in_ms=0\"]')",
                rusqlite::params![id, item, s0, e0],
            )
            .unwrap();
        }
        drop(c);
        assert_eq!(
            ex.unexplained(t3 + 5_000, 5_110, &[(l, t3 - 1_000, t3 + 60_000)], &all),
            None
        );
        let why = ex
            .unexplained(t3 + 5_000, 5_110, &[], &all)
            .expect("dead air");
        assert!(why.contains("audible") || why.contains("no item"), "{why}");
        assert_eq!(quiet_threshold_dbfs(None), -40.0);
        assert_eq!(quiet_threshold_dbfs(Some(12.0)), -71.0);
        assert_eq!(quiet_threshold_dbfs(Some(-25.0)), -40.0);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The loop model's filter: a 3rd-order Butterworth high-pass at 790 Hz is -3 dB at the corner, -18.1 dB
    /// an octave below (10 log10(1 + 2^6)), and carries the loop's gain in the passband.
    #[test]
    fn the_loop_model_filters_like_a_butterworth_high_pass() {
        let m = LoopModel::parse("-4.5,790,3", -60.0, 3.0).unwrap();
        let level = |hz: f64| {
            let n = 48_000usize;
            let x: Vec<f32> = (0..n)
                .flat_map(|i| {
                    let v = (std::f64::consts::TAU * hz * i as f64 / 48_000.0).sin() as f32;
                    [v, v]
                })
                .collect();
            let mut l = LoopLevel::new(&m, 0.0);
            let mut sq = 0.0;
            // Run the filter, then measure the steady state through the same sections.
            l.feed(&x, 2, 48_000);
            let mut st = vec![[0.0f64; 4]; l.sections.len()];
            for (k, f) in x.chunks_exact(2).enumerate() {
                let mut y = f64::from(f[0]) * l.scale;
                for (s, z) in l.sections.iter().zip(st.iter_mut()) {
                    let o =
                        s.b[0] * y + s.b[1] * z[0] + s.b[2] * z[1] - s.a[0] * z[2] - s.a[1] * z[3];
                    z[1] = z[0];
                    z[0] = y;
                    z[3] = z[2];
                    z[2] = o;
                    y = o;
                }
                if k >= 24_000 {
                    sq += y * y;
                }
            }
            10.0 * (sq / 24_000.0 * 2.0).log10()
        };
        assert!((level(10_000.0) + 4.5).abs() < 0.2, "{}", level(10_000.0));
        assert!((level(790.0) + 7.5).abs() < 0.3, "{}", level(790.0));
        assert!((level(395.0) + 22.6).abs() < 0.5, "{}", level(395.0));
        assert_eq!(
            LoopModel::parse("-5.3", -60.0, 3.0).unwrap().highpass_order,
            0
        );
        assert_eq!(
            LoopModel::parse("0,100", -60.0, 3.0)
                .unwrap()
                .highpass_order,
            2
        );
        assert!(LoopModel::parse("x", -60.0, 3.0).is_err());
        assert!(LoopModel::parse("0,100,3,9", -60.0, 3.0).is_err());
    }

    /// Box A's case (2026-10-01): a funk track whose rests carry only bass, recorded through a mic input that
    /// loses everything under about 800 Hz. The rests record under -60 dBFS while the source sits at -20 dBFS:
    /// with the loop's measured model they are the music's own quiet, without it they are "dead air". A
    /// dropout in the middle of a long hit is dead air either way.
    #[test]
    fn a_loop_that_loses_the_bass_explains_bass_only_rests_and_still_catches_a_dropout() {
        let d = std::env::temp_dir().join(format!("hotclock-explain-loop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let lib = Library::open(Paths::from_data_dir(&d.join("data"))).unwrap();
        // 100 Hz bass throughout at 0.1; a 2 kHz hit at 0.1 for 2 s, then a 0.5 s rest (bass only), repeated.
        write(
            &d.join("funk.wav"),
            |i| {
                let t = i as f32 / 48_000.0;
                let bass = 0.1 * (std::f32::consts::TAU * 100.0 * t).sin();
                let hit = if (i % 120_000) < 96_000 {
                    0.1 * (std::f32::consts::TAU * 2_000.0 * t).sin()
                } else {
                    0.0
                };
                bass + hit
            },
            20,
        );
        let item = match import_file(
            &lib,
            &d.join("funk.wav"),
            &ImportOptions {
                category: Some("music".into()),
                source: "t".into(),
            },
        ) {
            ImportOutcome::Done { item_id, .. } => item_id,
            o => panic!("{o:?}"),
        };
        let state = d.join("state.db");
        let c = Connection::open(&state).unwrap();
        c.execute_batch(
            "CREATE TABLE as_run (id INTEGER PRIMARY KEY, item_id INTEGER, title TEXT, origin TEXT, started_ms INTEGER, ended_ms INTEGER, flags TEXT);
             CREATE TABLE track_cache (item_id INTEGER PRIMARY KEY, markers TEXT);",
        )
        .unwrap();
        let t0 = 1_000_000i64;
        c.execute(
            "INSERT INTO as_run VALUES (1, ?1, 'funk', 'rotation', ?2, ?3, '[\"bound_in_ms=0\",\"gain_db=1.7\"]')",
            rusqlite::params![item, t0, t0 + 20_000],
        )
        .unwrap();
        drop(c);
        let all = RowView::default();
        let model = LoopModel::parse("-4.5,790,3", -60.0, LOOP_MARGIN_DB).unwrap();
        let mut looped = Explainer::new(&state, &d.join("data")).with_loop(model.clone());
        let mut flat = Explainer::new(&state, &d.join("data")).with_loop(LoopModel::flat(-60.0));
        let mut output = Explainer::new(&state, &d.join("data"));
        // 80 ms in the rest from 4.5 s to 5.0 s.
        let rest = (t0 + 4_650, 80);
        assert_eq!(looped.unexplained(rest.0, rest.1, &[], &all), None);
        for ex in [&mut flat, &mut output] {
            let why = ex.unexplained(rest.0, rest.1, &[], &all).expect("bass");
            assert!(why.contains("audible"), "{why}");
        }
        // 50 ms in the middle of the hit from 5.0 s to 7.0 s: a dropout under every rule.
        for ex in [&mut looped, &mut flat, &mut output] {
            let why = ex.unexplained(t0 + 6_000, 50, &[], &all).expect("dropout");
            assert!(why.contains("audible"), "{why}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
