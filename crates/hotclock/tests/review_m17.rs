//! Tests added by the M1.7 review (2026-09-30): the API under load never holds up the station, and an upload
//! that is cut off part way leaves nothing behind.

mod support_api;
use serve::http::client;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use support_api::*;

fn frames_written(e: &Env) -> f64 {
    let m = get(&e.st(), "/metrics").text();
    m.lines()
        .find(|l| l.starts_with("hotclock_frames_written_total"))
        .and_then(|l| l.rsplit(' ').next())
        .and_then(|v| v.parse().ok())
        .expect("hotclock_frames_written_total")
}

/// Sixteen clients reading `/health`, `/metrics` and the state as fast as they can, and four sending commands
/// as fast as they can, for 8 s: the station keeps rendering in real time, `/health` never reports it stale,
/// and a command is answered (done, or `503 busy` when too many wait) but never times out.
#[test]
fn hammering_the_api_never_holds_up_the_station() {
    let mut e = Env::new("hammer", &Opts::default());
    e.spawn_station(&[]);
    wait_for("playing", 20.0, || {
        get(&e.st(), "/api/v1/state").json()["now"]["title"]
            .as_str()
            .map(|_| ())
    });
    let stop = Arc::new(AtomicBool::new(false));
    let (reads, stale, posts, busy, other) = (
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
    );
    let f0 = frames_written(&e);
    let t0 = Instant::now();
    let mut threads = vec![];
    for i in 0..16 {
        let (addr, stop, reads, stale) = (e.st(), stop.clone(), reads.clone(), stale.clone());
        threads.push(std::thread::spawn(move || {
            let path = ["/health", "/metrics", "/api/v1/state", "/api/v1/now"][i % 4];
            while !stop.load(Ordering::Relaxed) {
                if let Ok(r) =
                    client::request(&addr, "GET", path, &[], b"", Duration::from_secs(10))
                {
                    reads.fetch_add(1, Ordering::Relaxed);
                    if path == "/health" && r.json()["stale"] == true {
                        stale.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }));
    }
    for _ in 0..4 {
        let (addr, stop, posts, busy, other) = (
            e.st(),
            stop.clone(),
            posts.clone(),
            busy.clone(),
            other.clone(),
        );
        threads.push(std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let r = client::request(
                    &addr,
                    "POST",
                    "/api/v1/internal/reload",
                    &[JSON],
                    b"{}",
                    Duration::from_secs(10),
                );
                match r.map(|r| r.status) {
                    Ok(s) if s < 300 => posts.fetch_add(1, Ordering::Relaxed),
                    Ok(503) => busy.fetch_add(1, Ordering::Relaxed),
                    _ => other.fetch_add(1, Ordering::Relaxed),
                };
            }
        }));
    }
    std::thread::sleep(Duration::from_secs(8));
    stop.store(true, Ordering::Relaxed);
    for t in threads {
        t.join().unwrap();
    }
    let secs = t0.elapsed().as_secs_f64();
    let rate = (frames_written(&e) - f0) / secs;
    println!(
        "8 s of load: {} reads, {} commands done, {} busy, {} other; frames written {rate:.0}/s; stale health {}",
        reads.load(Ordering::Relaxed),
        posts.load(Ordering::Relaxed),
        busy.load(Ordering::Relaxed),
        other.load(Ordering::Relaxed),
        stale.load(Ordering::Relaxed)
    );
    assert!(reads.load(Ordering::Relaxed) > 100);
    assert!(posts.load(Ordering::Relaxed) > 20);
    assert_eq!(other.load(Ordering::Relaxed), 0, "no command timed out");
    assert_eq!(stale.load(Ordering::Relaxed), 0, "the view was never stale");
    // The published counter moves in 250 ms steps; 8 s at 48 kHz within 5 %.
    assert!(
        (rate - 48_000.0).abs() < 2_400.0,
        "the station kept real time: {rate:.0} frames/s"
    );
    let h = get(&e.st(), "/health");
    assert_eq!(h.status, 200, "{}", h.text());
}

/// An upload whose connection drops inside its second file: the first file (already received) and the partial
/// second leave nothing in `upload-tmp/`, and no job is left `queued`.
#[test]
fn an_upload_cut_off_part_way_leaves_nothing_behind() {
    let mut e = Env::new(
        "cutoff",
        &Opts {
            tones: 1,
            ..Opts::default()
        },
    );
    e.spawn_library();
    let one = make_tone(&e.dir.join("src"), "one", 500.0, 3.0, -20.0);
    let two = make_tone(&e.dir.join("src"), "two", 600.0, 3.0, -20.0);
    let (ct, body) = multipart(
        Some("music"),
        &[
            ("one.wav", std::fs::read(&one).unwrap()),
            ("two.wav", std::fs::read(&two).unwrap()),
        ],
    );
    // Everything up to the middle of the second file, then the connection ends.
    let cut = body.len() - std::fs::metadata(&two).unwrap().len() as usize / 2;
    let mut s = std::net::TcpStream::connect(e.lb()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    write!(
        s,
        "POST /api/v1/imports HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: {ct}\r\nX-Requested-With: test\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .unwrap();
    s.write_all(&body[..cut]).unwrap();
    s.shutdown(std::net::Shutdown::Write).unwrap();
    let r = client::read_reply(&mut std::io::BufReader::new(&s)).unwrap();
    assert_eq!(r.status, 400, "{}", r.text());
    let tmp = e.data.join("upload-tmp");
    let left: Vec<_> = std::fs::read_dir(&tmp)
        .map(|d| d.flatten().map(|x| x.path()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "left in upload-tmp: {left:?}");
    let jobs = get(&e.lb(), "/api/v1/imports").json();
    let jobs = jobs["jobs"].as_array().unwrap();
    assert!(
        jobs.iter().all(|j| j["status"] != "queued"),
        "a job was left queued: {jobs:?}"
    );
    assert!(
        jobs.iter()
            .any(|j| j["source"] == "one.wav" && j["status"] == "failed"),
        "{jobs:?}"
    );
    // The service still takes a whole upload.
    let (ct, body) = multipart(Some("music"), &[("one.wav", std::fs::read(&one).unwrap())]);
    let r = client::request(
        &e.lb(),
        "POST",
        "/api/v1/imports",
        &[("Content-Type", &ct), ("X-Requested-With", "test")],
        &body,
        Duration::from_secs(20),
    )
    .unwrap();
    assert_eq!(r.status, 202, "{}", r.text());
}

/// The upload limits agree with their message: a request whose files together are over 1 GB is not refused
/// for its total (each file may be 1 GB, one request 16 GB), and a request over 16 GB is refused up front
/// with a message that says so. (The bodies are not sent: the first request is cut off after its headers and
/// answers `400` from the body reader, not `413`.)
#[test]
fn the_upload_limit_is_per_file_and_the_request_limit_says_what_it_is() {
    let mut e = Env::new("uplimit", &Opts::default());
    e.spawn_library();
    let ct = "multipart/form-data; boundary=xyz";
    let send = |len: u64| {
        let mut s = std::net::TcpStream::connect(e.lb()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        write!(
            s,
            "POST /api/v1/imports HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: {ct}\r\nX-Requested-With: test\r\nContent-Length: {len}\r\n\r\n"
        )
        .unwrap();
        s.write_all(b"--xyz--\r\n").unwrap();
        s.shutdown(std::net::Shutdown::Write).unwrap();
        client::read_reply(&mut std::io::BufReader::new(&s)).unwrap()
    };
    // 1.5 GB in all (two files of 750 MB): past the old request limit. Not 413.
    let r = send(3 << 29);
    assert_ne!(r.status, 413, "{}", r.text());
    // 20 GB: refused before anything is read, and the message names both limits.
    let r = send(20 << 30);
    assert_eq!(r.status, 413, "{}", r.text());
    let t = r.text();
    assert!(t.contains("16 GB in all") && t.contains("1 GB"), "{t}");
}
