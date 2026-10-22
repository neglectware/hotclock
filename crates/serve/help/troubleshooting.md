# Troubleshooting

This page lists the most common problems. For each one it says what to check and what to do, in the order that usually finds the cause fastest.

Start here:

1. Open [System](system). Make sure that each station says `ok`.
2. Open [On Air](on-air). Look for an alert.
3. Find the alert's code on [Alerts](alerts). That page says what to do.
4. For more detail, read the journal. For a station: `journalctl -u hotclock-station@<name> -e`. For the web pages: `journalctl -u hotclock-library -e`.

## The web page does not open

- Make sure that the service runs: `systemctl status hotclock-library`, and `curl -s http://localhost:8300/health`.
- **The page answers `421`.** You reached the box by a name that it does not know, for example a CNAME or a VPN alias. Add the name to `allowed_hosts` in the global file, then restart the library service. The box's own name, `localhost` and IP addresses always work.
- **The request is refused with `forbidden_origin`.** The request came from the origin of another web page. Use the box's own address.
- There is no login. Keep the box on a trusted network.

## A station is silent or says `critical`

1. Open [On Air](on-air) and read the alert. The common ones are `device_lost` and `silence`.
2. List the cards: `cat /proc/asound/cards` and `aplay -L | grep CARD=`.
3. Compare the list with `device` in the station file or on [Settings](settings).
4. Make sure that no other program holds the card. A sound server (PulseAudio, PipeWire, JACK), or another station on the same card, stops the station from opening it. To find the program that has it, run `fuser -v /dev/snd/*`.
5. Check the cable and the port. Look at `dmesg | tail` for USB resets.
6. Make sure that the user `hotclock` is in the group `audio`. The package does this.

A USB interface must not go to sleep. The package installs a rule that keeps USB audio out of autosuspend.

If the card was gone for less than 30 seconds, the item continues where it was. After a longer outage, the station goes to the next item.

## There is a card but there is no sound, or the sound is harsh

1. Check the output level on [System](system) and on the next device.
2. If the sound is crunchy but the limiter does nothing, lower the **Output trim**.
3. Raise the input of the next device by the same amount.

See [Output and level](output-and-level#output-trim). You do not need the card's own mixer (alsamixer).

## The stream does not reach OBS, or OBS does not reconnect {#stream}

1. Open the stream address in a player: `ffplay http://<box>:<port>/stream/program.ogg`. If it plays, the station is correct and the problem is in OBS.
2. Check the listener count in `GET /api/v1/stream`.
3. If the station has no `/stream/` address, the stream is off. Look for `[stream] enabled = true`.

Other points:

- OBS needs a **Reconnect Delay** (3 seconds) to come back after the station restarts. If your version of OBS has no such setting, use a script that restarts the source when there are no listeners. See [The stream and OBS](stream-obs#if-obs-does-not-reconnect).
- `stream_no_listener` in the alert list means that nothing is connected.
- A station serves at most `stream.max_listeners` listeners at once. A client that is stuck holds a place for up to twenty seconds after it disappears.
- If the stream falls behind or runs ahead over days, the clocks of the two computers differ. See `rate_trim_ppm` on the [stream page](stream-obs#latency-and-clocks).

## A station is in safe mode

The alert `safe_mode` is critical, and the station plays the emergency playlist. The station went into safe mode because it restarted five times in two minutes.

1. Find the cause in the journal. Examples are a bad setting, a missing file, or a card that does not open.
2. Correct the cause.
3. Press **Ack** on the alert.

See [Failures and recovery](failures-and-recovery#safe-mode).

## A station does not start {#a-station-will-not-start}

1. Run `journalctl -u hotclock-station@<name> -e`. The journal names a configuration problem with its file and key.
2. Correct the file. The [Settings](settings) page keeps the previous file beside it as `<name>.toml.bak`.

A station cannot have the name `test`.

## A setting does not save {#settings-read-only}

The page says that the file is read-only, or the save returns `not_writable`.

The web service writes the files as the user `hotclock`. The package makes `/etc/hotclock` and the station files writable for the group `hotclock`. If someone changed their owner or mode, set them back:

```
sudo chgrp hotclock /etc/hotclock /etc/hotclock/stations /etc/hotclock/stations/*.toml
sudo chmod 775 /etc/hotclock /etc/hotclock/stations
sudo chmod 664 /etc/hotclock/stations/*.toml /etc/hotclock/hotclock.toml
```

**The page cannot restart a unit.** The box does not have the rule that lets the `hotclock` user restart these units. The same rule lets it start and stop the station units, which the Stations page needs. The package installs the rule, and `polkit` must be installed. You can always complete a change that needs a restart with `sudo systemctl restart hotclock-station@<name>`.

## Plex import does not work

- **The chip says `no token file` or `token unreadable`.** Set the path of the file that holds the token. Make the file readable by the `hotclock` user. Put only the token on its first line.
- **`Plex:` errors with a refusal.** Check the server address, and make sure that the token is valid. The box must be able to reach the Plex server's own address (`http://<host>:32400`).
- **Nothing is imported the second time.** This is correct. The import does not copy tracks that are already in the library.
- **A file fails to import.** Read the line that the job shows. Opus, WMA and AC-3 files need `ffmpeg` on the box.

## An import fails

- **`not enough disk space`.** An upload needs twice its size free (a temporary copy and the stored copy), and it must leave at least 2 GB free.
- **A file does not decode.** The file is damaged, or its format is not supported. Try the file in another player.
- **A file is over 1 GB.** The web page refuses it. Copy the file into the inbox folder on the box instead.

## A timed block did not fire

1. Open [Schedule](schedule#recent-firings) and look at the block log.
2. If the block is there, the **How** column says why it did not fire: `skipped` (with the reason), `missed` (late), or `passed`.
3. If the block is not there, check these:
   - The block is on.
   - The time zone is correct (`time_zone`).
   - The station was not stopped. An ID does not start a stopped station.
   - The clock is correct.

See [Timed blocks](timed-blocks).

## The times are wrong

The schedule uses the `time_zone` setting of the global file. The web pages show times in the time zone of your browser. If a time is one hour wrong, the zone or the clock is wrong.

1. Run `timedatectl`. Make sure that a time service is running.
2. If you change `time_zone`, restart the library service and every station.

## The status display {#display}

- **No picture.** With no monitor connected, the display writes this to its log once and tries again every two seconds. It finds a monitor that you connect later in about five seconds. The stations are never affected. To see what the display found (the connector, the modes, the mode it set), run `journalctl -u hotclock-display -f`.
- **The screen goes blank.** Add `consoleblank=0` to `GRUB_CMDLINE_LINUX_DEFAULT` in `/etc/default/grub`. Run `sudo update-grub` and restart the box.
- **The panel reports a wrong size.** Do nothing: the mode comes from `display.mode`. If the system refuses the mode, the display logs the reason. It then runs on the mode that the connector offers, with the small layout, and shows an alarm line.
- **The text is too small or too large.** Set `display.diagonal_in` to the real diagonal and `display.view_distance_cm` to the real distance. See [The status display](status-display#layouts).
- **`NO CONNECTION` in the top bar.** The display cannot reach the station. Make sure that the station runs.
- **You want to see a layout without a screen.** Run `hotclock display --render-png out.png --size 1024x600 --view split --feed demo:both`.

## Email does not arrive

1. On [Notifications](notifications#email-channels), press **Send test** on the channel.
2. Read the server's reply or error under the buttons. Common causes are a wrong port or security mode, a login that the server refuses, and a sender address that the server does not accept.
3. In **Recent messages**, look for `suppressed` (held back by the rate limit) or `failed`.

## The start of a song was suddenly loud

The station watches for a sudden rise in the output's loudness. When the momentary loudness rises by 3 LU or more in half a second, after a steady program, the station writes one warning to its log: `output loudness stepped up`. It writes this at most once in 30 seconds.

The warning names the item on air and its gain. It also gives the gain of every processing stage at that moment: the AGC (with its fast and program levels), the multiband reduction in each band, the clipper, the loudness controller and the limiters.

1. Read the log: `journalctl -u hotclock-station@NAME`.
2. Look at the AGC gain first. If it is several dB above the gain the song settles at, the AGC rode a fade up and had not yet come down.

A loud hit after a quiet passage also writes the warning. So the warning alone does not mean a fault.

## Disk space

Check `df -h /var/lib/hotclock`. Below 5 GB, the station raises `disk_low`. The media store uses most of the space. Delete items that you no longer use from the [Library](library). The nightly snapshots are small.
