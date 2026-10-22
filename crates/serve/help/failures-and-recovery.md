# Failures and recovery

This page explains what the station does when something goes wrong. It tells you what to expect, what the station corrects by itself, and when it needs you.

**The rule:** Hotclock does everything it can to keep playing what it is supposed to play.

- When one file has a problem, the station works around it. It skips the file with no dead air and plays the next candidate under the usual rules. It marks the item so that it does not try it again. It never stops.
- Only two things stop the sound: a hardware failure of the sound card, or an operator action. Even then, the program continues to run and recovers by itself.

Every failure has an **alert code**. See [Alerts](alerts) for each code and what to do. Alerts show on [On Air](on-air#alerts), on the [status display](status-display), in `/health` and by email (see [Notifications](notifications)).

## A problem with one item

| What happened | What the station does | Alert |
|---|---|---|
| A packet of the file does not decode | It plays silence for that packet and counts it. More than 20 in one item is a fatal decode error. | `decode_glitch` (info) |
| The decoder fails in the middle of an item | It starts the next item at the point where the sound runs out, with a quick fade of the failing item. | `item_decode_failed` |
| The file is missing or cannot be read | The station checks this when the item goes into the queue, and again before it plays. It removes the item from the queue, selects a replacement and quarantines the item. | `item_missing` |
| The file is shorter than it should be, or its content no longer matches | A short file is the same as a missing file. If a file ends early on air, the station makes an emergency transition at the real end and quarantines the item at once. | `item_truncated` |
| The file plays but is silent | The station traces the silence to the item on air. After two such skips, it quarantines the item. | `item_silent` |
| The markers make no sense | The item plays from its start to its end with its automatic markers, and gets the flag `marker_invalid`. The station never skips an item for this reason. | `item_marker_invalid` |
| The decoder falls behind and the item runs out of audio | The station fills the gap with silence, and the rest of the item plays late. If the decoder does not catch up in about two seconds, the station goes to the next item. The item is not blamed, because a slow disk starves every item. | `decode_slow` |
| A new analysis of an item fails | The item keeps its previous analysis and stays playable. | `analysis_failed` |

## Quarantine {#quarantine}

The station **quarantines** an item when it fails in a way that is the fault of the file:

- once, for a missing or cut-short file;
- three times, for decode errors;
- twice, for silence.

A quarantined item:

- is never selected by a rotation, a block or an insert rule;
- is removed from the queue before it plays;
- stays in the library, with its history and the reason;
- is listed on the [System](system#quarantine) page and flagged in the [Library](library).

The quarantine raises `item_quarantined`. If it leaves a playlist with fewer than two playable items, it also raises `playlist_depleted`.

### Release an item {#release-an-item}

**Caution:** Correct the cause first: restore the file or replace it. If you do not, the item goes back into quarantine.

Use one of these:

- Press **Release** on the [System](system#quarantine) page. This releases the item in the library and on every station.
- Press **Release from quarantine** on the item's own page (the [item editor](item-editor#quarantine)). This also releases it in the library and on every station.
- On the box, run `hotclock ctl <station> release <item id>`. This releases it on one station.

A quarantine also ends by itself when you replace the file with a different one.

## Silence {#silence}

The station listens to its own output. If the program stays below `silence_dbfs` (default -50 dBFS) while the station plays, the station acts in three steps. Each step comes one **window** after the previous one:

1. **Skip.** The station skips to the next item. It blames the item that was on air for the silence (`item_silent`) and raises `silence` as a warning.
2. **Emergency playlist.** If the silence continues for another window, the station switches to the emergency playlist. `silence` becomes critical.
3. **RAM loop.** If the emergency playlist is also silent or cannot be read, the station plays the RAM loop.

The window is `silence_s` (default 15 seconds). Some items get a longer window:

- An item with a long quiet part of its own gets the longest quiet part inside the item, plus five seconds.
- Ambient items and album runs get at least 60 seconds.
- An item with **Quiet piece** selected is never skipped for silence.

A station that is held, or that waits in assist mode on purpose, is not expected to make sound.

## The emergency playlist and the RAM loop {#emergency-playlist}

- The **emergency playlist** (`emergency_playlist`) is the playlist the station uses as a fallback. It plays in safe mode, after a long silence, when the queue is empty and when the media store fails.
- The **RAM loop** is the last resort. At start, the station decodes up to five minutes (`ram_loop_seconds`) of the emergency playlist into memory. So it can play even if the disk fails. It plays in a loop, with normal transitions between items. If the station cannot read the emergency playlist, the RAM loop plays.

While the station plays one of these, the chip on [On Air](on-air#the-now-card) shows `emergency`, `ram_loop` or `safe`. When the cause goes away, the station goes back to its normal programming by itself. Safe mode is the exception (see [Safe mode](#safe-mode)).

## When the queue empties

The planner can have nothing to play: a playlist has no items left, or its items are quarantined. Then the station plays from its emergency playlist. It raises `queue_starved` or `playlist_depleted` until there is something to play again.

A playlist is never considered finished because of quarantine.

## The disk

- **`media_store_failing` (critical).** The station could not read three different items in ten minutes. It stops trusting the disk, plays the RAM loop and tries the queue again every minute.
- **`disk_low` (warning).** Less than 5 GB is free on the state disk. The box also refuses an upload that would leave less than 2 GB free.
- **`disk_full` (critical).** The station cannot write its database. Playing does not need the disk. The station keeps the history in memory (up to 10,000 rows) and writes it when there is space.
- **`state_db_corrupt` and `db_unavailable`.** See [The databases](#the-databases) below.

## The sound card

- **Underruns.** The card ran out of audio. The station recovers it and continues. More than five underruns in ten minutes raise `xrun_burst`. The cause is usually the card, a cable, a USB port or a busy box.
- **The card is gone.** An unplugged or failed card raises `device_lost` (critical). The station's clock pauses, and every item keeps its place. The station tries to open the card again every two seconds.
  - If the card comes back in 30 seconds or less, the station continues at the same place with a 150 millisecond fade-in.
  - If it takes longer, the station goes to the next item as if the current item had ended. The scheduler checks its timed blocks against the clock again.
- **The audio thread stops.** If the audio thread does not advance for three seconds, the station raises `engine_stall` and asks the service manager to restart it. The service manager kills the station and starts it again. This is a bug of the program, not of your setup. Please report it.

## Restart, resume and crash loops {#restarts}

The station writes a **checkpoint** every two seconds. The checkpoint holds the item, the place in it, the queue, the playlist and its shuffle order, and the mode. After a restart or a crash:

- If the checkpoint is less than 120 seconds old, and at least 20 seconds of the item remain before its segue, the station continues the item at the place it would be now. It uses a 150 millisecond fade-in. If not, it starts the queued next item. The aim is sound within three seconds of the start.
- The history records the gap as a `restart` row with the seconds without sound, and the station raises `restarted`. The figure is an upper limit: the station counts from the last checkpoint, and it stopped up to 2 seconds after that checkpoint.
- `restarted` clears by itself after the station has been on air and healthy for 10 minutes ([`restart_alert_clear_s`](settings-reference#restart-alert-clear-s)).
- The queue, the shuffle order, the insert counters, the block log and the alert history all survive.

### Safe mode {#safe-mode}

A station that crashes in a loop must not continue to crash. So five starts in 120 seconds put the station in safe mode. In safe mode the station:

- ignores the schedule and the playlist;
- plays the emergency playlist (or the RAM loop);
- raises `safe_mode` (critical).

Safe mode stays until an operator **acknowledges** the alert. To leave safe mode:

1. Find out why the station was crashing. The journal tells you: `journalctl -u hotclock-station@<name>`.
2. Press **Ack** on the alert, or run `hotclock ctl <station> ack safe_mode`. The station goes back to its normal programming.

## The databases

- **The station's database is corrupt.** At start, the station moves the file aside as `state.db.corrupt-<time>` and makes a new one. It builds the track cache again from the library and starts its default playlist. While running, the station continues to play from memory, and it restarts itself at the next item to repair the file. `state_db_corrupt` is critical. Acknowledge it when you have looked.
- **A database is busy, locked or read-only.** The station continues to play. It tries again with backoff and keeps the history in memory. `db_unavailable` is a warning, and critical after a minute.
- **The library cannot be read.** The library service does not start, and it raises an alert. The stations continue to play from their track caches and do not reload. Restore a snapshot. See [Upgrading, backup and restore](upgrading-backup).
- **The library service is down.** Stations do not need it to play. Nothing is lost.

## The clock

The audio runs on sample counts, not on the clock. So a jump of the wall clock does not change the sound.

If the clock moves by more than two seconds, the scheduler sets its time again and raises `clock_jump`:

- After a step forward, timed blocks skipped in the last five minutes fire once. The station logs older ones as `missed`.
- After a step back, nothing fires twice.

Keep the clock correct with a time service such as chrony.
