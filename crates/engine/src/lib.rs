//! The audio engine: outputs, the real-time output thread, sources.
//!
//! Rules for the `rt-out` thread (architecture 1.2), which this crate follows from the start:
//! it never allocates, frees, locks, logs, or touches files or the database. It talks to the rest
//! of the process through `rtrb` rings and atomics only. Everything it needs is allocated before it
//! starts. The one exception is the `File` output backend, which writes a WAV file and is a test
//! and offline-render tool, never used on air.
//!
//! Milestone M1.1: the `Output` trait and its `Alsa`, `Null` and `File` backends, ALSA
//! hardware-parameter negotiation (logic separate from the syscalls), the rt-out thread with xrun
//! and device-loss recovery, a tone source, and the offline tone renderer.
//!
//! Milestone M1.2: file decoding and resampling (`decode`), the decoder pool with head preloads
//! (`pool`), voice rings (`feed`), the eight-voice mixer with envelopes, declick, safety limiter
//! and master gain (`mixer`, `envelope`, `limiter`), and a sequential player (`player`).

#![forbid(unsafe_code)]

pub mod capture;
pub mod chain;
pub mod convert;
pub mod decode;
pub mod envelope;
pub mod fault;
pub mod feed;
pub mod limiter;
pub mod loudness;
pub mod mixer;
pub mod negotiate;
pub mod output;
pub mod player;
pub mod pool;
pub mod render;
pub mod rt;
pub mod source;
pub mod sys;
pub mod tap;
pub mod testing;
pub mod tplimiter;
pub mod wav;

#[cfg(target_os = "linux")]
pub mod alsa_out;

pub use negotiate::{Granted, HwRequest};
pub use output::{open_output, open_output_trimmed, Output, OutputError, OutputSpec};
pub use rt::{Command, CommandKind, Event, EventKind, RtHandle, RtOptions, Stats};
pub use source::{Source, ToneSource};
