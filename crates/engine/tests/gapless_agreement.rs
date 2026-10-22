//! The engine's decoder and the analysis decoder use one front end (`gapless`) and one trimming
//! rule, so a marker in milliseconds means the same sample in both (the M1.3 review's first
//! item: AAC in MP4 was 43 ms off on air). Every test prints its measured numbers.
//!
//! * a synthetic AAC-in-MP4 with an `iTunSMPB` tag whose priming was measured against the known
//!   source, and an MP3 with a LAME header: the two decoders agree to 0 frames on the first
//!   audible sample and on the length, and the decoded copy lines up with the source;
//! * a real AAC file of the elevator set (skipped when `HOTCLOCK_TESTDATA` or
//!   `~/Developer/hotclock-testdata` is missing).

#![allow(clippy::needless_range_loop)]

use engine::decode::FileDecoder;
use std::path::{Path, PathBuf};
use symphonia::core::audio::SampleBuffer;

const RATE: u32 = 48_000;

fn engine_decode(path: &Path, rate: u32) -> (Vec<f32>, Option<u64>) {
    let mut d = FileDecoder::open(path, rate, 0.0).expect("open");
    let expected = d.expected_frames();
    let mut out = Vec::new();
    while d.decode_next(&mut out).expect("decode") {}
    (out.chunks_exact(2).map(|f| f[0]).collect(), expected)
}

/// The analysis decoder's first channel, at the file's own rate.
fn analysis_decode(path: &Path) -> (Vec<f32>, u32) {
    let mut left = Vec::new();
    let info = analysis::decode::decode_file(path, |s, ch, _| {
        left.extend(s.chunks_exact(ch).map(|f| f[0]));
    })
    .expect("analysis decode");
    (left, info.sample_rate)
}

/// The decoded stream with **no trim applied at all** (what playout used to give for AAC).
fn raw_decode(path: &Path) -> Vec<f32> {
    let mut o = gapless::open(path).expect("open");
    let mut out = Vec::new();
    while let Ok(p) = o.format.next_packet() {
        if p.track_id() != o.track_id {
            continue;
        }
        let Ok(buf) = o.decoder.decode(&p) else {
            continue;
        };
        let spec = *buf.spec();
        let mut sb = SampleBuffer::<f32>::new(buf.capacity() as u64, spec);
        sb.copy_interleaved_ref(buf);
        let ch = spec.channels.count();
        out.extend(sb.samples().chunks_exact(ch).map(|f| f[0]));
    }
    out
}

/// The lag `k` (decoded index = source index + k) that maximises the correlation of the first
/// 20,000 source samples with the decoded stream, searched over +-4,000.
fn lag(src: &[f32], dec: &[f32]) -> i64 {
    let n = 20_000.min(src.len());
    let mut best = (f64::NEG_INFINITY, 0i64);
    for k in -4000i64..=4000 {
        let mut acc = 0.0f64;
        for i in 0..n {
            let j = i as i64 + k;
            if j >= 0 && (j as usize) < dec.len() {
                acc += f64::from(src[i]) * f64::from(dec[j as usize]);
            }
        }
        if acc > best.0 {
            best = (acc, k);
        }
    }
    best.1
}

fn first_audible(x: &[f32], thr: f32) -> Option<usize> {
    x.iter().position(|v| v.abs() > thr)
}

fn source_f32() -> Vec<f32> {
    fixtures::gapless_source()
        .iter()
        .map(|&v| v as f32)
        .collect()
}

fn fixture(name: &str) -> Option<PathBuf> {
    let (dir, rep) = fixtures::shared_dir(&[name]);
    if !rep.skipped.is_empty() {
        eprintln!("SKIPPED: {:?}", rep.skipped);
        return None;
    }
    Some(dir.join(name))
}

#[test]
fn aac_in_mp4_priming_is_trimmed_identically_by_playout_and_analysis() {
    let Some(orig) = fixture("gapless_aac.m4a") else {
        return;
    };
    let src = source_f32();
    let n = src.len() as u64;
    // A working copy carrying the iTunSMPB tag. The priming is measured, not assumed: the
    // untrimmed decode of the file, correlated with the known source.
    let path = orig.with_file_name(format!("gapless_aac_tagged_{}.m4a", std::process::id()));
    std::fs::copy(&orig, &path).unwrap();
    let raw = raw_decode(&path);
    let priming = lag(&src, &raw);
    assert!(
        priming > 0,
        "an AAC decode is late by its priming: {priming}"
    );
    fixtures::set_itunsmpb(&path, priming as u32, n).unwrap();
    assert_eq!(gapless::itunsmpb(&path), Some((priming as u64, n)));

    let (eng, expected) = engine_decode(&path, RATE);
    let (ana, ana_rate) = analysis_decode(&path);
    assert_eq!(ana_rate, RATE);
    let lag_eng = lag(&src, &eng);
    let lag_ana = lag(&src, &ana);
    let fa_eng = first_audible(&eng, 0.001).unwrap();
    let fa_ana = first_audible(&ana, 0.001).unwrap();
    let fa_raw = first_audible(&raw, 0.001).unwrap();
    let maxdiff = eng
        .iter()
        .zip(&ana)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    println!(
        "AAC-in-MP4, iTunSMPB priming {priming} (measured against the source), valid {n}: \
         untrimmed decode first audible sample {fa_raw}, lag {}; engine first audible {fa_eng}, \
         lag {lag_eng}, {} frames (expected_frames {expected:?}); analysis first audible {fa_ana}, \
         lag {lag_ana}, {} frames; first-audible difference engine - analysis {} frames (limit 0); \
         largest sample difference {maxdiff} (limit 0)",
        lag(&src, &raw),
        eng.len(),
        ana.len(),
        fa_eng as i64 - fa_ana as i64
    );
    assert_eq!(fa_eng, fa_ana, "first audible sample");
    assert_eq!(eng.len(), ana.len(), "length");
    assert_eq!(eng.len() as u64, n, "valid frames");
    assert_eq!(expected, Some(n), "the declared length is the valid length");
    assert_eq!(maxdiff, 0.0, "the same samples");
    assert!(
        lag_eng.abs() <= 1 && lag_ana.abs() <= 1,
        "aligned with the source"
    );
    // Playout used to be late by the priming: the untrimmed decode lags the source by it.
    assert_eq!(
        lag(&src, &raw),
        priming,
        "the untrimmed decode is late by the priming"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn aac_seek_start_lands_on_the_same_sample() {
    let Some(orig) = fixture("gapless_aac.m4a") else {
        return;
    };
    let src = source_f32();
    let n = src.len() as u64;
    let path = orig.with_file_name(format!("gapless_aac_seek_{}.m4a", std::process::id()));
    std::fs::copy(&orig, &path).unwrap();
    let priming = lag(&src, &raw_decode(&path));
    fixtures::set_itunsmpb(&path, priming as u32, n).unwrap();
    let (full, _) = engine_decode(&path, RATE);
    for start_s in [0.25f64, 1.0, 2.5] {
        let mut d = FileDecoder::open(&path, RATE, start_s).unwrap();
        let mut out = Vec::new();
        while d.decode_next(&mut out).unwrap() {}
        let part: Vec<f32> = out.chunks_exact(2).map(|f| f[0]).collect();
        let from = (start_s * f64::from(RATE)) as usize;
        let want = &full[from..];
        let len_ok = part.len() == want.len();
        // The encoder's noise substitution (PNS) draws from a random generator whose state depends
        // on everything decoded before, so a decode that starts mid-file differs from the full
        // decode in noise-like bands and only alignment can be compared: the lag of the seeked
        // decode against the full one, over 12,000 samples after the run-up.
        let mut best = (f64::MIN, 0i64);
        for k in -20i64..=20 {
            let mut acc = 0.0f64;
            for i in 8192..20_000 {
                let j = i as i64 + k;
                if j >= 0 && (j as usize) < want.len() {
                    acc += f64::from(part[i]) * f64::from(want[j as usize]);
                }
            }
            if acc > best.0 {
                best = (acc, k);
            }
        }
        println!(
            "seek to {start_s} s: {} frames (want {}), lag against the full decode {} frames (limit 0)",
            part.len(),
            want.len(),
            best.1
        );
        assert!(len_ok, "length after a seek to {start_s}");
        assert_eq!(best.1, 0, "alignment after a seek to {start_s}");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn mp3_with_a_lame_header_agrees_and_lines_up() {
    let Some(path) = fixture("gapless_mp3.mp3") else {
        return;
    };
    let src = source_f32();
    let n = src.len();
    let (eng, expected) = engine_decode(&path, RATE);
    let (ana, _) = analysis_decode(&path);
    let raw = raw_decode(&path);
    let lag_eng = lag(&src, &eng);
    let fa_eng = first_audible(&eng, 0.001).unwrap();
    let fa_ana = first_audible(&ana, 0.001).unwrap();
    let maxdiff = eng
        .iter()
        .zip(&ana)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    println!(
        "MP3 with LAME header: engine {} frames (expected_frames {expected:?}), analysis {} frames, \
         source {n}; first audible engine {fa_eng} analysis {fa_ana} (difference {}, limit 0); \
         lag to the source {lag_eng} (limit 1); largest sample difference {maxdiff} (limit 0); \
         symphonia's own trim already applied (raw {} frames)",
        eng.len(),
        ana.len(),
        fa_eng as i64 - fa_ana as i64,
        raw.len()
    );
    assert_eq!(fa_eng, fa_ana);
    assert_eq!(eng.len(), ana.len());
    assert_eq!(maxdiff, 0.0);
    assert!(lag_eng.abs() <= 1, "lag {lag_eng}");
    assert!(
        (eng.len() as i64 - n as i64).abs() <= 1,
        "length {} against the source {n}",
        eng.len()
    );
}

fn testdata() -> Option<PathBuf> {
    let d = std::env::var("HOTCLOCK_TESTDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join("Developer/hotclock-testdata")
        });
    d.is_dir().then_some(d)
}

fn find_aac(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if p.is_dir() {
            find_aac(&p, out);
        } else if p
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("m4a") || x.eq_ignore_ascii_case("aac"))
        {
            out.push(p);
        }
    }
}

#[test]
fn a_real_aac_file_agrees_between_playout_and_analysis() {
    let Some(root) = testdata() else {
        eprintln!("SKIPPED: no test data directory");
        return;
    };
    let mut files = vec![];
    find_aac(&root.join("elevator"), &mut files);
    let Some(path) = files
        .iter()
        .find(|p| gapless::itunsmpb(p).is_some())
        .cloned()
    else {
        eprintln!(
            "SKIPPED: no AAC file with iTunSMPB in the elevator set ({} files found, first {:?})",
            files.len(),
            files.first()
        );
        return;
    };
    let (ana, rate) = analysis_decode(&path);
    let (eng, expected) = engine_decode(&path, rate);
    let raw = raw_decode(&path);
    let (priming, valid) = gapless::itunsmpb(&path).unwrap();
    let fa_eng = first_audible(&eng, 1e-4);
    let fa_ana = first_audible(&ana, 1e-4);
    let fa_raw = first_audible(&raw, 1e-4);
    let maxdiff = eng
        .iter()
        .zip(&ana)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    println!(
        "real file {}: {rate} Hz, iTunSMPB priming {priming} valid {valid}; engine {} frames \
         (expected_frames {expected:?}), analysis {} frames, untrimmed {} frames; first audible \
         sample engine {fa_eng:?} analysis {fa_ana:?} untrimmed {fa_raw:?} (engine - analysis: {:?}, limit 0); \
         largest sample difference {maxdiff} (limit 0)",
        path.display(),
        eng.len(),
        ana.len(),
        raw.len(),
        fa_eng.zip(fa_ana).map(|(a, b)| a as i64 - b as i64)
    );
    assert_eq!(fa_eng, fa_ana);
    assert_eq!(eng.len(), ana.len());
    assert_eq!(eng.len() as u64, valid);
    assert_eq!(expected, Some(valid));
    assert_eq!(maxdiff, 0.0);
    assert!(raw.len() as u64 - eng.len() as u64 >= priming);
}

/// Seeking into an MP3 whose LAME header `symphonia` trims (review M1.4): `sam` and `fm` open
/// every item at its cue-in, so the seek must land on the same sample as the full decode, like
/// the AAC case above. MP3 has no noise substitution, so after the run-up the content is compared
/// as well as the alignment.
#[test]
fn mp3_seek_start_lands_on_the_same_sample() {
    let Some(path) = fixture("gapless_mp3.mp3") else {
        return;
    };
    let (full, _) = engine_decode(&path, RATE);
    for start_s in [0.25f64, 1.0, 2.5] {
        let mut d = FileDecoder::open(&path, RATE, start_s).unwrap();
        let mut out = Vec::new();
        while d.decode_next(&mut out).unwrap() {}
        let part: Vec<f32> = out.chunks_exact(2).map(|f| f[0]).collect();
        let from = (start_s * f64::from(RATE)).round() as usize;
        let want = &full[from..];
        let mut best = (f64::MIN, 0i64);
        for k in -3000i64..=3000 {
            let mut acc = 0.0f64;
            for i in 8192..20_000 {
                let j = i as i64 + k;
                if j >= 0 && (j as usize) < want.len() {
                    acc += f64::from(part[i]) * f64::from(want[j as usize]);
                }
            }
            if acc > best.0 {
                best = (acc, k);
            }
        }
        let maxdiff = part[8192..20_000]
            .iter()
            .zip(&want[8192..20_000])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        println!(
            "MP3 seek to {start_s} s: {} frames (want {}), lag against the full decode {} frames \
             (limit 0), largest difference after the run-up {maxdiff}",
            part.len(),
            want.len(),
            best.1
        );
        assert_eq!(best.1, 0, "alignment after a seek to {start_s}");
        assert_eq!(part.len(), want.len(), "length after a seek to {start_s}");
    }
}
