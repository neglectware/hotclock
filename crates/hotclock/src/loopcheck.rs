//! The analysis behind `hotclock loopcheck` (M1.8): what a recording of the station's output, made through
//! a second sound card, says about gaps and glitches. Pure functions on frames, so every rule is tested
//! on synthetic signals; the capture and the report files are in `loopcheck_cmd`.
//!
//! * **Gaps.** The recording is cut into 10 ms windows. A window whose louder channel is under the
//!   threshold (default -60 dBFS RMS) is quiet; a run of quiet windows of at least `min_gap_ms` (20) is a
//!   *gap*. `abrupt` says the level fell from at least -45 dBFS in one window: a dropout, not a fade to
//!   silence. The resolution is one window.
//! * **Tone continuity** (`--tone`, a steady tone played by the other card): every 50 ms the tone's
//!   phase and amplitude are measured at the tone's frequency (a Goertzel sum). The phase advance between
//!   blocks is compared with the running average of it. A block that deviates by more than 0.08 rad
//!   (a one-sample slip at 1 kHz is 0.131 rad) or 1.5 dB is a *glitch*. The running average is also the
//!   measured tone frequency, so the two cards' clock offset comes out in ppm.
//! * **Capture faults** (overruns, device loss) are counted by the caller and passed in as events.

use std::f64::consts::TAU;

pub const WINDOW_MS: u32 = 10;
pub const TONE_BLOCK_MS: u32 = 50;

#[derive(Debug, Clone)]
pub struct Config {
    pub rate: u32,
    /// A window quieter than this (dBFS RMS, louder channel) is quiet.
    pub threshold_db: f64,
    /// The shortest run of quiet windows reported, ms.
    pub min_gap_ms: u32,
    /// The tone to follow, Hz (`None`: no continuity check).
    pub tone_hz: Option<f64>,
}

impl Config {
    pub fn new(rate: u32) -> Self {
        Self {
            rate,
            threshold_db: -60.0,
            min_gap_ms: 20,
            tone_hz: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Gap {
        /// Frames since the start of the recording.
        start_frame: u64,
        ms: u32,
        level_before_db: f64,
        level_after_db: f64,
        /// The level inside the gap (dBFS RMS over its windows, louder channel): a dropout to digital silence
        /// records at the capture's own noise floor, quiet program somewhat above it.
        level_db: f64,
        abrupt: bool,
    },
    ToneGlitch {
        frame: u64,
        phase_err_rad: f64,
        amp_err_db: f64,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Totals {
    pub frames: u64,
    pub gaps: u64,
    pub gap_ms_total: u64,
    pub longest_gap_ms: u32,
    pub glitches: u64,
    pub peak_db: f64,
    /// Peak of each channel since the last status line.
    pub tone_ppm: Option<f64>,
}

fn db(x: f64) -> f64 {
    if x <= 1e-12 {
        -120.0
    } else {
        10.0 * x.log10()
    }
}

pub struct Analyzer {
    cfg: Config,
    win_frames: usize,
    // Window accumulation.
    win_fill: usize,
    win_sq: [f64; 2],
    win_peak: f32,
    // Gap tracking.
    quiet_run: u32,
    quiet_start: u64,
    prev_level: f64,
    before_level: f64,
    /// Sum of the quiet windows' mean squares (louder channel), for the gap's own level.
    quiet_sq: f64,
    // Tone tracking.
    tone_block: usize,
    tone_fill: usize,
    tone_re: f64,
    tone_im: f64,
    tone_phase_pos: f64,
    last_phase: Option<f64>,
    last_amp: Option<f64>,
    adv_ema: Option<f64>,
    pub totals: Totals,
    pub events: Vec<Event>,
    /// Frames counted, for event positions.
    frame: u64,
    /// Level of the last full window (dBFS), for status lines.
    pub last_level_db: f64,
    /// Peak (absolute) since the last `take_peak`.
    peak: f32,
}

impl Analyzer {
    pub fn new(cfg: Config) -> Self {
        let win_frames = (cfg.rate as usize * WINDOW_MS as usize) / 1000;
        let tone_block = (cfg.rate as usize * TONE_BLOCK_MS as usize) / 1000;
        Self {
            cfg,
            win_frames,
            win_fill: 0,
            win_sq: [0.0; 2],
            win_peak: 0.0,
            quiet_run: 0,
            quiet_start: 0,
            prev_level: -120.0,
            before_level: -120.0,
            quiet_sq: 0.0,
            tone_block,
            tone_fill: 0,
            tone_re: 0.0,
            tone_im: 0.0,
            tone_phase_pos: 0.0,
            last_phase: None,
            last_amp: None,
            adv_ema: None,
            totals: Totals {
                peak_db: -120.0,
                ..Totals::default()
            },
            events: vec![],
            frame: 0,
            last_level_db: -120.0,
            peak: 0.0,
        }
    }

    /// Feed interleaved stereo frames.
    pub fn feed(&mut self, interleaved: &[f32]) {
        for f in interleaved.chunks_exact(2) {
            let (l, r) = (f[0], f[1]);
            self.win_sq[0] += f64::from(l) * f64::from(l);
            self.win_sq[1] += f64::from(r) * f64::from(r);
            self.win_peak = self.win_peak.max(l.abs()).max(r.abs());
            self.win_fill += 1;
            if self.win_fill == self.win_frames {
                self.close_window();
            }
            if let Some(hz) = self.cfg.tone_hz {
                let x = f64::from(l);
                let (s, c) = self.tone_phase_pos.sin_cos();
                self.tone_re += x * c;
                self.tone_im += x * s;
                self.tone_phase_pos =
                    (self.tone_phase_pos + TAU * hz / f64::from(self.cfg.rate)) % TAU;
                self.tone_fill += 1;
                if self.tone_fill == self.tone_block {
                    self.close_tone_block();
                }
            }
            self.frame += 1;
        }
        self.totals.frames = self.frame;
    }

    fn close_window(&mut self) {
        let n = self.win_fill as f64;
        let ms_sq = self.win_sq[0].max(self.win_sq[1]) / n;
        let level = db(ms_sq);
        self.last_level_db = level;
        self.peak = self.peak.max(self.win_peak);
        let pk = db(f64::from(self.win_peak).powi(2));
        if pk > self.totals.peak_db {
            self.totals.peak_db = pk;
        }
        let start = self.frame + 1 - self.win_fill as u64;
        let quiet = level < self.cfg.threshold_db;
        if quiet {
            if self.quiet_run == 0 {
                self.quiet_start = start;
                self.before_level = self.prev_level;
                self.quiet_sq = 0.0;
            }
            self.quiet_run += 1;
            self.quiet_sq += ms_sq;
        } else if self.quiet_run > 0 {
            self.end_gap(level);
        }
        self.prev_level = level;
        self.win_fill = 0;
        self.win_sq = [0.0; 2];
        self.win_peak = 0.0;
    }

    fn end_gap(&mut self, level_after: f64) {
        let ms = self.quiet_run * WINDOW_MS;
        if ms >= self.cfg.min_gap_ms {
            self.totals.gaps += 1;
            self.totals.gap_ms_total += u64::from(ms);
            self.totals.longest_gap_ms = self.totals.longest_gap_ms.max(ms);
            self.events.push(Event::Gap {
                start_frame: self.quiet_start,
                ms,
                level_before_db: self.before_level,
                level_after_db: level_after,
                level_db: db(self.quiet_sq / f64::from(self.quiet_run)),
                abrupt: self.before_level >= -45.0,
            });
        }
        self.quiet_run = 0;
    }

    /// Close a gap that is still open at the end of the recording.
    pub fn finish(&mut self) {
        if self.quiet_run > 0 {
            let l = self.last_level_db;
            self.end_gap(l);
        }
    }

    fn close_tone_block(&mut self) {
        let n = self.tone_fill as f64;
        let amp = 2.0 * (self.tone_re.powi(2) + self.tone_im.powi(2)).sqrt() / n;
        let phase = self.tone_im.atan2(self.tone_re);
        self.tone_re = 0.0;
        self.tone_im = 0.0;
        self.tone_fill = 0;
        // Too quiet to have a phase (the tone is not there): the gap rule reports it.
        if amp < 1e-4 {
            self.last_phase = None;
            self.last_amp = None;
            return;
        }
        let hz = self.cfg.tone_hz.unwrap_or(1000.0);
        if let (Some(p0), Some(a0)) = (self.last_phase, self.last_amp) {
            // `phase` is pi/2 minus the tone's phase, so a tone running fast makes it fall.
            let mut adv = p0 - phase;
            while adv > std::f64::consts::PI {
                adv -= TAU;
            }
            while adv < -std::f64::consts::PI {
                adv += TAU;
            }
            let ema = *self.adv_ema.get_or_insert(adv);
            let err = adv - ema;
            let amp_err = 20.0 * (amp / a0).log10();
            if err.abs() > 0.08 || amp_err.abs() > 1.5 {
                self.totals.glitches += 1;
                self.events.push(Event::ToneGlitch {
                    frame: self.frame + 1 - self.tone_block as u64,
                    phase_err_rad: err,
                    amp_err_db: amp_err,
                });
                // The glitch is not the new normal: the average stays.
            } else {
                self.adv_ema = Some(ema + 0.02 * err);
            }
            // Measured frequency from the average advance (per block, radians): the block's nominal
            // advance is zero because the reference oscillator runs at the nominal frequency.
            let block_s = self.tone_block as f64 / f64::from(self.cfg.rate);
            let df = self.adv_ema.unwrap_or(0.0) / TAU / block_s;
            self.totals.tone_ppm = Some(df / hz * 1e6);
        }
        self.last_phase = Some(phase);
        self.last_amp = Some(amp);
    }

    /// The recording was interrupted (a card came back after being away): a gap must not be reported across
    /// the join, and the tone's phase starts over.
    pub fn reset_continuity(&mut self) {
        self.win_fill = 0;
        self.win_sq = [0.0; 2];
        self.win_peak = 0.0;
        self.quiet_run = 0;
        self.quiet_sq = 0.0;
        self.prev_level = -120.0;
        self.tone_fill = 0;
        self.tone_re = 0.0;
        self.tone_im = 0.0;
        self.last_phase = None;
        self.last_amp = None;
    }

    /// Take the events found since the last call.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// The peak (linear) since the last call.
    pub fn take_peak(&mut self) -> f32 {
        std::mem::take(&mut self.peak)
    }
}

/// Analyze a whole recording (interleaved stereo).
#[cfg(test)]
pub fn analyze(cfg: Config, interleaved: &[f32]) -> (Totals, Vec<Event>) {
    let mut a = Analyzer::new(cfg);
    for chunk in interleaved.chunks(2048) {
        a.feed(chunk);
    }
    a.finish();
    let ev = a.take_events();
    (a.totals, ev)
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: u32 = 48_000;

    fn tone(seconds: f64, hz: f64, db_rms: f64) -> Vec<f32> {
        let n = (seconds * f64::from(R)) as usize;
        let amp = 10f64.powf(db_rms / 20.0) * std::f64::consts::SQRT_2;
        let mut v = Vec::with_capacity(n * 2);
        for i in 0..n {
            let x = (amp * (TAU * hz * i as f64 / f64::from(R)).sin()) as f32;
            v.push(x);
            v.push(x);
        }
        v
    }

    fn zero_frames(v: &mut [f32], from_frame: usize, frames: usize) {
        for x in &mut v[from_frame * 2..(from_frame + frames) * 2] {
            *x = 0.0;
        }
    }

    #[test]
    fn a_clean_tone_has_no_gap_and_no_glitch() {
        let mut c = Config::new(R);
        c.tone_hz = Some(1000.0);
        let (t, ev) = analyze(c, &tone(10.0, 1000.0, -20.0));
        assert!(ev.is_empty(), "{ev:?}");
        assert_eq!(t.gaps, 0);
        assert_eq!(t.glitches, 0);
        assert!(t.peak_db > -18.0 && t.peak_db < -16.0, "{}", t.peak_db);
    }

    #[test]
    fn a_60_ms_hole_is_one_abrupt_gap_of_about_that_length() {
        let mut v = tone(3.0, 440.0, -20.0);
        zero_frames(&mut v, 48_000, 2_880); // 60 ms starting at 1.000 s
        let (t, ev) = analyze(Config::new(R), &v);
        assert_eq!(t.gaps, 1, "{ev:?}");
        match &ev[0] {
            Event::Gap {
                start_frame,
                ms,
                abrupt,
                level_before_db,
                ..
            } => {
                assert!((60..=80).contains(ms), "{ms}");
                assert!(start_frame.abs_diff(48_000) <= 480, "{start_frame}");
                assert!(*abrupt, "{level_before_db}");
            }
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn a_gap_reports_the_level_inside_it() {
        // A -20 dBFS tone dropped to -70 for 100 ms: the gap's own level is -70, not digital silence.
        let mut v = tone(3.0, 440.0, -20.0);
        let k = 10f32.powf(-50.0 / 20.0);
        for x in &mut v[2 * 48_000..2 * 52_800] {
            *x *= k;
        }
        let (_, ev) = analyze(Config::new(R), &v);
        match &ev[..] {
            [Event::Gap { level_db, .. }] => assert!((level_db + 70.0).abs() < 1.0, "{level_db}"),
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn a_hole_under_the_minimum_is_not_reported() {
        let mut v = tone(2.0, 440.0, -20.0);
        zero_frames(&mut v, 48_000, 240); // 5 ms
        let (t, ev) = analyze(Config::new(R), &v);
        assert_eq!(t.gaps, 0, "{ev:?}");
    }

    #[test]
    fn a_fade_to_silence_is_a_gap_but_not_abrupt() {
        // -20 dBFS, then a slow fade over 1 s to nothing, then 200 ms of silence, then the tone again.
        let mut v = tone(2.0, 440.0, -20.0);
        for i in 0..48_000usize {
            let g = 1.0 - i as f32 / 48_000.0;
            v[(48_000 + i) * 2] *= g;
            v[(48_000 + i) * 2 + 1] *= g;
        }
        let mut w = v.clone();
        w.extend(std::iter::repeat_n(0.0f32, 2 * 9_600));
        w.extend(tone(1.0, 440.0, -20.0));
        let (t, ev) = analyze(Config::new(R), &w);
        assert_eq!(t.gaps, 1, "{ev:?}");
        match &ev[0] {
            Event::Gap { abrupt, ms, .. } => {
                assert!(!abrupt, "{ev:?}");
                assert!(*ms >= 200, "{ms}");
            }
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn silence_at_the_end_is_closed_by_finish() {
        let mut v = tone(1.0, 440.0, -20.0);
        v.extend(std::iter::repeat_n(0.0f32, 2 * 48_000));
        let (t, _) = analyze(Config::new(R), &v);
        assert_eq!(t.gaps, 1);
        assert!(t.longest_gap_ms >= 990);
    }

    #[test]
    fn a_one_sample_slip_in_a_tone_is_a_glitch() {
        let mut v = tone(4.0, 1000.0, -20.0);
        // Remove one frame at 2 s: everything after moves one sample early.
        v.drain(2 * 96_000..2 * 96_000 + 2);
        let mut c = Config::new(R);
        c.tone_hz = Some(1000.0);
        let (t, ev) = analyze(c, &v);
        assert_eq!(t.glitches, 1, "{ev:?}");
        match ev.iter().find(|e| matches!(e, Event::ToneGlitch { .. })) {
            Some(Event::ToneGlitch { frame, .. }) => {
                assert!(frame.abs_diff(96_000) <= 2_400 + 100, "{frame}")
            }
            _ => panic!("{ev:?}"),
        }
    }

    #[test]
    fn a_gain_step_in_a_tone_is_a_glitch() {
        let mut v = tone(3.0, 1000.0, -20.0);
        for x in &mut v[2 * 72_000..] {
            *x *= 0.5;
        }
        let mut c = Config::new(R);
        c.tone_hz = Some(1000.0);
        let (t, ev) = analyze(c, &v);
        assert_eq!(t.glitches, 1, "{ev:?}");
    }

    #[test]
    fn a_clock_offset_between_the_cards_is_measured_in_ppm() {
        // The far card's clock runs 100 ppm fast: its tone arrives at 1000.1 Hz.
        let mut c = Config::new(R);
        c.tone_hz = Some(1000.0);
        let (t, ev) = analyze(c, &tone(20.0, 1000.1, -20.0));
        assert!(ev.is_empty(), "{ev:?}");
        let ppm = t.tone_ppm.unwrap();
        assert!((ppm - 100.0).abs() < 8.0, "{ppm}");
    }

    #[test]
    fn a_one_channel_recording_counts_by_the_louder_channel() {
        // Left dead, right playing: no gap.
        let mut v = tone(1.0, 440.0, -20.0);
        for f in v.chunks_exact_mut(2) {
            f[0] = 0.0;
        }
        let (t, _) = analyze(Config::new(R), &v);
        assert_eq!(t.gaps, 0);
    }
}
