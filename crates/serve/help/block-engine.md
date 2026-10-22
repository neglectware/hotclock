# The block engine

This page explains how a playlist of blocks runs as a program. It covers where the program's times come from, what the cursor does, how waits, loops and nested playlists work, how back-timing makes a song end on time, and what the blocks that send messages do.

You build a playlist of items and blocks on the [Playlists](playlists) page. [Timed blocks](timed-blocks) describes a block that acts at a clock time. This page is about the rest: blocks that run **in sequence**, and the program that runs them.

## The program, T0 and the cursor {#program}

The **program** is the playlist that a station runs at the top level. It has a start time, **T0**, and a **cursor**. The cursor is the entry of the playlist that the program has reached.

- **T0** is the moment the program started. A block timed *after T0* moves with T0. A block at a fixed time of day, or a recurrence, never moves.
- **How T0 is set.** For a program started by hand, T0 is the moment of the press, early or late. You start a program by hand with **Change program…** on [On Air](on-air#start-program), the API, or a key on a control surface. For a program that starts itself on a schedule, T0 is the scheduled time. A program joined in progress can use the scheduled time as T0, even when it starts later (see [late starts](#late-starts)).
- **Expected starts.** A start block set to *expected, started by hand* only tells when the show is expected. Nothing starts by itself. The page shows "expected 18:45" with a countdown. A hand start within the start block's window (60 minutes by default) uses up that occurrence, and the page stops offering it.
- **Fallback start.** You can give a start block a fallback. If nobody starts the show by its time plus a grace period, the show starts itself. See [the fallback start](#fallback-start) below.

### Start a program {#start-a-program}

A start by hand always starts the program again from the top, with T0 at the press. It does not matter what the station is doing. You start a program by hand with the Stream Deck key **Start Playlist as Program**, with **Change program…** on [On Air](on-air#start-program), or with `POST /api/v1/program/start`.

- **The station is stopped or held**, or a Stop or Fade out block ended the audio: the audio starts again. The program's first Play block plays at once.
- **Something else plays**: a playlist, another program, a cart or a held item. It fades out over 2 seconds, and the program starts right after.
- **The same program runs**: it starts again from its first row. Its timed blocks move to the new T0. The blocks at T0 + 0, like an unmute, run again once. The blocks of the earlier start do not run.
- **A stop is fading out**: it finishes first, and the program starts right after it.

The program card on On Air says who started the program: `by hand from the Stream Deck`, `by hand on this page`, or `by hand through the API`. Only a [fallback start](#fallback-start) says `automatically`. Other starts say why: `by the schedule`, `by a playlist switch`, `when the program before it ended`, `after an abort` or `when the station started`.

### The fallback start {#fallback-start}

The fallback is off until the block turns it on: set `fallback_start` with `enabled: true` and `grace_s` in the Start block.

The example Monday program leaves the fallback off on purpose. A show that someone starts by hand must never start itself over a press that is late but on its way.

To turn the fallback on or off for the next occurrence only, press **Arm** or **Disarm** on the On Air page. The station refuses to arm a block that has no fallback (409), and the message tells you what to set.

### How the cursor moves {#how-the-cursor-moves}

The cursor moves down the list. Items and albums play. A block that takes no time is done at once, and the cursor continues. Examples are a message, a gain change and a loop that jumps. A block that takes time holds the cursor until the block is done.

### Blocks in sequence and blocks at a time

| A block is | It acts | Examples |
|---|---|---|
| **in sequence** | when the cursor reaches it, at the song change where the step before it ended | Play, Wait, Loop, Stop, a message between two songs |
| **at a time** | at its time, whatever is playing | Fade out at T0 + 14:40, a message at T0 + 15:00 |
| **standing** | all the time that the playlist runs | An insert rule (a liner every 4 songs) |

A block at a time that decides what plays **preempts**. At its time, these things occur:

1. The step that is running ends, in the way the block says.
2. The cursor passes over the entries between it and the block. The block log shows them as `passed`.
3. The cursor continues from the block.

If the cursor reaches a block at a time before its time, the cursor waits. A Play block before it continues to play during the wait.

A block at a time that only sends a message or changes the gain does not move the cursor.

## Play {#play}

A Play block plays from a source: a playlist, a category, or one item. **Until** sets how long it plays.

| Until | The block ends |
|---|---|
| **next** (the default) | The cursor moves on at once. The source continues to play until a later block takes over. A Wait after it keeps the music going. |
| **forever** | Never by itself. A block at a time still preempts it. |
| **a time** or **a duration** | At that time, or that long after the cursor reached the block. |
| **a count** | After that many items have been dealt. |

**End** sets how the play stops at its time:

- **at a song change**: at the first song change at or after the time;
- **fade**: the whole output fades over *fade seconds* at the time, then stops;
- **so a song finishes at the end time**: this is [back-timing](#back-timing).

### Nested playlists {#nested-playlists}

When the source of a Play block is a sequential playlist, that playlist runs **nested**:

- Its own blocks run.
- Its own times count from its own start.
- The parent's blocks always win at the same moment.

Nesting can go four levels deep. A playlist cannot contain itself. A shuffle playlist plays as a rotation, and its blocks do not run when it is nested.

## Wait and Loop {#wait-and-loop}

**Wait** holds the cursor. It waits for one of these:

- a time (after T0, or a time of day);
- a number of seconds;
- the operator, who presses **Continue** (on the On Air page, through the API, or on a control surface).

While a Wait holds, a Play block before it with *until next* continues to play. The block after the Wait acts at the first song change at or after the end of the wait.

**Loop** moves the cursor back to an earlier row, a number of times (or forever). *Times* counts all passes of the body, so 10 runs the body 10 times in total. If the entries in the loop take no time, the loop repeats forever, so the checker refuses it.

Example: a liner, a Play with *until a count of 3*, and a Loop with times 10. This plays ten liners, with three songs after each.

## Stop, fade and end {#stopping}

- **Fade out and stop** moves the **master gain** down to silence, along a cosine curve, over the seconds you give. Then it stops everything. The master gain is after the processing chain and the limiter, so nothing pushes the level back up during the fade. The next sound brings the master gain back to where the gain and mute blocks left it.
- **Stop** stops now (with a fade), at the next song change, or after the song on air.
- **End** ends the playlist that it is in. When a nested playlist ends, the cursor goes back to its parent. When the top-level program ends, the station continues with its scheduled playlist or its default playlist. If a stop or fade left the station stopped, it stays stopped.
- **Start playlist** replaces the program with another playlist, with a new T0. The pending blocks of the replaced program are cancelled.

After a stop or fade in sequence, nothing is dealt until the stop is complete. The next song or Play block then starts the sound again.

## Gain and mute {#gain-and-mute}

**Gain** sets the station's master gain over a ramp of milliseconds. It can lower the level but not raise it. The master gain is after the safety limiter, so the block refuses a level above 0 dB. To raise the level, use the output trim ([Output and level](output-and-level)).

**Mute** mutes or unmutes the station output over a ramp.

Both blocks ramp from the present value of the master gain.

## Messages to other equipment {#messages}

Three block types send one message and then continue. They never block the audio. A worker thread does this:

1. It sends the message at its time.
2. If the send fails, it tries once more after 250 ms.
3. It logs the result.

A message that cannot be sent raises the `block_failed` alert and writes a `failed` row in the block log.

| Block | What it sends | Settings |
|---|---|---|
| **OSC message** | One OSC message over UDP | Address of the receiver, port, OSC address, typed arguments (`i` whole number, `f` number, `s` text) |
| **MIDI message** | One MIDI message through a raw MIDI device on the box | The port (a device such as `hw:1,0`, or a device file), the message (control change, note on, note off, program change or system exclusive), channel 1 to 16, data bytes |
| **Web request** | One HTTP request | Method, an `http://` address, headers, body, timeout (1 to 30 s) |

A web request is plain HTTP. The program has no TLS, so use a local bridge for a service that needs `https`.

### Use a template {#templates}

The block form has templates with ready-made settings. The mixing-console channel mute sends the OSC address `/ch/N/mute` with the whole number 1 to port 2223 (and 0 to unmute).

Check these three things on the console:

1. The channel number has no leading zero.
2. The console accepts OSC writes. Its remote lock for OSC must be off.
3. The console's address is the one on the block.

These values come from the console maker's published OSC documentation. Confirm them on the console when you rehearse.

### Test a message {#test-a-message}

**Caution:** **Fire** sends the message for real. Point the block at a test receiver first.

To test a block, press **Fire** on its row on the On Air page. Press it a second time within 3 seconds to confirm. The station sends the block's message at once.

## Back-timing {#back-timing}

Back-timing is an option of a Play block. It is off by default. It plans the fill so that a **song finishes at the end time**, and a fade does not cut it.

The end time is the block's own *until* time or duration. When the block's *until* is *next*, the end time is the time of the next block at a time (for example, the Fade out).

The planner works from the real start of the program, including a late hand start. It looks at these things:

- the rest of the source;
- the real overlaps of the crossfade style;
- the items' own markers.

It then selects the songs that bring the last one to the end time. It plans again after every song change. Each plan is one row of the block log (kind `back_time`, `planned`).

| Mode | The last song |
|---|---|
| **Best fit** (default) | Is the one that ends nearest to the end time. The plan counts as met when it ends within the **tolerance** (45 s by default). |
| **Exact** | Starts at a fixed time, so that its end is on the end time to within 21 ms. If the song before it must be cut, it fades over 2 s. This is the old "end at" action. |

The plan shows as **END +3 s** (the planned end minus the end time; a positive value is late). It shows on the program panel, in the queue and on the status display. Its state is one of these:

- `planning`;
- `on_target` (within 0.5 s);
- `within_tolerance`;
- `fallback`: no plan reached the end time within the tolerance. The block's fade (or the next block's own fade) closes the fill at the end time.

**Not built:** ending the last song early at a natural quiet point, and a short fade on the last song. The tolerance and the selection of the last song do that work for now.

The API call `POST /api/v1/end-at` makes a one-off program. It plays a playlist with an exact back-timed end, and then it can start another playlist at the song change.

## Late starts and restarts {#late-starts}

A block at a fixed time can already be in the past. This occurs when you join a program in progress, or when the station restarts. Each block has a **late policy**:

- **skip**: log the block as `missed` and continue;
- **run now**: act at once, in order;
- **run if within** a number of seconds: act at once if the block is late by no more than that, else skip.

The default is *run if within 60 s*. Fade out, Stop and End are the exceptions: by default they are skipped, so that a late join to a show never stops the music at once.

The cursor still moves past a missed flow block, so the program keeps its shape. For the T0 you select, the start dialog on [On Air](on-air) and the dry run on [Playlists](playlists) list every block already in the past and what its policy will do.

A restart continues the program. The station keeps T0, the cursor and the block log. So a fade at 18:59:40 still fires at 18:59:40 after a restart at 18:52.

## Program actions {#actions}

| Action | Effect |
|---|---|
| **Change program…** (start a program) | Starts a playlist as the program. T0 is the press, the scheduled time, or a time you give. |
| **Continue** | Releases a Wait for the operator. |
| **Fire** (fire a block) | A block of the running program acts now, as if its time had come. It is logged as fired by hand and does not fire again. |
| **Skip** (skip the next block) | The cursor passes the block where it stands, or the next block at a time is skipped (logged `skipped`). The button is on the row of the next block. |
| **End program** (the ⋯ menu) | The program ends with no more blocks. **End program, music continues** leaves what plays. **End program and stop the audio** stops it too. Each item needs a hold of one second. |
| **Arm** / **Disarm** | Turns the fallback start of the next occurrence on or off. |

An outside program can also send `POST /api/v1/external/continue` to release a Wait for the operator.
