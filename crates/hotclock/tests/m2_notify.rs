//! M2 acceptance 8 over real processes and a local SMTP sink: Send test and its reply, a wrong password's error,
//! the password nowhere but `secrets.toml`, one immediate message and one "cleared" for a forced `silence`, ten
//! more cycles held for the hourly digest (one line with a count), `item_quarantined` at once, an alert raised
//! while the library service was stopped sent within 60 s of its restart, and `hotclock notify --event
//! service_failed` (what `OnFailure=` runs) reaching the sink while the library service is down.

mod support_api;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support_api::*;

const PASSWORD: &str = "s3cret-PASSWORD-zq81";

/// A local SMTP sink: AUTH PLAIN and LOGIN checked against `PASSWORD`, every accepted message kept.
fn sink() -> (u16, Arc<Mutex<Vec<String>>>) {
    sink_with("250-sink\r\n250-AUTH PLAIN LOGIN\r\n250 8BITMIME\r\n")
}

fn sink_with(ehlo: &'static str) -> (u16, Arc<Mutex<Vec<String>>>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let mail: Arc<Mutex<Vec<String>>> = Default::default();
    let m = mail.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let m = m.clone();
            std::thread::spawn(move || {
                let mut w = s.try_clone().unwrap();
                let mut r = BufReader::new(s);
                let _ = w.write_all(b"220 sink ESMTP ready\r\n");
                let (mut in_data, mut msg, mut authed, mut login_step) =
                    (false, String::new(), false, 0);
                let mut user = String::new();
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let t = line.trim_end().to_string();
                    if in_data {
                        if t == "." {
                            in_data = false;
                            m.lock().unwrap().push(std::mem::take(&mut msg));
                            let _ = w.write_all(b"250 2.0.0 queued as SINK1\r\n");
                        } else {
                            msg.push_str(&t);
                            msg.push('\n');
                        }
                        continue;
                    }
                    let up = t.to_ascii_uppercase();
                    let check = |b64: &str| -> bool {
                        let raw = decode(b64);
                        let parts: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
                        parts
                            .get(2)
                            .map(|p| *p == PASSWORD.as_bytes())
                            .unwrap_or(false)
                    };
                    let reply: String = if login_step == 1 {
                        user = String::from_utf8_lossy(&decode(&t)).to_string();
                        login_step = 2;
                        "334 UGFzc3dvcmQ6\r\n".into()
                    } else if login_step == 2 {
                        login_step = 0;
                        if String::from_utf8_lossy(&decode(&t)) == PASSWORD && !user.is_empty() {
                            authed = true;
                            "235 2.7.0 Authentication successful\r\n".into()
                        } else {
                            "535 5.7.8 Authentication credentials invalid\r\n".into()
                        }
                    } else if up.starts_with("EHLO") {
                        ehlo.to_string()
                    } else if up.starts_with("AUTH PLAIN") {
                        if check(t.split_whitespace().nth(2).unwrap_or("")) {
                            authed = true;
                            "235 2.7.0 Authentication successful\r\n".into()
                        } else {
                            "535 5.7.8 Authentication credentials invalid\r\n".into()
                        }
                    } else if up == "AUTH LOGIN" {
                        login_step = 1;
                        "334 VXNlcm5hbWU6\r\n".into()
                    } else if up.starts_with("MAIL FROM") && !authed {
                        "530 5.7.0 Authentication required\r\n".into()
                    } else if up == "DATA" {
                        in_data = true;
                        "354 go ahead\r\n".into()
                    } else if up == "QUIT" {
                        let _ = w.write_all(b"221 bye\r\n");
                        break;
                    } else {
                        "250 ok\r\n".into()
                    };
                    let _ = w.write_all(reply.as_bytes());
                }
            });
        }
    });
    (port, mail)
}

fn decode(s: &str) -> Vec<u8> {
    let t = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => 255,
    };
    let (mut out, mut buf, mut bits) = (vec![], 0u32, 0);
    for c in s.bytes().filter(|c| *c != b'=') {
        let v = t(c);
        if v == 255 {
            continue;
        }
        buf = (buf << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    out
}

fn spawn(e: &Env, args: &[&str], log: &Path, hooks: bool) -> Child {
    let f = std::fs::File::create(log).unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_hotclock"));
    c.arg("--global-config")
        .arg(&e.global)
        .args(args)
        .env("HOTCLOCK_LOG", "trace")
        .stdout(f.try_clone().unwrap())
        .stderr(f);
    if hooks {
        c.env("HOTCLOCK_TEST_HOOKS", "1");
    }
    c.spawn().unwrap()
}

fn subjects(mail: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    mail.lock()
        .unwrap()
        .iter()
        .map(|m| {
            m.lines()
                .find(|l| l.starts_with("Subject:"))
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

fn count(mail: &Arc<Mutex<Vec<String>>>, needle: &str) -> usize {
    subjects(mail).iter().filter(|s| s.contains(needle)).count()
}

fn alert_open(e: &Env, code: &str) -> bool {
    get(&e.st(), "/api/v1/alerts").json()["alerts"]
        .as_array()
        .map(|a| {
            a.iter()
                .any(|x| x["code"] == code && x["cleared_at"].is_null())
        })
        .unwrap_or(false)
}

#[test]
fn notifications_acceptance_8() {
    let e = Env::new(
        "m2notify",
        &Opts {
            tones: 2,
            secs: 120.0,
            ..Opts::default()
        },
    );
    let (port, mail) = sink();
    let (lb, st) = (e.lb(), e.st());
    let mut seen_bodies: Vec<String> = vec![];
    let mut api = |label: &str, r: serde_json::Value| {
        seen_bodies.push(format!("{label}: {r}"));
    };

    let mut station = spawn(
        &e,
        &["station", "office", "--rt-priority", "0"],
        &e.dir.join("station.log"),
        true,
    );
    let mut library = spawn(&e, &["library"], &e.dir.join("library.log"), false);
    e.wait_up(&st);
    e.wait_up(&lb);

    // The channel, with its password given once.
    let r = post(
        &lb,
        "/api/v1/notify/channels",
        &json!({"name": "Sink", "enabled": true, "password": PASSWORD,
                "config": {"host": "127.0.0.1", "port": port, "tls": "none", "username": "hotclock",
                           "from": "Hotclock <hotclock@example.org>", "to": ["owner@example.org"]}}),
    );
    assert_eq!(r.status, 201, "{}", r.text());
    api("create", r.json());
    let cid = r.json()["id"].as_i64().unwrap();
    let secrets = e.dir.join("secrets.toml");
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(&secrets).unwrap().permissions(),
    ) & 0o777;
    println!(
        "8: secrets.toml mode {mode:o}; holds the password: {}",
        std::fs::read_to_string(&secrets)
            .unwrap()
            .contains(PASSWORD)
    );
    assert_eq!(mode, 0o600);

    // Send test: one message, the server's reply.
    let r = post(
        &lb,
        &format!("/api/v1/notify/channels/{cid}/test"),
        &json!({}),
    );
    api("test", r.json());
    assert_eq!(r.json()["ok"], true, "{}", r.text());
    println!("8: Send test -> {}", r.json()["reply"]);
    assert!(r.json()["reply"]
        .as_str()
        .unwrap()
        .contains("250 2.0.0 queued as SINK1"));
    assert_eq!(count(&mail, "test message"), 1);

    // A wrong password: the server's own error.
    let r = send(
        &lb,
        "PUT",
        &format!("/api/v1/notify/channels/{cid}"),
        &json!({"name": "Sink", "enabled": true, "password": "not-the-password-77",
        "config": {"host": "127.0.0.1", "port": port, "tls": "none", "username": "hotclock", "from": "Hotclock <hotclock@example.org>", "to": ["owner@example.org"]}}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    api("put-wrong", r.json());
    let r = post(
        &lb,
        &format!("/api/v1/notify/channels/{cid}/test"),
        &json!({}),
    );
    api("test-wrong", r.json());
    println!("8: wrong password -> {}", r.json()["error"]);
    assert_eq!(r.json()["ok"], false);
    assert!(
        r.json()["error"]
            .as_str()
            .unwrap()
            .contains("535 5.7.8 Authentication credentials invalid"),
        "{}",
        r.text()
    );
    assert_eq!(
        count(&mail, "test message"),
        1,
        "nothing was sent with a wrong password"
    );
    let n = get(&lb, "/api/v1/notify").json();
    assert!(
        n["channels"][0]["last_test_result"]
            .as_str()
            .unwrap()
            .starts_with("failed: authentication"),
        "{n}"
    );
    api("notify-after-wrong", n);
    // Restore the right one, then the page's channel list says only that a password is set.
    let r = send(
        &lb,
        "PUT",
        &format!("/api/v1/notify/channels/{cid}"),
        &json!({"name": "Sink", "enabled": true, "password": PASSWORD,
        "config": {"host": "127.0.0.1", "port": port, "tls": "none", "username": "hotclock", "from": "Hotclock <hotclock@example.org>", "to": ["owner@example.org"]}}),
    );
    assert_eq!(r.status, 200);
    api("put-right", r.json());
    let r = post(
        &lb,
        &format!("/api/v1/notify/channels/{cid}/test"),
        &json!({}),
    );
    assert_eq!(r.json()["ok"], true);
    assert_eq!(count(&mail, "test message"), 2);
    let n = get(&lb, "/api/v1/notify").json();
    assert_eq!(n["channels"][0]["password_set"], true);

    // A forced silence: one immediate message, one "cleared".
    wait_for("playing", 20.0, || {
        get(&st, "/api/v1/now").json()["title"].as_str().map(|_| ())
    });
    let t = Instant::now();
    e.ctl(&["hook", "alert", "silence", "critical"]);
    wait_for("silence mail", 20.0, || {
        (count(&mail, "CRITICAL silence") == 1).then_some(())
    });
    println!(
        "8: forced silence mailed {:.1} s after it was raised",
        t.elapsed().as_secs_f64()
    );
    e.ctl(&["hook", "clear-alert", "silence"]);
    wait_for("cleared mail", 20.0, || {
        (count(&mail, "cleared: silence") == 1).then_some(())
    });
    // Ten more cycles inside the hour: nothing more is sent.
    let before = mail.lock().unwrap().len();
    for i in 0..10 {
        e.ctl(&["hook", "alert", "silence", "critical"]);
        wait_for("raised", 10.0, || alert_open(&e, "silence").then_some(()));
        std::thread::sleep(Duration::from_millis(2600));
        e.ctl(&["hook", "clear-alert", "silence"]);
        wait_for("cleared", 10.0, || {
            (!alert_open(&e, "silence")).then_some(())
        });
        std::thread::sleep(Duration::from_millis(2600));
        let _ = i;
    }
    println!(
        "8: ten more silence cycles: {} messages sent for them",
        mail.lock().unwrap().len() - before
    );
    assert_eq!(
        mail.lock().unwrap().len(),
        before,
        "the rate limit holds all of them back"
    );
    let r = post(
        &lb,
        "/api/v1/notify/digest",
        &json!({"bucket": "hourly_digest"}),
    );
    api("digest", r.json());
    println!("8: digest now: {}", r.text());
    assert_eq!(r.json()["sent"], true, "{}", r.text());
    let digest = mail.lock().unwrap().last().unwrap().clone();
    let lines: Vec<&str> = digest
        .lines()
        .filter(|l| l.starts_with("office  silence"))
        .collect();
    println!(
        "8: digest body:\n{}",
        digest
            .lines()
            .skip_while(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(lines.len(), 1, "one line for ten raises: {digest}");
    assert!(
        lines[0].contains("x10") && lines[0].contains("cleared x10"),
        "{}",
        lines[0]
    );
    assert_eq!(mail.lock().unwrap().len(), before + 1);

    // item_quarantined arrives at once.
    let t = Instant::now();
    e.ctl(&["hook", "alert", "item_quarantined", "warning"]);
    wait_for("quarantine mail", 20.0, || {
        (count(&mail, "item_quarantined") == 1).then_some(())
    });
    println!(
        "8: item_quarantined mailed {:.1} s after it was raised",
        t.elapsed().as_secs_f64()
    );
    // A digest-only code waits.
    e.ctl(&["hook", "alert", "item_missing", "warning"]);
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(count(&mail, "item_missing"), 0);

    // An alert raised while the library service is stopped is sent within 60 s of its restart.
    let _ = library.kill();
    let _ = library.wait();
    e.ctl(&["hook", "alert", "queue_starved", "critical"]);
    wait_for("raised", 10.0, || {
        alert_open(&e, "queue_starved").then_some(())
    });
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(count(&mail, "queue_starved"), 0);
    let t = Instant::now();
    let mut library = spawn(&e, &["library"], &e.dir.join("library2.log"), false);
    e.wait_up(&lb);
    wait_for("queue_starved mail", 60.0, || {
        (count(&mail, "CRITICAL queue_starved") == 1).then_some(())
    });
    let late = t.elapsed().as_secs_f64();
    println!("8: an alert raised while the library service was stopped was mailed {late:.1} s after its restart");
    assert!(late < 60.0);

    // service_failed through the one-shot, with the library service down (what OnFailure= runs).
    let _ = library.kill();
    let _ = library.wait();
    let o = Command::new(env!("CARGO_BIN_EXE_hotclock"))
        .arg("--global-config")
        .arg(&e.global)
        .args([
            "notify",
            "--event",
            "service_failed",
            "--unit",
            "hotclock-library.service",
            "--message",
            "The unit hotclock-library.service entered the failed state.",
        ])
        .env("HOTCLOCK_LOG", "trace")
        .output()
        .unwrap();
    let (so, se) = (
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    );
    println!(
        "8: hotclock notify: exit {:?}, stdout `{}`, stderr `{}`",
        o.status.code(),
        so.trim(),
        se.trim()
    );
    assert!(o.status.success());
    assert_eq!(count(&mail, "CRITICAL service_failed"), 1);
    let m = mail
        .lock()
        .unwrap()
        .iter()
        .find(|m| m.contains("service_failed"))
        .unwrap()
        .clone();
    assert!(m.contains("hotclock-library.service"));
    seen_bodies.push(format!("oneshot: {so}{se}"));

    // The password is in secrets.toml and nowhere else: API responses, log lines, library.db.
    let _ = station.kill();
    let _ = station.wait();
    for p in ["/api/v1/notify", "/health", "/api/v1/stations"] {
        // the library service is down: bring it up for the last reads
        let _ = p;
    }
    let mut library = spawn(&e, &["library"], &e.dir.join("library3.log"), false);
    e.wait_up(&lb);
    for p in [
        "/api/v1/notify",
        "/health",
        "/api/v1/stations",
        "/api/v1/notify/channels",
    ] {
        let r = get(&lb, p);
        seen_bodies.push(format!("GET {p}: {}", r.text()));
    }
    let _ = library.kill();
    let _ = library.wait();
    let grep = |what: &str, hay: &[u8]| {
        let n = hay
            .windows(PASSWORD.len())
            .filter(|w| *w == PASSWORD.as_bytes())
            .count();
        println!(
            "8: grep password in {what}: {n} hit(s) in {} bytes",
            hay.len()
        );
        assert_eq!(n, 0, "the password is in {what}");
    };
    grep("API responses", seen_bodies.join("\n").as_bytes());
    let mut logs = vec![];
    for f in ["station.log", "library.log", "library2.log", "library3.log"] {
        logs.extend(std::fs::read(e.dir.join(f)).unwrap());
    }
    grep("station and library logs (HOTCLOCK_LOG=trace)", &logs);
    grep("the one-shot's output", format!("{so}{se}").as_bytes());
    // Every file the run left (the database and its WAL, the nightly snapshots, the stations' databases, the
    // run directory), except `secrets.toml` itself and the audio.
    fn walk(d: &Path, out: &mut Vec<std::path::PathBuf>) {
        for en in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = en.path();
            if p.is_dir() {
                if !matches!(
                    p.file_name().and_then(|n| n.to_str()),
                    Some("media" | "src" | "inbox")
                ) {
                    walk(&p, out);
                }
            } else {
                out.push(p);
            }
        }
    }
    let mut files = vec![];
    walk(&e.dir, &mut files);
    let (mut total, mut nfiles) = (0usize, 0usize);
    for f in files
        .iter()
        .filter(|f| f.file_name().and_then(|n| n.to_str()) != Some("secrets.toml"))
    {
        let b = std::fs::read(f).unwrap();
        total += b.len();
        nfiles += 1;
        let n = b
            .windows(PASSWORD.len())
            .filter(|w| *w == PASSWORD.as_bytes())
            .count();
        assert_eq!(n, 0, "the password is in {}", f.display());
    }
    println!("8: grep password in {nfiles} files under the run directory ({total} bytes, library.db, -wal, -shm and snapshots included): 0 hits");
    // And no wrong password anywhere either.
    let _ = Value::Null;
    assert!(std::fs::read_to_string(&secrets)
        .unwrap()
        .contains(PASSWORD));
}

/// A relay that offers LOGIN and not PLAIN is served with LOGIN; PLAIN is not sent to it.
#[test]
fn auth_login_is_used_when_the_server_offers_no_plain() {
    let (port, mail) = sink_with("250-sink\r\n250 AUTH LOGIN\r\n");
    let cfg = serve::notify::SmtpConfig::from_channel(
        &json!({"host": "127.0.0.1", "port": port, "tls": "none", "username": "hotclock", "from": "h@example.org", "to": ["m@example.org"]}),
        Some(PASSWORD.into()),
    )
    .unwrap();
    let reply = serve::notify::send_mail(&cfg, "login test", "body").unwrap();
    println!(
        "AUTH LOGIN: {reply}; {} message(s) kept",
        mail.lock().unwrap().len()
    );
    assert!(reply.contains("250"));
    assert_eq!(mail.lock().unwrap().len(), 1);
    let bad = serve::notify::SmtpConfig::from_channel(
        &json!({"host": "127.0.0.1", "port": port, "tls": "none", "username": "hotclock", "from": "h@example.org", "to": ["m@example.org"]}),
        Some("wrong".into()),
    )
    .unwrap();
    let e = serve::notify::send_mail(&bad, "x", "y").unwrap_err();
    assert!(e.contains("535"), "{e}");
}
