//! The native status display (architecture 11): layout and rendering independent of any screen, the station
//! clients, and the KMS output.

pub mod app;
pub mod canvas;
pub mod cvt;
pub mod feed;
pub mod frame;
pub mod headless;
#[cfg(target_os = "linux")]
pub mod kms;
pub mod layout;
pub mod model;
pub mod net;
pub mod output;
pub mod render;
pub mod scene;
pub mod view;
