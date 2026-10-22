# Hotclock

Hotclock is a radio-automation playout system for a station that runs all day and all night. It plays music, liners, IDs and promos from a library, in order, with crossfades. It can send its program to a sound card, or to the network as a lossless stream (for example to OBS, for a live stream). A web interface runs the library, the playlists, the schedule and the settings.

Hotclock is written in Rust. It is an AI-authored, human-directed project, published by Neglectware. Mark O'Brien runs the station that Hotclock was made for, set the requirements, tested the builds on real hardware and directed the project. Claude, an AI model from Anthropic, wrote the software and the documentation.

Hotclock is in active development (version 0.1.0). Expect rough edges. It has been used on one real station. It has not had a security review.

**Warning:** Hotclock has no login. The web interface and the API are for a trusted local network only. Do not make the box reachable from the internet.

## What it does

- A library service (port 8300) imports audio files, measures their loudness and finds their start and end points.
- One station process for each output. Each station has its own port (for example 8301), its own playlists and its own schedule.
- Gapless playback and crossfades (several styles), with a safety limiter, an optional processing chain and a loudness controller.
- A block engine for timed shows: wait, play, loop, start a program at a set time, or send a command to an outside device.
- Alerts and notifications (for example, by e-mail) when the station is silent or a file fails.
- A web interface with a built-in Help. Open the **Help** section: it is the main manual.
- Optional: a status display for a small monitor on the box, and an Elgato Stream Deck plugin (`streamdeck/`).

## Requirements

| | |
|---|---|
| Target system | Ubuntu Server 24.04, 64-bit Intel or AMD (amd64). Linux with systemd and ALSA. |
| Sound | An ALSA sound card (onboard or USB). No PulseAudio, PipeWire or JACK may hold the card. A station with no card can use the network stream. |
| Network | A trusted local network and a fixed address for the box. |
| To build from source | Rust 1.98.1 or newer (`rustup`), `pkg-config` and the ALSA headers (`libasound2-dev` on Ubuntu). |
| To build the `.deb` | Docker (the build runs in an Ubuntu 24.04 container). Not needed if you download a release. |
| To run the tests | `ffmpeg` and `flac` for the generated audio fixtures. |

Hotclock also builds on macOS (Apple silicon) for development. The sound-card output, the packaging and the systemd units are for Linux.

## Install (download a release)

Each [release](https://github.com/neglectware/hotclock/releases) has a Debian package: `hotclock_<version>_amd64.deb`. It is built for Ubuntu Server 24.04 on 64-bit Intel or AMD (amd64). Other Linux systems and other processors are not supported by the package; build from source instead.

1. Download the `.deb` file from the latest release, and download `install.sh` (it is also in this repository, in `packaging/`). Put both in one folder on the box.
2. Find the name of the sound card: run `cat /proc/asound/cards` and `aplay -L | grep CARD=`.
3. Run the installer as root. Each `--station` is `NAME=DEVICE:PORT`:

   ```
   chmod +x install.sh
   sudo ./install.sh hotclock_<version>_amd64.deb --station office=hw:CARD=PCH,DEV=0:8301
   ```

   For a station with no sound card and a network stream instead:

   ```
   sudo ./install.sh hotclock_<version>_amd64.deb --station office=null:8301 --stream office
   ```

   Or install by hand:

   ```
   sudo apt install ./hotclock_<version>_amd64.deb
   sudo systemctl enable --now hotclock-library
   ```

The package starts nothing by itself. Backup, restore and upgrades: [docs/deploy.md](docs/deploy.md).

## Build from source

```
rustup toolchain install 1.98.1
sudo apt install build-essential pkg-config libasound2-dev     # Linux
cargo build --release -p hotclock                              # target/release/hotclock
cargo test --workspace                                         # needs ffmpeg and flac
```

To make the `.deb` yourself, run `dev/package.sh` on a computer with Docker (Linux, or macOS with Colima or Docker Desktop). It builds in an Ubuntu 24.04 container and writes `dist/hotclock_<version>_amd64.deb`.

## First run

1. Open `http://<box>:8300/` in a browser.
2. A box with no station shows **Create your first station**. Press **Create a demo station** to try Hotclock with generated test tones, or follow the wizard: name, output (a sound card or the network stream), port, playlists.
3. Import your music: drop files on the **Import** card, or copy them into `/var/lib/hotclock/inbox/`, or run `sudo -u hotclock hotclock import DIR`.
4. Make a playlist, set it as the station's default playlist, and check the station on the **On Air** page.
5. Check the health of a station: `curl -s http://localhost:8301/health`.

Configuration files: `/etc/hotclock/hotclock.toml` (global) and `/etc/hotclock/stations/<name>.toml` (one for each station). Examples are in `packaging/examples/`. Most settings are also on the **Settings** page.

## Documentation

- The built-in Help (web interface, **Help**). Its source is `crates/serve/help/*.md`. Start with `getting-started.md`.
- [docs/deploy.md](docs/deploy.md): install, backup, restore, upgrade, status display.
- [docs/obs-stream.md](docs/obs-stream.md): send the program to OBS.
- [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md): the rules for a change (the Help changes with the code).
- [streamdeck/README.md](streamdeck/README.md): the Stream Deck plugin.

## License

Hotclock is free software under the GNU Affero General Public License, version 3 (AGPL-3.0). Read the [LICENSE](LICENSE) file.

Copyright © 2026 Mark O'Brien. Published by Neglectware.
