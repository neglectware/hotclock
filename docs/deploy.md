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
| `/lib/systemd/system/hotclock-display.service` | the status display on the box's monitor (on by default; Settings switches it) |
| `/usr/lib/systemd/system-generators/hotclock-stations-generator` | starts the stations (one per file in `/etc/hotclock/stations`) and the status display (unless `display.enabled = false`) at boot |
| `/lib/udev/rules.d/60-hotclock-usb-audio.rules` | keeps USB audio interfaces out of autosuspend |
| `/usr/lib/sysusers.d/hotclock.conf`, `/usr/lib/tmpfiles.d/hotclock.conf` | the `hotclock` user (in group `audio`) and the directories under `/var/lib/hotclock` |
| `/etc/systemd/journald.conf.d/hotclock.conf` | `SystemMaxUse=1G` |
| `/etc/hotclock/hotclock.toml` | the global configuration (a conffile: edits survive upgrades) |
| `/usr/share/hotclock/examples/` | `office.toml`, `eve.toml`: station configuration to copy |
| `/usr/share/hotclock/install.sh`, `upgrade.sh` | the install and upgrade scripts (also in the checkout under `packaging/`) |

Nothing starts by itself when the package is installed or upgraded. An operator decides when. At the next boot the
generator starts what the files say: every station with a file, and the status display unless it is switched off.

## Clean install

On the box, with the package in the current directory (the box needs the network once, for `curl` and
`libasound2t64` if they are missing):

    sudo ./install.sh hotclock_0.1.0-1_amd64.deb \
         --station office=hw:CARD=PCH,DEV=0:8301 \
         --station eve=hw:CARD=Device,DEV=0:8302

`install.sh` runs `apt install ./package.deb`, writes `/etc/hotclock/stations/<name>.toml` from the example
(device and port from the argument; an existing file is left alone), enables and starts the library and each
station, and prints how long each station's `/health` took to answer. A station needs a playlist to put on air: on
a box whose library has none yet (a fresh install), `install.sh` writes no file for it and does not start it (a
station with nothing to play exits, restarts and ends in safe mode, and its file alone starts it at boot), and says
so. Import the music, make the playlist its `default_playlist` names, then run `install.sh` again with the same
`--station`. By hand the same is:

    sudo apt install ./hotclock_0.1.0-1_amd64.deb
    sudo cp /usr/share/hotclock/examples/office.toml /etc/hotclock/stations/office.toml   # edit device, port
    sudo systemctl enable --now hotclock-library hotclock-station@office

(Or enable only the library and make the station on the web page: **Stations**, see below.)

Find the card ids with `cat /proc/asound/cards` and `aplay -L | grep CARD=`. The onboard card is usually `PCH`,
the C-Media USB dongle `Device`. No PulseAudio, PipeWire or JACK may hold the card. Then open
`http://<box>:8300/` (the library, the admin) and import the music: drop files on the Import card, or copy them
into `/var/lib/hotclock/inbox/`, or `sudo -u hotclock hotclock import DIR`. A station plays the playlist named
by `default_playlist`; until it exists the station plays another playlist of the library, and says so on `/health`.
A station whose library has no playlist at all does not start ("there is no playlist to run"): make one first.

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
carry on. Everything the restore writes or creates under `/var/lib/hotclock` (the databases, their `-wal` and
`-shm`, each station's `state.db.lock`, a station directory) is left owned like the rest of the tree, by `hotclock`.

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
stations' WebSockets and the library service's `display` topic, writes only its own state directory
(`/var/lib/hotclock-display`: `view.json`, its last view; `meters.json`; `output.json`, the monitor it drives) and
cannot touch the audio.

```
journalctl -u hotclock-display -f                    # what it found: the connector, the panel's modes, the mode set
```

### On or off: `display.enabled` (Settings, Status display)

The display is on by default and is switched in the web UI: **Settings**, This box, **Status display on this box's
monitor** (`display.enabled` in the `[display]` block of `/etc/hotclock/hotclock.toml`). No shell and no
`systemctl enable` are needed. Three things read the key:

| What | When | Effect |
|---|---|---|
| The library service | on Save of the Settings page | writes the file, then `systemctl start` or `stop hotclock-display.service` (polkit allows exactly these two verbs, and `restart`, on exactly this unit) |
| `hotclock-stations-generator` | at boot and at each `daemon-reload` | wants `hotclock-display.service` from `multi-user.target` while the key is not `false`, the way it wants the stations |
| The unit's `ExecCondition=hotclock display --check-enabled` | at every start of the unit | exits 1 while the key is `false`: systemd skips the start (the unit stays inactive, not failed, and `Restart=` does not apply) |

The condition is what makes "off" hold whatever pulls the unit in: an `enable` made by hand, a restart from the
Settings page, a boot. It uses the service's own config parser; the generator uses a small `awk` reader of the same
key, so for an unusual spelling the condition has the last word. The unit is a single unit, not a template, so there
is never a second display, whichever way it was started.

**Boxes where an operator ran `systemctl enable hotclock-display` by hand.** The package's postinst runs
`systemctl disable hotclock-display` when it finds the unit enabled (it does not stop it), and the generator owns it
from then on. Until that upgrade, or if someone enables it by hand again later, the two coexist: with the switch on
the unit is wanted twice and runs once; with the switch off the condition skips it at boot. The one side effect of a
hand-made `enable` with the switch off: the unit's `Conflicts=getty@tty1.service` can still stop the login prompt on tty1
at boot before the condition skips the start. Run `systemctl disable hotclock-display` to give tty1 back.

**Install and upgrade.** A fresh install leaves the key unset, so the display is on: `install.sh` starts it after the
library, and it starts at every boot. It is harmless without a monitor (it retries every 2 s and logs once a minute)
and without a station (it looks for a station file every 2 s). The first upgrade to a package with the switch keeps
the box as it was, once (marker `/var/lib/hotclock/.display-switch`): when the unit was not enabled by hand and the
file does not set the key, postinst writes `enabled = false` into the `[display]` block (or adds the block), checks it
with `hotclock display --check-enabled` and keeps the previous file as `hotclock.toml.pre-display-switch`. The file is
a conffile, so a later package that changes the shipped file asks about it, as it does for any edit made on the
Settings page. Later upgrades never touch the key.

**What the Settings page shows beside the switch** (`GET /api/v1/display/status`): `running` or `stopped` from
`systemctl is-active`, and, while it runs, what the display reported in `/var/lib/hotclock-display/output.json`: the
connector and the mode in use (`HDMI-A-2, 1024x600@60`), `no monitor connected`, or `waiting for a station` (no
station file yet). The display writes that file when
the output comes up, when it is lost and when none can be opened (only when it changes). It is a file in the display's
own state directory, not a message to the library service, so the display stays a pure client of the library (as
`view.json` is); the library reads it only while the process named in it is alive.

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

### Night color: `display.night_*` (Settings, Status display: night color)

The panel warms up after sunset and back after sunrise (f.lux / Night Shift style). Only the native display's frames
change: the browser display (`/display`, `screen.html`), On Air, OBS and the stream are never touched.

| Key | Default | |
|---|---|---|
| `night_mode` | `"sun"` | `sun`, `schedule` (`night_on` to `night_off`, local `HH:MM`, defaults `"20:00"`, `"07:00"`) or `off` |
| `night_kelvin` | `3400` | the night white point, 1900 to 6500 K (6500 is the panel untouched) |
| `night_dim_percent` | `0` | 0 to 50: less light at night (the panel's backlight is not reachable from the box) |
| `night_transition_min` | `60` | warming ends at sunset (starts this long before); cooling starts at sunrise; 0 to 180 |
| `night_location` | `"timezone"` | `timezone` (the zone's principal city from tzdata `zone1970.tab`/`zone.tab`), `internet` (opt-in), `manual` (`night_latitude`, `night_longitude`, degrees, north and east positive) |
| `night_time_zone` | `"system"` | `system` (the machine's zone: `TZ`, else `/etc/localtime`) or an IANA name |

All keys apply live: on Save the library service sends a `display_night` message on its `display` topic (the channel
the display already follows for the view and the meters) and the panel follows within a second; no restart. The
display keeps the last message in `/var/lib/hotclock-display/night.json` (for the internet location after a restart
with the library down). An upgrade from a package without these keys leaves them unset, so the defaults above apply:
night color is **on** from the first sunset after the upgrade, at 3400 K, at the time zone's city. The shipped
`hotclock.toml` is unchanged (it is a conffile; a change would make dpkg ask about it on upgrade).

- **How it is applied.** In the renderer, never in the GPU: no `GAMMA_LUT`, `DEGAMMA_LUT`, `CTM` or vendor ioctl, so
  it behaves the same on any KMS device and in the headless snapshots. The canvas stays untinted (the damage tracking
  compares against it); a 256-entry table per channel is applied to every pixel on its way out, in the copy into the
  KMS dumb buffer and in the PNG of `HOTCLOCK_DISPLAY_SNAPSHOT` and `--render-png`. By day and when off the table is
  the identity and the pass is skipped. A new table (at most one change every 5 s, during a transition) repaints the
  whole screen once. Cost on a 1024 x 600 frame: see `docs/BUILDLOG.md`, "Night color".
- **The look.** White points from the CIE 1931 2-degree Planckian locus (Planck's law and the CIE observer,
  `tools/blackbody-table.py`), in linear sRGB relative to 6500 K, largest channel 1.0, applied in linear light
  (`encode(decode(v) * w)`), transitions interpolated in mired.
- **Sunrise and sunset** come from the NOAA solar position algorithm on the box, no network. Polar days and nights
  stay day or night.
- **The internet location** (`night_location = "internet"` only): the library service asks `https://ipapi.co/json/`
  at most once a day (the attempt is recorded even when it fails; a connection or I/O error is retried once after 5 s,
  a bad answer is not) and keeps the answer in
  `/var/lib/hotclock/display-location.json`; until it answers, or when it fails, the time zone's city is used. No
  other setting makes a network request. `HOTCLOCK_GEOIP_URL` replaces the address and `HOTCLOCK_GEOIP_RETRY_MS` the retry delay (the tests).
- **API.** `GET /api/v1/display/night` (the state the Settings page shows: the look now, today's sunrise and sunset,
  the next change, the place); `POST /api/v1/display/night/preview` `{"kelvin": 2700, "dim_percent": 20}` (both
  optional) shows the night look on the panel for 15 s.
- **Judging the look on the panel:** `hotclock display --night-kelvin 3400 [--night-dim 30]` (stop the unit first)
  holds that look whatever the hour; with `--render-png` the PNG is the frame as the panel shows it, for example
  `hotclock display --render-png night.png --feed demo:both --night-kelvin 3400`.

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

## A stopped program unloads by itself: `programs.unload_after_min`

A program that stops and stays loaded (the Eve example's "Stop after midnight", a `fade_stop`, or the operator's
Stop) is unloaded by the station after **5 minutes** with nothing playing and no new start: the station runs End
program and stop the audio itself, the same code as the "⋯" menu (`POST /api/v1/program/abort`
`{"stop_audio": true}`). The queue empties, the Program unit says "No program is running", and both displays drop
the last item (the station sends every WebSocket client a fresh `snapshot` once the audio has stopped; the manual
End program does the same since this change). The block log (Schedule, Recent firings; `GET /api/v1/blocks/log`)
gets one `end_program` row per End program, `how` = `operator` or `automatic`; the journal has a line for each.

- **The setting** is in the global file, read by every station at start and changed live from Settings, This box,
  Programs and playback (a save is sent to each running station at once; no restart):

      [programs]
      unload_after_min = 5        # minutes (1 to 1440; the page offers 1, 2, 5, 10, 15, 30, 60), or "never"

  The key is absent from the shipped `hotclock.toml` (a conffile, left unchanged), so an upgraded box gets the
  default, 5 minutes. `"never"` keeps today's behavior: a stopped program stays loaded until someone ends it.
- **What counts as stopped:** the transport is `stopped`, a program is loaded, no voice sounds (a stop's fade has
  ended) and the station is not in safe mode. `held` is not stopped. An `end` block ends the program itself and needs
  no unload.
- **What cancels it:** anything that plays again ends the count (Play, Next, a program start, a switch). Any other
  operator action while it runs (a queue edit, Continue, Fire, Skip, or **Keep loaded** in the Program unit,
  `POST /api/v1/program/keep-loaded`) cancels it for this stop. A new stop counts afresh.
- **Restarts:** the time of the stop is kept in `state.db` with the scheduler's state, so a restart neither loses
  nor restarts the count; a station that comes back after the time has run out unloads at its first step.
- **The API:** `GET /api/v1/program/state` has `unload: {at, in_ms, after_min}` while the count runs (else `null`),
  and the `program` WebSocket event carries the same.
- **Test hook** (stations started with `HOTCLOCK_TEST_HOOKS=1` only): `hotclock ctl <station> hook unload-age <s>`
  moves the start of a running count `s` seconds back.

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
"Acceptance" playlists in the library, and the snapshots in `/var/lib/hotclock/backup`. A second run finds its two
playlists and empties and refills them (`hotclock playlist clear`) instead of creating them again.

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
| display restart | the `display.*` keys of the status display's screen and views |
| starts or stops at once | `display.enabled` (see "On or off" under Status display) |

The **crossfade style** (the station's default transition preset) is also on the On Air page beside the playlist picker and on the System page. A change applies from the next boundary that is not yet committed; a committed segue keeps its plan. Categories get a preset dropdown on the Settings page (inherit or a preset); category patches have no editor yet. Only the five built-in presets can be a station default for now.

Saving validates with the real config parser before writing, writes the toml atomically with `toml_edit` (comments and unknown keys are kept) and keeps the previous file as `<name>.toml.bak`. A runtime crossfade-style change is remembered in the station's `state.db` and wins at the next start unless the toml's value was edited by hand in the meantime.

**Privileges this needs (approved by the owner):**
- `/etc/hotclock` and `/etc/hotclock/stations` are `root:hotclock` 0775 and the station tomls `root:hotclock` 0664 (tmpfiles; `secrets.toml` is unchanged), and the library unit has `ReadWritePaths=/var/lib/hotclock /etc/hotclock`.
- `/usr/share/polkit-1/rules.d/50-hotclock.rules` lets the user `hotclock` run **only** `restart` on `hotclock-library.service`, `hotclock-display.service` and `hotclock-station@<name>.service`, (since 2026-10-07, for the Stations page below) `start` and `stop` on `hotclock-station@<name>.service`, and (since 2026-10-13, for the display's switch) `start` and `stop` on `hotclock-display.service`. Nothing else. The package recommends `polkitd`. `node dev/polkit-rule-test.mjs` checks the whole allowlist; the two smoke tests check it against the real polkit.

There is no login. The box is meant for a trusted LAN. The places an authorization layer would wrap are the library's `/api/v1/settings*` and `/api/v1/system/restart` (`crates/serve/src/settings_http.rs`) and the station's POST `/api/v1/transition-preset` and `/api/v1/live-settings` (`station_http.rs`). Every write and restart goes through `settings.rs` (`write_atomic`, `restart_unit`).

## Stations from the web page (Stations)

The **Stations** page (`#/stations`, Help: Stations) creates, renames and deletes stations and starts and stops them, so no one needs a shell for it. A new box needs only the package and `sudo systemctl enable --now hotclock-library`; with no station On Air offers **Create your first station** or **Create a demo station** (six generated tones, stream only). The API is `/api/v1/station-admin` (list, create, demo, rename with `dry_run`, delete with `confirm` and `data=keep|delete`, start, stop; documented in the Help page). Every create, rename and delete is a list of steps with an undo for each, run newest first when a later step fails (for a rename that includes starting the old station again).

**Privileges (extension of the Settings approval).** The owner approved the first class of privilege on 2026-10-06 ("Yes, I approve both": the service writes its own configuration and restarts its own units). On 2026-10-07 the coordinator relayed his words, "I approve all necessary access to the box.", for this page. What the page needs, kept to Hotclock's own units and paths:

- polkit (`packaging/polkit/50-hotclock.rules`): user `hotclock` may also `start` and `stop` `hotclock-station@[a-z0-9_-]+.service`. Not other units, not other users.
- **No `enable`/`disable`.** polkit cannot limit them to one unit: the action `org.freedesktop.systemd1.manage-unit-files` carries no unit name (checked on systemd 255 in the test container: a rule that wanted the unit never matched), so a grant would let the service enable any unit file on the box. Instead `/usr/lib/systemd/system-generators/hotclock-stations-generator` makes every file in `/etc/hotclock/stations/` start with the box (at boot and at each `daemon-reload`), and `hotclock-station@.service` has `ConditionPathExists=/etc/hotclock/stations/%i.toml`. A station exists exactly when its file does. Stations already enabled with `systemctl enable` keep working; a deleted one is skipped at boot.
- tmpfiles: `/var/lib/hotclock/archive` (0750 `hotclock`) holds the data of stations deleted with "keep its data". The station data directories, the snapshots in `/var/lib/hotclock/backup` and `/etc/hotclock/stations` were already writable by the library service.

`dev/stations-systemd-smoke.sh` runs all of this against real systemd 255 and polkit in a container: create, polkit allow and refuse, the generator across a restart, rename, delete both ways. `dev/display-systemd-smoke.sh` does the same for the display's switch: on after install, off through the API, off across a restart (also with a hand-made `enable`), the postinst's migration, polkit.

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
