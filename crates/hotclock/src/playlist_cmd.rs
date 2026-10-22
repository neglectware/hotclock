//! `hotclock playlist`: create and inspect playlists in the library from the command line (M1.5).
//! The editor and the API are M1.7; this is enough to set up the office rotation.

use clap::{Args, Subcommand};
use common::config::GlobalConfig;
use library::playlists::{Mode, NewBlock, NewPlaylist};
use library::{Library, Paths};
use std::path::Path;
use std::process::ExitCode;

#[derive(Args)]
pub struct PlaylistArgs {
    #[command(subcommand)]
    cmd: PlaylistCmd,
}

#[derive(Subcommand)]
enum PlaylistCmd {
    /// List playlists with their entries.
    List,
    /// Create a playlist.
    Create {
        name: String,
        /// `shuffle` (a rotation, freshly shuffled every pass) or `sequential`.
        #[arg(long, default_value = "shuffle")]
        mode: String,
        /// No track repeats within this many minutes where the playlist can keep it (4.3).
        #[arg(long, default_value_t = 120)]
        no_repeat_minutes: i64,
    },
    /// Append items (by id) to a playlist.
    Add { name: String, items: Vec<i64> },
    /// Append a standing `insert_every` block: an item of a category every N songs (4.5).
    InsertEvery {
        name: String,
        #[arg(long)]
        n: i64,
        /// The category the inserts come from (for example `liner`).
        #[arg(long)]
        category: String,
        #[arg(long, default_value_t = 0)]
        min_gap_minutes: i64,
    },
}

pub fn run(a: PlaylistArgs, global_path: &Path) -> ExitCode {
    let global = match GlobalConfig::load_or_default(global_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let lib = match Library::open_worker(Paths::from_config(&global)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot open the library: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = (|| -> Result<(), String> {
        match a.cmd {
            PlaylistCmd::List => {
                let rows = lib.playlists().map_err(|e| e.to_string())?;
                // The window each rotation actually keeps (4.3, cut to fit the playlist).
                let mut cat = station::catalog::Catalog::new(&lib.paths.media);
                cat.apply(library::tracks::read_all_tracks(lib.conn()).map_err(|e| e.to_string())?);
                let prog =
                    std::sync::Arc::new(station::programming::Programming::from_rows(&rows, &[]));
                let sched = station::sched::Scheduler::new(
                    station::sched::SchedConfig::default(),
                    prog.clone(),
                    0,
                );
                for p in rows {
                    println!(
                        "{:>3}  {}  ({}, no repeat {} min, {} entries)",
                        p.id,
                        p.name,
                        p.mode.name(),
                        p.no_repeat_minutes,
                        p.entries.len()
                    );
                    if let Some(pl) = prog
                        .playlists
                        .get(&p.id)
                        .filter(|_| p.mode == Mode::Shuffle)
                    {
                        let i = sched.rotation_info(pl, &cat);
                        println!(
                            "       rotation: {} units of {:.1} min on average; no_repeat_configured {} min, no_repeat_effective {:.0} min ({})",
                            i.units,
                            i.mean_minutes,
                            i.no_repeat_configured,
                            i.no_repeat_effective,
                            i.no_repeat_reason.as_deref().unwrap_or("as set")
                        );
                    }
                    for e in &p.entries {
                        use library::playlists::EntryKind::*;
                        match &e.kind {
                            Item { item_id } => println!("       item {item_id}"),
                            Album {
                                album_key,
                                continuity,
                            } => {
                                println!("       album {album_key} ({continuity})")
                            }
                            Block(b) => println!("       block {} {} {}", b.kind, b.name, b.params),
                        }
                    }
                }
            }
            PlaylistCmd::Create {
                name,
                mode,
                no_repeat_minutes,
            } => {
                let mut p = NewPlaylist::shuffle(&name);
                p.mode = match mode.as_str() {
                    "shuffle" => Mode::Shuffle,
                    "sequential" => Mode::Sequential,
                    o => return Err(format!("--mode must be shuffle or sequential, got `{o}`")),
                };
                p.no_repeat_minutes = no_repeat_minutes;
                let id = lib.create_playlist(&p).map_err(|e| e.to_string())?;
                println!("created playlist {id} `{name}`");
            }
            PlaylistCmd::Add { name, items } => {
                let id = lib
                    .playlist_id(&name)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("no playlist `{name}`"))?;
                for i in &items {
                    lib.add_item_to_playlist(id, *i, 1.0)
                        .map_err(|e| e.to_string())?;
                }
                println!("added {} items to `{name}`", items.len());
            }
            PlaylistCmd::InsertEvery {
                name,
                n,
                category,
                min_gap_minutes,
            } => {
                let id = lib
                    .playlist_id(&name)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("no playlist `{name}`"))?;
                if n < 1 {
                    return Err(format!("--n must be at least 1, got {n}"));
                }
                if min_gap_minutes < 0 {
                    return Err(format!(
                        "--min-gap-minutes cannot be negative, got {min_gap_minutes}"
                    ));
                }
                let cid = lib
                    .category_id(&category)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("no category `{category}`"))?;
                let mut b = NewBlock::insert_every(
                    &format!("{category} every {n}"),
                    n,
                    serde_json::json!({ "category_id": cid }),
                );
                b.params["min_gap_minutes"] = serde_json::json!(min_gap_minutes);
                lib.add_block_to_playlist(id, &b)
                    .map_err(|e| e.to_string())?;
                println!("`{name}`: an item of `{category}` after every {n} songs");
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
