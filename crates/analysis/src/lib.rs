//! Audio inspection and analysis. M1.1 has the format census; M1.3 adds the analysis pass
//! (loudness, envelopes, markers, ramps).

#![forbid(unsafe_code)]

pub mod analyze;
pub mod blocks;
pub mod census;
pub mod decode;
pub mod effective;
pub mod env;
pub mod kweight;
pub mod markers;
pub mod quiet;
pub mod ramp;
pub mod ramp_fit;
pub mod tail;
