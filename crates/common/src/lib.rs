//! Types shared by every crate: the `Clock` abstraction (architecture 1.6) and configuration
//! loading with errors that name the file and the key (milestone M1.1).

#![forbid(unsafe_code)]

pub mod chain;
pub mod clock;
pub mod config;
pub mod localtime;
pub mod meters;
pub mod timespec;
pub mod transition;

pub use clock::{Clock, RealClock, VirtualClock};
