//! M5 acceptance 3b: the same mute block as `osc_out` and as `midi_out` reaches a test receiver of each
//! kind, with the same logging, retry-once and never-blocks behavior; and acceptance 6's 20 ms OSC bound.

use station::side::*;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn job(id: u64, at: i64, action: SideAction) -> SideJob {
    SideJob {
        id,
        block_id: 7,
        name: "Wing mute".into(),
        at_wall_ms: at,
        handed_ms: now_ms(),
        action,
    }
}

fn wait_results(w: &mut SideWorker, n: usize) -> Vec<SideResult> {
    let mut out = vec![];
    let t = Instant::now();
    while out.len() < n && t.elapsed() < Duration::from_secs(5) {
        out.extend(w.results());
        std::thread::sleep(Duration::from_millis(2));
    }
    out
}

#[test]
fn osc_reaches_a_loopback_receiver_at_its_time() {
    let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
    rx.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let port = rx.local_addr().unwrap().port();
    let mut w = SideWorker::spawn();
    let mut worst = 0i64;
    for i in 0..20 {
        // Handed over 200 ms early, as the station does; the worker holds it until its time.
        let at = now_ms() + 200;
        assert!(w.submit(job(
            i,
            at,
            SideAction::Osc {
                host: "127.0.0.1".into(),
                port,
                address: "/ch/2/mute".into(),
                args: vec![OscArg::Int(1)],
            }
        )));
        let mut buf = [0u8; 256];
        let (n, _) = rx.recv_from(&mut buf).unwrap();
        let arrived = now_ms();
        let (addr, args) = osc_parse(&buf[..n]).unwrap();
        assert_eq!(addr, "/ch/2/mute");
        assert_eq!(args, vec![OscArg::Int(1)]);
        worst = worst.max(arrived - at);
        assert!(arrived >= at - 1, "not before its time: {}", arrived - at);
    }
    let r = wait_results(&mut w, 20);
    assert!(r.iter().all(|x| x.ok && x.attempts == 1));
    println!("OSC: 20 messages through the worker to a loopback receiver, the latest arrived {worst} ms after its wall time (limit 20)");
    assert!(worst <= 20, "{worst}");
}

#[test]
fn midi_reaches_a_device_file_with_the_same_logging_and_the_same_retry() {
    let dir = fixtures::testdir::process_dir().join(format!("m5-midi-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dev = dir.join("midiC9D0");
    std::fs::write(&dev, b"").unwrap();
    let mut w = SideWorker::spawn();
    let at = now_ms() + 100;
    let msg = MidiMsg {
        kind: MidiKind::Cc,
        channel: 1,
        data1: 7,
        data2: 0,
        data: vec![],
    };
    w.submit(job(
        1,
        at,
        SideAction::Midi {
            port: dev.to_string_lossy().into(),
            msg: msg.clone(),
        },
    ));
    let r = wait_results(&mut w, 1);
    assert!(r[0].ok && r[0].attempts == 1);
    assert_eq!(std::fs::read(&dev).unwrap(), vec![0xB0, 7, 0]);
    println!(
        "MIDI: {:?} written to the device file, sent {} ms after its time",
        std::fs::read(&dev).unwrap(),
        r[0].sent_wall_ms - at
    );
    // Retry once: the port is not there at the first attempt and is there 150 ms later.
    let late = dir.join("late");
    let l2 = late.clone();
    let creator = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(120));
        std::fs::write(&l2, b"").unwrap();
    });
    w.submit(job(
        2,
        now_ms(),
        SideAction::Midi {
            port: late.to_string_lossy().into(),
            msg: msg.clone(),
        },
    ));
    let r = wait_results(&mut w, 1);
    creator.join().unwrap();
    assert!(r[0].ok, "{:?}", r[0].error);
    assert_eq!(r[0].attempts, 2, "the second attempt worked");
    assert_eq!(std::fs::read(&late).unwrap(), vec![0xB0, 7, 0]);
    // A port that never appears fails after the one retry and says why; the worker goes on.
    w.submit(job(
        3,
        now_ms(),
        SideAction::Midi {
            port: dir.join("never").to_string_lossy().into(),
            msg,
        },
    ));
    let r = wait_results(&mut w, 1);
    assert!(!r[0].ok && r[0].attempts == 2);
    println!("MIDI failure: {}", r[0].error.as_deref().unwrap());
}

#[test]
fn a_web_request_reaches_a_local_server_and_a_refusal_is_a_failure() {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = std::thread::spawn(move || {
        let mut seen = String::new();
        for code in [200, 500, 500] {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).unwrap();
            if seen.is_empty() {
                seen = String::from_utf8_lossy(&buf[..n]).to_string();
            }
            let _ =
                s.write_all(format!("HTTP/1.1 {code} X\r\nContent-Length: 0\r\n\r\n").as_bytes());
        }
        seen
    });
    let mut w = SideWorker::spawn();
    let act = |path: &str| SideAction::Http {
        method: "POST".into(),
        url: format!("http://127.0.0.1:{port}{path}"),
        headers: vec![("X-Test".into(), "yes".into())],
        body: Some("{\"mute\":true}".into()),
        timeout_s: 3,
    };
    w.submit(job(1, now_ms(), act("/hook")));
    let r = wait_results(&mut w, 1);
    assert!(r[0].ok, "{:?}", r[0].error);
    w.submit(job(2, now_ms(), act("/fail")));
    let r = wait_results(&mut w, 1);
    assert!(!r[0].ok && r[0].attempts == 2, "{:?}", r[0]);
    let seen = h.join().unwrap();
    assert!(seen.starts_with("POST /hook HTTP/1.1"), "{seen}");
    assert!(seen.contains("X-Test: yes") && seen.ends_with("{\"mute\":true}"));
}

#[test]
fn a_full_queue_drops_jobs_and_never_blocks_the_caller() {
    let mut w = SideWorker::spawn();
    let far = now_ms() + 3_600_000;
    let t = Instant::now();
    let mut dropped = 0;
    for i in 0..(QUEUE_MAX as u64 + 50) {
        if !w.submit(job(
            i,
            far,
            SideAction::Osc {
                host: "127.0.0.1".into(),
                port: 9,
                address: "/x".into(),
                args: vec![],
            },
        )) {
            dropped += 1;
        }
    }
    println!(
        "{} submits took {:?}, {dropped} dropped",
        QUEUE_MAX + 50,
        t.elapsed()
    );
    assert!(t.elapsed() < Duration::from_millis(500));
}
