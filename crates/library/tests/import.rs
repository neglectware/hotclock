//! M1.3 acceptance 3, 4, 6 (database half), 8 and 10 (second half) against the synthetic
//! fixtures, through the real library code and a real SQLite file.

use analysis::analyze::analyze_file;
use analysis::effective::{effective_markers, EffectiveInput, Overrides};
use analysis::markers::{FileFacts, Flag, LongIntroPolicy, ProfileParams};
use library::items::OverridePatch;
use library::{import_file, ImportOptions, ImportOutcome, Library, LibraryError, Paths};
use rusqlite::params;
use std::path::{Path, PathBuf};

fn tmpdir(name: &str) -> PathBuf {
    let d = fixtures::testdir::process_dir().join(format!("{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn open(name: &str) -> (Library, PathBuf) {
    let d = tmpdir(name);
    (
        Library::open(Paths::from_data_dir(&d.join("data"))).unwrap(),
        d,
    )
}

fn fx(names: &[&str]) -> Option<PathBuf> {
    let (dir, report) = fixtures::shared_dir(names);
    if !report.skipped.is_empty() {
        eprintln!("SKIPPED (no ffmpeg?): {:?}", report.skipped);
        return None;
    }
    Some(dir)
}

fn count(lib: &Library, sql: &str) -> i64 {
    lib.conn().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut v = vec![];
    library::store::walk_files(dir, &mut v);
    v
}

fn opts(cat: Option<&str>) -> ImportOptions {
    ImportOptions {
        category: cat.map(str::to_string),
        source: "test".into(),
    }
}

fn done_id(o: &ImportOutcome) -> i64 {
    match o {
        ImportOutcome::Done { item_id, .. } => *item_id,
        other => panic!("expected Done, got {other:?}"),
    }
}

#[test]
fn schema_is_created_once_and_reopens() {
    let (lib, d) = open("schema");
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM categories"), 7);
    assert_eq!(
        count(
            &lib,
            "SELECT COUNT(*) FROM transition_presets WHERE builtin = 1"
        ),
        5
    );
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM segue_profiles"), 4);
    let tables = count(
        &lib,
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
    );
    assert!(tables >= 21, "{tables} tables");
    assert_eq!(
        count(
            &lib,
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key='schema_version'"
        ),
        library::db::SCHEMA_VERSION
    );
    let mode: String = lib
        .conn()
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let fk: i64 = lib
        .conn()
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fk, 1);
    drop(lib);
    let lib2 = Library::open(Paths::from_data_dir(&d.join("data"))).unwrap();
    assert_eq!(
        count(&lib2, "SELECT COUNT(*) FROM categories"),
        7,
        "seed rows must not be repeated"
    );
}

/// Acceptance 3.
#[test]
fn importing_the_same_file_twice_gives_one_item_and_a_duplicate_job() {
    let Some(dir) = fx(&["tone_fade.flac"]) else {
        return;
    };
    let (lib, _d) = open("dup");
    let src = dir.join("tone_fade.flac");
    let first = import_file(&lib, &src, &opts(None));
    let id = done_id(&first);
    let second = import_file(&lib, &src, &opts(None));
    assert_eq!(second, ImportOutcome::Duplicate { item_id: id });
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM items"), 1);
    assert_eq!(
        count(
            &lib,
            "SELECT COUNT(*) FROM import_jobs WHERE status='duplicate'"
        ),
        1
    );
    assert_eq!(
        count(&lib, "SELECT COUNT(*) FROM import_jobs WHERE status='done'"),
        1
    );
    // A copy under another name has the same bytes: still a duplicate.
    let copy = tmpdir("dup-copy").join("renamed.flac");
    std::fs::copy(&src, &copy).unwrap();
    assert_eq!(
        import_file(&lib, &copy, &opts(None)),
        ImportOutcome::Duplicate { item_id: id }
    );
    println!("second import: duplicate of item {id}; 1 item, 1 duplicate job (then a renamed copy: duplicate too)");
}

/// Acceptance 4.
#[test]
fn truncated_file_fails_readably_and_leaves_nothing() {
    let Some(dir) = fx(&["truncated.flac"]) else {
        return;
    };
    let (lib, _d) = open("trunc");
    let out = import_file(&lib, &dir.join("truncated.flac"), &opts(None));
    let ImportOutcome::Failed { error } = out else {
        panic!("{out:?}")
    };
    println!("truncated.flac: {error}");
    assert!(error.contains("truncated"), "{error}");
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM items"), 0);
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM analysis"), 0);
    assert_eq!(
        files_under(&lib.paths.media).len(),
        0,
        "the store must hold no file"
    );
    assert_eq!(files_under(&lib.paths.upload_tmp).len(), 0);
    assert_eq!(
        count(
            &lib,
            "SELECT COUNT(*) FROM import_jobs WHERE status='failed'"
        ),
        1
    );
    let stored: String = lib
        .conn()
        .query_row("SELECT error FROM import_jobs", [], |r| r.get(0))
        .unwrap();
    assert!(stored.contains("truncated"));
}

#[test]
fn corrupt_mp3_imports_with_a_warning_and_undecodable_junk_fails() {
    let Some(dir) = fx(&["corrupt.mp3"]) else {
        return;
    };
    let (lib, d) = open("corrupt");
    let out = import_file(&lib, &dir.join("corrupt.mp3"), &opts(None));
    let ImportOutcome::Done { warnings, .. } = &out else {
        panic!("{out:?}")
    };
    println!("corrupt.mp3: {warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("failed to decode")));
    let junk = d.join("junk.flac");
    std::fs::write(&junk, vec![7u8; 5000]).unwrap();
    let out = import_file(&lib, &junk, &opts(None));
    assert!(matches!(out, ImportOutcome::Failed { .. }), "{out:?}");
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM items"), 1);
    assert_eq!(files_under(&lib.paths.upload_tmp).len(), 0);
}

/// Acceptance 8, formats: stored bytes are hash-identical to the source.
#[test]
fn every_format_imports_as_is() {
    let names = [
        "art_embedded.flac",
        "art_folder/track.mp3",
        "alac.m4a",
        "aiff.aif",
        "mp4_named_aac.aac",
        "mono.wav",
        "rate_96k.flac",
    ];
    let Some(dir) = fx(&names) else { return };
    let (lib, _d) = open("formats");
    for n in names {
        let src = dir.join(n);
        let sha = library::util::sha256_file(&src).unwrap();
        let out = import_file(&lib, &src, &opts(None));
        let id = done_id(&out);
        let (store_path, stored_sha, format, rate, ch): (String, String, String, i64, i64) = lib
            .conn()
            .query_row(
                "SELECT store_path, sha256, format, sample_rate, channels FROM items WHERE id=?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        let stored = lib.paths.media.join(&store_path);
        let on_disk = library::util::sha256_file(&stored).unwrap();
        println!(
            "{n}: stored as {store_path} ({format}, {rate} Hz, {ch} ch), sha256 of stored bytes == source: {}",
            on_disk == sha
        );
        assert_eq!(on_disk, sha, "{n}");
        assert_eq!(stored_sha, sha);
        assert!(store_path.starts_with(&sha[..2]));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&stored).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o444);
    }
    // The MP4 named .aac is stored by content.
    let p: String = lib
        .conn()
        .query_row(
            "SELECT store_path FROM items WHERE orig_filename='mp4_named_aac.aac'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(p.ends_with(".m4a"), "{p}");
}

fn dims(p: &Path) -> (u32, u32) {
    image::ImageReader::open(p)
        .unwrap()
        .with_guessed_format()
        .unwrap()
        .into_dimensions()
        .unwrap()
}

/// Acceptance 8, artwork.
#[test]
fn artwork_is_normalized_shared_and_never_fatal() {
    let Some(dir) = fx(&[
        "art_embedded.flac",
        "art_folder/track.mp3",
        "album_t2.flac",
        "tone_fade.flac",
    ]) else {
        return;
    };
    let (lib, d) = open("art");
    // Embedded 1400 x 1400 PNG -> 600 and 144 JPEGs.
    let id = done_id(&import_file(
        &lib,
        &dir.join("art_embedded.flac"),
        &opts(None),
    ));
    let sha: String = lib
        .conn()
        .query_row("SELECT art_sha FROM items WHERE id=?1", params![id], |r| {
            r.get(0)
        })
        .unwrap();
    let p600 = lib.paths.media.join(library::store::art_rel(&sha, 600));
    let p144 = lib.paths.media.join(library::store::art_rel(&sha, 144));
    println!(
        "art_embedded.flac: 1400 x 1400 px source -> {:?} and {:?}",
        dims(&p600),
        dims(&p144)
    );
    assert_eq!(dims(&p600), (600, 600));
    assert_eq!(dims(&p144), (144, 144));
    // The MP3 with folder.jpg (800 x 800, nothing embedded).
    let id2 = done_id(&import_file(
        &lib,
        &dir.join("art_folder/track.mp3"),
        &opts(None),
    ));
    let (sha2, origin): (String, String) = lib
        .conn()
        .query_row(
            "SELECT i.art_sha, a.origin FROM items i JOIN artwork a ON a.sha256=i.art_sha WHERE i.id=?1",
            params![id2],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(origin, "folder");
    assert_eq!(
        dims(&lib.paths.media.join(library::store::art_rel(&sha2, 600))),
        (600, 600)
    );
    assert_eq!(
        dims(&lib.paths.media.join(library::store::art_rel(&sha2, 144))),
        (144, 144)
    );
    // Two tracks of one album share one stored image: album_t2's cover embedded in a second file.
    let id3 = done_id(&import_file(&lib, &dir.join("album_t2.flac"), &opts(None)));
    let cover = library::tags::read_tags(&dir.join("album_t2.flac"))
        .cover
        .expect("album_t2 has a cover");
    let second = d.join("second_track.flac");
    std::fs::copy(dir.join("tone_fade.flac"), &second).unwrap();
    {
        use lofty::config::WriteOptions;
        use lofty::picture::{MimeType, Picture, PictureType};
        use lofty::prelude::*;
        use lofty::tag::{Tag, TagExt, TagType};
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.set_album("Fixture Album".to_string());
        tag.push_picture(
            Picture::unchecked(cover)
                .pic_type(PictureType::CoverFront)
                .mime_type(MimeType::Png)
                .build(),
        );
        tag.save_to_path(&second, WriteOptions::default()).unwrap();
    }
    let id4 = done_id(&import_file(&lib, &second, &opts(None)));
    let a3: String = lib
        .conn()
        .query_row("SELECT art_sha FROM items WHERE id=?1", params![id3], |r| {
            r.get(0)
        })
        .unwrap();
    let a4: String = lib
        .conn()
        .query_row("SELECT art_sha FROM items WHERE id=?1", params![id4], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(a3, a4);
    let stored: usize = files_under(&lib.paths.media.join("art"))
        .iter()
        .filter(|p| p.to_string_lossy().contains(&a3))
        .count();
    println!(
        "two tracks of album 'Fixture Album': art_sha {} == {}; image files for it: {stored} (600 + 144)",
        &a3[..12],
        &a4[..12]
    );
    assert_eq!(stored, 2);
    assert_eq!(
        count(&lib, "SELECT COUNT(*) FROM artwork"),
        3,
        "art_embedded, folder.jpg, album cover"
    );
    // A corrupt cover image imports the audio with a warning and no art.
    let bad = d.join("badcover");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::copy(dir.join("art_folder/track.mp3"), bad.join("song.mp3")).unwrap();
    {
        // Different bytes from the MP3 imported above, so it is not a duplicate.
        use lofty::config::WriteOptions;
        use lofty::prelude::*;
        use lofty::tag::{Tag, TagExt, TagType};
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_title("Song with a bad cover".to_string());
        tag.save_to_path(bad.join("song.mp3"), WriteOptions::default())
            .unwrap();
    }
    std::fs::write(bad.join("cover.jpg"), b"this is not a jpeg at all").unwrap();
    let out = import_file(&lib, &bad.join("song.mp3"), &opts(None));
    let ImportOutcome::Done {
        item_id, warnings, ..
    } = &out
    else {
        panic!("{out:?}")
    };
    println!("corrupt cover: {warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("artwork")));
    let art: Option<String> = lib
        .conn()
        .query_row(
            "SELECT art_sha FROM items WHERE id=?1",
            params![item_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(art.is_none());
}

/// Acceptance 8, the untagged WAV dropped in `inbox/liner/`.
#[test]
fn inbox_folder_selects_the_category_and_untagged_files_are_titled_from_the_name() {
    let Some(dir) = fx(&["inbox/liner/Donate Liner 02.wav", "tone_fade.flac"]) else {
        return;
    };
    let (lib, _d) = open("inbox");
    let liner_dir = lib.paths.inbox.join("liner");
    std::fs::create_dir_all(&liner_dir).unwrap();
    let src = dir.join("inbox/liner/Donate Liner 02.wav");
    let dropped = liner_dir.join("Donate Liner 02.wav");
    std::fs::copy(&src, &dropped).unwrap();
    let sha = library::util::sha256_file(&src).unwrap();
    // A music file in the bare inbox.
    std::fs::copy(
        dir.join("tone_fade.flac"),
        lib.paths.inbox.join("tone_fade.flac"),
    )
    .unwrap();
    let results = library::inbox::scan_once(&lib.paths, 2, std::time::Duration::ZERO).unwrap();
    assert_eq!(results.len(), 2, "{results:?}");
    assert!(!dropped.exists(), "an imported file leaves the inbox");
    let (title, cat, key, sp): (String, String, String, String) = lib
        .conn()
        .query_row(
            "SELECT i.title, c.name, i.album_key, i.store_path FROM items i JOIN categories c ON c.id=i.category_id WHERE i.orig_filename='Donate Liner 02.wav'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    println!("wav_untagged: title {title:?}, category {cat}, album_key {key:?}, stored {sp}");
    assert_eq!(title, "Donate Liner 02");
    assert_eq!(cat, "liner");
    assert_eq!(key, "");
    assert_eq!(
        library::util::sha256_file(&lib.paths.media.join(sp)).unwrap(),
        sha
    );
    let music: String = lib
        .conn()
        .query_row(
            "SELECT c.name FROM items i JOIN categories c ON c.id=i.category_id WHERE i.orig_filename='tone_fade.flac'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(music, "music");
    // A failed file moves to .failed with its reason.
    std::fs::write(lib.paths.inbox.join("broken.flac"), vec![1u8; 4096]).unwrap();
    let r = library::inbox::scan_once(&lib.paths, 1, std::time::Duration::ZERO).unwrap();
    assert!(matches!(r[0].1, ImportOutcome::Failed { .. }));
    assert!(lib.paths.inbox.join(".failed/broken.flac").exists());
    assert!(lib
        .paths
        .inbox
        .join(".failed/broken.flac.error.txt")
        .exists());
    // Files newer than the settle time are left alone.
    std::fs::write(lib.paths.inbox.join("fresh.flac"), vec![1u8; 100]).unwrap();
    let r = library::inbox::scan_once(&lib.paths, 1, std::time::Duration::from_secs(60)).unwrap();
    assert!(r.is_empty());
}

#[test]
fn tags_and_album_key_are_read() {
    let Some(dir) = fx(&["album_t2.flac", "slowly_growing_deaf_t1.flac"]) else {
        return;
    };
    let (lib, _d) = open("tags");
    let a = done_id(&import_file(
        &lib,
        &dir.join("slowly_growing_deaf_t1.flac"),
        &opts(None),
    ));
    let b = done_id(&import_file(&lib, &dir.join("album_t2.flac"), &opts(None)));
    type Row = (
        String,
        String,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        String,
    );
    let row = |id: i64| -> Row {
        lib.conn()
            .query_row(
                "SELECT album, album_artist, disc_no, track_no, track_total, album_key FROM items WHERE id=?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap()
    };
    let (ra, rb) = (row(a), row(b));
    println!("{ra:?} / {rb:?}");
    assert_eq!(ra.0, "Fixture Album");
    assert_eq!((ra.2, ra.3, ra.4), (Some(1), Some(1), Some(2)));
    assert_eq!((rb.2, rb.3, rb.4), (Some(1), Some(2), Some(2)));
    assert_eq!(ra.5, "fixture artist|fixture album");
    assert_eq!(ra.5, rb.5, "two tracks of one album share the album_key");
}

/// Acceptance 6, database half: `{"segue_lu": -18}` on a stored item changes its effective
/// segue at once (under 50 ms for a track of over 5 minutes) and matches a full re-analysis.
#[test]
fn a_parameter_override_recomputes_immediately_and_matches_full_analysis() {
    let Some(dir) = fx(&["slowly_growing_deaf.flac"]) else {
        return;
    };
    let (lib, _d) = open("recompute");
    let src = dir.join("slowly_growing_deaf.flac");
    let id = done_id(&import_file(&lib, &src, &opts(None)));
    // A 5:52 cue-out override puts the segue inside the main body, where -18 LU moves it.
    let before = lib
        .set_overrides(
            id,
            &OverridePatch {
                cue_out: Some(Some(352_000)),
                ..Default::default()
            },
        )
        .unwrap();
    let t0 = std::time::Instant::now();
    let after = lib
        .set_overrides(
            id,
            &OverridePatch {
                params: Some(Some(serde_json::json!({"segue_lu": -18}))),
                ..Default::default()
            },
        )
        .unwrap();
    let dt = t0.elapsed();
    // A fresh decode of the file with the same parameter and override.
    let mut p = ProfileParams::music();
    p.apply_json(&serde_json::json!({"segue_lu": -18}));
    let full = analyze_file(&src, &p).unwrap();
    let reference = effective_markers(&EffectiveInput {
        facts: FileFacts {
            file_ms: full.file_ms,
            l_int: full.l_int,
            lra: full.lra,
        },
        env: &full.env,
        profile: &p,
        ovr: Overrides {
            cue_out: Some(352_000),
            ..Default::default()
        },
        policy: LongIntroPolicy::Play,
        base_flags: &full.base_flags,
        l_w_exact: None,
    });
    println!(
        "{} s track: segue {} ms at -12 LU, {} ms at -18 LU (recomputed and stored in {:?}, limit 50 ms); full re-analysis {} ms (limit 20 ms)",
        full.file_ms / 1000,
        before.segue,
        after.segue,
        dt,
        reference.segue
    );
    assert!(dt.as_millis() < 50, "{dt:?}");
    assert_ne!(before.segue, after.segue);
    assert!((after.segue - reference.segue).abs() <= 20);
    assert!(after.revision > before.revision);
    // The database row is what a station reads.
    assert_eq!(lib.item_effective(id).unwrap(), after);
}

/// Acceptance 10, second half: a cue moved past a stored manual segue flags
/// `override_outside_window`, ignores the stored value, and moving the cue back restores it.
#[test]
fn a_stored_marker_outside_the_window_is_ignored_then_restored() {
    let Some(dir) = fx(&["quote_unquote.flac"]) else {
        return;
    };
    let (lib, _d) = open("outside");
    let id = done_id(&import_file(
        &lib,
        &dir.join("quote_unquote.flac"),
        &opts(None),
    ));
    let auto = lib.item_effective(id).unwrap();
    let e = lib
        .set_overrides(
            id,
            &OverridePatch {
                segue: Some(Some(50_000)),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(e.segue, 50_000);
    assert!(!e.flags.contains(&Flag::OverrideOutsideWindow));
    // Move the cue-in past it.
    let e = lib
        .set_overrides(
            id,
            &OverridePatch {
                cue_in: Some(Some(70_000)),
                ..Default::default()
            },
        )
        .unwrap();
    println!(
        "cue-in moved to 70 s: segue {} (stored manual 50,000 ignored), flags {:?}",
        e.segue,
        e.flags.iter().map(|f| f.name()).collect::<Vec<_>>()
    );
    assert_ne!(e.segue, 50_000);
    assert!(e.flags.contains(&Flag::OverrideOutsideWindow));
    let stored: Option<i64> = lib
        .conn()
        .query_row(
            "SELECT ovr_segue FROM item_overrides WHERE item_id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        stored,
        Some(50_000),
        "the stored value is kept, not deleted"
    );
    // Move it back.
    let e = lib
        .set_overrides(
            id,
            &OverridePatch {
                cue_in: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(e.segue, 50_000);
    assert!(!e.flags.contains(&Flag::OverrideOutsideWindow));
    assert_eq!(e.cue_in, auto.cue_in);
    // Saving a marker outside the window is refused, and changes nothing.
    let rev = lib.item_effective(id).unwrap();
    let err = lib
        .set_overrides(
            id,
            &OverridePatch {
                attack: Some(Some(5_000)),
                ..Default::default()
            },
        )
        .unwrap_err();
    println!("refused: {err}");
    assert!(matches!(err, LibraryError::MarkerOutsideWindow { .. }));
    assert_eq!(lib.item_effective(id).unwrap(), rev);
    // A cue pair leaving under 250 ms is refused too.
    let err = lib
        .set_overrides(
            id,
            &OverridePatch {
                cue_in: Some(Some(60_000)),
                cue_out: Some(Some(60_100)),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(matches!(err, LibraryError::Invalid(_)), "{err}");
    assert_eq!(lib.item_effective(id).unwrap(), rev);
}

/// The exact window loudness job replaces the estimate.
#[test]
fn window_loudness_job_replaces_the_estimate() {
    let Some(dir) = fx(&["quote_unquote.flac"]) else {
        return;
    };
    let (lib, _d) = open("lw");
    let id = done_id(&import_file(
        &lib,
        &dir.join("quote_unquote.flac"),
        &opts(None),
    ));
    let e = lib
        .set_overrides(
            id,
            &OverridePatch {
                cue_in: Some(Some(33_980)),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(e.l_int_source, "window_estimate");
    assert!(e.flags.contains(&Flag::GainPending));
    let l = lib.compute_window_loudness(id).unwrap().unwrap();
    let e2 = lib.item_effective(id).unwrap();
    println!(
        "window loudness: estimate {:.3}, exact {l:.3} LUFS",
        e.l_int
    );
    assert_eq!(e2.l_int_source, "window");
    assert!(!e2.flags.contains(&Flag::GainPending));
    assert!((e2.l_int - l).abs() < 1e-9);
    assert!((e.l_int - l).abs() < 0.5);
    // Moving the window makes the stored exact value stale: back to an estimate.
    let e3 = lib
        .set_overrides(
            id,
            &OverridePatch {
                cue_in: Some(Some(34_500)),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(e3.l_int_source, "window_estimate");
}

/// Every write bumps the revision; the category decides the profile and the gain offset.
#[test]
fn revision_is_bumped_and_category_drives_the_profile() {
    let Some(dir) = fx(&["cold_start.flac"]) else {
        return;
    };
    let (lib, _d) = open("rev");
    let r0 = lib.revision().unwrap();
    let id = done_id(&import_file(
        &lib,
        &dir.join("cold_start.flac"),
        &opts(Some("liner")),
    ));
    let r1 = lib.revision().unwrap();
    assert!(r1 > r0);
    let cat: String = lib
        .conn()
        .query_row(
            "SELECT c.name FROM items i JOIN categories c ON c.id=i.category_id WHERE i.id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cat, "liner");
    let e = lib.item_effective(id).unwrap();
    assert_eq!(e.profile, "liner");
    assert_eq!(e.gain_offset_db, -1.0);
    assert!(e.transition_patch.contains("accept_lead_s"));
    // An unknown category is a readable failure (after the duplicate check, so a re-import of a
    // known file reports the duplicate).
    let d2 = tmpdir("rev2").join("other.flac");
    std::fs::copy(dir.join("cold_start.flac"), &d2).unwrap();
    assert!(matches!(
        import_file(&lib, &d2, &opts(Some("nonesuch"))),
        ImportOutcome::Duplicate { .. }
    ));
    let Some(dir2) = fx(&["mono.wav"]) else {
        return;
    };
    let out = import_file(&lib, &dir2.join("mono.wav"), &opts(Some("nonesuch")));
    let ImportOutcome::Failed { error } = out else {
        panic!("{out:?}")
    };
    assert!(error.contains("no such category"), "{error}");
}

/// The inbox service loop: a file dropped in the inbox is imported by the watcher (inotify on
/// Linux, FSEvents on macOS), not only by the 60 s rescan.
#[test]
fn the_inbox_watcher_imports_a_dropped_file() {
    let Some(dir) = fx(&["tone_fade.flac"]) else {
        return;
    };
    let d = tmpdir("watch");
    let paths = Paths::from_data_dir(&d.join("data"));
    let lib = Library::open(paths.clone()).unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (p2, s2) = (paths.clone(), stop.clone());
    let h = std::thread::spawn(move || library::inbox::run(&p2, 1, s2));
    std::thread::sleep(std::time::Duration::from_millis(500));
    let t0 = std::time::Instant::now();
    std::fs::copy(dir.join("tone_fade.flac"), paths.inbox.join("dropped.flac")).unwrap();
    let mut imported_after = None;
    while t0.elapsed() < std::time::Duration::from_secs(40) {
        if count(&lib, "SELECT COUNT(*) FROM items") == 1 {
            imported_after = Some(t0.elapsed());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    h.join().unwrap().unwrap();
    let t = imported_after.expect("the dropped file was not imported within 40 s");
    println!("inbox watcher: file dropped, imported after {t:?} (the 60 s rescan is the fallback)");
    assert!(t < std::time::Duration::from_secs(30), "{t:?}");
    assert!(!paths.inbox.join("dropped.flac").exists());
}

/// A near-duplicate (same title, artist and length within 2 s, different bytes) is imported and
/// flagged for review.
#[test]
fn a_near_duplicate_is_flagged_not_refused() {
    let Some(dir) = fx(&["tone_fade.flac"]) else {
        return;
    };
    let (lib, d) = open("neardup");
    let a = done_id(&import_file(&lib, &dir.join("tone_fade.flac"), &opts(None)));
    // Same audio, different bytes: a tag added to a copy with the same file name in another folder.
    let sub = d.join("other");
    std::fs::create_dir_all(&sub).unwrap();
    let copy = sub.join("tone_fade.flac");
    std::fs::copy(dir.join("tone_fade.flac"), &copy).unwrap();
    {
        use lofty::config::WriteOptions;
        use lofty::prelude::*;
        use lofty::tag::{Tag, TagExt, TagType};
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.set_comment("re-encoded".to_string());
        tag.save_to_path(&copy, WriteOptions::default()).unwrap();
    }
    let out = import_file(&lib, &copy, &opts(None));
    let ImportOutcome::Done {
        item_id,
        possible_duplicate,
        ..
    } = out
    else {
        panic!("{out:?}")
    };
    println!(
        "near-duplicate: item {item_id} flagged possible_duplicate of item {possible_duplicate:?}"
    );
    assert_eq!(possible_duplicate, Some(a));
    assert_eq!(count(&lib, "SELECT COUNT(*) FROM items"), 2);
    let tagged: i64 = lib
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM item_tags it JOIN tags t ON t.id=it.tag_id WHERE t.name='possible_duplicate' AND it.item_id=?1",
            params![item_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tagged, 1);
    let job: String = lib
        .conn()
        .query_row(
            "SELECT error FROM import_jobs WHERE item_id=?1",
            params![item_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(job.contains("possible_duplicate"));
}

/// A format `symphonia` cannot decode (Opus) is transcoded to FLAC by ffmpeg at import; the
/// original's hash is kept for duplicate detection.
#[test]
fn undecodable_formats_are_transcoded_to_flac() {
    let Some(dir) = fx(&["tone_fade.flac"]) else {
        return;
    };
    let d = tmpdir("opus");
    let opus = d.join("song.opus");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(dir.join("tone_fade.flac"))
        .args(["-c:a", "libopus", "-b:a", "96k"])
        .arg(&opus)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("SKIPPED: this ffmpeg cannot encode Opus");
        return;
    }
    let lib = Library::open(Paths::from_data_dir(&d.join("data"))).unwrap();
    let out = import_file(&lib, &opus, &opts(None));
    let id = done_id(&out);
    let (path, source, fmt): (String, String, String) = lib
        .conn()
        .query_row(
            "SELECT store_path, source, format FROM items WHERE id=?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    println!("opus: stored as {path} ({fmt}), source {source}");
    assert!(path.ends_with(".flac"));
    assert!(source.contains("orig_sha="));
    assert_eq!(files_under(&lib.paths.upload_tmp).len(), 0);
    // Importing the same Opus file again is a duplicate by its original hash.
    assert_eq!(
        import_file(&lib, &opus, &opts(None)),
        ImportOutcome::Duplicate { item_id: id }
    );
}

/// Ramps are stored at import, and recomputed inside the cue window when a cue is overridden.
#[test]
fn ramps_are_stored_and_follow_the_cue_window() {
    let Some(dir) = fx(&["quote_unquote.flac"]) else {
        return;
    };
    let (lib, _d) = open("ramps");
    let id = done_id(&import_file(
        &lib,
        &dir.join("quote_unquote.flac"),
        &opts(None),
    ));
    let (s0, e0) = lib.item_ramps(id).unwrap();
    assert!(
        s0.starts_with("-51.00 0.00;") && e0.starts_with("-51.00 "),
        "{s0} / {e0}"
    );
    lib.set_overrides(
        id,
        &OverridePatch {
            cue_in: Some(Some(33_980)),
            ..Default::default()
        },
    )
    .unwrap();
    let (s1, _e1) = lib.item_ramps(id).unwrap();
    println!(
        "quote_unquote start ramp over the file: {} points; inside the cue window: {} points",
        s0.matches(';').count(),
        s1.matches(';').count()
    );
    let start1 = analysis::ramp::parse_ramp(&s1);
    let start0 = analysis::ramp::parse_ramp(&s0);
    // Over the whole file the snoring intro makes the loud band start ~34 s in; inside the window
    // (which starts at the band) it is reached within a second.
    let t0 = analysis::ramp::lookup(&start0, -10.0);
    let t1 = analysis::ramp::lookup(&start1, -10.0);
    println!("-10 dB reached {t0:.2} s into the file, {t1:.2} s into the window");
    assert!(t0 > 30.0 && t1 < 1.0);
}
