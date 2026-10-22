# Alerts

This page lists every alert code. For each code it gives the meaning, the default delivery and what to do.

## How alerts work

A station raises an **alert** when it has a problem, or when it has to work around one. The alert stays active until it is **cleared**. An alert clears in one of two ways:

- The problem goes away.
- You acknowledge it. Press **Ack** on [On Air](on-air#alerts), or run `hotclock ctl <station> ack <code>`.

The severity of an alert is `info`, `warning` or `critical`. A few alerts start at one severity and escalate. Each section below gives the severity at the start.

The station's health is the worst severity of its active alerts: `ok`, `warning`, or `critical`. A critical alert has three effects:

- `/health` answers 503.
- The top bar of the [status display](status-display) turns red.
- In the rotate view, the display stays on that station.

You set how an alert reaches you by email on the [Notifications](notifications#rules) page. The **default delivery** in each section is the setting that ships. You can change it on that page.

Some codes are marked **Not raised in this version**. They are part of the design, and they are listed here so that this page and the Notifications page agree. Their features are coming.

Each section below has the code as its heading. You see the same code in the alert list and in the messages.

## The sound and the station

### `silence`

**Severity:** warning, then critical. **Default delivery:** immediate, also when it clears.

**What it means:** The station is playing, but the program has been silent for longer than its window. At the first step, the alert is a warning and the station skips to the next item. If the silence continues, the alert becomes critical and the station switches to the emergency playlist.

**What to do:** Look at what was on air. The item can be silent, or the source can be dead (the card, the cable, the next device). The alert clears when sound returns. See [Failures and recovery](failures-and-recovery#silence).

### `safe_mode`

**Severity:** critical. **Default delivery:** immediate, also when it clears.

**What it means:** The station restarted five times in two minutes, so it started in safe mode. In safe mode the station ignores the schedule and the playlist. It plays the emergency playlist until an operator acknowledges the alert.

**What to do:**

1. Find out why the station crashed: `journalctl -u hotclock-station@<name>`.
2. Press **Ack** on the alert, or run `hotclock ctl <station> ack safe_mode`. The station goes back to its normal programming.

See [Failures and recovery](failures-and-recovery#safe-mode).

### `queue_starved`

**Severity:** critical. **Default delivery:** immediate, also when it clears.

**What it means:** The queue is empty and the station could not plan anything, so it plays the emergency playlist.

**What to do:** Make sure that the program playlist has playable items, and that they are not all quarantined. The alert clears when the queue has something again.

### `playlist_depleted`

**Severity:** warning. **Default delivery:** immediate.

**What it means:** The playlist that is running has fewer than two playable items, so the station plays the emergency playlist. A quarantined item counts as not playable.

**What to do:** Add items to the playlist, or release quarantined ones. The alert clears when the playlist has enough items.

### `emergency_failed`

**Severity:** critical. **Default delivery:** hourly digest.

**What it means:** The station cannot read the emergency playlist, so it plays the RAM loop.

**What to do:** Make the emergency playlist playable. Make sure that it has items and that their files are readable. If the station has no emergency playlist, set `emergency_playlist`. See [Failures and recovery](failures-and-recovery#emergency-playlist).

### `device_lost`

**Severity:** critical. **Default delivery:** immediate, also when it clears.

**What it means:** The sound card is gone: unplugged, crashed or taken by another program. The station keeps its place and tries to open the card again every two seconds.

**What to do:** Check the cable and the USB port. Make sure that no other program holds the card. The alert clears when the card is back. If the card was gone for less than 30 seconds, the item continues from where it was.

### `xrun_burst`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** The card ran out of audio more than five times in ten minutes (more than five underruns).

**What to do:** Look for a busy box, a poor USB cable or port, or a card that does not suit the settings. A larger `periods` or `period_frames` gives more margin, with a little more delay.

### `engine_stall`

**Severity:** critical. **Default delivery:** immediate, also when it clears.

**What it means:** The audio thread stopped for three seconds. The station asks the service manager to restart it, and the service manager does.

**What to do:** This is a fault of the program. Write down what was happening and report it, with the station's journal.

### `restarted`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** The station started again and continued its program. The message gives an upper limit for the seconds without sound. The station counts from its last checkpoint, which it writes every 2 seconds. So the real break was up to 2 seconds shorter than the figure.

**When it clears:** The alert clears by itself after the station has been on air and healthy for 10 minutes. The setting is [`restart_alert_clear_s`](settings-reference#restart-alert-clear-s). A value of 0 keeps the alert until you acknowledge it.

- Healthy means no critical alert, no safe mode, and the sound card present.
- If the station restarts again in that time, the alert is raised again with the new figure, and the 10 minutes start again.
- The history keeps the `restart` row and the alert in both cases.

**What to do:** Find out why the station restarted. A restart that you caused (an upgrade, a settings change) is expected. See [Failures and recovery](failures-and-recovery#restarts).

### `scheduler_restart`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** **Not raised in this version.** Reserved for a restart of the scheduler inside a running station.

**What to do:** Nothing.

### `clock_jump`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** The wall clock moved by more than two seconds against the audio clock. The audio is not affected. The station checks the timed blocks again.

**What to do:** Make sure that a time service (chrony or systemd-timesyncd) keeps the box's time, and that nobody set the clock by hand.

## Items and files

### `item_missing`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** A file in the queue is missing or cannot be read. The station removed the item from the queue, selected a replacement and quarantined the item.

**What to do:** Restore the file or import it again. See [Quarantine](failures-and-recovery#quarantine).

### `item_truncated`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** A file is shorter than it should be, or its content no longer matches what was stored. The station quarantines the item at once.

**What to do:** Replace the file. A damaged disk can cause this. If it happens to more than one file, check the disk.

### `item_silent`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** An item was silent on air. After two such skips, the station quarantines it.

**What to do:** Listen to the file in the [item editor](item-editor). If it is damaged, replace it. If it is quiet on purpose, select **Quiet piece**.

### `item_decode_failed`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** The station could not decode an item while it played. After three failures, the station quarantines it.

**What to do:** Replace the file. To find out if the file itself is damaged, play it in another program.

### `item_quarantined`

**Severity:** warning. **Default delivery:** immediate.

**What it means:** An item was quarantined. The message gives the item and the reason. The item is not played again.

**What to do:** Correct the cause. Then release the item with `hotclock ctl <station> release <item id>`, or replace the file. See [Quarantine](failures-and-recovery#quarantine).

### `item_marker_invalid`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** The markers of an item make no sense, for example a cue-out before the cue-in. The station plays the whole file this time.

**What to do:** Open the item in the [item editor](item-editor). Press **Everything back to auto**, or correct the markers.

### `analysis_failed`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** The analysis of an item failed. The item keeps its previous analysis and stays playable.

**What to do:** Try the analysis again later. If it continues to fail, replace the file.

### `decode_glitch`

**Severity:** info. **Default delivery:** off.

**What it means:** A few packets of a file could not be decoded, and the station played them as silence. The message gives the number. It is off by default because, for most boxes, it is only noise.

**What to do:** Replace the file if it is important. To get messages about it, turn the rule on in [Notifications](notifications#rules).

### `decode_slow`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** The decoder of an item fell behind, and the item ran out of audio. The rest of the item plays late. If it does not catch up, the station goes to the next item.

**What to do:** This points to a slow or busy disk, not to the item. Check the disk and the load of the box.

### `media_store_failing`

**Severity:** critical. **Default delivery:** immediate, also when it clears.

**What it means:** The station could not read three or more different items in ten minutes. It plays the RAM loop and tries again every minute.

**What to do:** Check the disk: its health, its cables, the free space and the file system. This alert means that the hardware is probably failing.

### `block_source_missing`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** **Not raised in this version.** A source that a block names was deleted or emptied. Today the station skips such a block and writes it as `skipped` on the [Schedule](schedule#recent-firings) page.

**What to do:** Nothing.

## Databases and disk

### `state_db_corrupt`

**Severity:** critical. **Default delivery:** immediate, also when it clears.

**What it means:** The station's own database is corrupt. At start, the station moves it aside and makes a new one. While running, the station restarts itself at the next item to repair it.

**What to do:** Check the disk. When you have looked, acknowledge the alert. The station keeps the old file as `state.db.corrupt-<time>`.

### `library_db_unavailable`

**Severity:** critical. **Default delivery:** immediate, also when it clears.

**What it means:** A station cannot read the library database. It plays from its track cache and does not reload.

**What to do:** Look at the library service (`systemctl status hotclock-library`). If the database is damaged, restore a snapshot. See [Upgrading, backup and restore](upgrading-backup).

### `db_unavailable`

**Severity:** warning, critical after a minute. **Default delivery:** immediate, also when it clears.

**What it means:** A database is busy, locked or read-only. Playing is not affected. The station tries again and keeps its history in memory.

**What to do:** Check the disk, and the owner and mode of the file. If the alert continues, look at the journal.

### `db_backlog`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** **Not raised in this version.** `/health` and `/metrics` report the number of database writes that wait.

**What to do:** Nothing.

### `disk_low`

**Severity:** warning. **Default delivery:** daily digest.

**What it means:** Less than 5 GB is free on the state disk.

**What to do:** Free some space, or add disk. You can delete old media in the Library.

### `disk_full`

**Severity:** critical. **Default delivery:** immediate, also when it clears.

**What it means:** A write failed because the disk is full. The station continues to play from memory and keeps the history in a buffer.

**What to do:** Free some space at once.

### `backup_check_failed`

**Severity:** warning. **Default delivery:** daily digest.

**What it means:** The nightly snapshot of a database failed, or it did not pass its integrity check. The box keeps the bad snapshot as `*.db.bad`, and a restore never uses it.

**What to do:** Check the disk and the free space. To try again, run `sudo hotclock backup`. See [Upgrading, backup and restore](upgrading-backup).

### `media_verify_mismatch`

**Severity:** warning. **Default delivery:** daily digest.

**What it means:** **Not raised in this version.** Reserved for a nightly check that the stored files still match their checksums.

**What to do:** Nothing.

### `import_failed`

**Severity:** warning. **Default delivery:** daily digest.

**What it means:** **Not raised in this version.** A failed import shows on the [Library](library#import) page, and the file moves to `inbox/.failed/` with its reason.

**What to do:** Nothing.

### `service_failed`

**Severity:** critical. **Default delivery:** immediate, also when it clears.

**What it means:** The library service went into the failed state. systemd then runs a one-time command that sends this message. So the message reaches you even though the service itself cannot send it.

**What to do:** Look at the journal (`journalctl -u hotclock-library`). The message contains its last lines.

## The network stream

### `stream_no_listener`

**Severity:** warning. **Default delivery:** immediate.

**What it means:** `stream.expect_listener` is on, and nobody has been connected to the stream for `stream.no_listener_alert_s` seconds (60 by default).

**What to do:** Make sure that the receiving program is running and uses the stream address. If nobody watches the stream all the time, turn `stream.expect_listener` off. See [The stream and OBS](stream-obs).

### `stream_encoder_lag`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** More than one second of audio waits for the stream encoder. The stream can fall behind.

**What to do:** The box is possibly overloaded. Check the load.

### `stream_encoder_failed`

**Severity:** warning, or critical on a station with no sound card. **Default delivery:** immediate, also when it clears.

**What it means:** The stream encoder cannot run, or it fails again and again. On a station whose only output is the stream, the station is off the air, so the alert is critical. When a sound card also plays the program, the alert is a warning.

**What to do:** Look at the journal. The encoder restarts inside the same stream, so listeners hear a short gap. See [The stream and OBS](stream-obs).

### `stream_dropped`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** **Not raised in this version.** The stream's status and `/metrics` count the dropped stream periods.

**What to do:** Nothing.

### `stream_encoder_restart`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** **Not raised in this version.** The stream's status counts the encoder restarts.

**What to do:** Nothing.

## Timed blocks and programming

### `block_failed`

**Severity:** warning. **Default delivery:** immediate.

**What it means:** A block that sends a message (an OSC message, a MIDI message or a web request) could not send it. It tried once more, 250 ms later. The audio is not affected.

- The alert gives the block and the reason: a closed port, a missing device file, or a web server that answered with an error.
- The block log has a `failed` row for it.
- The alert clears when the same block sends successfully.

**What to do:** Check the address and the port of the block. Make sure that the device or the receiving program is on. To try the block by hand, press **Fire** on the [On Air](on-air#program) page.

### `block_missed`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** **Not raised in this version.** When a timed block cannot act in time, the station writes it to the block log as `missed`. You see it on the [Schedule](schedule#recent-firings) page.

**What to do:** Nothing.

### `fallback`

**Severity:** warning. **Default delivery:** hourly digest.

**What it means:** **Not raised in this version.** Reserved for a message when a show starts by itself because nobody started it by hand. The fallback start itself works (see [The block engine](block-engine)), but it raises no alert yet.

**What to do:** Nothing.

## Loudness

### `loudness_at_limit`

**Severity:** warning. **Default delivery:** daily digest.

**What it means:** The [loudness controller](output-and-level#loudness-controller) held its gain at the end of its range (`gain_min_db` or `gain_max_db`) for 10 minutes, and the program is still away from the target. The output is louder or quieter than the target, and the controller cannot correct it.

**What to do:**

1. Look at the gain on the Loudness card of the Processing page.
2. If the gain is at the top, the program is much quieter than the target. Check the track reference level (`target_lufs`) and the items' normalization.
3. For more range, raise `loudness.gain_max_db` or lower `gain_min_db`.

The alert clears when the gain moves away from the limit.

### `loudness_frozen`

**Severity:** info. **Default delivery:** daily digest.

**What it means:** The station was playing, but for 30 minutes the controller did not move the gain. It had less than `min_valid_s` (30 s) of program it could measure. The cause is silence, a stopped transport, or only items of gated categories.

**What to do:** Usually nothing. Music that is mostly voice or IDs gives the controller nothing to measure. If the station is supposed to play music, make sure that the program is audible. The alert clears when the controller can measure again.
