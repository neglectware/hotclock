//! A mock Plex Media Server for the `import-plex` tests (library and CLI): the shapes of the real API
//! (`/identity`, paged `/playlists` and `/playlists/{key}/items` in JSON with `Media.Part`, the part
//! download plain and chunked). It checks the token on every route but `/identity` and records every request.
#![allow(dead_code)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Method, target and the token header of every request.
pub type RequestLog = Arc<Mutex<Vec<(String, String, Option<String>)>>>;

pub const TOKEN: &str = "mock-token-AbC123xyz";

pub fn tmpdir(name: &str) -> PathBuf {
    let d = fixtures::testdir::process_dir().join(format!("plex-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[derive(Clone)]
pub struct MockTrack {
    pub key: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub file_name: String,
    pub bytes: Vec<u8>,
    /// The album's picture path, if Plex lists one (albums 0 and 1 serve it, 2 serves something that is not a picture, 3 lists it but answers 404, 4 has none).
    pub thumb: Option<String>,
}

pub struct Mock {
    pub port: u16,
    pub log: RequestLog, // method, target, token header
    pub tracks: Arc<Mutex<Vec<MockTrack>>>,
    pub broken: Arc<Mutex<Vec<String>>>, // rating keys whose download answers 404
    pub stop: Arc<AtomicBool>,
}

pub fn sine_wav(freq: f64, secs: f64) -> Vec<u8> {
    let rate = 44_100u32;
    let n = (rate as f64 * secs) as usize;
    let mut data = Vec::with_capacity(n * 4);
    for i in 0..n {
        let env = (i as f64 / 2000.0).min(1.0) * ((n - i) as f64 / 2000.0).min(1.0);
        let v = (0.2
            * env
            * (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin()
            * 32767.0) as i16;
        data.extend_from_slice(&v.to_le_bytes());
        data.extend_from_slice(&v.to_le_bytes());
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

pub fn track(i: usize) -> MockTrack {
    MockTrack {
        key: format!("{}", 9000 + i),
        title: format!("Plex Title {i:02}"),
        artist: format!("Artist {}", i % 7),
        album: format!("Album {}", i % 5),
        file_name: format!("{:02} - raw_file_name_{i}.wav", i + 1),
        bytes: sine_wav(200.0 + 17.0 * i as f64, 1.5),
        thumb: (i % 5 != 4).then(|| format!("/library/metadata/{}/thumb/1700000000", 8000 + i % 5)),
    }
}

fn media_json(t: &MockTrack) -> Value {
    let id: i64 = t.key.parse().unwrap();
    let mut j = json!({
        "ratingKey": t.key, "type": "track", "title": t.title,
        "grandparentTitle": t.artist, "parentTitle": t.album, "index": 1, "parentIndex": 1,
        "parentYear": 1999, "duration": 1500,
        "Media": [{ "container": "wav", "Part": [{
            "id": id, "key": format!("/library/parts/{id}/1700000000/file.wav"),
            "file": format!("/music/{}", t.file_name), "size": t.bytes.len(), "container": "wav" }] }]
    });
    if let Some(th) = &t.thumb {
        j["parentThumb"] = json!(th);
    }
    j
}

/// A small PNG whose colour depends on the album, so two albums are two pictures.
pub fn album_png(album: i64) -> Vec<u8> {
    let img = image::RgbImage::from_fn(64, 64, |x, y| {
        image::Rgb([(album * 50 % 256) as u8, (x * 4) as u8, (y * 4) as u8])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn respond(s: &mut TcpStream, status: &str, ctype: &str, body: &[u8], chunked: bool) {
    let mut h = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nConnection: close\r\n");
    if chunked {
        h.push_str("Transfer-Encoding: chunked\r\n\r\n");
        s.write_all(h.as_bytes()).unwrap();
        for c in body.chunks(7000) {
            write!(s, "{:x}\r\n", c.len()).unwrap();
            s.write_all(c).unwrap();
            s.write_all(b"\r\n").unwrap();
        }
        s.write_all(b"0\r\n\r\n").unwrap();
    } else {
        h.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
        let _ = s.write_all(h.as_bytes());
        let _ = s.write_all(body);
    }
}

impl Mock {
    pub fn start(tracks: Vec<MockTrack>) -> Mock {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        l.set_nonblocking(true).unwrap();
        let m = Mock {
            port,
            log: Arc::default(),
            tracks: Arc::new(Mutex::new(tracks)),
            broken: Arc::default(),
            stop: Arc::new(AtomicBool::new(false)),
        };
        let (log, tr, br, stop) = (
            m.log.clone(),
            m.tracks.clone(),
            m.broken.clone(),
            m.stop.clone(),
        );
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match l.accept() {
                    Ok((s, _)) => {
                        let (log, tr, br) = (log.clone(), tr.clone(), br.clone());
                        std::thread::spawn(move || serve(s, log, tr, br));
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
                }
            }
        });
        m
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn thumb_requests(&self) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, t, _)| t.contains("/thumb/"))
            .count()
    }

    pub fn downloads(&self) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, t, _)| t.contains("/library/parts/"))
            .count()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn serve(
    mut s: TcpStream,
    log: RequestLog,
    tracks: Arc<Mutex<Vec<MockTrack>>>,
    broken: Arc<Mutex<Vec<String>>>,
) {
    s.set_nonblocking(false).unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let mut it = line.split_whitespace();
    let (method, target) = (
        it.next().unwrap_or("").to_string(),
        it.next().unwrap_or("").to_string(),
    );
    let (mut token, mut start, mut size) = (None, 0usize, 200usize);
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap() == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "x-plex-token" => token = Some(v.trim().to_string()),
                "x-plex-container-start" => start = v.trim().parse().unwrap_or(0),
                "x-plex-container-size" => size = v.trim().parse().unwrap_or(200),
                _ => {}
            }
        }
    }
    log.lock()
        .unwrap()
        .push((method.clone(), target.clone(), token.clone()));
    let path = target.split('?').next().unwrap_or("");
    if path == "/identity" {
        let body = json!({"MediaContainer": {"size": 0, "apiVersion": "1.1.1", "claimed": true,
            "machineIdentifier": "mock", "version": "1.42.2.10156-f737b826c"}});
        return respond(
            &mut s,
            "200 OK",
            "application/json",
            body.to_string().as_bytes(),
            false,
        );
    }
    if token.as_deref() != Some(TOKEN) {
        return respond(&mut s, "401 Unauthorized", "text/plain", b"nope", false);
    }
    let tracks = tracks.lock().unwrap().clone();
    if path == "/playlists" {
        let all = vec![
            json!({"ratingKey": "700", "title": "Office Mix", "playlistType": "audio", "smart": false,
                   "leafCount": tracks.len(), "duration": 1500 * tracks.len()}),
            json!({"ratingKey": "701", "title": "Smart Thing", "playlistType": "audio", "smart": true,
                   "leafCount": 3, "duration": 4500}),
        ];
        let body =
            json!({"MediaContainer": {"size": all.len(), "totalSize": all.len(), "Metadata": all}});
        return respond(
            &mut s,
            "200 OK",
            "application/json",
            body.to_string().as_bytes(),
            false,
        );
    }
    if path == "/playlists/700/items" {
        // The mock never returns more than 20 at a time, so the importer's paging is exercised.
        let page: Vec<Value> = tracks
            .iter()
            .skip(start)
            .take(size.min(20))
            .map(media_json)
            .collect();
        let body = json!({"MediaContainer": {"size": page.len(), "totalSize": tracks.len(), "Metadata": page}});
        return respond(
            &mut s,
            "200 OK",
            "application/json",
            body.to_string().as_bytes(),
            false,
        );
    }
    if let Some(rest) = path.strip_prefix("/library/metadata/") {
        let mut it = rest.split('/');
        let id = it.next().unwrap_or("");
        if it.next() == Some("thumb") {
            let n: i64 = id.parse().unwrap_or(0);
            if n == 8003 || !(8000..8005).contains(&n) {
                return respond(&mut s, "404 Not Found", "text/plain", b"no picture", false);
            }
            if n == 8002 {
                return respond(
                    &mut s,
                    "200 OK",
                    "image/png",
                    b"this is not a picture",
                    false,
                );
            }
            if n == 8001 {
                // Plex serves some pictures as WebP.
                let img = image::RgbImage::from_fn(64, 64, |x, y| {
                    image::Rgb([200, (x * 4) as u8, (y * 4) as u8])
                });
                let mut out = std::io::Cursor::new(Vec::new());
                img.write_to(&mut out, image::ImageFormat::WebP).unwrap();
                return respond(&mut s, "200 OK", "image/webp", &out.into_inner(), false);
            }
            return respond(&mut s, "200 OK", "image/png", &album_png(n - 8000), false);
        }
        if let Some(t) = tracks.iter().find(|t| t.key == id) {
            let body = json!({"MediaContainer": {"size": 1, "Metadata": [media_json(t)]}});
            return respond(
                &mut s,
                "200 OK",
                "application/json",
                body.to_string().as_bytes(),
                false,
            );
        }
    }
    if let Some(rest) = path.strip_prefix("/library/parts/") {
        let id = rest.split('/').next().unwrap_or("");
        let key = id.to_string();
        if broken.lock().unwrap().contains(&key) {
            return respond(&mut s, "404 Not Found", "text/plain", b"gone", false);
        }
        if let Some(t) = tracks.iter().find(|t| t.key == key) {
            let n: usize = key.parse().unwrap();
            return respond(&mut s, "200 OK", "audio/wav", &t.bytes, n.is_multiple_of(2));
        }
    }
    respond(&mut s, "404 Not Found", "text/plain", b"no", false);
}
