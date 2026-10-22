# Getting started

This page takes a new box from the package file to a station that plays. It also shows where you do each step in the web UI.

## What you need

- A PC with Ubuntu Server 24.04 (64-bit Intel or AMD) and a fixed address on a trusted network.
- The package file `hotclock_<version>_amd64.deb`.
- For a station with a sound card: a card that Linux sees (onboard audio or a USB interface). Hotclock talks to ALSA directly, so a sound server (PulseAudio, PipeWire or JACK) must not hold the card.
- For a station with no sound card: a program on another computer that reads the network stream. See [The stream and OBS](stream-obs).
- Some audio files. Music is usually FLAC, MP3 or AAC. Liners, IDs and promos are usually WAV, AIFF or FLAC.

There is no login. The web UI and the API are for a trusted local network only.

**Warning:** Do not make the box reachable from the internet.

## Install Hotclock {#install}

Install the package, then open the web UI. You create stations on the **[Stations](stations)** page, so you do not need a shell after the install.

1. Run `sudo apt install ./hotclock_<version>_amd64.deb`.
2. Run `sudo systemctl enable --now hotclock-library`. The library service starts.
3. Open `http://<box>:8300/` in a browser. A box with no station shows **Create your first station**. To make a station that plays generated test tones, press **Create a demo station**. On a phone or a tablet, the sidebar is hidden: press the menu button at the top left to see the pages (see [Help](overview#how-to-use-the-help)).
4. Answer the questions of the wizard. It asks for the name, the output (a sound card from the list, or the network stream), the port and the playlists. The wizard writes the station file, starts the station and waits until it answers. See [Stations](stations).

The command-line install below does the same. Use it for scripted installs.

### Install with the script

Copy the package and `install.sh` to the box, then run the script as root. Each `--station` is `NAME=DEVICE:PORT`.

```
sudo ./install.sh hotclock_0.1.0-1_amd64.deb \
     --station office=hw:CARD=PCH,DEV=0:8301
```

- `NAME` is the name of the station. It is also the file name of its configuration.
- `DEVICE` is the sound card. To list the cards, run `cat /proc/asound/cards` and `aplay -L | grep CARD=`. The onboard card is usually `PCH`. A USB interface has the name that the list shows. Use `null` for a station with no card.
- `PORT` is the port of the station's API. Each station needs its own port. The library service uses 8300.
- To turn on the network stream of a station with no card, add `--stream NAME`: `--station office=null:8301 --stream office`.

The script does these things:

- It installs the package.
- It writes `/etc/hotclock/stations/<name>.toml`. It does not change a file that already exists.
- It starts the library service and each station.
- It shows how long each station took to answer its health check.

Nothing starts by itself when you install or upgrade a package. A person decides when.

Then open `http://<box>:8300/` in a browser. This is the web UI. You add more stations later on the [Stations](stations) page.

## Add music

Open **[Library](library)**. There are three ways to add files:

1. **Drop files** on the Import card, or select them. Select the category first (music, liner, id, promo and so on).
2. **Copy files** into `/var/lib/hotclock/inbox/` on the box. A folder with the name of a category (`inbox/liner/`, `inbox/id/`) puts its files in that category. Hotclock checks the inbox all the time and removes each file when it is done.
3. **Import a Plex playlist** with the Plex tools. See [Plex tools](plex-tools).

Hotclock decodes and analyzes each file once. It measures the loudness, finds where the music really starts and ends, and finds where the next item comes in. It stores the result with the item. See [Markers and analysis](markers-and-analysis). To listen to a file and move its markers, use the [item editor](item-editor).

## Create your first playlist {#make-a-first-playlist}

1. Open **[Playlists](playlists)**.
2. Type a name under "New playlist name" and press **Create**.
3. Search the library with the box under Entries.
4. Press **Add track** for each song, or press **Add album**.
5. Leave **Mode** on `shuffle` for a rotation. A shuffle never plays the same song twice within the no-repeat time. For a fixed order, select `sequential`.
6. Press **Validate**. Hotclock checks the playlist and shows each problem on its row. See [Playlist checks](playlist-checks).
7. Press **Start on <station>**. The playlist goes on air.

A station always resumes what it was doing when it stopped. The `default_playlist` key in [Settings](settings) sets the playlist that a station plays when it starts fresh, so it matters only for a new station.

## Create an emergency playlist {#make-an-emergency-playlist}

1. Create a second playlist of music that is safe to play at any time. Put at least a few songs in it.
2. In the station's settings, set `emergency_playlist` to this playlist.

The station plays this playlist in safe mode, after a long silence, and when the queue is empty. The station also keeps up to five minutes of it in memory (`ram_loop_seconds`), so it can continue to play when the disk fails. See [Failures and recovery](failures-and-recovery).

## Add station IDs and liners {#put-station-ids-and-liners-in}

- For a **liner every N songs**, make an [insert rule](insert-rules).
- For an **ID at the top of every hour**, add a station block on the [Schedule](schedule) page with "Add an ID".

## Check the health {#check-that-it-is-healthy}

```
curl -s http://localhost:8301/health      # a station: 200 for ok and warning, 503 for critical
curl -s http://localhost:8300/health      # the library service
journalctl -u hotclock-station@office -f
```

The [System](system) page shows the same health for the library service and for each station. A monitoring tool can read the same `/health` addresses.

## The first hour: a checklist

- On [On Air](on-air), the station shows PLAYING and the meters move.
- The output level of the station suits the next device. If it does not, set the output trim. See [Output and level](output-and-level).
- The [Notifications](notifications) page has an email channel, and **Send test** works.
- You know where the backups are and how to restore one. See [Upgrading, backup and restore](upgrading-backup).
- If a status screen is attached, read [The status display](status-display).
