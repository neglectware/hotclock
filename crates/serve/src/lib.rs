//! The network side of Hotclock (milestone M1.7): a small HTTP/1.1 and WebSocket server, the security
//! rules of architecture 13, the meter thread and its 220-byte frame (11.3), the station API and the
//! library API, `/health` and `/metrics` (12), the notifier (12.1) and the embedded web pages (9, 11).

#![forbid(unsafe_code)]

pub mod analyzer;
pub mod chain_api;
pub mod config_history;
pub mod config_history_http;
pub mod config_labels;
pub mod help;
pub mod http;
pub mod library_http;
pub mod meters;
pub mod multipart;
pub mod notify;
pub mod observe;
pub mod plex_http;
pub mod security;
pub mod settings;
pub mod settings_http;
pub mod station_api;
pub mod station_http;
pub mod stations_admin;
pub mod stations_http;
pub mod stream_http;
pub mod sysinfo;
pub mod util;
pub mod web;
pub mod ws;
