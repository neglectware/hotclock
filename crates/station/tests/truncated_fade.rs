//! The automatic fade of an item flagged `truncated_fade`, rendered through the real conductor and mixer: a quiet
//! track (about -31 LUFS) whose file stops in the middle of its fade, at about -60 dBFS, gets the full +12 dB of
//! normalization. Without the fade its tail plays at about -48 dBFS and stops dead; with it, the sound reaches
//! silence at the cue-out. The fade is applied after the item's gain (the mixer multiplies them), so the gain
//! cannot keep it from reaching silence.

mod support;

use fixtures::files::write_wav24;
use fixtures::signals::{pink, ramp_db};
use station::scenario::Rendered;
use support::*;

fn quiet_cut_track(dir: &std::path::Path) {
    let mut x = pink(RATE, -33.0, 40.0, 11);
    let mut fade = pink(RATE, -33.0, 24.0, 12);
    ramp_db(&mut fade, 0.0, -27.0);
    x.append(&mut fade);
    write_wav24(&dir.join("cut.wav"), &x, RATE, true).unwrap();
    write_wav24(
        &dir.join("next.wav"),
        &pink(RATE, -20.0, 20.0, 13),
        RATE,
        true,
    )
    .unwrap();
}

fn render_with(dir: &std::path::Path, preset: &str, fade: &str, queue: &str) -> Rendered {
    let yaml = format!(
        "station: {{ preset: {preset}, target_lufs: -18, max_boost_db: 12{fade} }}\nitems:\n  a: {{ file: cut.wav }}\n  b: {{ file: next.wav }}\nqueue:\n{queue}"
    );
    run(&yaml, dir)
}

fn rms_at(r: &Rendered, from: u64, to: u64) -> f64 {
    rms_db(&r.samples[from as usize * 2..to as usize * 2])
}

#[test]
fn the_automatic_fade_reaches_silence_at_the_cue_out_after_the_gain() {
    let dir = fixtures::testdir::process_dir().join(format!("st-tf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    quiet_cut_track(&dir);
    let alone = "  - { item: a }\n";

    // The item alone (no successor) under plexamp, the automatic fade on (the default, 2 s).
    let r = render_with(&dir, "plexamp", "", alone);
    let a = voice(&r, "a");
    let cue_out = a.cue_out_frame;
    let end = a.ended_frame.unwrap();
    let stop = a.stop.as_ref().expect("a stop");
    let tail = rms_at(&r, cue_out - 480, cue_out);
    let before = rms_at(&r, cue_out - 2 * 48_000 - 24_000, cue_out - 2 * 48_000);
    let after = r.samples[cue_out as usize * 2..]
        .iter()
        .fold(0f32, |m, v| m.max(v.abs()));
    println!(
        "gain {:+.1} dB, cue-out frame {cue_out}, voice ended {end}, stop {stop:?}; \
         level before the fade {before:.1} dBFS, last 10 ms before the cue-out {tail:.1} dBFS, peak after it {after:e}",
        a.gain_db
    );
    assert!(a.gain_db > 11.9, "the full boost");
    assert!(
        before > -52.0 && before < -40.0,
        "the tail is audible after the gain"
    );
    assert!(tail < -100.0, "silence at the cue-out");
    assert!(end <= cue_out + 96, "the voice ends at the cue-out");
    assert_eq!(after, 0.0);

    // Off (`truncated_fade = false`): the tail plays at the same level up to the file's end and stops dead.
    let r0 = render_with(&dir, "plexamp", ", truncated_fade_s: 0", alone);
    let a0 = voice(&r0, "a");
    let end0 = a0.ended_frame.unwrap();
    let last = rms_at(&r0, end0 - 480, end0);
    println!("without the fade: voice ended {end0} (file end), last 10 ms {last:.1} dBFS");
    assert!(a0.stop.is_none());
    assert!(last > -60.0, "without the fade the tail stops dead");

    // With a successor (`none`: the next item starts at the cue-out): the same fade, before the next item.
    let r = render_with(&dir, "none", "", "  - { item: a }\n  - { item: b }\n");
    let (a, b) = (voice(&r, "a"), voice(&r, "b"));
    let tail = rms_at(
        &r,
        a.cue_out_frame - 480,
        a.cue_out_frame.min(b.start_frame),
    );
    println!(
        "a then b (none): b starts {}, a's cue-out {}, last 10 ms of a {tail:.1} dBFS",
        b.start_frame, a.cue_out_frame
    );
    assert!(tail < -100.0);
}
