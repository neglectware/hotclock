# Timed blocks

A timed block is an entry of a playlist that acts at a set time. For example, it plays a station ID near the top of the hour, switches to another playlist at 18:00, or fades the music out and stops at 00:30.

To add a timed block, go to the [Playlists](playlists) page and use the card **Add a timed block** under the entries. To change a timed block, press **Edit** on its row.

Station blocks apply under every playlist. They are on the [Schedule](schedule#station-blocks) page and use the same settings.

## The block types

| Type | What it does | Counts as |
|---|---|---|
| **Insert an item (an ID)** | Puts one item from a category into the queue at a song change, then continues. The playlist does not move. Use it for the top-of-the-hour ID. | side block |
| **Play a source** | Plays from a category or from another playlist, from the time until the next timed block takes over. | flow block |
| **Switch playlist** | Replaces the program with another playlist. The new playlist gets a new T0. | flow block |
| **Stop** | Stops the station. | flow block |
| **Fade out and stop** | Fades the whole output to silence and stops the station. | flow block |
| **End the program** | Ends the playlist that the block is in. | flow block |

A **flow block** decides what plays. A **side block** does its action, and nothing else changes.

A timed flow block preempts. When its time comes, the block ends what the playlist is doing, in the way the block says. The playlist then continues from the block.

[The block engine](block-engine) describes the other kinds of block:

- blocks that run in sequence (wait, loop, back-timing);
- blocks that send an OSC or MIDI message or a web request;
- blocks that set the gain or mute.

You can also give a block of those kinds a time here. A message or a gain change at a time never moves the cursor.

## When it acts

Select one of four ways to say when the block acts. Times are in the box's time zone.

| Kind | Written as | Example |
|---|---|---|
| **repeats** | A five-field recurrence: `minute hour day month weekday` | `0 * * * *` every hour on the hour. `*/30 * * * *` every half hour. `15 18 * * 1-5` at 18:15 on weekdays. |
| **after T0** | `hh:mm:ss` after the playlist became the program | `00:14:40` |
| **at a time of day** | `hh:mm`, with **next day** for a time after midnight | `00:30` with next day: half past midnight after the program started |
| **on one date** | `YYYY-MM-DDThh:mm:ss` | `2026-12-31T23:59:00` |

In a recurrence, each field is one of these:

- `*` (any value);
- a number;
- a range, such as `1-5`;
- a list, such as `1,3,5`;
- a step, such as `*/15`.

The weekday is 0 to 7. Both 0 and 7 are Sunday, and Monday is 1. The buttons **every hour** and **every half hour** fill in the two most common recurrences.

**T0** is the moment the playlist became the station's program: when it was switched to, started, or started itself on its schedule. Times after T0 move with it. Recurrences and dates are fixed times and never move. Station blocks have no T0, so they accept only recurrences and dates.

Hotclock handles daylight saving time for you. At the hour when clocks go back, an hourly recurrence fires once per real hour. On the day clocks go forward, a time that does not exist fires at the first real instant after it.

## How it acts: the mode {#modes}

A block that acts at a song change has a **mode** and a **window**. These are **Insert**, **Play a source** and **Switch playlist**. The window goes from a number of seconds before the block's time to a number of seconds after it.

| Mode | What it does |
|---|---|
| `soft_then_hard` | The default. The planner selects songs so that a song change falls inside the window, as near to the time as possible. The block acts at that change. If the window ends with no change, the block cuts in: the item on air fades over one and a half seconds and the block's item starts. |
| `soft` | Acts at the first song change at or after the time. It never cuts in. |
| `hard` | Acts at the exact time, in the middle of a song if necessary. |
| `hard_wait` | Acts at the first song change after the time, with no planning for it. |

The window of an **Insert** block is 60 seconds before and 30 seconds after the time by default. So an ID plays between one minute before and half a minute after the top of the hour. Every other type has a default window of 0 before and 30 seconds after.

**When nothing plays** (after a stop, or while the station waits), there is no song change to wait for. A block that acts at a song change then acts at its exact time.

A **Stop** block has **how**:

- `now` stops in the middle of the song.
- `boundary` waits for the next song change. If no change comes, it stops hard 30 seconds after the time.
- `after_current` lets the song finish and never cuts it.

A **Stop** block also has a fade time in milliseconds.

A **Fade out and stop** block has a fade length in seconds (7 by default). It always acts at its time, in the middle of a song.

## A block whose time has passed

A block can be late because the station was stopped or was restarting, or because the clock was stepped. The rules are:

- A block that acts at a song change still acts if it is not more than 60 seconds late.
- A late **Stop**, **Fade out and stop** or **End** block is skipped, because a late stop is wrong.
- After a clock step forward, the blocks skipped in the last five minutes fire once. Older ones are logged as `missed`.
- After a clock step back, nothing fires again.

Each decision goes into the block log. See [Schedule](schedule#recent-firings).

An **Insert** block does not start a stopped station. If the station is stopped when the block's window ends, the block is logged as `skipped`.

## Priority

Two blocks can be due at the same moment. They go in this order:

1. the program's flow blocks;
2. station blocks;
3. priority (higher first);
4. list order.

A block that is dropped is logged as `skipped`, with the reason.

## Checks

A problem never stops you from saving. But the playlist page shows a note on a block that cannot work, for example:

- two flow blocks at the same time;
- times that run backward;
- a missing source;
- a silent gap after a stop.

See [Playlist checks](playlist-checks).
