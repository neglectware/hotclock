# Stations

The Stations page creates, renames, deletes, starts and stops stations. With it, nobody has to log in to the box to do these things.

A station is one program, with its own playlists, output, port and settings. Each station runs as a separate program, so trouble in one station never stops another. The page is in the sidebar as **Stations**. Station management is kept apart from On Air on purpose, so the On Air page does not change during a show.

## What the page shows

The page shows one card for each station. The card shows the display name and health, the short name and systemd unit, the port, the output (sound card, network stream, or both), the stream address, the playlists and the crossfade style.

The buttons and boxes on a card:

- **On Air** and **Settings** open those pages for the station.
- **Stop** takes the station off the air. **Start** brings it back. A stopped station stays stopped until you start it or the box restarts.
- **Rename short name...** and **Delete...** are described below.
- The **Display name** box changes the name that people see. Hotclock saves it like any other setting. The station shows the new name everywhere after its next restart. After a save, the card says that the station needs a restart and has a **Restart station now** button, as the Settings page does.

## Create a station {#create-a-station}

1. Press **Create a station**. The wizard opens.
2. Fill in the fields (see the table below).
3. Press **Create station**.

| Field | Meaning |
|---|---|
| Display name | What people see. Any text up to 60 characters. You can change it at any time. |
| Short name | Lowercase letters a to z, digits, `-` and `_`, up to 32 characters. It starts with a letter or a digit. It must not be in use, and it must not be a reserved name (`test`, `library`, `display`, `notify`, `backup`, `restore`, `archive`, `stations`, `media`, `inbox`, `default`, `hotclock`). It is the file name, the systemd unit and part of the API address. |
| Output | **Network stream only** (no sound card; a program such as OBS reads the stream), **Sound card** (select one of the cards that the box lists), or both. |
| Port | The station's own API port. The wizard fills in the next free port from 8301. The port must not belong to another station or the library, and no other program can be using it. |
| Default playlist, Emergency playlist | Select from the playlists of the library. Either one can be empty. The page then warns you that the station plays its emergency playlist, or silence, until you start a playlist. |
| Crossfade style | One of the five built-in crossfade styles. See [Crossfades](crossfades). |
| Processing | The shipped preset: `bypass`, `gentle` or `radio`. See [The processing chain](processing). |
| Loudness control | Holds the stream at -14 LUFS. See [Output and level](output-and-level). |

When you press **Create station**, the page does these things:

1. It writes `/etc/hotclock/stations/<name>.toml`.
2. It starts the unit `hotclock-station@<name>`.
3. It waits up to 30 seconds for the station's `/health` to answer.

If the station does not start, the page stops it and removes the file. It tells you why and gives you the journal command to read. Nothing is left half made.

You do not have to do anything more. The library, the Settings page and the status display list the new station by themselves. (The status display restarts once to read the new list.)

A stream-only station does not expect a listener until you turn that on in Settings (`stream.expect_listener`). So a new station does not send an alert because nothing reads its stream yet.

## First run and the demo station {#first-run}

On a box with no station, On Air shows **No station yet** and two buttons:

- **Create your first station** opens the wizard.
- **Create a demo station** does everything in one step. It makes six generated test tones (30 seconds each) and the playlists "Demo tones" and "Demo tones emergency" in the library. It then makes a stream-only station with the name `demo` that plays them.

The page shows the stream address and a player. Chrome and Safari do not always play an Ogg FLAC stream. Firefox, VLC and OBS play it. A second demo station gets the name `demo-2`.

When you no longer need the demo station, delete it. The tones stay in the library.

## Rename the short name {#rename}

**Rename short name...** guides you through the change.

1. Press **Rename short name...**.
2. Type the new name.
3. Press **Show what will change**. The page shows a list of the changes.
4. Read the list.
5. Press **Rename now**.

The list names these things:

- The systemd unit, the file, and the `name` key in the file.
- The station's data folder (queue, history, state) and its nightly snapshot files.
- The records in the library that mention the station (its station blocks, schedules and notification history) and its own meter settings.
- What stays the same: the port, the display name and the stream address. OBS sources that read the stream directly continue to work.
- What you must change yourself: anything that reaches the station through the library as `/api/v1/stations/<old>/...` (Stream Deck buttons, scripts), and monitoring of the unit name.

The page stops the station, moves everything, starts the station under the new name, and waits for `/health`. The station is off the air for a few seconds. It comes back on the same item at the same place.

If a step fails, Hotclock undoes every step already done, in reverse order, and starts the old station again. The answer tells you what it undid. Hotclock refuses a new name that already has a file, a data folder, snapshots or library records. It refuses before it changes anything.

## Delete a station {#delete}

**Delete...** stops the station and removes its file, so the station no longer starts with the box. You select what happens to its data:

- **Keep its data (archive)** is the default. The file and the data folder move to `/var/lib/hotclock/archive/<name>-<date>-<time>/`. The backup snapshots and the library's records stay. The Stations page lists archived stations.
- **Delete its data too** removes the file, the data folder, the backup snapshots and the library's records of the station. Playlists that were limited to the station become usable by any station. Playlists and music are not touched.

**Warning:** You cannot undo **Delete its data too**.

To delete a station:

1. Press **Delete...**.
2. Select **Keep its data (archive)** or **Delete its data too**.
3. Type the short name of the station. The **Delete station** button stays disabled until the name matches.
4. Press **Delete station**.

To bring back an archived station:

1. As the `hotclock` user, copy the file to `/etc/hotclock/stations/`.
2. Copy the `state` folder to `/var/lib/hotclock/stations/<name>/`.
3. Start the station.

## For scripts and automation {#api}

The page uses these routes of the library service. A Stream Deck button or a script can call them too. They have no login (see [Getting started](getting-started)). If a login comes in the future, it wraps the prefix `/api/v1/station-admin`.

| Route | What it does |
|---|---|
| `GET /api/v1/station-admin` | Stations with status and health, archived stations, the next free port, sound cards, playlists, reserved names. |
| `POST /api/v1/station-admin` | Create. Body: `name`, `display_name`, `output` (`stream`, `card` or `both`), `device`, `port`, `default_playlist`, `emergency_playlist`, `transition_preset`, `processing_preset`, `loudness`, `expect_listener`. The answer is `201`, or `400` with an `errors` list that names the fields, or `502` when the station did not start. |
| `POST /api/v1/station-admin/demo` | The demo station. |
| `POST /api/v1/station-admin/<name>/rename` | Body `{"new_name": "...", "dry_run": true}` returns the plan. Without `dry_run`, it renames. |
| `DELETE /api/v1/station-admin/<name>?confirm=<name>&data=keep` | Delete. `data` is `keep` or `delete`. |
| `POST /api/v1/station-admin/<name>/start`, `.../stop` | Start or stop the unit. |

Only one change runs at a time. A second request gets `409`.

## How the box starts the stations

A station starts at boot because its file exists. A small systemd generator adds `hotclock-station@<name>` for each file in `/etc/hotclock/stations/`. It does this at every boot and at every `systemctl daemon-reload`. The unit refuses to start when its file is missing. So the page never needs `systemctl enable`.

The service user can only `start`, `stop` and `restart` the `hotclock-station@` units, and `restart` the library and the display. Stations that you enabled by hand with `systemctl enable` continue to work.
