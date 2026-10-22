//! The station: the conductor's segue executor (milestones M1.4 and M1.4a). It turns items with
//! markers into voices on the mixer's timeline: the transition model of architecture 2.6 (the
//! presets and their resolution live in `common::transition`), the pure boundary planner
//! (`plan`), the executor that loads voices and sends the commands (`conductor`), and the
//! offline scenario renderer (`scenario`) behind `hotclock render --scenario`.
//!
//! The scheduler, the queue and the playlist block model are M1.5 and are not here.

#![forbid(unsafe_code)]

pub mod acts;
pub mod alerts;
pub mod api_view;
pub mod backtime;
pub mod catalog;
pub mod conductor;
pub mod control;
pub mod deck;
pub mod failure;
pub mod item;
pub mod offline;
pub mod plan;
pub mod preflight;
pub mod preview;
pub mod program;
pub mod programming;
pub mod ram;
pub mod replay;
pub mod rng;
pub mod runtime;
pub mod scenario;
pub mod sched;
pub mod side;
pub mod sidecar;
pub mod sim;
pub mod state;
pub mod supervise;
pub mod sync;
pub mod timed;
