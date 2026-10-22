# The network stream and OBS (M2a)

A station with a `[stream]` block serves its program as **lossless FLAC in Ogg** over HTTP, on the station's own API
port. OBS on another computer reads it with a **Media Source**, so the office needs no sound card, no audio interface and no
DAC-to-ADC round trip. Nothing here is a write path: the mount answers `GET` (and `HEAD`) and reads nothing from the
client. It is for the LAN and the tailnet only, like the rest of the API (architecture 13).

## On the box

    # /etc/hotclock/stations/office.toml
    device = "null"          # no sound card: the system clock paces the station
    [stream]
    enabled = true           # the rest are the defaults: mount "program", FLAC level 5, 4 listeners

(`install.sh ... --station office=null:8301 --stream office` writes that.) A station that also has a card
(`device = "hw:..."`) plays through the card and the stream at once; the card's output is sample-identical with the
stream on or off.

* URL: `http://<box>:<listen port>/stream/<mount>.ogg`, for example `http://radiobox:8301/stream/program.ogg`.
* `GET /api/v1/stream`: listeners (id, address, seconds connected, bytes, lag), the encoder (state, queued audio,
  restarts, CPU), dropped periods and gaps, disconnect counts. `POST /api/v1/stream/disconnect` with
  `{"id": N}` or `{"all": true}` ends listeners. `GET /health` has a `stream` object; `/metrics` the
  `hotclock_stream_*` series.
* Alerts: `stream_no_listener` (warning) after `no_listener_alert_s` (60 s by default) with nobody connected (set
  `expect_listener = false` for a mount that is not always watched); `stream_encoder_lag` (warning) when more than
  1 s of audio is queued for the encoder; `stream_encoder_failed` when the encoder cannot run or has restarted 5
  times in 30 s. That last one is **critical on a stream-only station** (`device = "null"`: the stream is its only
  output, so it is off the air) and a warning when a card also plays the program; it is notified at once, and
  again when it clears.
* An encoder failure (an error or a panic) restarts the encoder **inside the same Ogg stream**: same serial number,
  headers and granule positions, the audio it lost replaced by the same length of silence. Listeners stay
  connected and hear a short gap; OBS sees nothing else.

## In OBS (owner)

Add a **Media Source**, name it `Hotclock office`.

| Setting | Value | Status |
|---|---|---|
| Local File | off | as the design says |
| Input | `http://radiobox:8301/stream/program.ogg` | the URL is verified with ffmpeg and ffplay (below) |
| Input Format | blank (ffmpeg detects Ogg), or `ogg` | both verified with ffmpeg |
| Restart playback when source becomes active | off | **unconfirmed in OBS** |
| Close file when inactive | off | **unconfirmed in OBS** |
| Use hardware decoding | off | **unconfirmed in OBS** |
| Show nothing when playback ends | on | **unconfirmed in OBS** |
| Network Buffering | 1 MB (the default is 2 MB) | **unconfirmed**: smaller means a lower lead and a quicker resync |
| Reconnect Delay | 3 s | **unconfirmed**: this property shows for network inputs in recent OBS builds, and is what brings the source back after the box restarts. If this OBS build has no such property, see "If OBS does not reconnect" |
| Mixer: Monitoring | off (unless testing); fader 0 dB; no filters | |

The stream is the program after the processing chain, at 24 bits: the signal the card would have received. There
is no stream gain; the source fader is the trim.

**Check it**, in this order:

1. **Level.** Play the office's normal program. The OBS meter should peak around -14 to -6 dBFS (as the old input at
   unity), and `GET /api/v1/stream` shows `listeners: 1`.
2. **Continuity.** Watch the meter for 10 minutes across three or more transitions: no gaps, no jumps.
3. **The timing tone.** Stop the station and run `hotclock station office --stream-test` (a 1 kHz tone, a 3 kHz
   blip every second, a counter every 10 s); the tone should play without clicks. Stop it and start the station
   normally again.
4. **Station restart.** `sudo systemctl restart hotclock-station@office`: the source should resume within 10 s with
   no click in OBS.
5. **OBS restart.** Quit and reopen OBS: it reconnects, and the station log shows one new listener.

**If OBS does not reconnect** after step 4: note exactly what the source does (frozen picture of the meter, a
stopped Media Source, an error in the OBS log) and tell the builder. Two fallbacks exist: a scene-collection refresh
or a Lua/timer script that restarts the source when `listeners` is 0, and SRT (architecture 1.3a option (c)), which
is only built if this path fails the drift soak or cannot reconnect.

## What was verified without OBS

OBS's Media Source decodes with libavformat, the same library as `ffmpeg` and `ffplay`, so those are the client the
tests use (`crates/hotclock/tests/stream_m2a.rs`, `crates/stream/tests/`):

* `ffmpeg -i http://box:8301/stream/program.ogg` decodes the stream; the decoded samples are bit-exact to what the
  output received (24-bit); `ffplay` plays it.
* An encoder panic does not end the connection: ffmpeg keeps reading the same stream (checked by the reference
  `flac -t` and by ffmpeg, frame numbers and granules continuous).
* With `-reconnect 1 -reconnect_at_eof 1 -reconnect_streamed 1 -reconnect_on_network_error 1 -reconnect_delay_max 3`,
  ffmpeg keeps going across the whole process being killed and restarted 2 s later (it retries after 0, 1 and
  3 s). Plain `ffmpeg -i` stops at the end of the stream: reconnecting is the client's job, and OBS's Reconnect
  Delay is its way of doing it.
* A new process is a new Ogg stream (a different serial number, the timeline starts at 0): a client that
  reconnects inside one demuxer sees a chained stream, which ffmpeg follows (a new serial is what makes it
  re-read the headers; keeping the old one would be worse). What OBS does with that is **unconfirmed**.

## Latency and clocks

Expect 0.3 to 0.5 s from the program to the stream's first byte at the listener (the device-model buffer of 170 ms,
the 2 ms fade hold-back, up to 4 packets per Ogg page, the connect burst) and a second or two in OBS with a 1 MB
buffer. This is a bed, not a monitor; latency is not a requirement.

The station is paced by the box's system clock; OBS consumes it on its own. `tools/stream-drift.py` measures the
difference (see `docs/hardware-tests.md`, "M2a on the target box"); if it is constant and larger than a few ppm,
`rate_trim_ppm` in the station file cancels it (positive runs the station faster).

## A client that stops reading

A listener whose connection stops accepting data is dropped (`disconnects.slow`), but the **kernel's socket
buffers sit between the server and a stalled client**: the client's receive buffer (auto-tuned, up to a few MB) and
the server's 64 KB send buffer have to fill first, and only then does the `listener_lag_s` write timeout start. On
loopback that took about 21 s on macOS and 14 s on Linux with `listener_lag_s = 2`. The server cannot see past the
client's receive buffer (the bytes are acknowledged once they are in it), so neither a smaller send buffer nor
counting accepted bytes would shorten it; it is left as it is (M2a review). It does not affect the station or other
listeners: the stuck client holds one of the `max_listeners` places for that long, nothing more. A client that is
merely slow (it reads, but behind) is cut by the lag rule at once, and one that vanishes from the network (no FIN)
is cut by the write timeout once the 64 KB send buffer is full.
