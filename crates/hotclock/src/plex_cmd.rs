//! `hotclock import-plex` (M2, architecture 5.4): a one-time, read-only copy of Plex playlists into the library.

use clap::Args;
use library::plex::{import_playlist, read_token_file, PlexClient, PlexImportOptions};
use library::{Library, Paths};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Args)]
pub struct ImportPlexArgs {
    /// The Plex Media Server, `http://host:32400`.
    #[arg(long, default_value = "http://192.0.2.3:32400")]
    server: String,
    /// A file holding the Plex token alone on its first line (never given on the command line, never printed).
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// The playlist to import; repeat for several. Each becomes a static playlist of the same name.
    #[arg(long)]
    playlist: Vec<String>,
    /// List the server's audio playlists and exit.
    #[arg(long)]
    list: bool,
    /// Category for the imported items (a category name; default `music`).
    #[arg(long)]
    category: Option<String>,
    /// The new library playlist's mode: `sequential` keeps Plex's order, `shuffle` makes a rotation.
    #[arg(long, default_value = "sequential")]
    mode: String,
    /// Name the library playlist differently (only with one `--playlist`).
    #[arg(long = "as")]
    as_name: Option<String>,
    /// Fill the album picture (and blank title, artist, album and year) from Plex for items already imported that
    /// have none; never replaces a picture. Scope: `--playlist NAME` or `--all-plex-items`.
    #[arg(long)]
    fill_art: bool,
    /// When importing: do not fetch album pictures from Plex.
    #[arg(long)]
    no_art: bool,
    /// With `--fill-art`: every item imported from Plex.
    #[arg(long)]
    all_plex_items: bool,
    /// Say what would be downloaded and added; change nothing.
    #[arg(long)]
    dry_run: bool,
    /// Override `data_dir` of the global config.
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

pub fn run(a: ImportPlexArgs, global: &Path) -> ExitCode {
    match run_inner(a, global) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("import-plex: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(a: ImportPlexArgs, global: &Path) -> Result<ExitCode, String> {
    let token = match &a.token_file {
        Some(p) => read_token_file(p).map_err(|e| e.to_string())?,
        None => {
            return Err(
                "--token-file is required (a file with the Plex token on its first line)".into(),
            )
        }
    };
    let plex = PlexClient::new(&a.server, token).map_err(|e| e.to_string())?;
    let id = plex.identity().map_err(|e| e.to_string())?;
    println!(
        "Plex Media Server {} (API {}) answered",
        id["version"].as_str().unwrap_or("?"),
        id["apiVersion"].as_str().unwrap_or("?")
    );
    if a.fill_art {
        let paths = match &a.data_dir {
            Some(d) => Paths::from_data_dir(d),
            None => Paths::from_config(
                &common::config::GlobalConfig::load_or_default(global)
                    .map_err(|e| e.to_string())?,
            ),
        };
        let lib =
            Library::open_worker(paths).map_err(|e| format!("cannot open the library: {e}"))?;
        let scopes: Vec<library::plex::FillScope> = if a.all_plex_items {
            vec![library::plex::FillScope::AllPlexItems]
        } else if a.playlist.is_empty() {
            return Err("--fill-art needs --playlist NAME or --all-plex-items".into());
        } else {
            a.playlist
                .iter()
                .cloned()
                .map(library::plex::FillScope::Playlist)
                .collect()
        };
        let sink = library::plex::Sink(Some(std::sync::Arc::new(|l: &str| println!("{l}"))));
        let mut failed = 0;
        for scope in scopes {
            let st = library::plex::fill_art(&lib, &plex, &scope, a.dry_run, &sink)
                .map_err(|e| e.to_string())?;
            println!(
                "{}checked {}, had art {}, {} {}, Plex has none {}, failed {}; blank title, artist, album or year {} {}{}",
                if a.dry_run { "(dry run) " } else { "" },
                st.checked,
                st.had_art,
                if a.dry_run { "Plex can fill" } else { "filled from Plex" },
                if a.dry_run { st.would_fill } else { st.filled },
                st.plex_none,
                st.failed,
                if a.dry_run { "to fill" } else { "filled" },
                st.meta_filled,
                if st.not_in_library > 0 { format!("; {} playlist tracks are not in the library", st.not_in_library) } else { String::new() },
            );
            failed += st.failed;
        }
        return Ok(if failed > 0 {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        });
    }
    if a.list || a.playlist.is_empty() {
        for p in plex.playlists().map_err(|e| e.to_string())? {
            println!(
                "{:>6}  {}  ({} tracks, {:.0} min{})",
                p.rating_key,
                p.title,
                p.leaf_count,
                p.duration_ms as f64 / 60000.0,
                if p.smart {
                    ", smart: imported as the tracks it holds now"
                } else {
                    ""
                }
            );
        }
        if !a.list {
            println!("name one or more with --playlist \"<name>\" to import it");
        }
        return Ok(ExitCode::SUCCESS);
    }
    if a.as_name.is_some() && a.playlist.len() != 1 {
        return Err("--as needs exactly one --playlist".into());
    }
    let mode = match a.mode.as_str() {
        "sequential" => library::playlists::Mode::Sequential,
        "shuffle" => library::playlists::Mode::Shuffle,
        o => return Err(format!("--mode must be sequential or shuffle, got `{o}`")),
    };
    let paths = match &a.data_dir {
        Some(d) => Paths::from_data_dir(d),
        None => Paths::from_config(
            &common::config::GlobalConfig::load_or_default(global).map_err(|e| e.to_string())?,
        ),
    };
    let lib = Library::open_worker(paths).map_err(|e| format!("cannot open the library: {e}"))?;
    let mut failed = 0usize;
    for name in &a.playlist {
        let rep = import_playlist(
            &lib,
            &plex,
            name,
            &PlexImportOptions {
                category: a.category.clone(),
                mode,
                as_name: a.as_name.clone(),
                dry_run: a.dry_run,
                fill_art: !a.no_art,
                ..Default::default()
            },
        )
        .map_err(|e| e.to_string())?;
        for l in &rep.lines {
            println!("{l}");
        }
        for s in &rep.skipped {
            println!("skipped   {s}");
        }
        println!(
            "playlist `{}`{}: {} tracks in Plex; {} downloaded and imported, {} already in the library, {} failed; {} added to the playlist, {} already in it{}{}",
            rep.playlist,
            match (a.dry_run, rep.playlist_created) {
                (true, _) => " (dry run)".to_string(),
                (_, true) => " (created)".to_string(),
                _ => String::new(),
            },
            rep.tracks,
            rep.imported,
            rep.known,
            rep.failed.len(),
            rep.entries_added,
            rep.entries_present,
            if rep.metadata_filled > 0 { format!("; {} titles or artists filled from Plex", rep.metadata_filled) } else { String::new() },
            if rep.skipped.is_empty() { String::new() } else { format!("; {} skipped", rep.skipped.len()) },
        );
        println!(
            "pictures: checked {}, had {}, {} {}, Plex has none {}, failed {}",
            rep.art.checked,
            rep.art.had_art,
            if a.dry_run {
                "Plex can fill"
            } else {
                "filled from Plex"
            },
            if a.dry_run {
                rep.art.would_fill
            } else {
                rep.art.filled
            },
            rep.art.plex_none,
            rep.art.failed
        );
        failed += rep.failed.len();
    }
    Ok(if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}
