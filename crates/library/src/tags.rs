//! Tags (architecture 5.4 step 4, 3.7 `album_key`) and the fallbacks for untagged files.

use crate::util::collapse_ws;
use lofty::file::TaggedFileExt;
use lofty::picture::PictureType;
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::ItemKey;
use std::io::BufReader;
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct ReadTags {
    pub present: bool,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub year: Option<i64>,
    pub disc: Option<i64>,
    pub track: Option<i64>,
    pub track_total: Option<i64>,
    pub musicbrainz_release: Option<String>,
    /// The front cover picture (or the first picture) embedded in the file.
    pub cover: Option<Vec<u8>>,
}

fn nonempty(s: Option<impl AsRef<str>>) -> Option<String> {
    s.map(|s| collapse_ws(s.as_ref())).filter(|s| !s.is_empty())
}

/// Read tags by content (a reader, so the extension plays no part). A file lofty cannot read
/// simply has none.
pub fn read_tags(path: &Path) -> ReadTags {
    let mut out = ReadTags::default();
    let Ok(file) = std::fs::File::open(path) else {
        return out;
    };
    let Ok(probe) = Probe::new(BufReader::new(file)).guess_file_type() else {
        return out;
    };
    let Ok(tagged) = probe.read() else {
        return out;
    };
    let mut cover_any: Option<Vec<u8>> = None;
    for tag in tagged.tags() {
        for p in tag.pictures() {
            if p.pic_type() == PictureType::CoverFront && out.cover.is_none() {
                out.cover = Some(p.data().to_vec());
            } else if cover_any.is_none() {
                cover_any = Some(p.data().to_vec());
            }
        }
    }
    if out.cover.is_none() {
        out.cover = cover_any;
    }
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        out.present = true;
        out.title = nonempty(tag.title());
        out.artist = nonempty(tag.artist());
        out.album = nonempty(tag.album());
        out.album_artist = nonempty(tag.get_string(ItemKey::AlbumArtist));
        out.year = tag
            .get_string(ItemKey::Year)
            .or_else(|| tag.get_string(ItemKey::RecordingDate))
            .and_then(|y| y.trim().get(..4))
            .and_then(|y| y.parse().ok());
        out.disc = tag.disk().map(i64::from);
        out.track = tag.track().map(i64::from);
        out.track_total = tag.track_total().map(i64::from);
        out.musicbrainz_release = nonempty(tag.get_string(ItemKey::MusicBrainzReleaseId));
    }
    out
}

/// The title of an untagged file: the file name without its extension, underscores turned into
/// spaces, whitespace collapsed; for music a leading track number such as `01 - ` or `03.` is
/// also removed (5.4).
pub fn title_from_filename(path: &Path, strip_track_number: bool) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled");
    let mut t = collapse_ws(&stem.replace('_', " "));
    if strip_track_number {
        // `01 - X`, `03. X`, `01 X`, and the disc-track form `02-08 X`: up to two numbers.
        for _ in 0..2 {
            let digits = t.chars().take_while(char::is_ascii_digit).count();
            if digits == 0 || digits > 3 {
                break;
            }
            let rest = &t[digits..];
            let trimmed = rest.trim_start_matches([' ', '.', '-', ')', '\u{2013}']);
            let sep_len = rest.len() - trimmed.len();
            // A bare single-digit number followed by a space only ("2 Fast 2 Furious") is text.
            let bare_space = sep_len == 1 && rest.starts_with(' ');
            if sep_len == 0 || trimmed.is_empty() || (bare_space && digits < 2) {
                break;
            }
            t = collapse_ws(trimmed);
        }
    }
    if t.is_empty() {
        "untitled".into()
    } else {
        t
    }
}

/// `album_key` (3.7): the MusicBrainz release id when present, else the casefolded,
/// whitespace-collapsed album artist (or artist) and album joined by `|`; empty without an album.
pub fn album_key(t: &ReadTags) -> String {
    if let Some(mb) = &t.musicbrainz_release {
        return mb.to_lowercase();
    }
    let Some(album) = &t.album else {
        return String::new();
    };
    let who = t
        .album_artist
        .as_ref()
        .or(t.artist.as_ref())
        .cloned()
        .unwrap_or_default();
    format!(
        "{}|{}",
        collapse_ws(&who).to_lowercase(),
        collapse_ws(album).to_lowercase()
    )
}

/// Normalized title for the near-duplicate check.
pub fn norm(s: &str) -> String {
    collapse_ws(s)
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_titles() {
        let p = |s: &str| title_from_filename(Path::new(s), false);
        assert_eq!(p("Donate_Liner  02.wav"), "Donate Liner 02");
        let m = |s: &str| title_from_filename(Path::new(s), true);
        assert_eq!(m("01 - Big Entrance.flac"), "Big Entrance");
        assert_eq!(m("03. Cat Walk.mp3"), "Cat Walk");
        assert_eq!(m("02-08 Polka Dot.aac"), "Polka Dot");
        assert_eq!(m("01 Big Entrance.flac"), "Big Entrance");
        assert_eq!(m("1999.flac"), "1999");
        assert_eq!(m("2 Fast 2 Furious.flac"), "2 Fast 2 Furious");
        assert_eq!(m("7.flac"), "7");
    }
}
