//! Example programs the user can create from the web UI (architecture 4.9, 9; milestone M5). Each is a
//! playlist of blocks built from the same parameters the block editor writes, so what an example makes is
//! what a person would have dragged together.

use crate::playlists::{Mode, NewBlock, NewPlaylist};
use serde_json::{json, Value};

/// Where the program's mute message goes.
#[derive(Debug, Clone)]
pub struct OscTarget {
    pub host: String,
    pub port: u16,
    pub address: String,
    /// The value sent: 1 mutes a WING channel.
    pub value: i32,
}

impl OscTarget {
    /// The Behringer WING channel mute (port 2223, `/ch/N/mute`, 1 = muted).
    pub fn wing_channel_mute(host: &str, channel: u32) -> OscTarget {
        OscTarget {
            host: host.into(),
            port: 2223,
            address: format!("/ch/{channel}/mute"),
            value: 1,
        }
    }
}

/// What an example needs from the library: the two playlists it plays and where the OSC message goes.
#[derive(Debug, Clone)]
pub struct ItaMonday {
    pub name: String,
    pub station_id: Option<String>,
    /// The pre-show playlist (id).
    pub pre_show: i64,
    /// The elevator-bed playlist (id).
    pub elevator: i64,
    pub osc: OscTarget,
    /// Mondays at 18:45 are expected (the cron of the `start` block).
    pub expected_cron: String,
}

impl ItaMonday {
    pub fn new(pre_show: i64, elevator: i64, osc: OscTarget) -> ItaMonday {
        ItaMonday {
            name: "Eve Monday".into(),
            station_id: None,
            pre_show,
            elevator,
            osc,
            expected_cron: "45 18 * * 1".into(),
        }
    }

    pub fn playlist(&self) -> NewPlaylist {
        NewPlaylist {
            name: self.name.clone(),
            mode: Mode::Sequential,
            no_repeat_minutes: 0,
            separation: Value::Object(Default::default()),
            station_id: self.station_id.clone(),
        }
    }

    /// The six blocks of 4.9, in list order.
    pub fn blocks(&self) -> Vec<NewBlock> {
        let rel = |ms: i64| json!({"kind": "relative", "offset_ms": ms});
        vec![
            NewBlock {
                name: "Expected Mondays".into(),
                kind: "start".into(),
                trigger: "first".into(),
                priority: 100,
                params: json!({
                    "schedule": [{"cron": self.expected_cron}], "anchor": "manual", "arm_window_min": 60,
                    "fallback_start": {"enabled": false}
                }),
            },
            NewBlock {
                name: "Pre-show music".into(),
                kind: "play".into(),
                trigger: "sequence".into(),
                priority: 100,
                params: json!({"source": {"playlist_id": self.pre_show}, "until": "next"}),
            },
            NewBlock {
                name: "Fade out".into(),
                kind: "fade_stop".into(),
                trigger: "timed".into(),
                priority: 100,
                params: json!({"when": rel(14 * 60_000 + 40_000), "seconds": 7, "curve": "cosine"}),
            },
            NewBlock {
                name: "Mute the Wing".into(),
                kind: "osc_out".into(),
                trigger: "timed".into(),
                priority: 100,
                params: json!({
                    "when": rel(15 * 60_000),
                    "host": self.osc.host, "port": self.osc.port, "address": self.osc.address,
                    "args": [{"type": "i", "value": self.osc.value}]
                }),
            },
            NewBlock {
                name: "Elevator bed".into(),
                kind: "play".into(),
                trigger: "timed".into(),
                priority: 100,
                params: json!({
                    "when": rel(15 * 60_000 + 1000),
                    "source": {"playlist_id": self.elevator}, "until": "next"
                }),
            },
            NewBlock {
                name: "Stop after midnight".into(),
                kind: "stop".into(),
                trigger: "timed".into(),
                priority: 100,
                params: json!({
                    "when": {"kind": "local", "at": "00:30", "day_offset": 1},
                    "how": "now", "fade_ms": 3000, "late_policy": "skip"
                }),
            },
        ]
    }

    /// Create the playlist and its blocks in the library; returns the playlist id.
    pub fn create(&self, lib: &crate::Library) -> crate::Result<i64> {
        let id = lib.create_playlist(&self.playlist())?;
        for b in self.blocks() {
            lib.add_block_to_playlist(id, &b)?;
        }
        Ok(id)
    }
}
