# Schedule

The Schedule page arranges a station's programming in time. It shows a weekly grid of the playlists that start by themselves, the station-wide blocks such as the hourly ID, and one-time changes. It also shows what happens next and what happened recently.

Select the station with the buttons at the top. The line under the title gives the time zone. All times on this page are **local time in the box's time zone** (the `time_zone` setting), unless a section says otherwise.

A scheduled start switches at a song change and never cuts a song, unless you select the `now` option.

## Week {#week}

The grid shows the week, Monday first, with one row for each hour. Each playlist with a scheduled start shows as a colored slot at its time. The slot's label is the time and the playlist name.

A dashed slot is a start that is only **expected**: a person starts it by hand. It does not start by itself.

To move a start, drag its slot to another cell. The slot takes the day of the cell and the time under the pointer, rounded to a quarter of an hour. This changes the playlist's Start block. Put the pointer on a slot to see its details.

## Scheduled starts {#scheduled-starts}

Each card is the Start block of one playlist. It has these fields:

- **The days and the time.** Select the days. The time is one time of day.
- **How**: what happens to the music on air when the start time comes. See the table below.
- **starts itself** or **expected, started by hand.** A start of the first kind begins by itself at each time. A start of the second kind never starts by itself: the page shows it as expected, and a person starts the playlist.

  While a playlist that a person started runs as a program, it holds back the scheduled starts of other playlists until it ends. A held start still happens when the program ends, if the end falls inside the start's late window. If not, the station logs it as `missed`.
- **may start up to N s late.** The late window. Sometimes the station cannot start the playlist on time, because it is busy, stopped or restarting. It can then still start the playlist up to this many seconds late. The default for a new start is 300 seconds.
- **skip dates** (`YYYY-MM-DD`, separated by commas). On those dates, the recurrence does nothing.
- **one-off:** a date and time on which the playlist also starts. The dates already set are listed beside the box.
- **Times.** One row for each time of day that the start happens, each row with its own days. A start made by dragging in the week grid can have several rows.
  - **Add a time** adds a row (18:00, with no days selected).
  - **Remove time** removes a row.
  - When you save, the page drops a row with no day selected.

| How | What happens |
|---|---|
| `segue` | The new playlist starts at the first song change after the time. Nothing is cut. This is the default. |
| `now` | The new playlist starts at the time. The item on air fades out. |
| `end` | The new playlist starts after the item on air ends, with no overlap. |

The buttons:

- **Save** replaces the schedule with every row shown (one recurrence for each row), plus the one-off dates.
- **Remove** deletes the start after you confirm.

### Add a scheduled start {#add-a-scheduled-start}

1. Select a playlist.
2. Select the days. Monday to Friday are selected at first.
3. Type a time, and select how the start happens.
4. Press **Add**.

Two playlists of one station must not start at the same time on the same day. The playlist checks report this. See [Playlist checks](playlist-checks).

### When a playlist ends {#when-a-playlist-ends}

A playlist ends when a sequential playlist plays its last entry, or when an End block ends it. The station then continues with the **scheduled program**: the playlist whose start fired most recently. If there is none, it uses the station's `default_playlist`.

## Station blocks {#station-blocks}

Station blocks apply under every program of a station. A playlist can turn them off while it plays (see [Playlists](playlists#station-blocks-switch)). The usual station block is the **hourly ID**.

Each row has these parts:

- the type and a name,
- the recurrence (`0 * * * *` is every hour on the hour),
- the window, from N seconds before to N seconds after,
- the mode,
- an **on** checkbox,
- **Save** and **Remove** buttons.

The page edits only recurrences. See [Timed blocks](timed-blocks) for the modes and the recurrence format.

### Add an hourly ID {#add-an-hourly-id}

1. Select a category. `id` is selected when the station has one.
2. Type a recurrence.
3. Press **Add an ID**.

The page adds a block named *Top of the hour* with the default window: from 60 seconds before to 30 seconds after. The ID plays at the first song change inside that window. If no song change comes, the ID cuts in at the end of the window. The planner selects songs so that a song change lands inside the window.

The [Insert rules](insert-rules) page lists the station blocks that are not timed (insert rules).

## Schedule a one-shot change {#one-shot-change}

A one-shot change switches the station to a playlist once, at one moment.

**Caution:** the page reads the time that you type in your **browser's** time zone, not the box's.

1. Select the playlist.
2. Type the time.
3. Select how: `segue`, `now` or `end`.
4. Press **Schedule**.

The pending changes are listed under the form. **Cancel** removes one.

## What fires next {#what-fires-next}

This table shows what is due in the next 48 hours, nearest first. Its times are in your browser's time zone.

| Column | Meaning |
|---|---|
| Block | The block's name. |
| Type | `insert`, `play`, `switch`, `start`, `oneshot` or another kind. |
| Target | The time the block is meant to act. |
| Expected | When the planner expects the block to act, from the songs now in the queue. |
| Window | The earliest and latest time the block can act. |
| Mode | The mode. |
| Planned | `yes` when the queue is planned to meet the block. |

## Recent firings {#recent-firings}

This is the block log for the last 48 hours. It lists each block that acted or was dropped, with these values:

- its target time,
- the time it acted,
- how it acted,
- the **deviation**: how many seconds after the target it acted.

The `How` column says what happened:

| How | Meaning |
|---|---|
| `soft` | The block acted at a song change inside its window. |
| `hard` | The block cut in, at the end of its window or at its exact time. |
| `fired` | A block with no song change to wait for (a stop, a fade, an end, a switch) acted at its time. |
| `late_run` | The block acted after its window, because its late rule allowed it. |
| `skipped` | The block was dropped on purpose. The station keeps the reason, for example: its source has nothing playable, the station was stopped, or the playlist was already running. |
| `passed` | A later timed flow block took over before the program reached this block. |
| `missed` | The block's time passed and its late rule said skip, or a clock step skipped it. |

A block acts once for each target time, even if the clock steps back.
