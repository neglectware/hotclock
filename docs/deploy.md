# Installing and running Hotclock on a box

For Ubuntu Server 24.04 (amd64). The package is `hotclock_<version>_amd64.deb`, built on the Mac by
`dev/package.sh`. This file is also in the package, as `/usr/share/doc/hotclock/deploy.md`.

## What the package puts on the box

| Path | What |
|---|---|
| `/usr/bin/hotclock` | the one binary (`station`, `library`, `import`, `import-plex`, `notify`, `backup`, `restore`, `loopcheck`, ...) |
| `/usr/bin/hotclock-restore` | stops the units, restores the newest snapshots, starts them again |
| `/lib/systemd/system/hotclock-library.service` | the library service (port 8300) |
| `/lib/systemd/system/hotclock-station@.service` | one station per instance: `hotclock-station@office` |
| `/lib/udev/rules.d/60-hotclock-usb-audio.rules` | keeps USB audio interfaces out of autosuspend |
| `/usr/lib/sysusers.d/hotclock.conf`, `/usr/lib/tmpfiles.d/hotclock.conf` | the `hotclock` user (in group `audio`) and the directories under `/var/lib/hotclock` |
| `/etc/systemd/journald.conf.d/hotclock.conf` | `SystemMaxUse=1G` |
| `/etc/hotclock/hotclock.toml` | the global configuration (a conffile: edits survive upgrades) |
| `/usr/share/hotclock/examples/` | `office.toml`, `eve.toml`: station configuration to copy |
| `/usr/share/hotclock/install.sh`, `upgrade.sh` | the install and upgrade scripts (also in the checkout under `packaging/`) |

Nothing starts by itself when the package is installed or upgraded. An operator decides when.

## Clean install

On the box, with the package in the current directory (the box needs the network once, for `curl` and
`libasound2t64` if they are missing):

    sudo ./install.sh hotclock_0.1.0-1_amd64.deb \
         --station office=hw:CARD=PCH,DEV=0:8301 \
         --station eve=hw:CARD=Device,DEV=0:8302

`install.sh` runs `apt install ./package.deb`, writes `/etc/hotclock/stations/<name>.toml` from the example
(device and port from the argument; an existing file is left alone), enables and starts the library and each
station, and prints how long each station's `/health` took to answer. By hand the same is:

    sudo apt install ./hotclock_0.1.0-1_amd64.deb
    sudo cp /usr/share/hotclock/examples/office.toml /etc/hotclock/stations/office.toml   # edit device, port
    sudo systemctl enable --now hotclock-library hotclock-station@office

(Or enable only the library and make the station on the web page: **Stations**, see below.)

Find the card ids with `cat /proc/asound/cards` and `aplay -L | grep CARD=`. The onboard card is usually `PCH`,
the C-Media USB dongle `Device`. No PulseAudio, PipeWire or JACK may hold the card. Then open
`http://<box>:8300/` (the library, the admin) and import the music: drop files on the Import card, or copy them
into `/var/lib/hotclock/inbox/`, or `sudo -u hotclock hotclock import DIR`. A station plays the playlist named
by `default_playlist`; until it exists the station plays its emergency playlist or the RAM loop, and says so on
`/health`.

The ports: library 8300, each station keeps its own wherever it runs (office 8301, Eve 8302). Nothing else is
opened. The API is for the LAN and the tailnet (architecture 13): the box does not need to be reachable from
the internet and must not be.

## A station with no sound card: the network stream (M2a)

The office can have no card at all. Its program is served as lossless FLAC in Ogg and OBS on another computer reads it:

    sudo ./install.sh hotclock_0.1.0-1_amd64.deb --station office=null:8301 --stream office

(`device = "null"` is paced by the system clock; the `[stream]` block turns the stream on.) The URL is
`http://<box>:8301/stream/program.ogg`; the OBS settings, the checks and what is verified are in
`docs/obs-stream.md`, and the drift soak that must pass before the cutover is in `docs/hardware-tests.md`.

## Health

    curl -s http://localhost:8301/health          # a station: 200 for ok and warning, 503 for critical
    curl -s http://localhost:8300/health          # the library
    journalctl -u hotclock-station@office -f
    systemctl show hotclock-station@office -p NRestarts

A status panel can read the same `/health` URLs.

## Backup and restore

Each process that owns a database writes a consistent snapshot every night at 02:30 local time (`VACUUM INTO`,
then `PRAGMA integrity_check` on the copy), keeps the newest seven in `/var/lib/hotclock/backup/`
(`library-YYYYMMDD.db`, `station-<name>-YYYYMMDD.db`), and reports the time and the check on `/health`
(`db.last_snapshot`, `db.snapshot_check`). A snapshot that fails its check is kept as `*.db.bad`, is never used
by a restore, and raises the `backup_check_failed` alert. `sudo hotclock backup` writes the same snapshots now
(safe beside the running services).

**restic.** The live databases are WAL files and must not be copied while the services run: exclude them and
back up the snapshots instead. The lines to add to your restic backup script:

    # Hotclock (architecture 5.5)
    INCLUDE="$INCLUDE /etc/hotclock /var/lib/hotclock/backup /var/lib/hotclock/media /var/lib/hotclock/secrets.toml"
    EXCLUDE="$EXCLUDE /var/lib/hotclock/library.db* /var/lib/hotclock/stations/*/state.db* /var/lib/hotclock/upload-tmp /var/lib/hotclock/inbox"

The media store is content-addressed and read-only once written, so restic stores each file once, for ever.
The first upload of a full library to the off-site repositories should start on a quiet night.

**Restore.** After restoring the files from restic (`/etc/hotclock`, `/var/lib/hotclock/backup`, `/var/lib/hotclock/secrets.toml`, the media):

    sudo hotclock-restore --dry-run            # lists what would be restored
    sudo hotclock-restore                      # newest snapshots; --date YYYYMMDD for another day

It stops the running units, moves the live database files aside (`*.replaced-<stamp>`), copies the snapshots in,
raises each station's `library_outbox` counter to where the library remembers it (so the station's next
quarantine decision is not mistaken for one already applied), and starts the units again. The stations resume
with their playlists, queue position and quarantine list as of the snapshot, then find the library changed and
carry on.

## Upgrading

    sudo /usr/share/hotclock/upgrade.sh install hotclock_0.1.1-1_amd64.deb   # snapshots, keeps the package, installs; restarts nothing
    sudo /usr/share/hotclock/upgrade.sh restart                               # library, then each station in turn, health-checked
    sudo /usr/share/hotclock/upgrade.sh rollback                              # the previous kept package, same order

A restart of a station is a break of about 3 s (the unit's `RestartSec` plus the start), and the station comes
back on the same item at the same place (architecture 1.7). Do it between songs, not during a live show. The
script stops at the first station that does not come back healthy, and leaves the others on the old binary.

## Status display (M7)

`hotclock display` draws the stations' status on the box's own monitor (architecture 11): a KMS/DRM dumb-buffer
renderer on the text console of tty1, with no X, Wayland, desktop or browser on the box. It is a client of the
stations' WebSockets and the library service's `display` topic, writes only its own last view
(`/var/lib/hotclock-display/view.json`) and cannot touch the audio.

```
sudo systemctl enable --now hotclock-display        # a box with a screen; the package does not enable it
journalctl -u hotclock-display -f                    # what it found: the connector, the panel's modes, the mode set
```

- **The screen** is the `[display]` block of `/etc/hotclock/hotclock.toml` (commented example in the shipped file):
  `mode` (default `1024x600@60`, set explicitly through KMS; the panel's EDID is never trusted for the resolution, and
  a mode the connector does not list is added as a custom CVT mode), `diagonal_in` (default 7; the pixel density and
  so the minimum text size follow from it, 11.2), `view_distance_cm` (default 70). A 15.6-inch 1920 x 1080 panel is
  `mode = "1920x1080@60"` and `diagonal_in = 15.6`: nothing else changes.
- **The view** (`single`, `split`, `rotate`, `processing`) is the box's setting, switched from the Stream Deck's Display
  View action or the web UI's On Air page (`PUT /api/v1/display {"view": "processing", "station": "office"}`); the
  display follows within a second and keeps its last view if the library service is down or restarts. `processing` is
  one station's chain as gain-reduction meters; `rotate` goes through each station's processing view as well with
  `rotate_processing = true`. Other keys of the block: `proc_range_db` (the bars' scale, 18) and `proc_spectrum`
  (the spectrum under the bars on a screen with room for it, off). The meter ballistics of the bars (release, peak tick)
  are on On Air and in Settings.
- **No output:** with no monitor connected the display logs it once and retries every 2 s (then once a minute in the
  log); the stations are not affected. A monitor plugged in later is picked up within about 5 s; one unplugged is
  noticed within 2 s and the display comes back when it returns.
- **The console must not blank** (`consoleblank=0`): the display switches tty1 to graphics mode while it runs, which
  stops the kernel's console timer, but set it anyway so the text console never blanks between runs. In
  `/etc/default/grub` add `consoleblank=0` to `GRUB_CMDLINE_LINUX_DEFAULT`, then `sudo update-grub` and reboot.
- **A panel with a bad EDID** needs nothing: the mode comes from `display.mode`. If the kernel refuses it, the display
  logs the failure, runs on the mode the connector offers with the 800 x 480 fallback layout, and shows an alarm line
  on the screen.
- **The fallback** of architecture 11.4 is `cage` plus Chromium in kiosk mode on `http://localhost:8300/display`.
  Its unit is shipped as `/usr/share/hotclock/examples/hotclock-kiosk.service` and is not installed; its packages are
  not installed on the target box. The instructions are at the top of that file.
- **Previewing without a screen:** `hotclock display --render-png out.png --size 1024x600 --view split --feed demo:both`
  draws one frame of a built-in or recorded feed (`--log out.json` writes every glyph run and rectangle drawn).
## Output level: `output_trim_db`

Each station has one output gain, `output_trim_db`, in dB (-40 to +6, default 0). It sits at the very end of the
signal path (architecture 2.4: processing, safety limiter, master gain, **trim**, then conversion), so it
applies to the sound card and to the network stream alike. Use it instead of `alsamixer` to match a card's
analog stage to the next device: for example, a laptop-class line-out that sounds a little crispy at the
mixer's 0 dB while the safety limiter never acts is running out of analog headroom, not out of digital
headroom; trim it down (try -3, then -6) and raise the next device's input instead.

- **Where to set it:** in `/etc/hotclock/stations/<name>.toml` (`output_trim_db = -6.0`, read at start), or at
  run time from the web page (System, under the station: "Output trim (dB)") or the API
  (`GET` and `POST /api/v1/output-trim`, body `{"db": -6.0}`; the answer's `result.output_trim_db` is the value
  in force; out of range is a 400). No restart. The change arrives over 50 ms (a linear ramp in gain), so there is no click.
- **It is kept.** A value set at run time is saved in the station's `state.db` and wins over the toml's after a
  restart; to go back to the toml's, set it again to that value. (`hotclock-restore` brings back the value as of the
  snapshot.)
- **Above 0 dB** can push the output above the safety limiter's -1 dBFS ceiling (the trim is after it, by design);
  the web page and the API allow up to +6 for a card that is quiet, but a program at the limiter's ceiling plus
  +3 will clip the converter. Prefer negative values.
- The silence detector and `/health` read the signal **before** the trim, so a strong trim down does not raise a
  false silence alert. The meters and the stream carry the trimmed signal (what the listener hears).

## Which playlist a station comes back on

A station always resumes what it was doing when it stopped, including after a crash or a restart (architecture
1.7): the playlist, the queue position and the item. `default_playlist` is only used when a station has **no
history yet** (a new `state.db`) or its resumed playlist no longer exists. So changing `default_playlist` in the
toml does **not** move a station that already has a history; to move it, switch its playlist from the web page
(or start it with an empty `state.db` on purpose).

That matters after a test. `packaging/acceptance.sh` makes its own "Acceptance" playlists, so it puts the box back
when it finishes: the station's toml is rewritten with only the keys the test needs (`name`, `listen`, `device`,
`default_playlist`, `emergency_playlist`; every other key, comment and table is kept) and copied back as it was
at the end, and the station's `state.db` is put back too (or removed, on a station that had none, so it starts
on its own `default_playlist`). The station is then started again, on the playlist it was on before the test.
`RESTORE=0` skips this (the test toml and test state stay), `LEAVE_STOPPED=1` leaves the unit stopped. The
script refuses to run while the station is on air. What stays behind: the six imported tones, the two
"Acceptance" playlists in the library, and the snapshots in `/var/lib/hotclock/backup`.

## Removing

`sudo apt remove hotclock` stops the units and removes the package's files; `purge` also leaves `/etc/hotclock`
and `/var/lib/hotclock` alone on purpose. Delete them by hand if that is what is meant.

## Settings (web page)

Every station and global setting can be changed from the web UI: **Settings** in the sidebar (`#/settings`), one tab per station plus "This box". Each control shows the current value, the default, the allowed range and a line of help, and saving reports for each changed key whether it was **applied now**, applies **at the next boundary or item**, or **needs a restart**. In the last case a **Restart station now** (or library) button runs the same health-checked restart as `upgrade.sh restart`, for that one unit.

| Applies | Keys |
|---|---|
| live | `lock_ahead_s`, `silence_dbfs`, `silence_s` |
| next boundary or item | `transition_preset`, `target_lufs`, `max_boost_db`, `peak_allowance_db` |
| station restart | `name` (read-only), `display_name`, `listen`, `device`, `rate`, `period_frames`, `periods`, `formats`, `rate_trim_ppm`, `default_playlist`, `emergency_playlist`, `ram_loop_seconds`, `processing_preset`, `library`, every `stream.*` key, every `chain.*` key (the Processing panel on the System page changes the chain live and writes these) |
| library restart | `data_dir`, `stations_dir`, `run_dir`, `time_zone`, `library.listen`, `brand.display_name`, `allowed_hosts`, `secrets_file` |

The **crossfade style** (the station's default transition preset) is also on the On Air page beside the playlist picker and on the System page. A change applies from the next boundary that is not yet committed; a committed segue keeps its plan. Categories get a preset dropdown on the Settings page (inherit or a preset); category patches have no editor yet. Only the five built-in presets can be a station default for now.

Saving validates with the real config parser before writing, writes the toml atomically with `toml_edit` (comments and unknown keys are kept) and keeps the previous file as `<name>.toml.bak`. A runtime crossfade-style change is remembered in the station's `state.db` and wins at the next start unless the toml's value was edited by hand in the meantime.

**Privileges this needs (approved by the owner):**
- `/etc/hotclock` and `/etc/hotclock/stations` are `root:hotclock` 0775 and the station tomls `root:hotclock` 0664 (tmpfiles; `secrets.toml` is unchanged), and the library unit has `ReadWritePaths=/var/lib/hotclock /etc/hotclock`.
- `/usr/share/polkit-1/rules.d/50-hotclock.rules` lets the user `hotclock` run **only** `restart` on `hotclock-library.service`, `hotclock-display.service` and `hotclock-station@<name>.service`, and (since 2026-10-07, for the Stations page below) `start` and `stop` on `hotclock-station@<name>.service`. Nothing else. The package recommends `polkitd`.

There is no login. The box is meant for a trusted LAN. The places an authorization layer would wrap are the library's `/api/v1/settings*` and `/api/v1/system/restart` (`crates/serve/src/settings_http.rs`) and the station's POST `/api/v1/transition-preset` and `/api/v1/live-settings` (`station_http.rs`). Every write and restart goes through `settings.rs` (`write_atomic`, `restart_unit`).

## Stations from the web page (Stations)

The **Stations** page (`#/stations`, Help: Stations) creates, renames and deletes stations and starts and stops them, so no one needs a shell for it. A new box needs only the package and `sudo systemctl enable --now hotclock-library`; with no station On Air offers **Create your first station** or **Create a demo station** (six generated tones, stream only). The API is `/api/v1/station-admin` (list, create, demo, rename with `dry_run`, delete with `confirm` and `data=keep|delete`, start, stop; documented in the Help page). Every create, rename and delete is a list of steps with an undo for each, run newest first when a later step fails (for a rename that includes starting the old station again).

**Privileges (extension of the Settings approval).** The owner approved the first class of privilege on 2026-10-06 ("Yes, I approve both": the service writes its own configuration and restarts its own units). On 2026-10-07 the coordinator relayed his words, "I approve all necessary access to the box.", for this page. What the page needs, kept to Hotclock's own units and paths:

- polkit (`packaging/polkit/50-hotclock.rules`): user `hotclock` may also `start` and `stop` `hotclock-station@[a-z0-9_-]+.service`. Not other units, not other users.
- **No `enable`/`disable`.** polkit cannot limit them to one unit: the action `org.freedesktop.systemd1.manage-unit-files` carries no unit name (checked on systemd 255 in the test container: a rule that wanted the unit never matched), so a grant would let the service enable any unit file on the box. Instead `/usr/lib/systemd/system-generators/hotclock-stations-generator` makes every file in `/etc/hotclock/stations/` start with the box (at boot and at each `daemon-reload`), and `hotclock-station@.service` has `ConditionPathExists=/etc/hotclock/stations/%i.toml`. A station exists exactly when its file does. Stations already enabled with `systemctl enable` keep working; a deleted one is skipped at boot.
- tmpfiles: `/var/lib/hotclock/archive` (0750 `hotclock`) holds the data of stations deleted with "keep its data". The station data directories, the snapshots in `/var/lib/hotclock/backup` and `/etc/hotclock/stations` were already writable by the library service.

`dev/stations-systemd-smoke.sh` runs all of this against real systemd 255 and polkit in a container: create, polkit allow and refuse, the generator across a restart, rename, delete both ways.

The login, when it exists, wraps the one prefix `/api/v1/station-admin` (`LibraryApi::station_admin` in `crates/serve/src/stations_http.rs`), as it wraps `/api/v1/settings*`.

## The soaks

The accelerated soak (`hotclock soak`, 30 virtual days on a virtual clock) runs on the Mac and needs no
hardware. The 72-hour hardware soak on a spare target box is one command, `dev/soak-hardware.sh start`, and a report
generator, `hotclock soak-report`; the procedure is in `docs/hardware-tests.md` ("M1.8 on the target box").

The network stream's soak (M2a acceptance 5) is separate and needs the receiving machine: `docs/hardware-tests.md`,
"M2a on the target box".


## Migration tools and notifications (M2)

- **Plex playlists, read-only, one time:** put the Plex token alone on the first line of a file (mode 0600, kept
  off the box's backups), then
  `sudo -u hotclock hotclock import-plex --token-file /path/token --list` (the server defaults to
  `http://192.0.2.3:32400`; `--server` changes it), `--playlist "Name" --dry-run`, then without `--dry-run`.
  Each playlist becomes a static library playlist of the same name in Plex's order (`--mode shuffle` for a
  rotation, `--as` to rename). Running it again downloads and adds nothing already there.
- **Notifications:** the page at `http://<box>:8300/#/notify` takes the SMTP server, port, TLS mode, login and
  addresses; the password is written to `secrets.toml` and never shown again. **Send test** shows the server's
  reply or error. `hotclock-notify@.service` is the one-shot behind `OnFailure=` of the library unit
  (`hotclock notify --event service_failed --unit <unit>`).

## Timed blocks and the time zone (M3)

Timed blocks (the hourly ID, scheduled starts, a stop at 00:30) are written in local time. `time_zone` in `hotclock.toml` (default
`America/New_York`) names the zone; the box needs the system tz database (`tzdata`, present on Debian and Ubuntu), and the clock must be disciplined
(chrony): the block log (`GET /api/v1/blocks/log`, Schedule screen, `hotclock_block_deviation_seconds_*` in `/metrics`) records how far from its
time each block acted. A step of the wall clock of more than 2 s raises `clock_jump`: what was due within 5 minutes before the old time fires once,
a step back refires nothing. The station and the library service both read `time_zone`; change it in the one file and restart both.
