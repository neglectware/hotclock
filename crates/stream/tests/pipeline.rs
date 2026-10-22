//! The encoder pipeline against an independent decoder (symphonia): bit-exact audio, an exact
//! timeline across gaps, and a restart.

use engine::tap::TapPeriod;
use std::io::Cursor;
use std::sync::Arc;
use stream::encoder::{EncoderOptions, Pipeline};
use stream::flac::to_i24;
use stream::mount::{Mount, MountConfig, Next};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

fn mount() -> Arc<Mount> {
    Mount::new(MountConfig {
        name: "program".into(),
        display_name: "Test".into(),
        rate: 48_000,
        codec: "flac",
        burst_ms: 100_000, // everything the ring holds
        max_listeners: 4,
        listener_lag_s: 600,
        expect_listener: false,
    })
}

/// Everything a new listener would receive: the headers, then every page in the ring.
fn collect(m: &Arc<Mount>) -> Vec<u8> {
    let mut l = m.join("test", None).unwrap();
    let mut out = l.headers().to_vec();
    while let Next::Page(p) = l.next(std::time::Duration::ZERO) {
        out.extend_from_slice(&p.bytes);
    }
    out
}

/// Decode an Ogg FLAC byte stream to interleaved 24-bit integers.
fn decode(bytes: Vec<u8>) -> Vec<i32> {
    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("ogg");
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .expect("probe");
    let mut fmt = probed.format;
    let track = fmt.default_track().expect("track").clone();
    assert_eq!(track.codec_params.sample_rate, Some(48_000));
    assert_eq!(track.codec_params.channels.map(|c| c.count()), Some(2));
    let mut dec = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .expect("decoder");
    let mut out = vec![];
    while let Ok(pkt) = fmt.next_packet() {
        let buf = dec.decode(&pkt).expect("decode");
        let mut sb = SampleBuffer::<i32>::new(buf.capacity() as u64, *buf.spec());
        sb.copy_interleaved_ref(buf);
        // symphonia scales 24-bit samples to the full i32 range.
        out.extend(sb.samples().iter().map(|s| s >> 8));
    }
    out
}

fn period(start: u64, frames: usize, f: impl Fn(u64) -> f32) -> (TapPeriod, Vec<f32>) {
    let mut v = Vec::with_capacity(frames * 2);
    for i in 0..frames as u64 {
        let x = f(start + i);
        v.push(x);
        v.push(-x);
    }
    (
        TapPeriod {
            start_frame: start,
            frames,
        },
        v,
    )
}

fn sine(n: u64) -> f32 {
    ((n as f64 * 2.0 * std::f64::consts::PI * 1000.0 / 48_000.0).sin() * 0.1) as f32
}

#[test]
fn the_decoded_stream_is_bit_exact_to_the_input() {
    let m = mount();
    let mut p = Pipeline::new(EncoderOptions::new(48_000, "Test"), m.clone()).unwrap();
    // Periods of 1,024 frames and one odd length, 60 periods.
    let mut t = 0u64;
    let mut want: Vec<i32> = vec![];
    for i in 0..60 {
        let n = if i == 30 { 700 } else { 1024 };
        let (tp, v) = period(t, n, sine);
        want.extend(v.iter().map(|x| to_i24(*x)));
        p.feed(tp, &v).unwrap();
        t += n as u64;
    }
    p.finish().unwrap();
    let got = decode(collect(&m));
    // The first 2 ms are faded in and the end is faded and padded: compare the middle exactly.
    let fade = 96 * 2;
    assert!(got.len() >= want.len());
    assert_eq!(
        &got[fade..want.len() - 2048],
        &want[fade..want.len() - 2048],
        "decoded audio differs from the input"
    );
    assert_eq!(m.stats.gaps.load(std::sync::atomic::Ordering::Relaxed), 0);
}

#[test]
fn a_gap_is_filled_with_silence_of_the_right_length_and_nothing_repeats() {
    let m = mount();
    let mut p = Pipeline::new(EncoderOptions::new(48_000, "Test"), m.clone()).unwrap();
    // Counter signal: sample n carries the value n (small, exact in 24 bits).
    let ramp = |n: u64| (n % 4_000_000) as f32 / 8_388_608.0 + 1.0 / 8_388_608.0;
    let mut t = 0u64;
    for _ in 0..10 {
        let (tp, v) = period(t, 1024, ramp);
        p.feed(tp, &v).unwrap();
        t += 1024;
    }
    // rt-out dropped 100 periods.
    t += 100 * 1024;
    for _ in 0..10 {
        let (tp, v) = period(t, 1024, ramp);
        p.feed(tp, &v).unwrap();
        t += 1024;
    }
    p.finish().unwrap();
    let st = &m.stats;
    assert_eq!(st.gaps.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(
        st.gap_frames.load(std::sync::atomic::Ordering::Relaxed),
        100 * 1024
    );
    let got = decode(collect(&m));
    // Left channel: sample k of the stream is frame k of the station clock, minus nothing: the
    // first period started at 0 and the gap is silence, so the timeline equals the station's.
    let left: Vec<i32> = got.iter().step_by(2).copied().collect();
    let station_frames = t as usize;
    assert!(
        left.len() >= station_frames,
        "{} < {station_frames}",
        left.len()
    );
    // Before the gap: the ramp (after the 2 ms fade-in and before the 2 ms fade-out).
    for (k, v) in left.iter().enumerate().take(10 * 1024 - 100).skip(200) {
        assert_eq!(*v, k as i32 + 1, "frame {k}");
    }
    // In the gap (past its fade-out): digital silence.
    for (k, v) in left
        .iter()
        .enumerate()
        .take(110 * 1024 - 100)
        .skip(10 * 1024 + 100)
    {
        assert_eq!(*v, 0, "frame {k}");
    }
    // After the gap: the ramp again at the right place, no repeat.
    for (k, v) in left
        .iter()
        .enumerate()
        .take(120 * 1024 - 200)
        .skip(110 * 1024 + 200)
    {
        assert_eq!(*v, k as i32 + 1, "frame {k}");
    }
}

/// The Ogg pages in `bytes`: (flags, granule, serial, sequence).
fn pages(bytes: &[u8]) -> Vec<(u8, i64, u32, u32)> {
    let mut out = vec![];
    let mut i = 0;
    while i + 27 <= bytes.len() {
        assert_eq!(&bytes[i..i + 4], b"OggS", "page at {i}");
        let nseg = bytes[i + 26] as usize;
        let body: usize = bytes[i + 27..i + 27 + nseg]
            .iter()
            .map(|b| *b as usize)
            .sum();
        out.push((
            bytes[i + 5],
            i64::from_le_bytes(bytes[i + 6..i + 14].try_into().unwrap()),
            u32::from_le_bytes(bytes[i + 14..i + 18].try_into().unwrap()),
            u32::from_le_bytes(bytes[i + 18..i + 22].try_into().unwrap()),
        ));
        i += 27 + nseg + body;
    }
    assert_eq!(i, bytes.len());
    out
}

/// One logical stream: one serial, one BOS, sequence numbers without a hole, granules that never go back.
fn assert_one_logical_stream(bytes: &[u8]) {
    let pg = pages(bytes);
    assert!(pg.iter().all(|p| p.2 == pg[0].2), "serial changed");
    assert_eq!(
        pg.iter().filter(|p| p.0 & 2 != 0).count(),
        1,
        "one BOS page"
    );
    for (k, p) in pg.iter().enumerate() {
        assert_eq!(p.3 as usize, k, "page sequence");
    }
    let granules: Vec<i64> = pg.iter().map(|p| p.1).filter(|g| *g >= 0).collect();
    for w in granules.windows(2) {
        assert!(w[1] >= w[0], "granule went back");
    }
}

/// The reference decoder (`flac -t`, which checks every frame's CRC and the frame numbering) and ffmpeg accept
/// the stream, when they are installed.
fn independent_decoders_agree(bytes: &[u8], name: &str) {
    let dir = fixtures::testdir::process_dir().join(format!("pipe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join(format!("{name}.oga"));
    std::fs::write(&f, bytes).unwrap();
    let run = |cmd: &str, args: &[&str]| {
        std::process::Command::new(cmd)
            .args(args)
            .arg(&f)
            .output()
            .ok()
    };
    match run("flac", &["-t", "--ogg", "-s"]) {
        Some(o) => assert!(
            o.status.success(),
            "flac -t: {}",
            String::from_utf8_lossy(&o.stderr)
        ),
        None => eprintln!("SKIPPED: no flac"),
    }
    if let Ok(o) = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-xerror", "-i"])
        .arg(&f)
        .args(["-f", "null", "-"])
        .output()
    {
        assert!(
            o.status.success() && o.stderr.is_empty(),
            "ffmpeg: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    } else {
        eprintln!("SKIPPED: no ffmpeg");
    }
}

/// Counter signal: sample n carries the value n + 1 (exact in 24 bits).
fn ramp(n: u64) -> f32 {
    (n % 4_000_000) as f32 / 8_388_608.0 + 1.0 / 8_388_608.0
}

#[test]
fn a_restart_continues_the_same_stream_with_silence_for_what_it_dropped() {
    let m = mount();
    let mut p = Pipeline::new(EncoderOptions::new(48_000, "Test"), m.clone()).unwrap();
    let mut l = m.join("a", None).unwrap();
    let first_headers = l.headers();
    let mut t = 7_000_000u64; // the station clock does not start at 0
    for _ in 0..40 {
        let (tp, v) = period(t, 1024, ramp);
        p.feed(tp, &v).unwrap();
        t += 1024;
    }
    p.restart().unwrap();
    // Nothing that had not reached a page survives: the timeline is back at the last page's end.
    let dropped_at = p.timeline_frames();
    assert!(dropped_at > 0 && dropped_at < 40 * 1024, "{dropped_at}");
    assert_eq!(dropped_at % 1024, 0);
    for _ in 0..40 {
        let (tp, v) = period(t, 1024, ramp);
        p.feed(tp, &v).unwrap();
        t += 1024;
    }
    p.finish().unwrap();
    // The listener was not closed, and a new one gets the same headers.
    let mut bytes = first_headers.to_vec();
    loop {
        match l.next(std::time::Duration::ZERO) {
            Next::Page(pg) => bytes.extend_from_slice(&pg.bytes),
            Next::Idle => break,
            Next::Closed(r) => panic!("listener closed: {r:?}"),
        }
    }
    assert_eq!(*m.join("b", None).unwrap().headers(), *first_headers);
    assert_one_logical_stream(&bytes);
    assert_eq!(pages(&bytes).last().unwrap().0 & 4, 4, "EOS at the end");
    independent_decoders_agree(&bytes, "restart");
    let st = &m.stats;
    assert_eq!(
        st.encoder_restarts
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert_eq!(
        st.gap_frames.load(std::sync::atomic::Ordering::Relaxed),
        40 * 1024 - dropped_at,
        "the gap is exactly what was dropped"
    );
    let left: Vec<i32> = decode(bytes).iter().step_by(2).copied().collect();
    let t0 = 7_000_000u64;
    let total = (t - t0) as usize;
    assert!(left.len() >= total, "{} < {total}", left.len());
    // Before the restart point: the audio, exactly (past the opening fade).
    for (k, v) in left.iter().enumerate().take(dropped_at as usize).skip(200) {
        assert_eq!(*v, ((t0 + k as u64) % 4_000_000 + 1) as i32, "frame {k}");
    }
    // What was dropped is silence; after it, the audio is at its right place on the station clock.
    for (k, v) in left
        .iter()
        .enumerate()
        .take(40 * 1024)
        .skip(dropped_at as usize)
    {
        assert_eq!(*v, 0, "frame {k}");
    }
    for (k, v) in left
        .iter()
        .enumerate()
        .take(total - 200)
        .skip(40 * 1024 + 200)
    {
        assert_eq!(*v, ((t0 + k as u64) % 4_000_000 + 1) as i32, "frame {k}");
    }
}

#[test]
fn a_restart_before_any_page_starts_cleanly() {
    let m = mount();
    let mut p = Pipeline::new(EncoderOptions::new(48_000, "Test"), m.clone()).unwrap();
    p.restart().unwrap();
    let (tp, v) = period(5_000, 1024, sine);
    p.feed(tp, &v).unwrap();
    p.restart().unwrap();
    assert_eq!(p.timeline_frames(), 0);
    for i in 1..20 {
        let (tp, v) = period(5_000 + i * 1024, 1024, sine);
        p.feed(tp, &v).unwrap();
    }
    p.finish().unwrap();
    let bytes = collect(&m);
    assert_one_logical_stream(&bytes);
    assert!(decode(bytes).len() >= 20 * 1024 * 2);
}

/// The real thread: a panic in the middle of encoding is caught, the stream continues for the listener that
/// was connected, and the timeline stays exact.
#[test]
fn a_real_panic_in_the_encoder_thread_is_survived() {
    use std::sync::atomic::Ordering::Relaxed;
    use std::time::{Duration, Instant};
    let m = mount();
    let (mut tx, rx) = engine::tap::stream_tap(4 * 48_000, 4 * 48_000 / 1024 + 64);
    let enc = stream::spawn(EncoderOptions::new(48_000, "Test"), rx, m.clone()).unwrap();
    let wait = |what: &str, f: &dyn Fn() -> bool| {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(10), "timed out: {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    wait("headers", &|| {
        !m.join("x", None).unwrap().headers().is_empty()
    });
    let mut l = m.join("a", None).unwrap();
    let mut bytes = l.headers().to_vec();
    let drain = |l: &mut stream::mount::Listener, bytes: &mut Vec<u8>| loop {
        match l.next(Duration::ZERO) {
            Next::Page(pg) => bytes.extend_from_slice(&pg.bytes),
            Next::Idle => break,
            Next::Closed(r) => panic!("listener closed: {r:?}"),
        }
    };
    let mut t = 0u64;
    let mut push = |n: usize, t: &mut u64| {
        for _ in 0..n {
            let (_, v) = period(*t, 1024, ramp);
            assert!(tx.push(*t, &v));
            *t += 1024;
        }
    };
    push(60, &mut t);
    wait("first half encoded", &|| {
        m.stats.frames_encoded.load(Relaxed) >= 59 * 1024
    });
    drain(&mut l, &mut bytes);
    m.hook_panic();
    push(60, &mut t);
    wait("restart", &|| m.stats.encoder_restarts.load(Relaxed) == 1);
    wait("second half encoded", &|| {
        m.stats.frames_encoded.load(Relaxed) >= 119 * 1024
    });
    assert_eq!(m.encoder_state(), stream::EncoderState::Running);
    assert!(!enc.is_finished());
    enc.stop();
    drain(&mut l, &mut bytes);
    assert_one_logical_stream(&bytes);
    let gap = m.stats.gap_frames.load(Relaxed) as usize;
    assert!(gap > 0 && gap <= 2 * 4096 + 1024, "{gap}");
    let left: Vec<i32> = decode(bytes).iter().step_by(2).copied().collect();
    assert!(left.len() >= t as usize);
    // Every frame is either the station's own sample at its own place, or silence (in the gap and its fades):
    // nothing moved or repeated.
    let mut silent = 0;
    for (k, v) in left.iter().enumerate().take(t as usize) {
        let want = ((k as u64) % 4_000_000 + 1) as i32;
        if *v != want {
            assert!(v.abs() <= want, "frame {k}: {v}, want {want} or silence");
            silent += 1;
        }
    }
    // The gap plus the opening and the two edge fades (96 frames each).
    assert!(silent <= gap + 3 * 96, "{silent} frames differ, gap {gap}");
}

/// Panics over and over: the encoder reports `failed` rather than restarting forever unseen.
#[test]
fn an_encoder_that_keeps_failing_is_reported_failed() {
    use std::sync::atomic::Ordering::Relaxed;
    use std::time::{Duration, Instant};
    let m = mount();
    let (mut tx, rx) = engine::tap::stream_tap(4 * 48_000, 4 * 48_000 / 1024 + 64);
    let enc = stream::spawn(EncoderOptions::new(48_000, "Test"), rx, m.clone()).unwrap();
    let mut t = 0u64;
    for k in 1..=5u64 {
        m.hook_panic();
        let start = Instant::now();
        while m.stats.encoder_restarts.load(Relaxed) < k {
            let (_, v) = period(t, 1024, sine);
            tx.push(t, &v);
            t += 1024;
            std::thread::sleep(Duration::from_millis(3));
            assert!(start.elapsed() < Duration::from_secs(10));
        }
    }
    assert_eq!(m.encoder_state(), stream::EncoderState::Failed);
    assert!(!enc.is_finished(), "it keeps trying");
    enc.stop();
}

/// flacenc on real material (M2a review): the office set is 44.1 kHz FLAC, resampled to the 48 kHz output by
/// the engine's decoder. A few minutes of it through the pipeline at levels 5 and 8 decode bit-exactly. Skipped
/// without `~/Developer/hotclock-testdata`.
#[test]
fn the_office_set_resampled_to_48k_round_trips_bit_exact() {
    let root = std::env::var("HOTCLOCK_TESTDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join("Developer/hotclock-testdata")
        })
        .join("office");
    let mut files = vec![];
    fn walk(d: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        let mut v: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        v.sort();
        for p in v {
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "flac") {
                out.push(p);
            }
        }
    }
    walk(&root, &mut files);
    if files.is_empty() {
        eprintln!("SKIPPED: no office set at {}", root.display());
        return;
    }
    // 90 s from each of two tracks, back to back: 3 minutes per level.
    let mut program: Vec<f32> = vec![];
    for (n, f) in files.iter().take(2).enumerate() {
        let mut d = engine::decode::FileDecoder::open(f, 48_000, 30.0).expect("open");
        assert_eq!(d.source_rate(), Some(44_100), "{}", f.display());
        let target = (n + 1) * 90 * 96_000;
        while program.len() < target && d.decode_next(&mut program).expect("decode") {}
        program.truncate(target);
    }
    program.truncate(program.len() / 2048 * 2048);
    let want: Vec<i32> = program.iter().map(|x| to_i24(*x)).collect();
    let peak = want.iter().map(|x| x.unsigned_abs()).max().unwrap();
    assert!(peak > 1 << 20, "real music, not silence: peak {peak}");
    for level in [5u8, 8] {
        let m = Mount::new(MountConfig {
            burst_ms: 600_000,
            ..mount().cfg.clone()
        });
        let mut o = EncoderOptions::new(48_000, "Test");
        o.flac_level = level;
        let mut p = Pipeline::new(o, m.clone()).unwrap();
        for (i, chunk) in program.chunks_exact(2048).enumerate() {
            let tp = TapPeriod {
                start_frame: i as u64 * 1024,
                frames: 1024,
            };
            p.feed(tp, chunk).unwrap();
        }
        p.finish().unwrap();
        let bytes = collect(&m);
        let ratio = bytes.len() as f64 / (want.len() as f64 * 3.0);
        let got = decode(bytes);
        let fade = 96 * 2;
        assert_eq!(
            &got[fade..want.len() - fade],
            &want[fade..want.len() - fade],
            "level {level}"
        );
        println!(
            "office set, level {level}: {:.1} min bit-exact, {:.0} % of 24-bit PCM",
            want.len() as f64 / 96_000.0 / 60.0,
            ratio * 100.0
        );
    }
}

/// Stopping the thread encodes what is still queued in the tap (at shutdown rt-out stops first, so the queue
/// is the end of the program). Before the review the loop left it behind: a tail of up to the queued periods
/// was missing from the stream, intermittently.
#[test]
fn stopping_the_encoder_drains_the_tap_first() {
    let m = mount();
    let (mut tx, rx) = engine::tap::stream_tap(4 * 48_000, 4 * 48_000 / 1024 + 64);
    // Everything is queued before the thread can take any of it.
    for i in 0..100u64 {
        let (_, v) = period(i * 1024, 1024, ramp);
        assert!(tx.push(i * 1024, &v));
    }
    let enc = stream::spawn(EncoderOptions::new(48_000, "Test"), rx, m.clone()).unwrap();
    enc.stop();
    let bytes = collect(&m);
    assert_eq!(pages(&bytes).last().unwrap().0 & 4, 4, "EOS at the end");
    let left: Vec<i32> = decode(bytes).iter().step_by(2).copied().collect();
    assert!(left.len() >= 100 * 1024, "{}", left.len());
    for (k, v) in left.iter().enumerate().take(100 * 1024 - 200).skip(200) {
        assert_eq!(*v, k as i32 + 1, "frame {k}");
    }
}
