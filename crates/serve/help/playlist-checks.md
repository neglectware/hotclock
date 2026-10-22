# Playlist checks

This page lists every message that the playlist checker can show. For each message it gives the meaning and what to do.

The checker runs when you press **Validate** on the [Playlists](playlists) page. It runs again when a playlist is enabled. Each message appears in a chip under the row of the block it belongs to.

There are two kinds of message:

- An **error** (red) means that the block cannot work as written.
- A **warning** (amber) means that the block works, but probably not as you expect.

Through the API, a playlist with errors cannot be saved as enabled. Correct the errors before you rely on a block.

Each section below has the check's code as its heading. You also see this code in the API.

### `unreadable`

**Error.** The settings of the block cannot be read. Examples are a time that is not a time, or an unknown mode. The message gives the reason.

The station never runs a block with this error. To correct it, open the block with **Edit**, correct the field and save.

### `negative_offset`

**Error.** `a negative relative offset`. A time written as T0 plus a number cannot be negative. Use a time of zero or later, or use a time of day.

### `source_missing`

**Error.** `its source playlist N does not exist`, `its source category does not exist` or `its source item N does not exist`. The block names a playlist, category or item that was deleted. Select a source that exists.

### `in_sequence_in_shuffle`

**Error.** A block that runs in sequence is in a shuffle playlist. Such a block runs when the playlist reaches it, but a shuffle playlist has no order, so the block has no place. Make the playlist sequential, or make the block timed.

### `same_time`

**Error.** `two timed flow blocks at the same time in one playlist`. Two blocks that decide what plays are set for the same moment, so neither one can win. Move one of them.

This check compares times after T0 and times of day. Recurrences are exempt.

### `times_backward`

**Error.** `timed flow blocks whose times run backward in list order`. A block lower in the list has an earlier time than a block above it. Put the entries in an order where the times increase down the list, or change the times.

### `nesting_cycle`

**Error.** `playing this playlist nests a playlist inside itself`. A Play block names a playlist that plays the first playlist again, directly or through other playlists. Remove one of the Play blocks.

### `nesting_depth`

**Error.** `nesting is N deep; at most 4 are allowed`. A playlist played inside a playlist can go at most four levels deep. Make the chain shorter.

### `insert_every_n`

**Error.** `insert_every needs n of at least 1`. An insert rule needs a number of songs of 1 or more. Change the number.

### `self_source`

**Error.** `a playlist cannot be its own insert source`. An insert rule inside a playlist takes its items from that same playlist. Select another source.

### `start_no_schedule`

**Error.** `an absolute start block needs a schedule`. A start that begins by itself has no day and time. Add a day and time on the [Schedule](schedule) page, or change the start to *expected, started by hand*.

### `start_clash`

**Error.** `playlists A and B both start at the same time (blocks X and Y)`. Two playlists of one station have recurring starts on the same weekday and time. Change one of them.

This check looks at the whole station, not only one playlist.

### `station_flow_block`

**Error.** `a station block cannot be a <type> block (flow blocks belong to a program)`. A station block applies under every playlist. So it cannot be a block that decides what plays (play, switch playlist, stop, fade, end). Move the block into a playlist.

### `station_relative_time`

**Error.** `a station block has no T0; give it a recurrence or a date`. A station block has no playlist start, so it cannot use a time after T0. Use a recurrence or a date.

### `never_fires`

**Warning.** `this block follows an end and can never fire`. An enabled block is below an End block in the list. The playlist finishes before it reaches the block. Remove the block, or move it above the End block.

The dry run shows the same code, with another message, for a block whose time is after a stop at a fixed time.

### `thin_source`

**Warning.** `a play source with fewer than 2 playable items`. A Play block takes songs from a playlist or category with fewer than two playable items, so it cannot rotate. Quarantined items do not count. Add items, or release quarantined ones.

### `dst_nonexistent`

**Warning.** `<time> does not exist in <zone> (a clock change); it fires at the first valid instant`, or `a time between 02:00 and 03:30 does not exist on the spring clock change; it fires at the first valid instant`.

On the night the clocks go forward, some local times do not exist. The block still fires, at the first real instant after the missing time. If the exact time is important, select another time.

### `silent_gap`

**Warning.** `the station is silent for N s before this block (after a stop or fade_stop)`. After a Stop block or a Fade out and stop block, the next timed flow block starts later than the stop ends. The station is silent in between.

This is correct if you want it, for example a pause between two parts of a show. If not, move the next block earlier or remove the stop.

### `loop_target`

**Error.** `a loop goes back to an earlier entry of the list`, or `a loop goes to a position from 1, and runs 1 or more times`. A Loop block jumps back to an entry above it. To correct it:

- Give it a position above the block. Rows count from 1.
- Or leave the position empty. The loop then jumps to the first entry after the start block.
- Give it a number of times of 1 or more, or "forever".

### `loop_no_time`

**Error.** `the body of this loop takes no time`. The entries between the loop's target and the Loop block have no song, no Play block and no wait. The loop would repeat forever and play nothing. Put a Play block, a song or a Wait block inside the loop.

### `gain_range`

**Error.** `the master gain sits after the safety limiter and cannot be raised above 0 dB`. A Gain block can lower the level of the whole station, but not raise it. Use a level of 0 dB or less. To raise the level, use the output trim (see [Output and level](output-and-level)).

### `back_time_no_end`

**Error.** `back-timing needs an end time`. A Play block is set to finish a song at its end time, but it has no end time. Do one of these:

- Give it an end time or a duration.
- Put a block with a time (for example a fade) after it in the list.

### `back_time_short`

**Warning.** `the back-timing window is N s, shorter than the shortest item of the source`. The time from the start of the Play block to its end time is shorter than the shortest song of its source. No song can finish at the end time. Move the end time later, or use a source with shorter songs.

### `past_at_start`

**Warning.** `<block> is N s in the past at this start; its late policy (...) will run it now or skip it`. This warning appears only in the dry run. It appears when the start time you ask for puts a timed block in the past, for example a late start or joining a show in progress. The message says what the block's late policy does with it.

By default, a block runs if it is at most 60 s late. Fade out, Stop and End blocks are the exception: they are skipped. This way, a late start of a show never stops the music at once.
