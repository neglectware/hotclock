//! M1.8: the accelerated soak and `loopcheck`, as the commands a user runs.

use engine::render::{render_tone, ToneRender};
use engine::wav::{WavFormat, WavWriter};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn dir(name: &str) -> PathBuf {
    let d = fixtures::testdir::process_dir().join(format!("s18-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tone(p: &Path, hz: f64, secs: f64) {
    render_tone(&ToneRender {
        out: p.to_path_buf(),
        rate: 48_000,
        seconds: secs,
        hz,
        level_dbfs: -20.0,
        format: WavFormat::F32,
    })
    .unwrap();
}

/// A short accelerated soak on a set of generated tones, with far more actions and faults than a real day
/// holds: no panic, no step error, no dead air, and every kind of action and fault was exercised. (The 30-day
/// run on the real office set is in the BUILDLOG.)
#[test]
fn a_short_accelerated_soak_survives_random_actions_and_faults() {
    let d = dir("soak");
    let set = d.join("set");
    std::fs::create_dir_all(&set).unwrap();
    for i in 0..6 {
        tone(
            &set.join(format!("t{i}.wav")),
            300.0 + 90.0 * f64::from(i),
            45.0,
        );
    }
    let out = d.join("out");
    let r = Command::new(env!("CARGO_BIN_EXE_hotclock"))
        .args([
            "soak",
            "--days",
            "0.08",
            "--ops-per-day",
            "600",
            "--faults-per-day",
            "150",
        ])
        .arg("--set")
        .arg(&set)
        .arg("--out")
        .arg(&out)
        .arg("--work-dir")
        .arg(d.join("work"))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&r.stdout).to_string();
    let json: Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("summary.json")).unwrap())
            .unwrap_or_else(|e| panic!("{e}\n{text}"));
    println!("{}", serde_json::to_string_pretty(&json).unwrap());
    assert!(json["failure"].is_null(), "{}", json["failure"]);
    assert_eq!(
        json["dead_air"]["unexplained"], 0,
        "{}",
        json["dead_air"]["findings"]
    );
    assert_eq!(
        json["as_run"]["gaps_over_50ms_unexplained"], 0,
        "{}",
        json["as_run"]["findings"]
    );
    let acts = json["actions"].as_object().unwrap();
    for k in ["fault_restart", "fault_device_lost", "fault_xrun"] {
        assert!(
            acts.get(k).is_some_and(|v| v.as_u64().unwrap() > 0),
            "no {k} in {acts:?}"
        );
    }
    assert!(
        acts.keys().filter(|k| k.starts_with("op_")).count() >= 4,
        "{acts:?}"
    );
    assert!(json["as_run"]["rows_in_state_db"].as_u64().unwrap() > 20);
    assert!(r.status.success(), "{text}");
    let _ = std::fs::remove_dir_all(&d);
}

/// `loopcheck --wav`: a clean recording passes (exit 0); a recording with a 120 ms hole in the program fails
/// (exit 1), and the report names the gap with its time and saves a clip.
#[test]
fn loopcheck_reports_a_hole_in_a_recording_and_passes_a_clean_one() {
    let d = dir("loop");
    let clean = d.join("clean.wav");
    tone(&clean, 440.0, 4.0);
    let mut w = engine::wav::read_wav(&clean).unwrap();
    let holed = d.join("holed.wav");
    for x in &mut w.samples[2 * 96_000..2 * (96_000 + 5_760)] {
        *x = 0.0; // 120 ms at 2.000 s
    }
    let mut wr = WavWriter::create(&holed, 48_000, 2, WavFormat::F32).unwrap();
    let bytes: Vec<u8> = w.samples.iter().flat_map(|x| x.to_le_bytes()).collect();
    wr.write_bytes(&bytes).unwrap();
    wr.finish().unwrap();
    let run = |wav: &Path, name: &str| {
        let rep = d.join(format!("{name}.jsonl"));
        let r = Command::new(env!("CARGO_BIN_EXE_hotclock"))
            .args(["loopcheck", "--wav"])
            .arg(wav)
            .arg("--report")
            .arg(&rep)
            .arg("--clips-dir")
            .arg(d.join(format!("clips-{name}")))
            .stderr(Stdio::null())
            .status()
            .unwrap();
        (r, std::fs::read_to_string(&rep).unwrap())
    };
    let (ok, text) = run(&clean, "clean");
    assert!(ok.success(), "{text}");
    assert!(text.contains("\"gaps\":0"), "{text}");
    let (bad, text) = run(&holed, "holed");
    assert_eq!(bad.code(), Some(1), "{text}");
    let gap: Value = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|j| j["type"] == "gap")
        .unwrap_or_else(|| panic!("no gap line in {text}"));
    let ms = gap["ms"].as_u64().unwrap();
    assert!((120..=140).contains(&ms), "{ms}");
    assert!((gap["t_s"].as_f64().unwrap() - 2.0).abs() < 0.02, "{gap}");
    assert_eq!(gap["abrupt"], true);
    let clip = gap["clip"].as_str().expect("a clip was saved");
    assert!(std::fs::metadata(clip).unwrap().len() > 1000);
    let _ = std::fs::remove_dir_all(&d);
}

/// `loopcheck` on a card that is not there says so and fails, on every platform.
#[test]
fn loopcheck_without_a_device_fails_with_a_message() {
    let r = Command::new(env!("CARGO_BIN_EXE_hotclock"))
        .args([
            "loopcheck",
            "--input",
            "hw:CARD=NoSuchCard,DEV=0",
            "--seconds",
            "1",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!r.status.success());
    let e = String::from_utf8_lossy(&r.stderr);
    assert!(e.contains("loopcheck:"), "{e}");
}
