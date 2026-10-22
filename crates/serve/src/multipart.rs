//! A streaming `multipart/form-data` reader: each part's body goes to a writer as it arrives, so a 1 GB
//! upload never sits in memory.

use std::io::{Read, Write};

pub struct Part {
    pub name: String,
    pub filename: Option<String>,
}

pub struct Multipart<R: Read> {
    r: R,
    /// `\r\n--boundary`
    marker: Vec<u8>,
    buf: Vec<u8>,
    eof: bool,
    started: bool,
    finished: bool,
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The boundary parameter of a `Content-Type` value.
pub fn boundary_of(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|p| {
        let p = p.trim();
        p.strip_prefix("boundary=")
            .map(|b| b.trim_matches('"').to_string())
    })
}

impl<R: Read> Multipart<R> {
    pub fn new(r: R, boundary: &str) -> Self {
        Multipart {
            r,
            marker: format!("\r\n--{boundary}").into_bytes(),
            // The first boundary has no CRLF before it; supply one so every delimiter looks the same.
            buf: b"\r\n".to_vec(),
            eof: false,
            started: false,
            finished: false,
        }
    }

    fn fill(&mut self) -> std::io::Result<()> {
        if self.eof {
            return Ok(());
        }
        let mut tmp = [0u8; 64 * 1024];
        let n = self.r.read(&mut tmp)?;
        if n == 0 {
            self.eof = true;
        } else {
            self.buf.extend_from_slice(&tmp[..n]);
        }
        Ok(())
    }

    /// The next part's headers, or `None` after the closing delimiter. Unread body of the previous part is
    /// skipped.
    pub fn next_part(&mut self) -> std::io::Result<Option<Part>> {
        if self.finished {
            return Ok(None);
        }
        if self.started {
            self.body_to(&mut std::io::sink(), u64::MAX)?;
        }
        self.started = true;
        // Find the delimiter (it may be preceded by a preamble).
        loop {
            if let Some(i) = find(&self.buf, &self.marker) {
                self.buf.drain(..i + self.marker.len());
                break;
            }
            let keep = self.marker.len();
            if self.buf.len() > keep {
                let cut = self.buf.len() - keep;
                self.buf.drain(..cut);
            }
            if self.eof {
                self.finished = true;
                return Ok(None);
            }
            self.fill()?;
        }
        // `--` closes the body; CRLF starts a part.
        while self.buf.len() < 2 && !self.eof {
            self.fill()?;
        }
        if self.buf.starts_with(b"--") {
            self.finished = true;
            return Ok(None);
        }
        // Skip to the end of the delimiter line, then read headers up to a blank line.
        loop {
            if let Some(i) = find(&self.buf, b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&self.buf[..i]).into_owned();
                self.buf.drain(..i + 4);
                return Ok(Some(parse_headers(&head)));
            }
            if self.eof || self.buf.len() > 64 * 1024 {
                self.finished = true;
                return Ok(None);
            }
            self.fill()?;
        }
    }

    /// Write the current part's body to `w` (at most `limit` bytes; `Err` past it). Returns the length.
    ///
    /// The body ends at the next real delimiter line: the marker followed by `--`, or by optional white
    /// space and a line end. The same bytes inside the data (a marker followed by anything else) are data.
    pub fn body_to(&mut self, w: &mut dyn Write, limit: u64) -> std::io::Result<u64> {
        let mut total = 0u64;
        let mut from = 0usize; // where to look for the marker: past one that turned out to be data
        loop {
            if let Some(off) = find(&self.buf[from..], &self.marker) {
                let i = from + off;
                match delimiter_after(&self.buf[i + self.marker.len()..], self.eof) {
                    Some(true) => {
                        total += i as u64;
                        if total > limit {
                            return Err(std::io::Error::other("the part is too large"));
                        }
                        w.write_all(&self.buf[..i])?;
                        self.buf.drain(..i);
                        return Ok(total);
                    }
                    Some(false) => {
                        from = i + 1;
                        continue;
                    }
                    None => {
                        // Not enough bytes after the marker to tell: read more.
                        self.fill()?;
                        continue;
                    }
                }
            }
            // No marker: everything except a possible start of one at the end is body.
            let safe = self.buf.len().saturating_sub(self.marker.len() + 2);
            if safe > 0 {
                total += safe as u64;
                if total > limit {
                    return Err(std::io::Error::other("the part is too large"));
                }
                w.write_all(&self.buf[..safe])?;
                self.buf.drain(..safe);
                from = from.saturating_sub(safe);
            }
            if self.eof {
                return Err(std::io::Error::other("the upload ended inside a part"));
            }
            self.fill()?;
        }
    }

    /// A small text field.
    pub fn text(&mut self) -> std::io::Result<String> {
        let mut v = vec![];
        self.body_to(&mut v, 64 * 1024)?;
        Ok(String::from_utf8_lossy(&v).into_owned())
    }
}

/// Is what follows a marker a delimiter line (`--`, or white space and a line end)? `None`: cannot tell yet.
fn delimiter_after(tail: &[u8], eof: bool) -> Option<bool> {
    let ws = tail
        .iter()
        .take_while(|b| **b == b' ' || **b == b'\t')
        .count();
    let rest = &tail[ws..];
    if rest.starts_with(b"--") || rest.starts_with(b"\r\n") {
        return Some(true);
    }
    if rest.len() < 2 && !eof {
        return None;
    }
    Some(false)
}

fn parse_headers(head: &str) -> Part {
    let mut name = String::new();
    let mut filename = None;
    for line in head.lines() {
        let l = line.to_ascii_lowercase();
        if l.starts_with("content-disposition:") {
            for piece in line.split(';').skip(1) {
                let piece = piece.trim();
                if let Some(v) = piece.strip_prefix("name=") {
                    name = v.trim_matches('"').to_string();
                } else if let Some(v) = piece.strip_prefix("filename=") {
                    filename = Some(v.trim_matches('"').to_string());
                }
            }
        }
    }
    Part { name, filename }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(parts: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
        let mut b = vec![];
        for (n, f, d) in parts {
            b.extend_from_slice(b"--XyZ\r\n");
            match f {
                Some(f) => b.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{n}\"; filename=\"{f}\"\r\nContent-Type: audio/flac\r\n\r\n")
                        .as_bytes(),
                ),
                None => b.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{n}\"\r\n\r\n").as_bytes(),
                ),
            }
            b.extend_from_slice(d);
            b.extend_from_slice(b"\r\n");
        }
        b.extend_from_slice(b"--XyZ--\r\n");
        b
    }

    #[test]
    fn parts_come_out_intact_even_with_the_boundary_inside_the_data_almost() {
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
        let tricky = b"abc\r\n--Xy\r\n--XyZ-almost".to_vec();
        let raw = body(&[
            ("category", None, b"liner"),
            ("file", Some("a.flac"), &big),
            ("file", Some("b.flac"), &tricky),
            ("empty", Some("c.flac"), b""),
        ]);
        // A reader that hands out tiny pieces stresses the boundary search across reads.
        struct Slow<'a>(&'a [u8]);
        impl Read for Slow<'_> {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                let n = self.0.len().min(b.len()).min(1000);
                b[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let mut m = Multipart::new(Slow(&raw), "XyZ");
        let p = m.next_part().unwrap().unwrap();
        assert_eq!((p.name.as_str(), p.filename.as_deref()), ("category", None));
        assert_eq!(m.text().unwrap(), "liner");
        let p = m.next_part().unwrap().unwrap();
        assert_eq!(p.filename.as_deref(), Some("a.flac"));
        let mut got = vec![];
        m.body_to(&mut got, 1 << 30).unwrap();
        assert_eq!(got, big);
        let p = m.next_part().unwrap().unwrap();
        assert_eq!(p.filename.as_deref(), Some("b.flac"));
        let mut got = vec![];
        m.body_to(&mut got, 1 << 30).unwrap();
        assert_eq!(got, tricky);
        let _ = m.next_part().unwrap().unwrap();
        let mut got = vec![];
        m.body_to(&mut got, 10).unwrap();
        assert!(got.is_empty());
        assert!(m.next_part().unwrap().is_none());
    }

    #[test]
    fn the_size_limit_stops_a_part() {
        let raw = body(&[("file", Some("a"), &[7u8; 5000])]);
        let mut m = Multipart::new(&raw[..], "XyZ");
        m.next_part().unwrap().unwrap();
        assert!(m.body_to(&mut std::io::sink(), 1000).is_err());
    }

    #[test]
    fn boundary_parameter() {
        assert_eq!(
            boundary_of("multipart/form-data; boundary=abc123").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            boundary_of("multipart/form-data; boundary=\"q q\"").as_deref(),
            Some("q q")
        );
        assert_eq!(boundary_of("application/json"), None);
    }
}
