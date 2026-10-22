# The stream and OBS

This page explains the lossless network stream that a station can serve. It also tells you how to play the stream in OBS, so that a station needs no sound card at all.

## What the stream is

A station with a `[stream]` block in its station file serves its program as **lossless FLAC in an Ogg stream**. The stream uses plain HTTP on the station's own API port. A program such as OBS reads it with a Media Source. Nothing is converted to analog and back.

- The address is `http://<box>:<station port>/stream/<mount>.ogg`. The default mount is `program`, for example `http://box:8301/stream/program.ogg`.
- The stream is the program as the sound card gets it: after the safety limiter and the output trim, at 24 bits. There is no separate stream gain. The fader of the receiving program is the trim.
- A station can have a sound card and a stream at the same time. The sound card's output does not change when the stream is on or off.
- The stream is for the local network. The mount answers only `GET` and `HEAD`, and it reads nothing from the client.

## Run a station with no sound card {#a-station-with-no-sound-card}

Set `device = "null"` and turn the stream on:

```
device = "null"
[stream]
enabled = true
```

The system clock then paces the station. The command `install.sh --station office=null:8301 --stream office` writes exactly these lines. Nothing else plays the program, so the stream is the station's only output.

## Set up the OBS source {#obs-settings}

Add a **Media Source** and use these settings.

| Setting | Value |
|---|---|
| Local File | off |
| Input | the stream address above |
| Input Format | blank (OBS detects Ogg), or `ogg` |
| Restart playback when source becomes active | off |
| Close file when inactive | off |
| Use hardware decoding | off |
| Show nothing when playback ends | on |
| Network Buffering | 1 MB |
| Reconnect Delay | 3 s, if your OBS version has this setting |
| Monitoring | off, unless you are testing; fader at 0 dB; no filters |

The address, the input format and the stream itself are tested with ffmpeg and ffplay, which use the same library as OBS. The other settings are the recommended ones. Some behaviors can be different between OBS versions.

**Check the source** in this order:

1. **Level.** Play the normal program. With the fader at unity, the OBS meter peaks at about -14 to -6 dBFS. The stream status (below) shows one listener.
2. **Continuity.** Watch the meter for ten minutes, across a minimum of three song changes. Look for gaps or jumps. There must be none.
3. **Station restart.** Restart the station (`sudo systemctl restart hotclock-station@office`). The source starts to play again in about ten seconds.
4. **OBS restart.** Quit OBS and open it again. OBS connects again, and the station shows one new listener.

## Listeners and limits

| Setting | What it does |
|---|---|
| `stream.max_listeners` | The maximum number of listeners at the same time (1 to 32, default 4) |
| `stream.burst_ms` | The audio sent at once to a new listener, so that playback starts without a wait (default 1000) |
| `stream.listener_lag_s` | A listener that falls this far behind is disconnected (default 5) |
| `stream.expect_listener` | Give a warning when nobody is connected (default on) |
| `stream.no_listener_alert_s` | The seconds with nobody connected before that warning (default 60) |
| `stream.flac_level` | FLAC compression, 0 to 8 (default 5) |

The full list is in the [settings reference](settings-reference#station-network-stream).

To see the listeners, send `GET /api/v1/stream` to the station. The answer lists:

- the listeners (address, time connected, bytes, lag);
- the encoder (state, queued audio, restarts, CPU);
- the counts of dropped periods and disconnects.

To disconnect listeners, send `POST /api/v1/stream/disconnect` with `{"id": N}` or `{"all": true}`.

## What happens when the encoder fails

When the stream encoder fails, it restarts inside the same Ogg stream. Listeners stay connected and hear a short gap. The lost audio is replaced by the same length of silence.

Three alerts describe the stream: `stream_no_listener`, `stream_encoder_lag` and `stream_encoder_failed`. On a station with no sound card, the last one is **critical**, because the station is off the air. See [Alerts](alerts).

## Latency and clocks

Expect 0.3 to 0.5 seconds from the program to the first byte at the listener. In OBS with a 1 MB buffer, expect a second or two. The stream is a feed, not a monitor.

The box's clock paces a station with no sound card. OBS reads the stream on the clock of its own computer, so the two clocks are a little different. If the difference is constant and larger than a few parts per million, the station key `rate_trim_ppm` cancels it (a positive value runs the station faster).

Because of the delay, do not use a station that feeds a stream for anything that must be timed to the second against another source.

## If OBS does not connect again {#if-obs-does-not-reconnect}

Write down what the source does: a frozen picture, a stopped source, or an error in the OBS log. Two fallbacks work:

- a script that restarts the source when the listener count is zero;
- a different transport.

The station disconnects a client that stops reading. This occurs after the send buffers fill and then `stream.listener_lag_s` passes, which can take 15 to 20 seconds. It has no effect on the station or on the other listeners.
