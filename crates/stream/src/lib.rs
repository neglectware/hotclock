//! The network audio output (architecture 1.3a, milestone M2a): a station's program as a lossless
//! FLAC-in-Ogg stream.
//!
//! * [`flac`]: frames from a pure-Rust encoder.
//! * [`ogg`]: page writer and the Ogg FLAC header packets.
//! * [`mount`]: the page ring and the listeners that follow it.
//! * [`encoder`]: the `stream-enc` thread, from the rt-out tap ring to pages, with gap filling.
//! * [`testsrc`]: the timing-marker source of `--stream-test` (the drift soak).
//!
//! Nothing here runs on the rt-out thread, and nothing waits for a listener.

#![forbid(unsafe_code)]

pub mod encoder;
pub mod flac;
pub mod mount;
pub mod ogg;
pub mod testsrc;

pub use encoder::{spawn, EncoderOptions, EncoderThread};
pub use mount::{CloseReason, EncoderState, Listener, Mount, MountConfig, Next};
