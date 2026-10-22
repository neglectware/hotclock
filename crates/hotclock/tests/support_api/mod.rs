//! A running station and library service for the M1.7 tests: real processes, real HTTP.
#![allow(dead_code)]

use engine::render::{render_tone, ToneRender};
use engine::wav::WavFormat;
use library::playlists::{Mode, NewPlaylist};
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use serde_json::Value;
use serve::http::client::{self, Reply};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

pub fn tmpdir(name: &str) -> PathBuf {
    let d = fixtures::testdir::process_dir().join(format!("m17-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub struct Env {
    pub dir: PathBuf,
    pub global: PathBuf,
    pub data: PathBuf,
    pub st_port: u16,
    pub lib_port: u16,
    pub lib: Library,
    pub children: Vec<Child>,
    pub items: Vec<i64>,
}

pub const JSON: (&str, &str) = ("Content-Type", "application/json");

impl Drop for Env {
    fn drop(&mut self) {
        for c in &mut self.children {
            let _ = c.kill();
            let _ = c.wait();
        }
        // The tones and the recorded output are large: leave nothing behind unless asked.
        if std::env::var_os("HOTCLOCK_KEEP_TEST_DIRS").is_none() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

pub struct Opts {
    pub tones: usize,
    pub secs: f64,
    pub device: String,
    pub target_lufs: Option<f64>,
    /// RMS level of the tones, dBFS.
    pub level: f64,
    pub extra_station: String,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            tones: 4,
            secs: 30.0,
            device: "null".into(),
            target_lufs: None,
            level: -20.0,
            extra_station: String::new(),
        }
    }
}

pub fn make_tone(dir: &Path, name: &str, hz: f64, secs: f64, level: f64) -> PathBuf {
    let p = dir.join(format!("{name}.wav"));
    render_tone(&ToneRender {
        out: p.clone(),
        rate: 48_000,
        seconds: secs,
        hz,
        level_dbfs: level,
        format: WavFormat::F32,
    })
    .unwrap();
    p
}

impl Env {
    pub fn new(name: &str, o: &Opts) -> Env {
        let dir = tmpdir(name);
        let data = dir.join("data");
        let lib = Library::open(Paths::from_data_dir(&data)).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let mut items = vec![];
        for i in 0..o.tones {
            let p = make_tone(
                &dir.join("src"),
                &format!("t{i}"),
                300.0 + 100.0 * i as f64,
                o.secs,
                o.level,
            );
            match import_file(
                &lib,
                &p,
                &ImportOptions {
                    category: Some("music".into()),
                    source: "test".into(),
                },
            ) {
                ImportOutcome::Done { item_id, .. } => items.push(item_id),
                x => panic!("{x:?}"),
            }
        }
        let mut pl = NewPlaylist::shuffle("Rot");
        pl.mode = Mode::Sequential;
        pl.no_repeat_minutes = 0;
        let pid = lib.create_playlist(&pl).unwrap();
        for i in &items {
            lib.add_item_to_playlist(pid, *i, 1.0).unwrap();
        }
        let (st_port, lib_port) = (free_port(), free_port());
        let stations = dir.join("stations");
        std::fs::create_dir_all(&stations).unwrap();
        let mut st = format!(
            "name = \"office\"\nlisten = \"127.0.0.1:{st_port}\"\ndevice = \"{}\"\ndefault_playlist = \"Rot\"\n",
            o.device
        );
        if let Some(t) = o.target_lufs {
            st.push_str(&format!("target_lufs = {t}\n"));
        }
        st.push_str(&o.extra_station);
        std::fs::write(stations.join("office.toml"), st).unwrap();
        let global = dir.join("hotclock.toml");
        std::fs::write(
            &global,
            format!(
                "data_dir = \"{}\"\nstations_dir = \"{}\"\nrun_dir = \"{}\"\nsecrets_file = \"{}\"\n[library]\nlisten = \"127.0.0.1:{lib_port}\"\n",
                data.display(),
                stations.display(),
                dir.join("run").display(),
                dir.join("secrets.toml").display()
            ),
        )
        .unwrap();
        Env {
            dir,
            global,
            data,
            st_port,
            lib_port,
            lib,
            children: vec![],
            items,
        }
    }

    pub fn st(&self) -> String {
        format!("127.0.0.1:{}", self.st_port)
    }
    pub fn lb(&self) -> String {
        format!("127.0.0.1:{}", self.lib_port)
    }

    pub fn spawn_station(&mut self, envs: &[(&str, &str)]) {
        let mut c = Command::new(env!("CARGO_BIN_EXE_hotclock"));
        c.arg("--global-config")
            .arg(&self.global)
            .args(["station", "office", "--rt-priority", "0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (k, v) in envs {
            c.env(k, v);
        }
        self.children.push(c.spawn().expect("spawn station"));
        self.wait_up(&self.st());
    }

    pub fn spawn_library(&mut self) {
        let mut c = Command::new(env!("CARGO_BIN_EXE_hotclock"));
        c.arg("--global-config")
            .arg(&self.global)
            .arg("library")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        self.children.push(c.spawn().expect("spawn library"));
        self.wait_up(&self.lb());
    }

    pub fn wait_up(&self, addr: &str) {
        let t = Instant::now();
        loop {
            if client::request(addr, "GET", "/health", &[], b"", Duration::from_secs(2)).is_ok() {
                return;
            }
            assert!(t.elapsed().as_secs() < 30, "{addr} did not come up");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn pid(&self, i: usize) -> u32 {
        self.children[i].id()
    }

    pub fn ctl(&self, words: &[&str]) {
        let st = Command::new(env!("CARGO_BIN_EXE_hotclock"))
            .arg("--global-config")
            .arg(&self.global)
            .args(["ctl", "office"])
            .args(words)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(st.success());
    }
}

pub fn get(addr: &str, path: &str) -> Reply {
    client::request(addr, "GET", path, &[], b"", Duration::from_secs(10)).unwrap()
}

pub fn send(addr: &str, method: &str, path: &str, body: &Value) -> Reply {
    client::request(
        addr,
        method,
        path,
        &[JSON],
        body.to_string().as_bytes(),
        Duration::from_secs(15),
    )
    .unwrap()
}

pub fn post(addr: &str, path: &str, body: &Value) -> Reply {
    send(addr, "POST", path, body)
}

pub fn wait_for<T>(what: &str, secs: f64, mut f: impl FnMut() -> Option<T>) -> T {
    let t = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            t.elapsed().as_secs_f64() < secs,
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Tagged FLAC tracks of one album with an optional cover, imported into the library. Returns the item ids
/// in track order.
pub fn add_album(e: &Env, album: &str, artist: &str, tracks: usize, cover: bool) -> Vec<i64> {
    use fixtures::{tag_file, TagSpec};
    let mut ids = vec![];
    let png = {
        let img = image::RgbImage::from_fn(900, 900, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 120])
        });
        let mut b = std::io::Cursor::new(vec![]);
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut b, image::ImageFormat::Png)
            .unwrap();
        b.into_inner()
    };
    for t in 1..=tracks {
        let wav = make_tone(
            &e.dir.join("src"),
            &format!("{album}-{t}"),
            500.0 + 50.0 * t as f64,
            20.0,
            -20.0,
        );
        let flac = wav.with_extension("flac");
        let st = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-i"])
            .arg(&wav)
            .args(["-c:a", "flac"])
            .arg(&flac)
            .status()
            .unwrap();
        assert!(st.success());
        tag_file(
            &flac,
            &TagSpec {
                title: &format!("{album} track {t}"),
                album,
                album_artist: artist,
                disc: 1,
                track: t as u32,
                track_total: tracks as u32,
                cover: cover.then(|| (png.clone(), lofty::picture::MimeType::Png)),
            },
        )
        .unwrap();
        match import_file(
            &e.lib,
            &flac,
            &ImportOptions {
                category: Some("music".into()),
                source: "test".into(),
            },
        ) {
            ImportOutcome::Done { item_id, .. } => ids.push(item_id),
            x => panic!("{x:?}"),
        }
    }
    ids
}

/// A multipart body with a category field and the given files.
pub fn multipart(category: Option<&str>, files: &[(&str, Vec<u8>)]) -> (String, Vec<u8>) {
    let b = "----hotclockTestBoundary7a1";
    let mut body = vec![];
    if let Some(c) = category {
        body.extend_from_slice(
            format!("--{b}\r\nContent-Disposition: form-data; name=\"category\"\r\n\r\n{c}\r\n")
                .as_bytes(),
        );
    }
    for (name, data) in files {
        body.extend_from_slice(
            format!("--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{b}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={b}"), body)
}

pub fn png_bytes() -> Vec<u8> {
    let img = image::RgbImage::from_fn(200, 200, |x, y| {
        image::Rgb([200, (x % 256) as u8, (y % 256) as u8])
    });
    let mut b = std::io::Cursor::new(vec![]);
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut b, image::ImageFormat::Png)
        .unwrap();
    b.into_inner()
}

/// The PNG `add_album` embeds, for its hash.
pub fn album_cover_png() -> Vec<u8> {
    let img = image::RgbImage::from_fn(900, 900, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 120])
    });
    let mut b = std::io::Cursor::new(vec![]);
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut b, image::ImageFormat::Png)
        .unwrap();
    b.into_inner()
}
