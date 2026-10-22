//! The tap on the real rt-out thread (M2a acceptance 2, 3 and 4a at the engine level): the stream is bit-exact
//! to the buffer the output received, enabling the stream does not change what the output receives, and a
//! stalled encoder never disturbs the output.

use common::{Clock, RealClock};
use engine::output::FileOutput;
use engine::rt::{RtHandle, RtOptions};
use engine::source::{Source, ToneSource};
use engine::tap::stream_tap;
use engine::wav::WavFormat;
use engine::HwRequest;
use std::io::Cursor;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::Duration;
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
        burst_ms: 600_000,
        max_listeners: 4,
        listener_lag_s: 600,
        expect_listener: false,
    })
}

fn collect(m: &Arc<Mount>) -> Vec<u8> {
    let mut l = m.join("t", None).unwrap();
    let mut out = l.headers().to_vec();
    while let Next::Page(p) = l.next(Duration::ZERO) {
        out.extend_from_slice(&p.bytes);
    }
    out
}

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
        .unwrap();
    let mut fmt = probed.format;
    let track = fmt.default_track().unwrap().clone();
    let mut dec = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .unwrap();
    let mut out = vec![];
    while let Ok(pkt) = fmt.next_packet() {
        let buf = dec.decode(&pkt).unwrap();
        let mut sb = SampleBuffer::<i32>::new(buf.capacity() as u64, *buf.spec());
        sb.copy_interleaved_ref(buf);
        out.extend(sb.samples().iter().map(|s| s >> 8));
    }
    out
}

fn read_wav_f32(path: &std::path::Path) -> Vec<f32> {
    let b = std::fs::read(path).unwrap();
    // The data chunk of a 32-bit float WAV: find "data".
    let at = b.windows(4).position(|w| w == b"data").unwrap();
    let len = u32::from_le_bytes(b[at + 4..at + 8].try_into().unwrap()) as usize;
    b[at + 8..at + 8 + len]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

struct Run {
    xruns: u32,
    frames: u64,
}

/// Run `source` on the real rt-out thread into a paced WAV for `frames` frames, with or without the stream
/// (encoder thread included). Returns the mount when the stream was on.
fn run(
    path: &std::path::Path,
    source: Box<dyn Source>,
    frames: u64,
    stream: bool,
    stall_ms: Option<(u64, u64)>,
) -> (Run, Option<Arc<Mount>>) {
    let clock: Arc<dyn Clock> = Arc::new(RealClock::new());
    let req = HwRequest::defaults();
    let out = FileOutput::create(path, &req, WavFormat::F32, Some(clock.clone())).unwrap();
    let mut opts = RtOptions {
        max_frames: Some(frames),
        ..RtOptions::default()
    };
    let mut enc = None;
    let mut mnt = None;
    if stream {
        let (tx, rx) = stream_tap(4 * 48_000, 4 * 48_000 / 1024 + 64);
        opts.stream_tap = Some(tx);
        let m = mount();
        enc = Some(
            stream::spawn(stream::EncoderOptions::new(48_000, "Test"), rx, m.clone()).unwrap(),
        );
        mnt = Some(m);
    }
    let h = RtHandle::start(Box::new(out), source, clock, opts).unwrap();
    if let (Some((after, ms)), Some(m)) = (stall_ms, &mnt) {
        std::thread::sleep(Duration::from_millis(after));
        m.hook_stall(ms);
    }
    while !h.is_finished() {
        std::thread::sleep(Duration::from_millis(20));
    }
    let stats = h.stats();
    let mut out = h.wait();
    out.finish().unwrap();
    if let Some(e) = enc {
        e.stop();
    }
    (
        Run {
            xruns: stats.xruns,
            frames: stats.frames_written,
        },
        mnt,
    )
}

#[test]
fn the_stream_is_bit_exact_to_what_the_output_received_and_leaves_it_untouched() {
    let dir = fixtures::testdir::process_dir().join(format!("tap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let frames = 5 * 48_000;
    let (a, ma) = run(
        &dir.join("with.wav"),
        Box::new(ToneSource::new(48_000, 1000.0, -20.0)),
        frames,
        true,
        None,
    );
    let (b, _) = run(
        &dir.join("without.wav"),
        Box::new(ToneSource::new(48_000, 1000.0, -20.0)),
        frames,
        false,
        None,
    );
    assert_eq!((a.xruns, b.xruns), (0, 0));
    assert_eq!(a.frames, frames);
    // Acceptance 3: the output's samples are identical with the stream on and off.
    assert_eq!(
        std::fs::read(dir.join("with.wav")).unwrap(),
        std::fs::read(dir.join("without.wav")).unwrap(),
        "the output differs when the stream is enabled"
    );
    // Acceptance 2: the decoded stream equals the output's buffer, converted to 24 bits.
    let m = ma.unwrap();
    assert_eq!(m.stats.gaps.load(Relaxed), 0);
    assert_eq!(m.tap_stats().unwrap().dropped_periods.load(Relaxed), 0);
    let want: Vec<i32> = read_wav_f32(&dir.join("with.wav"))
        .iter()
        .map(|x| to_i24(*x))
        .collect();
    let got = decode(collect(&m));
    assert!(got.len() >= want.len());
    let skip = 96 * 2; // the 2 ms fade-in
    let end = want.len() - 2048; // the held-back 2 ms and the padded last block
    assert_eq!(
        &got[skip..end],
        &want[skip..end],
        "stream differs from the output"
    );
    // The decoded 1 kHz tone is at -20 dBFS RMS within 0.05 dB.
    let mid = &got[48_000..48_000 + 2 * 48_000];
    let rms = (mid
        .iter()
        .map(|x| (f64::from(*x) / 8_388_608.0).powi(2))
        .sum::<f64>()
        / mid.len() as f64)
        .sqrt();
    let db = 20.0 * rms.log10();
    assert!((db + 20.0).abs() < 0.05, "tone level {db} dBFS");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Sample n carries n (a counter, exact in 24 bits) in both channels: a repeat or a gap shows in the values.
struct Counter {
    n: u64,
}

impl Source for Counter {
    fn render(&mut self, out: &mut [f32]) {
        for f in out.chunks_exact_mut(2) {
            let v = ((self.n % 8_000_000) + 1) as f32 / 8_388_608.0;
            f[0] = v;
            f[1] = v;
            self.n += 1;
        }
    }
}

#[test]
fn a_stalled_encoder_never_disturbs_the_output_and_the_timeline_stays_exact() {
    let dir = fixtures::testdir::process_dir().join(format!("stall-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // 24 s of program; the encoder sleeps for 10 s from t = 3 s.
    let frames = 24 * 48_000;
    let (r, m) = run(
        &dir.join("o.wav"),
        Box::new(Counter { n: 0 }),
        frames,
        true,
        Some((3000, 10_000)),
    );
    let m = m.unwrap();
    assert_eq!(r.xruns, 0, "the output underran");
    assert_eq!(r.frames, frames);
    let tap = m.tap_stats().unwrap();
    let dropped = tap.dropped_frames.load(Relaxed);
    assert!(dropped > 0, "nothing was dropped");
    assert_eq!(m.stats.gaps.load(Relaxed), 1);
    assert_eq!(m.stats.gap_frames.load(Relaxed), dropped);
    // The lag peaked near the ring's capacity (4 s) and is back to zero.
    assert!(m.stats.encoder_lag_max_ms.load(Relaxed) >= 3000);
    assert_eq!(m.stats.encoder_lag_ms.load(Relaxed), 0);
    // The stream: values keep counting by one, the gap is silence of exactly the dropped length, nothing
    // repeats, and the total length is the program's.
    let got = decode(collect(&m));
    let left: Vec<i32> = got.iter().step_by(2).copied().collect();
    assert!(left.len() >= frames as usize, "{} < {frames}", left.len());
    let mut silent = 0u64;
    let (mut g0, mut g1) = (usize::MAX, 0usize);
    for (k, v) in left.iter().enumerate().take(frames as usize) {
        if *v == 0 && k > 200 {
            silent += 1;
            g0 = g0.min(k);
            g1 = k + 1;
        }
    }
    // Outside the 2 ms fades at the gap's edges (and the first 2 ms), frame k carries the value k + 1: the
    // timeline is the program's, so nothing repeated and nothing moved.
    for (k, v) in left.iter().enumerate().take(frames as usize - 2048) {
        let in_fade = k < 200 || (g0 - 200..g0).contains(&k) || (g1..g1 + 200).contains(&k);
        if *v != 0 && !in_fade {
            assert_eq!(*v, k as i32 + 1, "frame {k}");
        }
    }
    // The silence is the dropped frames plus the two 2 ms fades around the gap (and the first 2 ms).
    assert!(
        silent >= dropped && silent <= dropped + 400,
        "silence {silent} frames, dropped {dropped}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
