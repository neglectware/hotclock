//! M1.1 acceptance 1 and 5 through the real binary, plus the config error path.

use std::path::PathBuf;
use std::process::Command;

fn hotclock() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hotclock"))
}

fn tmp(name: &str) -> PathBuf {
    let d = fixtures::testdir::process_dir().join("cli");
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn rms_db(x: &[f32], channel: usize, channels: usize) -> f64 {
    let n = x.len() / channels;
    let ms: f64 = x
        .iter()
        .skip(channel)
        .step_by(channels)
        .map(|&s| (s as f64).powi(2))
        .sum::<f64>()
        / n as f64;
    10.0 * ms.log10()
}

/// Acceptance 1: `hotclock render --tone 1000 --seconds 10 --out t.wav` produces a WAV whose RMS
/// is within 0.05 dB of the requested level and whose length is exact to the frame.
#[test]
fn render_tone_rms_and_length_are_exact() {
    for (level, rate, fmt, tol_db) in [
        (-20.0, 48_000u32, "f32", 0.05),
        (-14.0, 48_000, "s24", 0.05),
        (-30.0, 44_100, "s16", 0.05),
        (-6.0, 48_000, "s32", 0.05),
    ] {
        let out = tmp(&format!("t-{fmt}-{rate}.wav"));
        let o = hotclock()
            .args([
                "render",
                "--tone",
                "1000",
                "--seconds",
                "10",
                "--level",
                &level.to_string(),
            ])
            .args(["--rate", &rate.to_string(), "--format", fmt, "--out"])
            .arg(&out)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let w = engine::wav::read_wav(&out).unwrap();
        let (l, r) = (rms_db(&w.samples, 0, 2), rms_db(&w.samples, 1, 2));
        println!(
            "render {fmt} {rate} Hz: {} frames (limit exactly {}); RMS L {l:.4} R {r:.4} dBFS for requested {level} (limit {tol_db} dB)",
            w.frames(),
            10 * rate
        );
        assert_eq!(w.frames(), 10 * rate as usize);
        assert_eq!(w.rate, rate);
        assert!((l - level).abs() < tol_db && (r - level).abs() < tol_db);
    }
}

#[test]
fn render_length_is_exact_for_odd_durations() {
    let out = tmp("odd.wav");
    let o = hotclock()
        .args(["render", "--tone", "440", "--seconds", "3.0007", "--out"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success());
    let w = engine::wav::read_wav(&out).unwrap();
    println!(
        "render 3.0007 s: {} frames (limit exactly {})",
        w.frames(),
        (3.0007f64 * 48_000.0).round() as usize
    );
    assert_eq!(w.frames(), (3.0007f64 * 48_000.0).round() as usize);
}

#[test]
fn render_rejects_a_clipping_level() {
    let o = hotclock()
        .args(["render", "--tone", "1000", "--level", "0", "--out"])
        .arg(tmp("clip.wav"))
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("clip"));
}

/// M1.4: `render --scenario` renders a YAML scenario through the real conductor and mixer and
/// writes the WAV and the sidecar JSON.
#[test]
fn render_scenario_writes_the_wav_and_the_sidecar() {
    let (dir, rep) = fixtures::shared_dir(&["tone_fade.flac", "pickup.flac"]);
    if !rep.skipped.is_empty() {
        eprintln!("SKIPPED: fixtures not available: {:?}", rep.skipped);
        return;
    }
    let yaml = tmp("scenario.yaml");
    std::fs::write(
        &yaml,
        "name: cli\nstation: { preset: sam }\nitems:\n  a: { file: tone_fade.flac }\n  b: { file: pickup.flac }\nqueue:\n  - { item: a }\n  - { item: b }\n",
    )
    .unwrap();
    let (wav, side) = (tmp("scenario.wav"), tmp("scenario.json"));
    let o = hotclock()
        .args(["render", "--scenario"])
        .arg(&yaml)
        .arg("--fixtures")
        .arg(&dir)
        .arg("--out")
        .arg(&wav)
        .arg("--sidecar")
        .arg(&side)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    println!("{}", String::from_utf8_lossy(&o.stdout));
    assert!(std::fs::metadata(&wav).unwrap().len() > 1_000_000);
    let j: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&side).unwrap()).unwrap();
    assert_eq!(j["voices"].as_array().unwrap().len(), 2);
    assert_eq!(j["boundaries"][0]["kind"], "segue");
    assert_eq!(j["scenario"], "cli");

    let o = hotclock()
        .args(["render", "--scenario", "/nonexistent/x.yaml"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("x.yaml"));
}

#[test]
fn station_test_on_null_plays_and_reports() {
    let o = hotclock()
        .args([
            "station",
            "test",
            "--tone",
            "--device",
            "null",
            "--seconds",
            "2",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    println!("{out}");
    assert!(out.contains("0 xruns"), "{out}");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("S32_LE 48000 Hz 2 ch, period 1024 frames"),
        "{err}"
    );
}

/// Acceptance 4 through the CLI hook: the stall shows up as one counted xrun.
#[test]
fn station_test_stall_hook_counts_one_xrun() {
    let o = hotclock()
        .args([
            "station",
            "test",
            "--tone",
            "--device",
            "null",
            "--seconds",
            "4",
            "--inject-stall-ms",
            "500",
            "--inject-stall-after",
            "1",
        ])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    println!("{out}");
    assert!(
        out.contains("1 xruns"),
        "{out}\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn station_config_errors_name_file_and_key() {
    let cfg = tmp("office.toml");
    std::fs::write(
        &cfg,
        "name = \"office\"\nlisten = \"0.0.0.0:8301\"\ndevice = \"null\"\nrate = 22050\n",
    )
    .unwrap();
    let o = hotclock()
        .args(["station", "test", "--tone", "--config"])
        .arg(&cfg)
        .output()
        .unwrap();
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    println!("{err}");
    assert!(
        err.contains("office.toml") && err.contains("key `rate`"),
        "{err}"
    );
}

#[test]
fn alsa_device_on_a_host_without_alsa_fails_readably() {
    let o = hotclock()
        .args([
            "station",
            "test",
            "--tone",
            "--device",
            "hw:CARD=Nope,DEV=0",
            "--seconds",
            "1",
        ])
        .output()
        .unwrap();
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    println!("{err}");
    assert!(err.contains("hw:CARD=Nope,DEV=0"), "{err}");
}

/// Acceptance 5: `census` over the fixtures reports `corrupt.mp3` as decodable-with-errors and
/// `truncated.flac` as truncated.
#[test]
fn census_over_fixtures() {
    let names = [
        "corrupt.mp3",
        "truncated.flac",
        "tone_fade.flac",
        "mp4_named_aac.aac",
        "art_folder/track.mp3",
    ];
    let (dir, report) = fixtures::shared_dir(&names);
    if !report.skipped.is_empty() {
        eprintln!("SKIPPED census over fixtures: {:?}", report.skipped);
        return;
    }
    // A directory holding only these files: copy them so other cached fixtures are not listed.
    let d = tmp("census-dir");
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("art_folder")).unwrap();
    for n in names {
        std::fs::copy(dir.join(n), d.join(n)).unwrap();
    }
    std::fs::copy(
        dir.join("art_folder/folder.jpg"),
        d.join("art_folder/folder.jpg"),
    )
    .unwrap();

    let o = hotclock().arg("census").arg(&d).output().unwrap();
    assert!(o.status.success());
    let text = String::from_utf8_lossy(&o.stdout);
    println!("{text}");
    assert!(text.contains("corrupt.mp3") && text.contains("decodable-with-errors"));
    assert!(text.contains("truncated"));
    assert!(
        text.contains("5 files: 3 ok, 1 decodable-with-errors, 1 truncated, 0 undecodable"),
        "{text}"
    );
    assert!(text.contains("extension `.aac` suggests adts-aac, content is mp4"));

    let o = hotclock()
        .arg("census")
        .arg(&d)
        .arg("--json")
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["summary"]["files"], 5);
    assert_eq!(v["summary"]["truncated"], 1);
}

/// M1.2: `hotclock play` renders a list of files gaplessly through the file backend; the WAV is
/// exactly as long as the files (resampled to 48 kHz), and a bad file is reported.
#[test]
fn play_renders_a_list_gaplessly_to_a_wav() {
    let (dir, report) = fixtures::shared_dir(&["mono.wav", "aiff.aif"]);
    if !report.skipped.is_empty() {
        eprintln!("SKIPPED: {:?}", report.skipped);
        return;
    }
    let out = tmp("play.wav");
    let o = hotclock()
        .arg("play")
        .arg(dir.join("mono.wav"))
        .arg(dir.join("aiff.aif"))
        .args(["--device", &format!("file:{}", out.display())])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout);
    println!("{stdout}");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let w = engine::wav::read_wav(&out).unwrap();
    // mono.wav is 10 s at 44.1 kHz -> 480,000 frames at 48 kHz; aiff.aif is on the milestone
    // table (48 kHz): its length is whatever the file holds, checked through the census.
    let aiff = analysis::census::inspect(&dir.join("aiff.aif"));
    let expect = 480_000
        + (aiff.decoded_frames as f64 * 48_000.0 / f64::from(aiff.sample_rate.unwrap())).round()
            as usize;
    println!("play: {} frames (exact {expect})", w.frames());
    assert_eq!(w.frames(), expect);
    assert_eq!(w.rate, 48_000);
    assert!(stdout.contains("played 2 files"));
}

#[test]
fn play_reports_an_unreadable_file_and_carries_on() {
    let (dir, report) = fixtures::shared_dir(&["mono.wav"]);
    if !report.skipped.is_empty() {
        return;
    }
    let bad = tmp("not-audio.flac");
    std::fs::write(&bad, b"this is not audio").unwrap();
    let out = tmp("play-bad.wav");
    let o = hotclock()
        .arg("play")
        .arg(&bad)
        .arg(dir.join("mono.wav"))
        .args(["--device", &format!("file:{}", out.display())])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout);
    println!("{stdout}");
    assert_eq!(o.status.code(), Some(1), "an error in a file gives exit 1");
    assert!(stdout.contains("ERROR"));
    let w = engine::wav::read_wav(&out).unwrap();
    assert_eq!(
        w.frames(),
        480_000,
        "the good file still plays, from frame 0"
    );
}
