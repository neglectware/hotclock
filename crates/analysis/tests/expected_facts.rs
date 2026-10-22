//! The facts in `tests/fixtures/expected.toml` that the census can check today (duration, rate,
//! channels, embedded pictures, folder image, tags) are checked against the generated fixtures, so
//! the hand-written expectations and the generator cannot drift apart. Marker values are M1.3's.

use analysis::census::inspect;
use std::path::Path;

fn expected() -> toml::Table {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/expected.toml");
    std::fs::read_to_string(p).unwrap().parse().unwrap()
}

fn file_for(key: &str) -> String {
    if key.contains('.') {
        key.to_string()
    } else {
        format!("{key}.flac")
    }
}

#[test]
fn expected_toml_matches_generated_fixtures() {
    let exp = expected();
    let names: Vec<String> = exp.keys().map(|k| file_for(k)).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    for n in &refs {
        assert!(
            fixtures::ALL.contains(n),
            "expected.toml names `{n}`, which the generator does not make"
        );
    }
    let (dir, report) = fixtures::shared_dir(&refs);
    if !report.skipped.is_empty() {
        eprintln!(
            "SKIPPED expected-facts check (no ffmpeg?): {:?}",
            report.skipped
        );
        return;
    }
    let mut checked = 0;
    for (key, val) in &exp {
        let name = file_for(key);
        let t = val.as_table().unwrap();
        let path = dir.join(&name);
        let r = inspect(&path);
        if let Some(d) = t.get("duration_ms").and_then(|v| v.as_integer()) {
            // MP3 and AAC carry encoder padding; everything else is exact to the frame.
            let tol = if name.ends_with(".mp3") || name.ends_with(".aac") || name.ends_with(".m4a")
            {
                60.0
            } else {
                0.5
            };
            let got = r.decoded_frames as f64 * 1000.0 / r.sample_rate.unwrap() as f64;
            println!("{name}: duration {got:.1} ms (expected {d}, limit {tol})");
            assert!((got - d as f64).abs() <= tol, "{name}: {got} vs {d}");
            checked += 1;
        }
        if let Some(sr) = t.get("sample_rate").and_then(|v| v.as_integer()) {
            assert_eq!(r.sample_rate, Some(sr as u32), "{name}");
        }
        if let Some(ch) = t.get("channels").and_then(|v| v.as_integer()) {
            assert_eq!(r.channels, Some(ch as usize), "{name}");
        }
        if let Some(n) = t.get("embedded_pictures").and_then(|v| v.as_integer()) {
            assert_eq!(r.artwork.embedded as i64, n, "{name}");
        }
        if let Some(f) = t.get("folder_image").and_then(|v| v.as_str()) {
            assert_eq!(r.artwork.folder_image.as_deref(), Some(f), "{name}");
        }
        for (k, got) in [
            ("album", &r.tags.album),
            ("album_artist", &r.tags.album_artist),
            ("title", &r.tags.title),
        ] {
            if let Some(v) = t.get(k).and_then(|v| v.as_str()) {
                assert_eq!(got.as_deref(), Some(v), "{name}: {k}");
            }
        }
        if let Some(b) = t.get("bits_per_sample").and_then(|v| v.as_integer()) {
            assert_eq!(r.bits_per_sample, Some(b as u32), "{name}");
        }
    }
    println!("{checked} durations checked against expected.toml");
    assert!(checked >= 15);
}
