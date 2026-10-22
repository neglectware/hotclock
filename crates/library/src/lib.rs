//! The library service's core (milestone M1.3): `library.db`, the content-addressed media
//! store, the import pipeline, the inbox watcher and the materialized effective markers.
//! The HTTP API is M1.7; everything here is a library the CLI and the service call.

#![forbid(unsafe_code)]

pub mod art;
pub mod backup;
pub mod db;
pub mod examples;
pub mod health;
pub mod import;
pub mod inbox;
pub mod items;
pub mod playlists;
pub mod plex;
pub mod presets;
pub mod store;
pub mod tags;
pub mod tracks;
pub mod util;

pub use db::{Library, Paths};
pub use import::{import_file, ImportOptions, ImportOutcome};

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("marker `{name}` = {value} ms lies outside the cue window [{c_in}, {c_out}] ms")]
    MarkerOutsideWindow {
        name: &'static str,
        value: i64,
        c_in: i64,
        c_out: i64,
    },
    #[error("no such item: {0}")]
    NoItem(i64),
}

pub type Result<T> = std::result::Result<T, LibraryError>;
