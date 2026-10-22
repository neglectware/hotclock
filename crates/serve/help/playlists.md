# Playlists

A playlist is a named list of what a station plays. On the Playlists page you create playlists, fill them with songs and albums, and set their order. You also build a program from blocks here: start, play, wait, loop, fade, stop and messages to other equipment.

The list on the left shows each playlist's name, its mode and how many entries it has.

- To edit a playlist, click it in the list.
- To create a playlist, type a name and press **Create**.

The editor has two cards: the playlist's settings and its entries.

## Playlist settings {#playlist-settings}

| Field | What it does |
|---|---|
| **Name** | Shown on the page, on the display and in the Change program list. |
| **Mode** | `shuffle` or `sequential`. See below. |
| **No-repeat (minutes)** | How long a track waits before it can play again in a shuffle playlist. Default 120. |
| **Station** | The station a program belongs to, or `(any station)`. A program of one station never starts on another: the station refuses it with a message, and the Stream Deck lists it only for its own station. |
| **Advanced: title, artist, album (min)** | Extra spacing. Title (default 60) keeps two copies of one song apart. Artist and album (default 0, off) keep songs by one artist, or from one album, apart. |

The buttons:

- **Save** keeps the changes. A line under the fields shows the rotation, for example `No repeat: set to 120 min, using 84 min (only 18 tracks)`.
- **Enabled** turns the playlist on or off. A disabled playlist has no scheduled starts.

  You cannot turn a playlist on while its checks report errors. If you try, the page shows each blocking error under the fields, the box goes back to off, and nothing changes. Correct the errors (see [Playlist checks](playlist-checks)) and select **Enabled** again.
- **Validate** runs the checks and reports the result. See [Playlist checks](playlist-checks).
- **Start on <station>** makes this playlist the station's program. On a playing station it acts like **Change program…** with the `now` option. On a stopped station it starts the first item immediately.
- **Copy** makes a copy of the playlist. See [Copy a playlist](#copy-a-playlist).
- **Delete** removes the playlist after you confirm.

### Copy a playlist {#copy-a-playlist}

Use a copy with a one-off start for a show that runs once on another day.

1. Press **Copy**.
2. Type a name for the copy.
3. Optional: type a date and time in **One-off start (optional)**.
4. Press **Make the copy**.

The copy has the same entries and blocks. With a one-off start, the copy's Start block is set to that date and time only. If there was no Start block, a Start block is added. The copy's weekly times and skip dates are removed. Use the [Dry run](#dry-run) to check the copy.

### Shuffle and sequential

**A shuffle playlist is a rotation.** Its items are a pool. The station deals them in a random order. At the end of the pool, it deals a new random order for the next pass. Use a shuffle playlist for a station that plays all day.

**A sequential playlist plays its entries in list order**, from the first entry to the last. Albums keep their track order. Use a sequential playlist for a show, a set or anything with a fixed order.

When a sequential playlist reaches its end, it ends. The station then continues with one of these:

- its scheduled program (the playlist whose recurring start fired most recently),
- its `default_playlist`, which can be this playlist again.

You cannot reshuffle a sequential playlist.

### How a shuffle playlist chooses {#how-a-shuffle-playlist-chooses}

- Every pass has a new order. The track on air and the track committed as next never change.
- Nothing repeats inside one pass.
- At the change from one pass to the next, a track that played near the end of the last pass cannot come near the start of the next pass. The station calculates this distance from the no-repeat time and the average track length. The distance is never more than a third of the playlist.
- A rule never stops the music. If no candidate passes the rules, the station uses the candidate that played longest ago. The history marks that play as `rule_relaxed`.

### No-repeat {#no-repeat}

The **No-repeat** value is what you ask for. The station never rejects or changes it. But a playlist that is too short cannot keep a long wait. The station therefore calculates the longest wait that it can really keep, and uses that.

The page shows both values: `set to 120 min, using 84 min (only 18 tracks)`. The wait that the station can keep is the smallest of these:

- the time you set,
- 65% of the playlist's total length (so the order changes from pass to pass),
- the playlist's length minus two average tracks (so at least two songs are always possible).

To get more spacing, add more tracks. Or lower the number so that it matches what the playlist can keep.

## Entries {#entries}

The **Entries** card is the content of the playlist.

- **Tracks and albums.** Search the library in the box, then press **Add track** or **Add album**. An album entry plays in disc and track order, with no gaps. In a shuffle playlist, an album is one unit in the pool and counts as one play.
- **Drag** a row to change the order.
  - Drag a song or album by any part of its row. Drag a block by the three-bar handle at its left.
  - Drop it on a row to put it before that row, or on the box at the end of the list.
  - The page saves the order immediately.
  - The order matters in a sequential playlist. In a shuffle playlist, it does not change how the pool is dealt.
- **x** removes an entry.
- A row with a badge (START, PLAY, FADE and others) is a block. See [The block palette](#the-block-palette).
- A red or amber note under a row is a problem that the checks found there. It shows the check's code and its message. [Playlist checks](playlist-checks) explains the codes.

### Station blocks switch {#station-blocks-switch}

The checkbox at the bottom, **The station blocks (the hourly ID, station-wide liners) apply while this playlist runs**, turns the station's blocks on or off while this playlist is the program. Turn it off for a live show that must not be interrupted. See [Schedule](schedule#station-blocks).

## Blocks

A block is an entry that does something instead of playing a song.

- A **block of the program** comes from the palette. It can be a start, play, wait, loop, fade, stop, end, start playlist, a liner or ID, a message, a gain change or a mute. See [The block palette](#the-block-palette).
- An **insert rule** puts a liner or an ID after every N songs. See [Insert rules](insert-rules).
- The older **Add a timed block** form under the entries also adds a block that acts at a clock time. It makes the same blocks as the palette does with **Runs: at a time**. See [Timed blocks](timed-blocks).

## The block palette {#the-block-palette}

The palette is the row of buttons above the entries: **Start**, **Play**, **Wait**, **Loop**, **Fade out**, **Stop**, **End**, **Start playlist**, **Liner or ID**, **OSC message**, **MIDI message**, **Web request**, **Gain** and **Mute**.

To add a block:

1. Drag a button into the list. A block dropped on a row goes before that row. A block dropped on the box at the end goes last. Or press a button to add the block at the end.

   The page makes the block immediately, with simple starting values, and opens its form.
2. Change the fields that you need.
3. Press **Save**. If a field is not valid, the page refuses the block and gives the reason.

Some rules apply:

- A playlist has one **Start** block.
- **Wait** and **Loop** run in sequence, so they need a **sequential** playlist. In a shuffle playlist, a new block runs **at a time**.

Each block is a card. Its line shows the badge, the name, the row number, and a summary of what the block does and when. **Edit** opens its form and **Close** closes it. **x** removes the block. A block that is switched off shows an `off` chip.

### What every block's form has {#block-form}

- **Name** and **Enabled**. The station ignores a block that is not enabled.
- **Runs**: **In sequence** (the block acts when the program reaches it) or **At a time** (it acts at its time, whatever is playing). Start, Wait and Loop have no choice: Wait and Loop are always in sequence, and Start is always the start. See [The block engine](block-engine#program).
- **At** (for a block that runs at a time): when the block acts. Select a kind, or type the time and the field selects the kind. A bare `14:40` uses the selected kind. [Timed blocks](timed-blocks#when-it-acts) explains the kinds. The forms are:
  - `T0 + 14:40`: a time after T0,
  - `00:30 next day`: a time of day after midnight,
  - a five-field recurrence, such as `0 * * * *`,
  - a date, such as `2027-01-21T18:45:00`.
- **If it is already past**: what the block does when its time went by before it could act (the program started late, or joined). See [The block engine](block-engine#late-starts). The options are:
  - **Default**: run it if it is at most 60 s late. Fade out, Stop and End are skipped.
  - **Skip it**.
  - **Run it now**.
  - **Run it if it is at most this late**, with a number of seconds.
- **Song-change window** (Play, Liner or ID and Start playlist, at a time): off until you select **set it**. Then it has a mode and the seconds before and after its time. See [Timed blocks](timed-blocks#modes).

### The forms of the blocks

| Block | The form |
|---|---|
| **Start** | **When** is a list. **Add a weekly time** adds days and a time. **Add a date** adds one date. **Add a recurrence** adds a five-field recurrence. **Starts**: *Expected, the operator starts it by hand*, or *Starts itself at that time*. **Arm window**: how many minutes before the time a hand start counts as this show (default 60). **Fallback**: start by itself after a number of seconds if nobody started it. **Skip these dates**. A start that starts itself also has **How it starts** and a **Late start window** (default 300 s). |
| **Play** | **Play**: a playlist, a category or one item. **For how long**: until the next block takes over (the default), for ever, until a time, for a length of time in seconds, or for a number of songs. **Back-timing** (see below). **At the end time**: at the first song change after the end time, or a fade out with its seconds (only with *until a time* or *a length of time*, and with back-timing off). |
| **Wait** | Until the operator presses **Continue**, for a number of seconds, or until a time. |
| **Loop** | **Go back to**: the first entry after the Start block, or one of the rows above. **Repeat**: a number of times, or for ever. The count includes the first pass. |
| **Fade out** | The seconds to fade (7 by default), on a cosine curve. Then the station stops. |
| **Stop** | How: now, at the next song change, or after the song on air ends. Also the fade in milliseconds (2000 by default). |
| **End** | No settings. It ends the playlist that it is in. |
| **Start playlist** | The playlist to switch to, and when: at the next song change, immediately, or when the song on air ends. |
| **Liner or ID** | The category, or one item, to put into the queue. |
| **OSC message** | **Template** (fills the other fields from a ready-made setting, such as the mixing-console channel mute), the **address of the receiver** and its **port**, the **OSC address**, and a list of **arguments**. Each argument is a whole number, a number or text. **Add an argument** adds one. |
| **MIDI message** | The **port** (a raw MIDI device of the box, such as `hw:1,0`), the message type (control change, note on, note off, program change, system exclusive), the channel from 1 to 16, and the two values. A system exclusive message takes its bytes as hex pairs separated by spaces. |
| **Web request** | The method, an `http://` address, headers (**Add a header**), the body and the timeout in seconds (1 to 30, default 5). A template fills them in. |
| **Gain** | The master gain in dB (0 or lower) and the ramp in milliseconds. The gain can lower the level. It never raises it. |
| **Mute** | Mute or unmute the output, and the ramp in milliseconds. |

[The block engine](block-engine#messages) describes the messages and what they do. Every form ends with **Save**. The checks run again after each save, and any problem shows on the row.

### Back-timing {#back-timing}

Back-timing makes the last song of a Play block finish at the end time, instead of being cut by a fade. The planner selects songs that fit.

To turn it on, select **End so that a song finishes at the end time (back-timing)** on a Play block. It is off by default.

The end time is one of these:

- the block's own time or length of time,
- when the block plays until the next block: the time of the next block that runs at a time.

The option is not available with *for ever* or *a number of songs*. Its settings are:

- **Best fit** ends on the song that finishes nearest the end time.
- **Exact** starts the last song so that it ends on the end time.
- **tolerance** (seconds, 45 by default) is how far from the end time a plan can land and still count as on target.

[The block engine](block-engine#back-timing) explains how the plan works and what `END +3 s` means. While a program runs, the plan shows on [On Air](on-air#program).

## Do a dry run {#dry-run}

The **Dry run** card at the bottom of a playlist shows what the program would do, without doing it. Nothing is sent and nothing plays.

1. Select **T0**:
   - **Now**,
   - **Expected start**: the most recent expected time of the Start block within the last day, or the next one if there is none,
   - **A time**: type `18:45` for today, or a full date and time such as `2027-01-18T18:45:00`, in the box's time zone.
2. Press **Dry run**.

The card then shows:

- a **table** of every block of the program: its row, name, type, whether it runs in sequence or at a time, the wall-clock time it would act, the rule that gave the time, and a note;
- the **silent gaps**: a stop or fade followed by a later block leaves the station silent, and the card shows the gap in seconds;
- the blocks **already past at this T0**, each with how late it is, its late policy and what would happen to it;
- the playlist's checks, with their codes.

Do a dry run before a show. It shows that the times fall where you want them, and what a late start would do. You see the same list when you start a program from [On Air](on-air#program).

## Make a program from an example {#examples}

The **Examples** card on the left of the page makes a ready-made program from the playlists you select.

1. Press the example's button.
2. Select the playlists.
3. Type the address and channel of the mixing console.
4. In **Station**, select the station that runs the show. It starts with the station selected on the page.
5. Press **Create the program**.

You get a new sequential playlist. Its blocks are the ones you would drag together yourself, and you can edit each of them.

The weekly show example has six blocks:

1. **Start**: expected on a weekday evening, started by hand.
2. **Play** the pre-show playlist, in sequence.
3. **Fade out** at T0 + 14:40, over 7 s.
4. **OSC message** at T0 + 15:00: mute a channel of the mixing console (the address and channel number you gave).
5. **Play** the elevator-bed playlist at T0 + 15:01.
6. **Stop** at 00:30 the next day, with the late policy **Skip it**.

The first two blocks run in sequence and the other four run at a time. The program therefore keeps its shape whatever time the show starts. Do a **Dry run** on it to see the times.

## Switching a playlist

You can switch the station to a playlist in four ways:

- at any time, from [On Air](on-air#switch-playlist),
- with **Start on <station>** on a playlist here,
- at a set time, with a scheduled start,
- at one set moment, with a one-shot change on the [Schedule](schedule#one-shot-change) page.
