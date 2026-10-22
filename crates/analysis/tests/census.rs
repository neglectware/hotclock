//! M1.1 acceptance 5 and the format/artwork coverage checks of the census, over synthetic
//! fixtures. Fixtures that need `ffmpeg` (MP3, ALAC, AAC) are skipped, with a message, when it is
//! not installed.

use analysis::census::{collect_files, detect_container, inspect, summarize, Verdict};
use fixtures::Generator;
use std::path::PathBuf;

fn fixture(name: &str) -> Option<PathBuf> {
    let (dir, report) = fixtures::shared_dir(&[name]);
    if let Some((n, why)) = report.skipped.iter().find(|(n, _)| n == name) {
        eprintln!("SKIPPED census check for {n}: {why}");
        return None;
    }
    let p = dir.join(name);
    assert!(p.exists(), "{name} was not generated");
    Some(p)
}

#[test]
fn corrupt_mp3_is_decodable_with_errors() {
    let Some(p) = fixture("corrupt.mp3") else {
        return;
    };
    let r = inspect(&p);
    println!(
        "corrupt.mp3: {} decode errors, {} frames decoded ({:?}) => {}",
        r.decode_errors,
        r.decoded_frames,
        r.detail,
        r.verdict.label()
    );
    assert_eq!(r.container, "mp3");
    assert_eq!(r.verdict, Verdict::DecodableWithErrors);
    assert!(r.decode_errors > 0);
    assert!(
        r.decoded_frames > 26 * 48_000 / 2,
        "should decode most of the file"
    );
}

#[test]
fn truncated_flac_is_truncated() {
    let Some(p) = fixture("truncated.flac") else {
        return;
    };
    let r = inspect(&p);
    let pct = r.decoded_frames as f64 * 100.0 / r.declared_frames.unwrap() as f64;
    println!(
        "truncated.flac: decoded {} of {} declared frames ({pct:.1} percent; file cut at 60 percent of bytes) => {}",
        r.decoded_frames,
        r.declared_frames.unwrap(),
        r.verdict.label()
    );
    assert_eq!(r.container, "flac");
    assert_eq!(r.verdict, Verdict::Truncated);
    assert!(pct < 100.0 && pct > 30.0);
}

#[test]
fn clean_files_of_every_format_are_ok() {
    for (name, container, codec_hint, rate, channels) in [
        ("tone_fade.flac", "flac", "flac", 48_000, 2),
        ("rate_44k.flac", "flac", "flac", 44_100, 2),
        ("rate_96k.flac", "flac", "flac", 96_000, 2),
        ("flac16.flac", "flac", "flac", 48_000, 2),
        ("mono.wav", "wav", "pcm_s24le", 44_100, 1),
        ("aiff.aif", "aiff", "pcm_s24be", 48_000, 2),
        ("alac.m4a", "mp4", "alac", 48_000, 2),
        ("art_folder/track.mp3", "mp3", "mp3", 48_000, 2),
    ] {
        let Some(p) = fixture(name) else { continue };
        let r = inspect(&p);
        println!(
            "{name}: {} / {} {} Hz {} ch, decoded {} of {:?} declared => {}",
            r.container,
            r.codec.as_deref().unwrap_or("?"),
            r.sample_rate.unwrap_or(0),
            r.channels.unwrap_or(0),
            r.decoded_frames,
            r.declared_frames,
            r.verdict.label()
        );
        assert_eq!(r.verdict, Verdict::Ok, "{name}: {:?}", r.detail);
        assert_eq!(r.container, container, "{name}");
        assert_eq!(r.codec.as_deref(), Some(codec_hint), "{name}");
        assert_eq!(r.sample_rate, Some(rate), "{name}");
        assert_eq!(r.channels, Some(channels), "{name}");
        assert_eq!(r.extension_mismatch, None, "{name}");
    }
}

#[test]
fn flac_frame_counts_are_exact() {
    let Some(p) = fixture("tone_fade.flac") else {
        return;
    };
    let r = inspect(&p);
    assert_eq!(r.decoded_frames, 1_272_000); // 26.5 s at 48 kHz
    assert_eq!(r.declared_frames, Some(1_272_000));
}

#[test]
fn tags_and_artwork() {
    if let Some(p) = fixture("art_embedded.flac") {
        let r = inspect(&p);
        println!(
            "art_embedded.flac: tags {:?}, artwork {:?}",
            r.tags, r.artwork
        );
        assert!(r.tags.present);
        assert_eq!(r.tags.title.as_deref(), Some("Embedded Art (fixture)"));
        assert_eq!(r.artwork.embedded, 1);
        assert_eq!(r.artwork.folder_image, None);
    }
    if let Some(p) = fixture("album_t2.flac") {
        let r = inspect(&p);
        assert_eq!(r.tags.album.as_deref(), Some("Fixture Album"));
        assert_eq!(r.tags.album_artist.as_deref(), Some("Fixture Artist"));
        assert_eq!(
            (r.tags.disc, r.tags.track, r.tags.track_total),
            (Some(1), Some(2), Some(2))
        );
        assert_eq!(r.artwork.embedded, 1);
    }
    if let Some(p) = fixture("art_folder/track.mp3") {
        let r = inspect(&p);
        assert_eq!(r.artwork.embedded, 0);
        assert_eq!(r.artwork.folder_image.as_deref(), Some("folder.jpg"));
        assert!(r.artwork.any());
    }
    if let Some(p) = fixture("inbox/liner/Donate Liner 02.wav") {
        let r = inspect(&p);
        println!(
            "Donate Liner 02.wav: tags {:?}, artwork {:?}",
            r.tags, r.artwork
        );
        assert!(r.tags.title.is_none() && r.tags.album.is_none());
        assert!(!r.artwork.any());
        assert_eq!(r.verdict, Verdict::Ok);
    }
}

/// Library music that is named `.aac` but is an MP4/M4A container (AAC-LC, embedded MJPEG cover).
/// Probing must go by content, not by extension.
#[test]
fn aac_named_file_that_is_really_mp4_is_probed_by_content() {
    let Some(p) = fixture("mp4_named_aac.aac") else {
        return;
    };
    assert_eq!(p.extension().unwrap(), "aac");
    let r = inspect(&p);
    println!(
        "mp4_named_aac.aac: container {} codec {:?} {:?} Hz, decoded {} frames, tags {:?}, artwork {:?}, note {:?} => {}",
        r.container, r.codec, r.sample_rate, r.decoded_frames, r.tags.title, r.artwork, r.extension_mismatch, r.verdict.label()
    );
    assert_eq!(r.container, "mp4");
    assert_eq!(r.codec.as_deref(), Some("aac"));
    assert_eq!(r.sample_rate, Some(48_000));
    assert_eq!(r.verdict, Verdict::Ok, "{:?}", r.detail);
    assert!(r.decoded_frames > 7 * 48_000);
    assert_eq!(r.tags.title.as_deref(), Some("Mislabelled AAC (fixture)"));
    assert_eq!(r.artwork.embedded, 1, "embedded MJPEG cover");
    assert!(r.extension_mismatch.as_deref().unwrap().contains("mp4"));
}

#[test]
fn container_detection_from_magic_bytes() {
    assert_eq!(detect_container(b"fLaC\0\0\0\x22"), "flac");
    assert_eq!(detect_container(b"RIFF\0\0\0\0WAVEfmt "), "wav");
    assert_eq!(detect_container(b"FORM\0\0\0\0AIFFCOMM"), "aiff");
    assert_eq!(detect_container(b"OggS\0\x02"), "ogg");
    assert_eq!(detect_container(b"\0\0\0\x20ftypM4A "), "mp4");
    assert_eq!(detect_container(b"ID3\x04\0\0"), "mp3");
    assert_eq!(detect_container(&[0xFF, 0xFB, 0x90, 0x00]), "mp3");
    assert_eq!(detect_container(&[0xFF, 0xF1, 0x50, 0x80]), "adts-aac");
    assert_eq!(detect_container(b"hello world!"), "unknown");
}

#[test]
fn non_audio_and_missing_files_are_undecodable() {
    let dir = fixtures::testdir::process_dir().join("census");
    std::fs::create_dir_all(&dir).unwrap();
    let junk = dir.join("junk.flac");
    std::fs::write(
        &junk,
        b"this is not audio at all, just text padding out the file",
    )
    .unwrap();
    let r = inspect(&junk);
    assert_eq!(r.verdict, Verdict::Undecodable);
    let r = inspect(&dir.join("missing.flac"));
    assert_eq!(r.verdict, Verdict::Undecodable);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn directory_and_m3u_collection() {
    let dir = fixtures::testdir::process_dir().join("census-list");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    for n in [
        "b.flac",
        "sub/a.mp3",
        "cover.jpg",
        ".hidden.flac",
        "._junk.flac",
        "notes.txt",
    ] {
        std::fs::write(dir.join(n), b"x").unwrap();
    }
    let files = collect_files(&dir).unwrap();
    let names: Vec<_> = files
        .iter()
        .map(|p| p.strip_prefix(&dir).unwrap().to_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["b.flac", "sub/a.mp3"]);

    std::fs::write(
        dir.join("list.m3u8"),
        "#EXTM3U\n#EXTINF:1,x\nsub/a.mp3\n\nb.flac\n/abs/x.wav\n",
    )
    .unwrap();
    let files = collect_files(&dir.join("list.m3u8")).unwrap();
    assert_eq!(files.len(), 3);
    assert_eq!(files[0], dir.join("sub/a.mp3"));
    assert_eq!(files[2], PathBuf::from("/abs/x.wav"));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn summary_counts() {
    let Some(a) = fixture("tone_fade.flac") else {
        return;
    };
    let Some(b) = fixture("truncated.flac") else {
        return;
    };
    let s = summarize(&[inspect(&a), inspect(&b)]);
    assert_eq!((s.files, s.ok, s.truncated), (2, 1, 1));
    let _ = Generator::new(std::path::Path::new("."));
}
