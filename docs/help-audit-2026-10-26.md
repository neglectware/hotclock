# Help coverage audit, 2026-10-26

Base: `auto-unload` at ba329f9 (one commit past b123532: a BUILDLOG entry and a test fix). Scope: every setting the
operator can change on the Settings page (`crates/serve/web/settings.js`, built from `crates/serve/src/settings.rs`),
and every key of `/etc/hotclock/hotclock.toml` that `docs/deploy.md` documents. Text changes only.

## How the Help covers settings

- The **Settings reference** (`help/settings-reference.md`) has a generated table (`{{generated:settings}}`). It is
  built from the same catalog as the Settings page. So every catalog key has a row with its type, range, default,
  when it takes effect and its help line. The tests in `crates/serve/src/help.rs` fail when a key has no group or no
  help line. The "Default stated" column below is "yes" for every key for that reason.
- What the generated row does not give is the hand-written explanation: when to change a key. The "Help page" column
  names the pages that explain the key in prose. "missing" would mean only the generated row covers it. Every key
  that was missing now has a short entry, marked "(added in this audit)".
- The `hotclock.toml` keys that `docs/deploy.md` documents are `time_zone`, `data_dir`, `stations_dir`, `run_dir`,
  `library.listen`, `brand.display_name`, `allowed_hosts`, `secrets_file`, `programs.unload_after_min` and the
  `display.*` keys (`enabled`, `mode`, `diagonal_in`, `view_distance_cm`, `rotate_processing`, `proc_range_db`,
  `proc_spectrum`, `night_*`). All of them are in the catalog, so they are rows of the table below. deploy.md
  documents no global key that the catalog lacks.

## Help pages

| Page | What it covers |
|---|---|
| `overview` | This is the full documentation of Hotclock. It is built into the program, so it always matches the version on the box you are using. |
| `getting-started` | This page takes a new box from the package file to a station that plays. It also shows where you do each step in the web UI. |
| `stream-obs` | This page explains the lossless network stream that a station can serve. It also tells you how to play the stream in OBS, so that a station needs no sound card at all. |
| `on-air` | This page shows what a station is playing now. It also has the keys that change it: the transport keys, the program, the queue and the alerts. |
| `library` | The Library page holds every audio file that the box can play. Here you add files, search and filter them, play an album, and open an item to change how it plays. |
| `item-editor` | The item editor shows one item of the library. It has the item's details, its waveform with the markers, a player, and the item's own transition settings. The markers tell the station where to start the item, where the next item comes in, and where to end it. |
| `plex-tools` | The Plex tools copy playlists from a Plex Media Server into the library, and fill in missing cover pictures and tags from Plex. They only read from Plex, and each import is a one-time copy. The tools are the Plex card on the [Library](library) page. |
| `playlists` | A playlist is a named list of what a station plays. On the Playlists page you create playlists, fill them with songs and albums, and set their order. You also build a program from blocks here: start, play, wait, loop, fade, stop and messages to other equipment. |
| `timed-blocks` | A timed block is an entry of a playlist that acts at a set time. For example, it plays a station ID near the top of the hour, switches to another playlist at 18:00, or fades the music out and stops at 00:30. |
| `insert-rules` | An insert rule plays an item from a category after every N songs. For example: a liner every four songs, or a station ID every three songs. This page lists the rules, and you add or remove rules here. |
| `schedule` | The Schedule page arranges a station's programming in time. It shows a weekly grid of the playlists that start by themselves, the station-wide blocks such as the hourly ID, and one-time changes. It also shows what happens next and what happened recently. |
| `history` | The History page is the as-run log. It lists everything the station played, newest first. For each item it shows how the item ended and how close it started to its planned time. |
| `notifications` | The Notifications page sends email when something goes wrong or needs a person. For each kind of event, you decide how the message is sent. |
| `settings` | The Settings page changes every setting of a station and of the box from the web UI. No one has to edit a file by hand. |
| `stations` | The Stations page creates, renames, deletes, starts and stops stations. With it, nobody has to log in to the box to do these things. |
| `system` | The System page shows the health of the library service and of every station. On it you set each station's output level and crossfade style, and you see the items that are in quarantine. |
| `block-engine` | This page explains how a playlist of blocks runs as a program. It covers where the program's times come from, what the cursor does, how waits, loops and nested playlists work, how back-timing makes a song end on time, and what the blocks that send messages do. |
| `crossfades` | This page explains how one item hands over to the next. It describes the five crossfade styles and tells you where to set a style for a station, a category, an item or one single transition. |
| `markers-and-analysis` | This page explains what Hotclock measures in each file at import. It describes the six markers that tell the station where to start, join and end the item, the flags that the analysis can set, and how Hotclock matches the loudness of items. |
| `output-and-level` | This page follows the sound from the library to the sound card and the stream. It explains each control that sets the level: the loudness target, the safety limiter and the output trim. It also describes the loudness controller and gives a summary of the processing chain, which has its own page. |
| `processing` | The processing chain is the broadcast processor between the mix and the limiters. It is a fixed row of optional blocks. Each block has an on/off switch, all its settings in view and its own presets. You edit the chain on the **Processing** page. |
| `status-display` | The status display is a screen for the room where the station runs. It shows what plays, what is next, the meters and any alarm, in text large enough to read from across the room. You control what it shows from the web UI. |
| `failures-and-recovery` | This page explains what the station does when something goes wrong. It tells you what to expect, what the station corrects by itself, and when it needs you. |
| `alerts` | This page lists every alert code. For each code it gives the meaning, the default delivery and what to do. |
| `stream-deck` | This page explains how to put a Stream Deck in front of a station. It tells you how to install the plugin, how to tell it where the box is, and how to set up the keys. |
| `upgrading-backup` | This page tells you how to install a new version with no surprises, how the box protects its databases, and how to put a snapshot back. |
| `troubleshooting` | This page lists the most common problems. For each one it says what to check and what to do, in the order that usually finds the cause fastest. |
| `command-line` | This page lists the commands of the `hotclock` program, what each one does, and the options you need most. The API can do everything that the web pages do, and a few jobs are done only here. |
| `settings-reference` | This page lists every setting of a station and of the box. For each setting, it gives the type, the range, the default, when a change takes effect, and what the setting does. |
| `playlist-checks` | This page lists every message that the playlist checker can show. For each message it gives the meaning and what to do. |
| `glossary` | This page explains the words that Hotclock and these pages use. The words are in alphabetical order. |

## Settings

The table lists the 188 keys of the catalog, in the order of the Settings page. "Help page" is the hand-written
coverage; every key also has its generated row in `settings-reference`.

| Setting | Settings page (tab: card) | Help page | Default stated | Default |
|---|---|---|---|---|
| `name` | Station: Identity | settings, stations | yes | `(none)` |
| `display_name` | Station: Identity | stations | yes | `(none)` |
| `listen` | Station: Identity | settings-reference (notes) (added in this audit) | yes | `(none)` |
| `device` | Station: Sound output | settings, stations, stream-obs, troubleshooting, settings-reference (notes) | yes | `(none)` |
| `rate` | Station: Sound output | settings-reference (notes) | yes | `48000` |
| `period_frames` | Station: Sound output | alerts, output-and-level, settings-reference (notes) | yes | `1024` |
| `periods` | Station: Sound output | alerts, output-and-level, settings-reference (notes) | yes | `8` |
| `formats` | Station: Sound output | settings-reference (notes) | yes | `[S32_LE,S24_3LE,S16_LE]` |
| `target_lufs` | Station: Loudness and crossfade | alerts, markers-and-analysis, output-and-level, settings-reference (notes) | yes | `-18.0` |
| `max_boost_db` | Station: Loudness and crossfade | markers-and-analysis, output-and-level, settings-reference (notes) | yes | `12.0` |
| `peak_allowance_db` | Station: Loudness and crossfade | markers-and-analysis, output-and-level, settings-reference (notes) | yes | `6.0` |
| `truncated_fade` | Station: Loudness and crossfade | crossfades, item-editor, library, markers-and-analysis, settings-reference (notes) | yes | `true` |
| `truncated_fade_s` | Station: Loudness and crossfade | crossfades, settings-reference (notes) | yes | `2.0` |
| `output_trim_db` | Station: Loudness and crossfade | output-and-level, settings-reference (notes) | yes | `0.0` |
| `default_playlist` | Station: Playlists | getting-started, playlists, schedule, settings, stations, settings-reference (notes) | yes | `(none)` |
| `transition_preset` | Station: Loudness and crossfade | crossfades, glossary, settings, stations | yes | `plexamp` |
| `emergency_playlist` | Station: Playlists | alerts, failures-and-recovery, getting-started, settings, stations, settings-reference (notes) | yes | `(none)` |
| `stopped_is_normal` | Station: Playlists | settings-reference (notes) (added in this audit) | yes | `false` |
| `ram_loop_seconds` | Station: Playlists | failures-and-recovery, getting-started | yes | `300` |
| `lock_ahead_s` | Station: Safety | glossary, on-air, settings-reference (notes) | yes | `20` |
| `silence_dbfs` | Station: Safety | failures-and-recovery, settings-reference (notes) | yes | `-50.0` |
| `silence_s` | Station: Safety | failures-and-recovery, settings-reference (notes) | yes | `15` |
| `restart_alert_clear_s` | Station: Safety | alerts, failures-and-recovery, settings-reference (notes) | yes | `600` |
| `processing_preset` | Station: Safety | output-and-level, processing, stations | yes | `bypass` |
| `library` | Station: Library | stations | yes | `(none)` |
| `rate_trim_ppm` | Station: Sound output | stream-obs, troubleshooting, settings-reference (notes) | yes | `0.0` |
| `stream.enabled` | Station: Network stream | stream-obs (added in this audit) | yes | `false` |
| `stream.mount` | Station: Network stream | stream-obs (added in this audit) | yes | `program` |
| `stream.codec` | Station: Network stream | stream-obs (added in this audit) | yes | `flac` |
| `stream.flac_level` | Station: Network stream | stream-obs | yes | `5` |
| `stream.opus_kbps` | Station: Network stream | stream-obs (added in this audit) | yes | `192` |
| `stream.burst_ms` | Station: Network stream | stream-obs | yes | `1000` |
| `stream.max_listeners` | Station: Network stream | stream-obs, troubleshooting | yes | `4` |
| `stream.listener_lag_s` | Station: Network stream | stream-obs | yes | `5` |
| `stream.expect_listener` | Station: Network stream | alerts, stations, stream-obs | yes | `true` |
| `stream.no_listener_alert_s` | Station: Network stream | alerts, stream-obs | yes | `60` |
| `stream.send_icy_metadata` | Station: Network stream | stream-obs (added in this audit) | yes | `false` |
| `loudness.preset` | Station: Loudness controller | output-and-level | yes | `ambient` |
| `loudness.enabled` | Station: Loudness controller | output-and-level (added in this audit) | yes | `false` |
| `loudness.target_lufs` | Station: Loudness controller | output-and-level | yes | `-14.0` |
| `loudness.true_peak_dbtp` | Station: Loudness controller | processing | yes | `-1.0` |
| `loudness.window_s` | Station: Loudness controller | output-and-level | yes | `240.0` |
| `loudness.max_slew_db_per_min` | Station: Loudness controller | output-and-level | yes | `1.0` |
| `loudness.deadband_lu` | Station: Loudness controller | output-and-level | yes | `0.25` |
| `loudness.gain_min_db` | Station: Loudness controller | status-display (added in this audit) | yes | `-10.0` |
| `loudness.gain_max_db` | Station: Loudness controller | alerts, status-display (added in this audit) | yes | `8.0` |
| `loudness.min_valid_s` | Station: Loudness controller | output-and-level (added in this audit) | yes | `30.0` |
| `loudness.gate_categories` | Station: Loudness controller | output-and-level | yes | `[liner, id, promo]` |
| `loudness.trim_tau_s` | Station: Loudness controller | output-and-level | yes | `600.0` |
| `loudness.trim_range_db` | Station: Loudness controller | output-and-level | yes | `3.0` |
| `loudness.within_item` | Station: Loudness controller | output-and-level | yes | `hold` |
| `loudness.item_memory_s` | Station: Loudness controller | output-and-level (added in this audit) | yes | `600.0` |
| `loudness.robust_items` | Station: Loudness controller | output-and-level | yes | `5` |
| `loudness.item_cap_s` | Station: Loudness controller | output-and-level | yes | `240.0` |
| `loudness.peak_allowance_db` | Station: Loudness controller | output-and-level | yes | `0.0` |
| `loudness.max_limiter_gr_db` | Station: Loudness controller | output-and-level | yes | `5.0` |
| `data_dir` | This box: Global | settings-reference (notes) (added in this audit) | yes | `/var/lib/hotclock` |
| `stations_dir` | This box: Global | settings-reference (notes) (added in this audit) | yes | `/etc/hotclock/stations` |
| `run_dir` | This box: Global | settings-reference (notes) (added in this audit) | yes | `/run/hotclock` |
| `time_zone` | This box: Global | overview, schedule, troubleshooting, settings-reference (notes) | yes | `America/New_York` |
| `library.listen` | This box: Global | settings, settings-reference (notes) | yes | `0.0.0.0:8300` |
| `brand.display_name` | This box: Global | settings-reference (notes) (added in this audit) | yes | `(none)` |
| `allowed_hosts` | This box: Global | troubleshooting, settings-reference (notes) | yes | `[]` |
| `secrets_file` | This box: Global | notifications | yes | `/etc/hotclock/secrets.toml` |
| `programs.unload_after_min` | This box: Programs and playback | on-air, settings | yes | `5` |
| `display.enabled` | This box: Status display (the box's own monitor) | status-display | yes | `true` |
| `display.mode` | This box: Status display (the box's own monitor) | status-display, troubleshooting | yes | `1024x600@60` |
| `display.diagonal_in` | This box: Status display (the box's own monitor) | status-display, troubleshooting | yes | `7.0` |
| `display.view_distance_cm` | This box: Status display (the box's own monitor) | status-display, troubleshooting | yes | `70.0` |
| `display.default_view` | This box: Status display (the box's own monitor) | settings-reference (notes) (added in this audit) | yes | `single` |
| `display.bar_product` | This box: Status display (the box's own monitor) | status-display | yes | `true` |
| `display.bar_clock` | This box: Status display (the box's own monitor) | status-display | yes | `true` |
| `display.bar_date` | This box: Status display (the box's own monitor) | status-display | yes | `true` |
| `display.bar_cpu` | This box: Status display (the box's own monitor) | status-display | yes | `true` |
| `display.bar_network` | This box: Status display (the box's own monitor) | status-display | yes | `false` |
| `display.bypassed_stages` | This box: Status display (the box's own monitor) | status-display | yes | `show` |
| `display.processing_view` | This box: Status display (the box's own monitor) | status-display | yes | `both` |
| `display.spectrum_source` | This box: Status display (the box's own monitor) | status-display | yes | `post` |
| `display.curve_range_db` | This box: Status display (the box's own monitor) | status-display | yes | `12.0` |
| `display.curve_include_makeup` | This box: Status display (the box's own monitor) | status-display | yes | `false` |
| `display.connector` | This box: Status display (the box's own monitor) | status-display | yes | `(none)` |
| `display.device` | This box: Status display (the box's own monitor) | status-display | yes | `(none)` |
| `display.rotate_pin_on_alert` | This box: Status display (the box's own monitor) | status-display | yes | `true` |
| `display.rotate_processing` | This box: Status display (the box's own monitor) | status-display | yes | `false` |
| `display.proc_range_db` | This box: Status display (the box's own monitor) | status-display | yes | `18.0` |
| `display.mini_processing` | This box: Status display (the box's own monitor) | status-display | yes | `auto` |
| `display.proc_spectrum` | This box: Status display (the box's own monitor) | status-display | yes | `false` |
| `display.night_mode` | This box: Status display: night color | status-display | yes | `sun` |
| `display.night_kelvin` | This box: Status display: night color | status-display | yes | `3400` |
| `display.night_dim_percent` | This box: Status display: night color | status-display | yes | `0` |
| `display.night_transition_min` | This box: Status display: night color | status-display | yes | `60` |
| `display.night_on` | This box: Status display: night color | status-display | yes | `20:00` |
| `display.night_off` | This box: Status display: night color | status-display | yes | `07:00` |
| `display.night_location` | This box: Status display: night color | status-display | yes | `timezone` |
| `display.night_latitude` | This box: Status display: night color | status-display | yes | `(none)` |
| `display.night_longitude` | This box: Status display: night color | status-display | yes | `(none)` |
| `display.night_time_zone` | This box: Status display: night color | status-display | yes | `system` |
| `chain.trim.gain_db` | Station: Processing chain: Input trim | processing | yes | `0.0` |
| `chain.highpass.enabled` | Station: Processing chain: Input high-pass | processing (by block) | yes | `false` |
| `chain.highpass.freq_hz` | Station: Processing chain: Input high-pass | processing (by block) | yes | `25.0` |
| `chain.highpass.order` | Station: Processing chain: Input high-pass | processing (by block) | yes | `2` |
| `chain.rotator.enabled` | Station: Processing chain: Phase rotator | processing (by block) | yes | `false` |
| `chain.rotator.freq_hz` | Station: Processing chain: Phase rotator | processing (by block) | yes | `200.0` |
| `chain.rotator.sections` | Station: Processing chain: Phase rotator | processing (by block) | yes | `3` |
| `chain.stereo.enabled` | Station: Processing chain: Stereo enhancer | processing (by block) | yes | `false` |
| `chain.stereo.width` | Station: Processing chain: Stereo enhancer | processing (by block) | yes | `1.25` |
| `chain.agc.enabled` | Station: Processing chain: Wideband AGC | processing (by block) | yes | `false` |
| `chain.agc.target_dbfs` | Station: Processing chain: Wideband AGC | processing (by block) | yes | `-20.0` |
| `chain.agc.max_gain_db` | Station: Processing chain: Wideband AGC | processing (by block) | yes | `12.0` |
| `chain.agc.max_reduction_db` | Station: Processing chain: Wideband AGC | processing (by block) | yes | `9.0` |
| `chain.agc.down_db_per_s` | Station: Processing chain: Wideband AGC | processing (by block) | yes | `3.0` |
| `chain.agc.up_db_per_s` | Station: Processing chain: Wideband AGC | processing (by block) | yes | `2.5` |
| `chain.agc.detector_ms` | Station: Processing chain: Wideband AGC | processing (by block) | yes | `400.0` |
| `chain.agc.program_ms` | Station: Processing chain: Wideband AGC | processing (by block) | yes | `3000.0` |
| `chain.agc.gate_db` | Station: Processing chain: Wideband AGC | processing (by block) | yes | `-35.0` |
| `chain.agc.fast_clamp` | Station: Processing chain: Wideband AGC | processing (by block) | yes | `true` |
| `chain.agc.fast_attack_ms` | Station: Processing chain: Wideband AGC | processing (by block) | yes | `10.0` |
| `chain.agc.fast_trigger_db` | Station: Processing chain: Wideband AGC | processing (by block) | yes | `6.0` |
| `chain.agc.hold_s` | Station: Processing chain: Wideband AGC | processing (by block) | yes | `0.0` |
| `chain.agc.ratio` | Station: Processing chain: Wideband AGC | processing (by block) | yes | `1.0` |
| `chain.eq.enabled` | Station: Processing chain: Equalizer | processing (by block) | yes | `false` |
| `chain.eq.position` | Station: Processing chain: Equalizer | processing (by block) | yes | `pre` |
| `chain.eq.bands` | Station: Processing chain: Equalizer | processing (by block) | yes | `4` |
| `chain.eq.b1_on` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `true` |
| `chain.eq.b1_type` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `low_shelf` |
| `chain.eq.b1_hz` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `100.0` |
| `chain.eq.b1_db` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `0.0` |
| `chain.eq.b1_q` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `0.7071067811865475` |
| `chain.eq.b2_on` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `true` |
| `chain.eq.b2_type` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `peak` |
| `chain.eq.b2_hz` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `250.0` |
| `chain.eq.b2_db` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `0.0` |
| `chain.eq.b2_q` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `1.0` |
| `chain.eq.b3_on` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `true` |
| `chain.eq.b3_type` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `peak` |
| `chain.eq.b3_hz` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `3000.0` |
| `chain.eq.b3_db` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `0.0` |
| `chain.eq.b3_q` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `1.0` |
| `chain.eq.b4_on` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `true` |
| `chain.eq.b4_type` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `high_shelf` |
| `chain.eq.b4_hz` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `10000.0` |
| `chain.eq.b4_db` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `0.0` |
| `chain.eq.b4_q` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `0.7071067811865475` |
| `chain.eq.b5_on` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `false` |
| `chain.eq.b5_type` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `peak` |
| `chain.eq.b5_hz` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `500.0` |
| `chain.eq.b5_db` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `0.0` |
| `chain.eq.b5_q` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `1.0` |
| `chain.eq.b6_on` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `false` |
| `chain.eq.b6_type` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `peak` |
| `chain.eq.b6_hz` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `1500.0` |
| `chain.eq.b6_db` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `0.0` |
| `chain.eq.b6_q` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `1.0` |
| `chain.eq.b7_on` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `false` |
| `chain.eq.b7_type` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `peak` |
| `chain.eq.b7_hz` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `6000.0` |
| `chain.eq.b7_db` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `0.0` |
| `chain.eq.b7_q` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `1.0` |
| `chain.eq.b8_on` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `false` |
| `chain.eq.b8_type` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `peak` |
| `chain.eq.b8_hz` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `12000.0` |
| `chain.eq.b8_db` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `0.0` |
| `chain.eq.b8_q` | Station: Processing chain: Equalizer | processing (as `b<n>_...`) | yes | `1.0` |
| `chain.bass.enabled` | Station: Processing chain: Bass enhancer | processing (by block) | yes | `false` |
| `chain.bass.freq_hz` | Station: Processing chain: Bass enhancer | processing (by block) | yes | `80.0` |
| `chain.bass.amount_pct` | Station: Processing chain: Bass enhancer | processing (by block) | yes | `30.0` |
| `chain.bass.drive` | Station: Processing chain: Bass enhancer | processing (by block) | yes | `3.0` |
| `chain.multiband.enabled` | Station: Processing chain: Multiband compressor | processing (by block) | yes | `false` |
| `chain.multiband.crossovers_hz` | Station: Processing chain: Multiband compressor | processing (by block) | yes | `[120.0,500.0,2000.0,6000.0]` |
| `chain.multiband.threshold_dbfs` | Station: Processing chain: Multiband compressor | processing (by block) | yes | `[-26.0,-24.0,-22.0,-22.0,-24.0]` |
| `chain.multiband.ratio` | Station: Processing chain: Multiband compressor | processing (by block) | yes | `[3.0,3.0,3.0,3.0,2.5]` |
| `chain.multiband.knee_db` | Station: Processing chain: Multiband compressor | processing (by block) | yes | `[6.0,6.0,6.0,6.0,6.0]` |
| `chain.multiband.attack_ms` | Station: Processing chain: Multiband compressor | processing (by block) | yes | `[40.0,25.0,12.0,6.0,4.0]` |
| `chain.multiband.release_ms` | Station: Processing chain: Multiband compressor | processing (by block) | yes | `[400.0,300.0,200.0,150.0,120.0]` |
| `chain.multiband.makeup_db` | Station: Processing chain: Multiband compressor | processing (by block) | yes | `[3.0,3.0,3.0,3.0,3.0]` |
| `chain.wideband.enabled` | Station: Processing chain: Wideband compressor | processing (by block) | yes | `false` |
| `chain.wideband.threshold_dbfs` | Station: Processing chain: Wideband compressor | processing (by block) | yes | `-20.0` |
| `chain.wideband.ratio` | Station: Processing chain: Wideband compressor | processing (by block) | yes | `2.0` |
| `chain.wideband.knee_db` | Station: Processing chain: Wideband compressor | processing (by block) | yes | `6.0` |
| `chain.wideband.attack_ms` | Station: Processing chain: Wideband compressor | processing (by block) | yes | `30.0` |
| `chain.wideband.release_ms` | Station: Processing chain: Wideband compressor | processing (by block) | yes | `500.0` |
| `chain.wideband.makeup_db` | Station: Processing chain: Wideband compressor | processing (by block) | yes | `0.0` |
| `chain.makeup.gain_db` | Station: Processing chain: Output makeup | processing | yes | `0.0` |
| `chain.clipper.enabled` | Station: Processing chain: Clipper | processing (by block) | yes | `false` |
| `chain.clipper.threshold_dbfs` | Station: Processing chain: Clipper | processing (by block) | yes | `-1.5` |
| `chain.clipper.softness_db` | Station: Processing chain: Clipper | processing (by block) | yes | `3.0` |
| `chain.limiter.enabled` | Station: Processing chain: True-peak limiter | processing (by block) | yes | `false` |
| `chain.limiter.ceiling_dbtp` | Station: Processing chain: True-peak limiter | processing (by block) | yes | `-1.0` |

Controls on the Settings page that are not catalog keys:

| Control | Settings page | Help page | Default stated |
|---|---|---|---|
| **History** (a station's settings versions) | Station tab, top | settings#settings-history, processing#settings-history | n/a |
| **default** button (removes the key) | every row | settings#reading-a-row | n/a |
| **Save** and the restart buttons | every tab | settings#saving | n/a |
| **Category crossfade style** drop-downs | Station tab | settings#category-crossfade-style, crossfades | yes (`inherit (station)`) |
| **Meter ballistics**: **Box default** and **Station** rows, preset, source, attack, falloff, peak hold, hold, tick falls, **GR bars** release and tick, **Use box default** | This box tab | status-display#meter-ballistics, settings#meter-ballistics | yes (presets generated; GR release 15 dB/s, tick 1000 ms) |
| **Preview night** | This box tab: Status display: night color | status-display#night-color-preview | n/a |

## Help that the code contradicted (corrected)

1. `status-display.md`, Change the meter ballistics: the steps said to select **this station** or **box default** in a
   first drop-down. The Settings card has no such drop-down. It has a **Box default** row and one row for each
   station, each with its own preset drop-down. The steps now say that.
2. `settings.md`: the section and card were called "Keys that arrive later". The card's title on the page is
   **Keys that arrive with other milestones**. Renamed.
3. `settings.md`, Pickers: the device row was called **Sound card**. The page shows no such label: the key is
   `device` in the **Sound output** card. The choice to type a value is **Other...**, not **Other**. Corrected.
4. `status-display.md`, night color table: the labels were **Night from**, **Day from**, **Latitude** and
   **Longitude**. The page shows **Night from (schedule)**, **Day from (schedule)**, **Latitude (manual)** and
   **Longitude (manual)**. Corrected.
5. `stream-obs.md`: "A station with a `[stream]` block ... serves its program". A `[stream]` block alone does not
   stream: `stream.enabled` defaults to off. Corrected to "with `stream.enabled` on".
6. `settings.md` and `settings-reference.md` say nothing about **Programs and playback** (`programs.unload_after_min`,
   new on this branch); only `on-air.md` did. Added a short section to `settings.md`, and `on-air.md` now names the
   control **Unload a stopped program after**.

No wrong default was found in the hand-written Help. Checked against the catalog: the stream table, the silence keys,
`restart_alert_clear_s`, `truncated_fade_s`, the loudness target and limits, the loudness presets
(`crates/common/src/config.rs`), the AGC, high-pass, bass, equalizer bands and limiter defaults, the status display
bar items and night color, the GR bars and `programs.unload_after_min` (choices 1, 2, 5, 10, 15, 30, 60, never).

## Also found, not changed (outside the Help)

- `docs/deploy.md`, the "Applies" table under Settings: the `live` row lists only `lock_ahead_s`, `silence_dbfs` and
  `silence_s`. The catalog also applies `output_trim_db`, `restart_alert_clear_s`, `programs.unload_after_min`,
  `display.bar_*` and `display.night_*` live. The `next boundary` row leaves out `truncated_fade` and
  `truncated_fade_s`. The table has no row for the `loudness.*` keys (station restart). The Help is right; the
  deploy table is out of date.
- The catalog's help line of `loudness.preset` says "The Loudness card on the System page". That line is in
  `settings.rs` (code), so it was left alone.

## Links from the Settings page to the Help

Every Settings tab links to the Help the way the other pages do: the `?` beside the **Settings** heading opens
`settings`. Each card's `?` opens its group in `settings-reference` (the anchor ids match the generated headings).
**Meter ballistics** opens `status-display#meter-ballistics` (through the `display` route), **Category crossfade style**
opens `crossfades#category-and-item-styles`, the night color panel opens `status-display#night-color`, and the
crossfade picker opens `crossfades#choosing-a-style`. No card lacks a link. The rows of single keys have no `?` of their
own, on this page as on the others; their help line is on the row.
