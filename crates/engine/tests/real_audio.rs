//! Real audio through the decoder, the resampler and the mixer, only when `HOTCLOCK_TESTDATA`
//! points at a directory holding `office/`, `preshow/` and `elevator/` (kept outside the
//! repository, never committed; skipped with a message when unset). The three sets are three
//! moods and are never mixed: each is played on its own.
//!
//! `HOTCLOCK_TESTDATA_FULL=1` additionally decodes every file of every set (a long run).

mod support;

use analysis::census::{collect_files, inspect};
use engine::decode::FileDecoder;
use std::path::PathBuf;
use support::render_files;

fn sets() -> Option<Vec<(&'static str, PathBuf)>> {
    let Ok(root) = std::env::var("HOTCLOCK_TESTDATA") else {
        eprintln!("SKIPPED: HOTCLOCK_TESTDATA is not set");
        return None;
    };
    let mut v = vec![];
    for set in ["office", "preshow", "elevator"] {
        let d = PathBuf::from(&root).join(set);
        if d.is_dir() {
            v.push((set, d));
        } else {
            eprintln!("SKIPPED {set}: {} does not exist", d.display());
        }
    }
    Some(v)
}

/// The frames of a file that are audio: what the census decoded, less the encoder priming and padding
/// an MP4's `iTunSMPB` tag declares (the decoder trims them since M1.4; `min(decoded - priming,
/// valid)`, read from the file itself, never a constant). Other files are as decoded.
fn audible_frames(file: &std::path::Path, decoded: u64) -> u64 {
    gapless::Trim::for_file(file)
        .declared(Some(decoded))
        .unwrap_or(decoded)
}

/// One real file from each set, decoded and resampled to 48 kHz through the whole path (pool,
/// mixer, limiter): no decode errors, and the duration within one frame of the file's declared
/// duration at 48 kHz.
#[test]
fn one_real_file_from_each_set_decodes_and_resamples() {
    let Some(sets) = sets() else { return };
    for (set, dir) in sets {
        let files = collect_files(&dir).unwrap();
        assert!(!files.is_empty(), "{set}: no files");
        // The first file in name order: deterministic.
        let file = &files[0];
        let census = inspect(file);
        let rate = census.sample_rate.unwrap();
        let source = audible_frames(file, census.decoded_frames);
        let want = (source as f64 * 48_000.0 / f64::from(rate)).round() as i64;
        let (out, rep) = render_files(std::slice::from_ref(file));
        let got = (out.len() / 2) as i64;
        let f = &rep.files[0];
        println!(
            "{set}: {} ({} Hz, {} ch): {got} frames at 48 kHz; expected {want} from {} source frames; \
             difference {} frames (limit 1); glitches {}; error {:?}; ended {:?}; starved {}",
            file.file_name().unwrap().to_string_lossy(),
            rate,
            census.channels.unwrap_or(0),
            source,
            got - want,
            f.glitches,
            f.error,
            f.end_reason,
            rep.starved_frames
        );
        assert_eq!(f.glitches, 0, "{set}");
        assert!(f.error.is_none(), "{set}: {:?}", f.error);
        assert_eq!(f.end_reason, Some(engine::mixer::EndReason::Eof), "{set}");
        assert!((got - want).abs() <= 1, "{set}: {got} vs {want}");
        // It is real audio: not silence, and within the ceiling.
        let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.01 && peak <= 0.8913, "{set}: peak {peak}");
    }
}

/// Every file of every set, decoded straight to 48 kHz: no errors, no glitches, exact lengths.
#[test]
fn every_real_file_decodes_when_asked() {
    if std::env::var("HOTCLOCK_TESTDATA_FULL").is_err() {
        eprintln!("SKIPPED: set HOTCLOCK_TESTDATA_FULL=1 to decode every file");
        return;
    }
    let Some(sets) = sets() else { return };
    for (set, dir) in sets {
        let files = collect_files(&dir).unwrap();
        let t0 = std::time::Instant::now();
        let mut frames = 0u64;
        let mut worst = 0i64;
        let mut bad = vec![];
        for file in &files {
            let census = inspect(file);
            let rate = census.sample_rate.unwrap();
            let source = audible_frames(file, census.decoded_frames);
            let want = (source as f64 * 48_000.0 / f64::from(rate)).round() as i64;
            let mut d = match FileDecoder::open(file, 48_000, 0.0) {
                Ok(d) => d,
                Err(e) => {
                    bad.push(format!("{}: {e}", file.display()));
                    continue;
                }
            };
            let mut buf: Vec<f32> = Vec::new();
            let mut err = None;
            loop {
                match d.decode_next(&mut buf) {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(e) => {
                        err = Some(e.to_string());
                        break;
                    }
                }
            }
            let got = (buf.len() / 2) as i64;
            frames += got as u64;
            worst = worst.max((got - want).abs());
            if err.is_some() || d.glitches() > 0 || (got - want).abs() > 1 {
                bad.push(format!(
                    "{}: got {got} want {want}, glitches {}, error {err:?}",
                    file.display(),
                    d.glitches()
                ));
            }
        }
        println!(
            "{set}: {} files, {frames} frames ({:.1} min of audio) at 48 kHz in {:.1} s; worst length difference {worst} frames (limit 1); {} problems",
            files.len(),
            frames as f64 / 48_000.0 / 60.0,
            t0.elapsed().as_secs_f64(),
            bad.len()
        );
        for b in &bad {
            println!("  {b}");
        }
        assert!(bad.is_empty(), "{set}");
    }
}
