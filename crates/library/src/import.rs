//! The import pipeline (architecture 5.4): hash, probe and decode (the decode test and the
//! analysis pass in one), tags, near-duplicate check, atomic write into the store, one
//! transaction for the rows, artwork.
//!
//! A file that fails at any step leaves no file under `media/` and no `items` row; the job row
//! records why.

use crate::art::{self, ArtSource};
use crate::db::Library;
use crate::items::materialize;
use crate::store::{copy_to_tmp, publish, store_rel, tmp_name};
use crate::tags::{album_key, norm, read_tags, title_from_filename, ReadTags};
use crate::util::{now_iso, sha256_file};
use crate::Result;
use analysis::analyze::{analyze_file, AnalysisError, FileAnalysis};
use analysis::census::detect_container;
use analysis::decode::DecodeError;
use analysis::markers::{flags_to_json, ProfileParams};
use rusqlite::{params, OptionalExtension};
use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Category name; `None` means `music`.
    pub category: Option<String>,
    /// Where the request came from: `cli`, `inbox`, `upload`, `plex:<ratingKey>`.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImportOutcome {
    Done {
        item_id: i64,
        /// Non-fatal problems (a corrupt cover, a few undecodable packets).
        warnings: Vec<String>,
        /// The id of an existing item with the same title, artist and length (within 2 s).
        possible_duplicate: Option<i64>,
    },
    Duplicate {
        item_id: i64,
    },
    Failed {
        error: String,
    },
}

/// The extension of the stored file, from its content.
fn stored_ext(head: &[u8]) -> &'static str {
    match detect_container(head) {
        "flac" => "flac",
        "mp3" => "mp3",
        "mp4" => "m4a",
        "wav" => "wav",
        "aiff" => "aif",
        "ogg" => "ogg",
        "adts-aac" => "aac",
        _ => "bin",
    }
}

fn format_name(a: &FileAnalysis, head: &[u8]) -> String {
    let codec = a.info.codec.as_str();
    if codec.starts_with("pcm") {
        match detect_container(head) {
            "aiff" => "aiff".into(),
            _ => "wav".into(),
        }
    } else if codec.is_empty() {
        detect_container(head).into()
    } else {
        codec.to_string()
    }
}

fn is_undecodable(e: &AnalysisError) -> bool {
    matches!(
        e,
        AnalysisError::Decode(
            DecodeError::NotAudio(_) | DecodeError::NoTrack | DecodeError::NoDecoder(_)
        )
    )
}

/// Transcode to FLAC at the source rate with ffmpeg (an import-time dependency only).
fn transcode_flac(src: &Path, out: &Path) -> std::result::Result<(), String> {
    let ffmpeg = std::env::var("HOTCLOCK_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let o = std::process::Command::new(ffmpeg)
        .args(["-v", "error", "-y", "-i"])
        .arg(src)
        .args(["-vn", "-map", "0:a:0", "-c:a", "flac", "-f", "flac"])
        .arg(out)
        .output()
        .map_err(|e| format!("cannot run ffmpeg to convert this format: {e}"))?;
    if o.status.success() {
        Ok(())
    } else {
        Err(format!(
            "ffmpeg could not convert this file: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ))
    }
}

fn find_existing(lib: &Library, sha: &str) -> Result<Option<i64>> {
    Ok(lib
        .conn
        .query_row(
            "SELECT id FROM items WHERE sha256 = ?1 OR source LIKE '%orig_sha=' || ?1 || '%'",
            params![sha],
            |r| r.get(0),
        )
        .optional()?)
}

fn category_of(lib: &Library, name: Option<&str>) -> Result<(i64, String, String)> {
    let name = name.unwrap_or("music");
    lib.conn
        .query_row(
            "SELECT id, segue_profile, kind FROM categories WHERE lower(name) = lower(?1)",
            params![name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| crate::LibraryError::Invalid(format!("no such category: {name}")))
}

fn finish_job(lib: &Library, job: i64, status: &str, item: Option<i64>, err: Option<&str>) {
    let _ = lib.conn.execute(
        "UPDATE import_jobs SET status=?2, item_id=?3, error=?4, finished_at=?5 WHERE id=?1",
        params![job, status, item, err, now_iso()],
    );
}

/// Import one file. Never panics on bad input; every failure is an `ImportOutcome::Failed`
/// with a message an operator can read.
pub fn import_file(lib: &Library, path: &Path, opts: &ImportOptions) -> ImportOutcome {
    let source = if opts.source.is_empty() {
        "cli"
    } else {
        &opts.source
    };
    let job = lib
        .conn
        .execute(
            "INSERT INTO import_jobs(source, status, created_at) VALUES(?1, 'running', ?2)",
            params![format!("{source}:{}", path.display()), now_iso()],
        )
        .map(|_| lib.conn.last_insert_rowid())
        .unwrap_or(0);
    let outcome = match run(lib, path, opts, source) {
        Ok(o) => o,
        Err(e) => ImportOutcome::Failed {
            error: e.to_string(),
        },
    };
    match &outcome {
        ImportOutcome::Done {
            item_id,
            warnings,
            possible_duplicate,
        } => {
            let mut note: Vec<String> = warnings.iter().map(|w| format!("warning: {w}")).collect();
            if let Some(d) = possible_duplicate {
                note.push(format!("possible_duplicate of item {d}"));
            }
            let note = (!note.is_empty()).then(|| note.join("; "));
            finish_job(lib, job, "done", Some(*item_id), note.as_deref());
        }
        ImportOutcome::Duplicate { item_id } => {
            finish_job(lib, job, "duplicate", Some(*item_id), None)
        }
        ImportOutcome::Failed { error } => finish_job(lib, job, "failed", None, Some(error)),
    }
    outcome
}

fn run(lib: &Library, path: &Path, opts: &ImportOptions, source: &str) -> Result<ImportOutcome> {
    let fail = |m: String| Ok(ImportOutcome::Failed { error: m });
    if !path.is_file() {
        return fail(format!("{}: not a file", path.display()));
    }
    // 1. hash
    let orig_sha = sha256_file(path)?;
    if let Some(id) = find_existing(lib, &orig_sha)? {
        return Ok(ImportOutcome::Duplicate { item_id: id });
    }
    let mut head = [0u8; 16];
    let n = std::fs::File::open(path)?.read(&mut head)?;
    let head = &head[..n];

    let (cat_id, profile_name, cat_kind) = match category_of(lib, opts.category.as_deref()) {
        Ok(c) => c,
        Err(e) => return fail(e.to_string()),
    };
    let mut profile = ProfileParams::builtin(&profile_name).unwrap_or_else(ProfileParams::music);
    profile.name = profile_name.clone();

    // 2-3. probe, decode, analyze; transcode formats symphonia cannot decode.
    let mut warnings: Vec<String> = vec![];
    let mut work: PathBuf = path.to_path_buf();
    let mut transcoded_tmp: Option<PathBuf> = None;
    let analysis = match analyze_file(path, &profile) {
        Ok(a) => a,
        Err(e) if is_undecodable(&e) => {
            let tmp = tmp_name(&lib.paths.upload_tmp, "transcode");
            if let Err(m) = transcode_flac(path, &tmp) {
                let _ = std::fs::remove_file(&tmp);
                return fail(format!("{}: {e}; {m}", path.display()));
            }
            work = tmp.clone();
            transcoded_tmp = Some(tmp);
            match analyze_file(&work, &profile) {
                Ok(a) => a,
                Err(e2) => {
                    if let Some(t) = &transcoded_tmp {
                        let _ = std::fs::remove_file(t);
                    }
                    return fail(format!("{}: {e2}", path.display()));
                }
            }
        }
        Err(e) => return fail(format!("{}: {e}", path.display())),
    };
    if analysis.decode_warnings > 0 {
        warnings.push(format!(
            "{} packets failed to decode and were replaced by silence",
            analysis.decode_warnings
        ));
    }

    // The stored bytes: the original, or the transcode.
    let (stored_sha, stored_head) = if transcoded_tmp.is_some() {
        let mut h = [0u8; 16];
        let n = std::fs::File::open(&work)?.read(&mut h)?;
        (sha256_file(&work)?, h[..n].to_vec())
    } else {
        (orig_sha.clone(), head.to_vec())
    };
    if transcoded_tmp.is_some() {
        if let Some(id) = find_existing(lib, &stored_sha)? {
            if let Some(t) = &transcoded_tmp {
                let _ = std::fs::remove_file(t);
            }
            return Ok(ImportOutcome::Duplicate { item_id: id });
        }
    }
    let ext = stored_ext(&stored_head);

    // 4. tags
    let tags: ReadTags = read_tags(&work);
    let strip = cat_kind == "music";
    let title = tags
        .title
        .clone()
        .unwrap_or_else(|| title_from_filename(path, strip));
    let key = album_key(&tags);

    // 5. near-duplicate
    let mut possible_duplicate = None;
    {
        let mut st = lib.conn.prepare(
            "SELECT id, title, artist FROM items WHERE deleted_at IS NULL AND ABS(file_ms - ?1) <= 2000",
        )?;
        let rows = st.query_map(params![analysis.file_ms], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for r in rows.flatten() {
            if norm(&r.1) == norm(&title)
                && norm(&r.2) == norm(tags.artist.as_deref().unwrap_or(""))
            {
                possible_duplicate = Some(r.0);
                break;
            }
        }
    }

    // 6. write into the store (temp, fsync, rename, 0444)
    let rel = store_rel(&stored_sha, ext);
    let dest = lib.paths.media.join(&rel);
    let created_file;
    if let Some(t) = &transcoded_tmp {
        created_file = publish(t, &dest)?;
    } else if dest.exists() {
        created_file = false;
    } else {
        let tmp = tmp_name(&lib.paths.upload_tmp, "copy");
        if let Err(e) = copy_to_tmp(path, &tmp) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        created_file = publish(&tmp, &dest)?;
    }
    let undo_file = |created: bool| {
        if created {
            let _ = std::fs::remove_file(&dest);
        }
    };

    // 8. artwork (never fails the import)
    let mut art_row: Option<art::Normalized> = None;
    let art_src = tags
        .cover
        .clone()
        .map(|bytes| ArtSource {
            bytes,
            origin: "embedded",
        })
        .or_else(|| {
            art::folder_image(path).and_then(|p| {
                std::fs::read(&p).ok().map(|bytes| ArtSource {
                    bytes,
                    origin: "folder",
                })
            })
        });
    if let Some(src) = art_src {
        match art::store_art(&lib.paths.media, &lib.paths.upload_tmp, &src) {
            Ok(n) => art_row = Some(n),
            Err(e) => warnings.push(format!("artwork skipped: {e}")),
        }
    }

    // 6 (cont). the rows, in one transaction
    let source_col = if transcoded_tmp.is_some() {
        format!("{source};orig_sha={orig_sha}")
    } else {
        source.to_string()
    };
    let now = now_iso();
    let orig_filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let file_bytes = std::fs::metadata(&dest).map_or(0, |m| m.len());
    let tx_result: Result<i64> = (|| {
        let tx = lib.conn.unchecked_transaction()?;
        if let Some(a) = &art_row {
            tx.execute(
                "INSERT OR IGNORE INTO artwork(sha256, origin, src_w, src_h, bytes_600, bytes_144) VALUES(?1,?2,?3,?4,?5,?6)",
                params![a.sha, a.origin, a.src_w, a.src_h, a.bytes_600 as i64, a.bytes_144 as i64],
            )?;
        }
        tx.execute(
            "INSERT INTO items(category_id, title, artist, album, year, album_artist, disc_no, track_no, track_total,
                album_key, art_sha, sha256, store_path, orig_filename, format, sample_rate, channels, file_ms,
                file_bytes, source, imported_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?21)",
            params![
                cat_id,
                title,
                tags.artist.clone().unwrap_or_default(),
                tags.album.clone().unwrap_or_default(),
                tags.year,
                tags.album_artist.clone().unwrap_or_default(),
                tags.disc,
                tags.track,
                tags.track_total,
                key,
                art_row.as_ref().map(|a| a.sha.clone()),
                stored_sha,
                rel,
                orig_filename,
                format_name(&analysis, &stored_head),
                analysis.info.sample_rate,
                analysis.info.channels as i64,
                analysis.file_ms,
                file_bytes as i64,
                source_col,
                now
            ],
        )?;
        let item_id = tx.last_insert_rowid();
        let a = &analysis;
        let wave = zstd::encode_all(&a.wave[..], 3).unwrap_or_default();
        tx.execute(
            "INSERT INTO analysis(item_id, algo_version, l_int, lra, true_peak, sample_peak, auto_cue_in, auto_attack,
                auto_segue, auto_cue_out, auto_intro, flags, env_e10, env_e50, env_k200, env_k10, wave,
                ramp_start, ramp_end, analyzed_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,NULL,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
            params![
                item_id,
                analysis::analyze::ALGO_VERSION,
                a.l_int,
                a.lra,
                a.true_peak,
                a.sample_peak,
                a.auto.cue_in,
                a.auto.attack,
                a.auto.segue,
                a.auto.cue_out,
                flags_to_json(&a.auto.flags),
                a.stored.e10,
                a.stored.e50,
                a.stored.k200,
                a.stored.k10,
                wave,
                a.ramp_start,
                a.ramp_end,
                now
            ],
        )?;
        materialize(&tx, item_id)?;
        if possible_duplicate.is_some() {
            tx.execute(
                "INSERT OR IGNORE INTO tags(name) VALUES('possible_duplicate')",
                [],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO item_tags(item_id, tag_id) SELECT ?1, id FROM tags WHERE name='possible_duplicate'",
                params![item_id],
            )?;
        }
        tx.commit()?;
        Ok(item_id)
    })();
    match tx_result {
        Ok(item_id) => Ok(ImportOutcome::Done {
            item_id,
            warnings,
            possible_duplicate,
        }),
        Err(e) => {
            undo_file(created_file);
            Ok(ImportOutcome::Failed {
                error: format!("{}: {e}", path.display()),
            })
        }
    }
}

/// Lower this thread's CPU priority (architecture 5.4: workers are niced to 10 so an import
/// never competes with playout). `ioprio` idle needs a syscall this crate does not make.
pub fn lower_priority() {
    #[cfg(target_os = "linux")]
    {
        let _ = rustix::process::nice(10);
    }
}

/// Import many files with `workers` threads (2 in the service), each with its own connection.
/// Results come back in the order of `jobs`.
pub fn import_many(
    paths: &crate::db::Paths,
    jobs: Vec<(PathBuf, ImportOptions)>,
    workers: usize,
) -> Vec<(PathBuf, ImportOutcome)> {
    let n = jobs.len();
    let queue: Arc<Mutex<VecDeque<(usize, PathBuf, ImportOptions)>>> = Arc::new(Mutex::new(
        jobs.into_iter()
            .enumerate()
            .map(|(i, (p, o))| (i, p, o))
            .collect(),
    ));
    type Slot = Option<(PathBuf, ImportOutcome)>;
    let results: Arc<Mutex<Vec<Slot>>> = Arc::new(Mutex::new((0..n).map(|_| None).collect()));
    let mut handles = vec![];
    for _ in 0..workers.max(1) {
        let queue = Arc::clone(&queue);
        let results = Arc::clone(&results);
        let paths = paths.clone();
        handles.push(std::thread::spawn(move || {
            lower_priority();
            let lib = match Library::open_worker(paths) {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!("import worker cannot open the library: {e}");
                    return;
                }
            };
            loop {
                let next = queue.lock().ok().and_then(|mut q| q.pop_front());
                let Some((i, p, o)) = next else { break };
                let out = import_file(&lib, &p, &o);
                if let Ok(mut r) = results.lock() {
                    r[i] = Some((p, out));
                }
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    let mut res = results
        .lock()
        .map(|mut r| std::mem::take(&mut *r))
        .unwrap_or_default();
    res.drain(..)
        .enumerate()
        .map(|(i, r)| {
            r.unwrap_or_else(|| {
                (
                    PathBuf::new(),
                    ImportOutcome::Failed {
                        error: format!("job {i} did not run"),
                    },
                )
            })
        })
        .collect()
}
