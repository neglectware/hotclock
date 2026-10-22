# Help

This is the full documentation of Hotclock. It is built into the program, so it always matches the version on the box you are using.

Hotclock is a radio automation system for streams and stations that run all day. It does these things:

- It plays a library of audio files in a planned order.
- It joins the songs the way a radio station does.
- It puts station IDs and liners in the correct places.
- It continues to play when something goes wrong.

A box runs one **library service** and one **station** for each program feed. The library service has the web pages you are looking at now, the database and the imports. Each station has its own queue, its own sound card or network stream, and its own settings.

## Use the Help {#how-to-use-the-help}

- **Find a page** in the list on the left. On a narrow screen, such as a phone or a tablet held upright, press **Contents** at the top of the page to open the list. The pages are in five groups: how to start, the pages of the web UI (one for each item in the sidebar), how the system works, how to run the box, and reference.
- **Open the sidebar on a small screen.** On a screen narrower than 1024 pixels, the sidebar is hidden. Press the menu button (three lines) at the top left to open it. Press a page name, the dark area, or Escape to close it.
- **Search** with the box at the top of the list. The search looks in every page, including the settings reference, and shows the section that matches. Press Escape to clear the search.
- **Use the ? buttons.** Most titles in the web UI have a small **?** button. It opens the Help page for that part of the screen.
- **Link to a section.** Put the pointer on a heading and click the **#** that appears. The address then opens that section for anyone on the same box, for example `#/help/on-air#assist-mode`.

## What is built and what is coming

This documentation describes what the installed version does. Features that are designed but not built yet are marked **Coming**. A page never promises a feature that is not there.

| Area | Built | Coming |
|---|---|---|
| Playout | Gapless and crossfaded playout, five crossfade styles, per-item markers, normalization to a target loudness, album runs | Context-aware transitions (optional) |
| Programming | Shuffle and sequential playlists, no-repeat rules, insert rules, timed blocks (insert, play, switch playlist, stop, fade and stop, end), blocks in sequence (play, wait, loop, stop, end, switch playlist), nested playlists, back-timing, gain and mute, blocks that send OSC, MIDI or web requests, program control (start, abort, continue, fire, skip), scheduled starts, station blocks, one-shot changes, a dry run, example programs | Listening for the console's own OSC messages |
| Sound | Output trim, safety limiter, meters and spectrum, the loudness controller with presets, the processing chain (AGC, equalizer, 2 to 6 band compressor, optional blocks, limiter) with presets for every block and for the whole chain | An external processor stage |
| Output | Sound card, lossless network stream | Other stream codecs |
| Monitoring | Alerts, email notifications, as-run history, the status display on the box's own monitor, a Stream Deck plugin | |
| Operation | Create, rename and delete stations from the web page (Stations), backup and restore, health-checked upgrade and rollback, Plex playlist import | |

## Conventions

- `code like this` is text you type, or a name you see on the screen, a file name or a setting key.
- **Bold** text is the label of a control, as the web UI shows it.
- A **station** is one program feed. A **box** is the computer. One box can run several stations.
- An **item** is one audio file in the library. A **song** is an item of music.
- Times on the schedule are in the **box's time zone** (the `time_zone` setting).
- "The station file" is `/etc/hotclock/stations/<name>.toml`. "The global file" is `/etc/hotclock/hotclock.toml`.

## Where to go next

- A new box: read [Getting started](getting-started) and [Stations](stations). If the station has no sound card, read [The stream and OBS](stream-obs) too.
- Day-to-day operation: [On Air](on-air), [Playlists](playlists) and [Alerts](alerts).
- A Stream Deck in front of a station: [The Stream Deck plugin](stream-deck).
- Something is wrong: [Troubleshooting](troubleshooting) and [Failures and recovery](failures-and-recovery).
- A word you do not know: the [Glossary](glossary).
