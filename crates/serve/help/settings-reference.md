# Settings reference

This page lists every setting of a station and of the box. For each setting, it gives the type, the range, the default, when a change takes effect, and what the setting does.

The list comes from the running system, from the same catalog that the [Settings](settings) page uses. A setting added to the program shows here at once, so this page cannot describe something that the program does not do. The search of the Help finds the settings on this page too.

The files:

- The settings of a **station** are in `/etc/hotclock/stations/<name>.toml`, as `key = value` lines. A key with a dot belongs to a table: `stream.enabled` is `enabled = true` under `[stream]`.
- The settings of the **box** are in `/etc/hotclock/hotclock.toml`. `library.listen` is `listen` under `[library]`, `brand.display_name` is under `[brand]`, and the `display.*` keys are under `[display]`.
- A key that the file does not set uses its default. To make the default apply again, remove the key from the file. The **default** button on the Settings page does this.

**Takes effect** tells you what a change needs:

- `live`: the change applies at once.
- `next boundary`: the change applies from the next item that the station plans.
- The three `restart` kinds: the service reads the key when it starts, so the change applies after a restart.

See [Settings](settings#when-a-change-takes-effect).

{{generated:settings}}

## Notes on a few settings {#notes}

### The sound card: `device`, `rate`, `period_frames`, `periods`, `formats`

`device` is one of these:

- An ALSA name such as `hw:CARD=PCH,DEV=0`.
- `null` for a station with no sound card.
- `file:/path/out.wav` to write a file.

The buffer of the card is `period_frames` times `periods` frames. At 48000 Hz the defaults (1024 frames, 8 periods) give 8192 frames, about 170 milliseconds. A smaller buffer gives a shorter delay, but less margin when the box is busy.

`formats` is the list of sample formats to try, best first. `rate` is 44100 or 48000. Set it to the rate that the card runs at. See [Troubleshooting](troubleshooting#a-station-is-silent-or-says-critical).

### `default_playlist` and `emergency_playlist`

`default_playlist` matters only for a station with no history. A station always resumes the playlist it was on when it stopped, even after a crash. To move a station that has a history to a different playlist, switch its playlist on [On Air](on-air#switch-playlist).

`emergency_playlist` is what the station plays in these cases: in safe mode, after a long silence, when the queue is empty, and when the media store fails. Give it plenty of safe material. See [Failures and recovery](failures-and-recovery#emergency-playlist).

### `target_lufs`, `max_boost_db`, `peak_allowance_db`, `output_trim_db`

The first three set how Hotclock matches the loudness of items. They apply from the next item. `output_trim_db` is the final level control. See [Output and level](output-and-level).

### `truncated_fade` and `truncated_fade_s`

Some files stop in the middle of their own fade-out. The analysis gives them the flag `truncated_fade`. With `truncated_fade` on (the default), the station fades such an item to silence at its cue-out, over `truncated_fade_s` seconds (default 2, from 0.5 to 10). Both apply from the next transition that is not committed. See [Crossfades](crossfades#truncated-fade).

### `lock_ahead_s`

When an item is this close to its start, it is **committed**, and the planner no longer changes it. A larger value makes the queue steadier, but the planner then accepts fewer of your late changes.

### `silence_dbfs` and `silence_s`

`silence_dbfs` is the level under which the program counts as silent. `silence_s` is the time before the station does something about it. See [Failures and recovery](failures-and-recovery#silence).

### `restart_alert_clear_s`

The number of seconds that the station must be on air and healthy before the `restarted` alert clears itself. The default is 600. A value of 0 keeps the alert until an operator acknowledges it. A change applies at once. See [`restarted`](alerts#restarted).

### `rate_trim_ppm`

A small correction of the clock of a station with no sound card, in parts per million. Use it only after you measure a steady difference with the program that reads the stream. See [The stream and OBS](stream-obs#latency-and-clocks).

### `time_zone`

An IANA name such as `America/New_York`. The schedule, the timed blocks, the nightly snapshot time and the daily digest all follow it. After you change it, restart the library service and every station.

### `allowed_hosts`

Other names for the box, in addition to its own name, such as an alias on a private network. If a name is not in the list, the page answers `421` to a request that uses it. See [Troubleshooting](troubleshooting#the-web-page-does-not-open).

### `library.listen`

The address and port of the web pages. If you change it, open the new address after the restart.
