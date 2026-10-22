//! Run a captured program through the processing chain offline and count what the AGC card would show.
//!
//! Input: raw interleaved stereo `f32` little-endian at 48 kHz (for example
//! `ffmpeg -i capture.ogg -f f32le -ac 2 -ar 48000 capture.f32`).
//!
//! ```text
//! cargo run --release -p engine --example agc_states -- capture.f32 [preset] [block_frames] [poll_ms] [trace.csv]
//! ```
//!
//! The chain is the named preset (default `radio`), run in blocks of `block_frames` (default 256) with the
//! meters published as on air. Every `poll_ms` of program (default 250, the card's poll) the state is read the
//! way `GET /api/v1/chain/agc` reads it. It prints the share of polls in each state, the share of blocks in each
//! state the AGC reports block by block, and the spread of the AGC's gain. With a fifth argument it writes one row
//! per poll: time, state, gain, fast level, program level.

use common::chain::preset_config;
use engine::chain::agc::AgcState;
use engine::chain::{Chain, ChainMeters};
use std::io::{Read, Write};
use std::sync::Arc;

const RATE: u32 = 48_000;

fn name(s: AgcState) -> &'static str {
    match s {
        AgcState::Riding => "riding",
        AgcState::Gated => "gated",
        AgcState::RidingFade => "fade",
        AgcState::Clamping => "clamping",
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let path = a
        .get(1)
        .expect("usage: agc_states <file.f32> [preset] [block] [poll_ms] [trace.csv]");
    let preset = a.get(2).map_or("radio", String::as_str);
    let block: usize = a.get(3).map_or(256, |s| s.parse().unwrap());
    let poll_ms: f64 = a.get(4).map_or(250.0, |s| s.parse().unwrap());
    let mut raw = vec![];
    std::fs::File::open(path)
        .unwrap()
        .read_to_end(&mut raw)
        .unwrap();
    let mut x: Vec<f32> = raw
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let frames = x.len() / 2;
    let cfg = preset_config(preset).expect("no such preset");
    let meters = Arc::new(ChainMeters::default());
    let mut ch = Chain::build(&cfg, RATE, meters.clone());
    let poll = (poll_ms * 0.001 * f64::from(RATE)) as usize;
    let mut trace = a
        .get(5)
        .map(|p| std::io::BufWriter::new(std::fs::File::create(p).unwrap()));
    if let Some(t) = &mut trace {
        writeln!(t, "t,state,gain_db,fast_dbfs,program_dbfs").unwrap();
    }
    let (mut polls, mut blocks) = ([0usize; 4], [0usize; 4]);
    let mut gains = vec![];
    let (mut done, mut next_poll) = (0usize, poll);
    for c in x.chunks_mut(block * 2) {
        ch.process(c, true);
        done += c.len() / 2;
        blocks[ch.agc_state() as usize] += 1;
        if done >= next_poll {
            next_poll += poll;
            let v = meters.agc_view();
            polls[v.state as usize] += 1;
            gains.push(v.gain_db);
            if let Some(t) = &mut trace {
                writeln!(
                    t,
                    "{:.3},{},{:.2},{:.2},{:.2}",
                    done as f64 / f64::from(RATE),
                    name(v.state),
                    v.gain_db,
                    v.fast_db,
                    v.program_db
                )
                .unwrap();
            }
        }
    }
    let pct =
        |c: &[usize; 4], i: usize| 100.0 * c[i] as f64 / c.iter().sum::<usize>().max(1) as f64;
    println!(
        "{path}: {:.1} s, preset {preset}, blocks of {block} frames, a poll every {poll_ms} ms",
        frames as f64 / f64::from(RATE)
    );
    for s in [
        AgcState::Riding,
        AgcState::Gated,
        AgcState::RidingFade,
        AgcState::Clamping,
    ] {
        println!(
            "  {:<9} card {:5.1} %   blocks {:5.1} %",
            name(s),
            pct(&polls, s as usize),
            pct(&blocks, s as usize)
        );
    }
    gains.sort_by(f64::total_cmp);
    let q = |p: f64| gains[((gains.len() - 1) as f64 * p) as usize];
    println!(
        "  gain dB: p5 {:.1}  p50 {:.1}  p95 {:.1}",
        q(0.05),
        q(0.5),
        q(0.95)
    );
}
