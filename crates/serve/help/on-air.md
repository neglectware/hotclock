# On Air

This page shows what a station is playing now. It also has the keys that change it: the transport keys, the program, the queue and the alerts.

The page works on a phone. A phone in portrait shows the same units in one column, with a bar of four keys at the bottom. See [The dock on a phone](#dock).

On a computer, the page fits the window, so you never scroll it. The queue and the program scroll inside their own boxes. Alerts stays in view at the bottom right. If the window is very short, the page scrolls as usual.

You can change the layout of the page with the **Layout** button. See [Layout](#layout).

Select the station with the buttons at the top. Each button shows the station's name and a short word for its state. The word is the most urgent of these: `safe`, `down`, `stopped`, `held`, `assist`. When none of them applies, the word is the station's health: `ok`, `warning` or `critical`. Everything on the page belongs to the selected station, except **Layout** and the **Box monitor** buttons on the right of the title. They belong to the whole box. See [Box monitor](#display-view).

## The on-air unit {#the-now-card}

The top unit shows the item on air. It has the cover, the title, the artist and album, and three clocks:

- **Elapsed** is the time played.
- **Left** is the time to the end of the item.
- **The third clock** says what happens next. See the list below.

A bar under the clocks shows the progress of the item. A white mark on the bar is the segue point, where the next item starts.

### The third clock

- `SEGUE IN 0:14` counts down to the point where the next item starts.
- `ALBUM: gapless` means the next item is the next track of the same album. It starts with no gap.
- `HOLDS AT END` means that the station will stop at the end of this item and wait. The line under it says why: `hold: then waits`, or `assist: then waits for Next` (or `for Continue`, while the program waits for you).
- `HELD, ENDS 0:40` means the station is held and the item ends in 40 seconds.
- A held or stopped station never counts below zero. The clock shows `HELD`, `STOPPED` or `SAFE MODE` and how long ago the item ended.

### The state label and the lamps

The colored label beside the station's name is its state: **PLAYING**, **HELD**, **STOPPED** or **SAFE MODE**. In [assist mode](#assist-mode) the label is amber and reads **ASSIST**. A red chip with another word (`emergency`, `ram_loop` or `safe`) means the station is not playing its normal programming. See [Failures and recovery](failures-and-recovery).

Four lamps are on the right of the title bar. On a phone, only the lit ones show.

| Lamp | Lit when |
|---|---|
| `PLAYING` (green) | The station plays. |
| `HOLD` (amber) | A hold is set, or the station is held. |
| `ASSIST` (amber) | Assist mode is on. |
| `SAFE` (red) | The station is in safe mode. |

A red `reconnecting` chip appears while the page has lost its link to the station. The page connects again by itself and then reads everything again. You do not need to reload it.

### Levels {#program-meters}

Two thin bars show the left and right levels of the program after the safety limiter. The thin white mark is the peak hold. The numbers under the bars are loudness in LUFS:

- **M**: momentary (the last 0.4 seconds).
- **S**: short-term (the last 3 seconds).
- **W**: the loudness of the last four minutes.
- **I**: integrated since the station started.

The bars follow the [meter ballistics](#meter-ballistics). See [The processing chain](processing) for what happens before the bars.

### Coming up

The list on the right shows the next two or three entries of the queue, with the time each one is expected to start. A block of the program shows as a badge, for example `WAIT`.

When the program waits for you, an amber box says **Waiting for you** and names the block. The item after the wait shows `after Continue` instead of a time. Press **Continue** to release the wait.

## The transport keys {#transport}

The keys act on the station immediately. They sit in one strip under the on-air unit. They are in groups: **Program**, **Advance**, **Mode** and, set apart on the right, **Hard actions**. Some keys have a guard, so that a slip of the finger cannot cause a fault on air. The guards belong to this page. The API and the Stream Deck do not have them.

### Continue

**Continue** releases a Wait block that holds for the operator. It is lit amber, and its small line says `release the Wait`, only while the program waits for you. At other times it is gray, it says `no Wait`, and it does nothing. See [The program unit](#program).

### Next

**Next** starts the next item now. The item on air fades out over **one second**. The next item starts so that its first strong beat lands where the fade ends. Use it to skip a song smoothly.

### Fade

**Fade** is like **Next**, with a slower fade: **two seconds**, on a smooth curve. The next item starts so that its first strong beat lands where the fade ends.

### Hold the station after this item {#hold}

**Hold** lets the item on air play to its end. The station then stops in the **held** state, and nothing else starts. The third clock counts down to the end.

**Hold** is a latching key. It has a lamp. When the lamp is lit, a hold is set, and the small line says `on`. Press the key again to cancel the hold. The station then continues with its queue.

### Use assist mode {#assist-mode}

Assist mode is for a live show in which a person starts every item. With assist **on**, the station plays the item on air to its end. It then **holds after every item until you press Next**. It never starts the next item by itself.

**Assist** is a latching key with a lamp. The small line says `on` or `off`.

- The queue is still planned and shown, so you can see what **Next** will start.
- **Next**, **Fade** and **Cut** start the next item immediately. While the station waits in silence, each of them starts the next item from its start point.
- If you turn assist off while an item plays, the hold is released. The station then joins items by itself again at the next segue.

The station keeps the mode when it restarts.

### Cut {#cut}

**Cut** stops the item on air with a 30 millisecond fade and starts the next item at the same moment. The short fade prevents a click. Use **Cut** when the change must happen at an exact instant.

**Cut** is under a cover. Press it once to lift the cover. The key turns red and its small line counts down from 5 seconds. Press it again within 5 seconds to cut. If you do not, the cover closes again and nothing happens. Escape also closes the cover. The cover does not delay the cut: you choose the exact instant with the second press. From the keyboard, Enter or Space lifts the cover, and a second press cuts.

### Stop and Play {#stop}

**Stop** fades the program out over about one and a half seconds. The station then stays stopped. A stopped station stays silent until someone presses **Play** or changes the program.

You must **hold Stop for one second**. A click or a short press does nothing. A red bar runs along the key while you hold it. Let go before it is full, and nothing happens. A touch screen works the same way. From the keyboard, hold Enter or Space for one second. A single tap on a key does nothing.

When the station is stopped, the key becomes **Play**. It is green and has no guard. **Play** starts the first item of the queue.

### The dock on a phone {#dock}

On a phone, the keys are in a grid: **Continue**, **Next** and **Fade** on top, **Hold** and **Assist** under them, then **Cut** and **Stop**. When you scroll the strip out of sight, a bar at the bottom of the screen keeps the keys you use all the time: **Continue**, **Next**, **Fade** and **Add**. The bar goes away when the strip is on the screen again. The keys are at least 44 pixels high. The page tells the phone not to open its own long-press menu on the keys.

### Add an item from a phone {#add}

**Add** opens the library search on the whole screen. Use it for a live show, when you need a song that is not planned.

1. Press **Add**.
2. Type a title, an artist or an album.
3. Under **Insert as**, **Next** is selected. Select **At the end** to put the item last.
4. Press **Add** on the item. The button then says `Added`.

You can add more than one item. Press **Close**, or Escape, to go back to the page. Each item is a pinned entry, as with the search box of the Queue unit. See [Insert an item from the library](#insert-from-the-library).

## The program unit {#program}

The **Program** unit is under the transport keys. It shows the program that the station runs. A program is a playlist that runs from a start time, T0. See [The block engine](block-engine#program) for how it works, and [Playlists](playlists#the-block-palette) for how to build one.

The header has the state of the program, **Change program…** and a **⋯** menu. The state is `RUNNING`, `WAITING` (it holds for the operator) or `HALTED`.

### A plain playlist

A playlist without blocks shows as a status block. It says what kind of playlist it is, for example `Shuffle playlist, 170 entries`. It says when the program started, with the date if it was not today, and how: `from the Stream Deck`, `through the API`, `from this page`, `automatically`, `at the expected start` or `by a Start block`.

When the no-repeat rule cannot be kept, a notice says so, for example `No repeat: set to 120 min, using 84 min (only 18 tracks)`. The first number is what the playlist asks for. The second is what the playlist can keep. The notice has a link to the playlist, so that you can add tracks. See [Playlists](playlists#no-repeat).

### A show program

A program with blocks shows more:

- **The expected start.** When a playlist's Start block says when the show is expected, a line counts down to it, for example `Weekly show, expected 18:45`. An `armed` chip shows that the fallback start is on.
- **The cursor.** For example `row 4, block 4 of 8`. When a Play block runs a nested playlist, a line shows the chain, for example `Show (row 2) > Pre-show (row 1)`.
- **The back-timing line.** This line appears when a Play block is back-timed. `END +3 s` is the planned end minus the end time, so plus is late. The line is green when the plan is on target, amber within the tolerance and red in fallback. The word for its state follows. See [The block engine](block-engine#back-timing).
- **The rundown.** One row for each block. The columns are the row number, the badge, the name (with `in sequence` or `at a time` under it), **At** (the time the block acts), **In** (the time to go) and the status. The time to go turns amber under 60 seconds and red under 10 seconds. The block where the program is now is highlighted. The rows of finished blocks are dim. On a phone, At and In move under the name.

The status of a block is `pending`, `fired`, `soft`, `hard`, `late_run`, `skipped`, `passed`, `missed`, `failed`, `done` or `current`. A fired block shows how far from its time it acted. `failed` means that a message could not be sent.

### Fire and Skip

**Fire** is on each pending block. It makes the block act now, as if its time had come, and it does not act again.

**Caution:** **Fire** really sends the block's message. Point the block at a test receiver first.

**Fire** has two steps. The first press changes the key to `Fire FADE?` (with the badge of the block). Press it again within 3 seconds to send the block. After 3 seconds, the key goes back.

**Skip** is on the next block only. The program passes that block.

### The ⋯ menu {#end-program}

The **⋯** menu in the header has these items:

- **Arm the fallback start** or **Disarm the fallback start.** It appears when the block has a fallback start, or when the next start is already armed. **Arm** turns on the fallback for the next occurrence only. If nobody starts the program, it then starts by itself. **Disarm** turns the fallback off.
- **Open the playlist** opens the playlist in the [Playlists](playlists) editor.
- **End program, music continues** ends the program. No more blocks run. The music on air keeps playing.
- **End program and stop the audio** ends the program and the audio stops too.

Each **End** item fires only when you **hold it for one second**. A click does nothing.

### The settings line {#settings-line}

The last line of the unit shows three settings of the station and a link:

- **Crossfade** shows the station's crossfade style. Press it to open the picker. See [Change the crossfade style](#crossfade-style).
- **No repeat** shows the no-repeat time of the playlist. When the playlist cannot keep it, a chip shows the time that it can keep.
- **Meters** shows the preset of the [meter ballistics](#meter-ballistics). Press it to go to the setting.
- **Station settings** opens the [Settings](settings) page on the station's tab.

### Change the crossfade style {#crossfade-style}

Press **Crossfade** in the settings line. A picker opens with the station's crossfade styles and one line about each. The change applies from the next boundary that is not already committed. See [Crossfades](crossfades#choosing-a-style). The picker has a link to the same setting in [Settings](settings).

## Change the program {#switch-playlist}

**Change program…** is the one way to change what the station plays from this page. Switching to another playlist and starting a show are the same operation: in the station, a switch is a program start with T0 set to now. The page asks only for what the playlist needs.

1. Press **Change program…**. A sheet opens.
2. Select a playlist. The list shows each playlist's mode and size. `on air now` marks the playlist that plays. `show` marks a playlist with blocks.
3. Select when the first song starts (see the table).
4. For a playlist with timed blocks, select T0 (see below).
5. Press the button at the bottom, **Start** and the name of the playlist.

| Choice | API word | What happens |
|---|---|---|
| **At the next song change** | `segue` | The first item of the new playlist becomes the next item. It starts at the normal segue point of the item on air. Nothing is cut short. |
| **Now** | `now` | A fade to next (two seconds) into the first item of the new playlist. |
| **When this song ends** | `end` | The first item of the new playlist starts after the item on air ends, with no overlap. |

The API keeps the English words in the middle column. A plain playlist starts with **At the next song change**. A show starts with **Now**.

A playlist without blocks calls the station's `playlist/switch`. The new playlist becomes the program. Entries that you placed by hand stay in the queue. On a stopped station, the first item starts immediately. The page refuses a playlist that is disabled or has nothing playable, and tells you why.

### Start a program with blocks {#start-program}

A playlist with blocks calls `program/start`. When the playlist has timed blocks or a Start block, the sheet also asks for T0, the zero of the show:

- **Now**: T0 is the moment of the press.
- **The expected start**: T0 is the time that the Start block expected.

The sheet then shows the **dry run** for that T0. It says:

- how many blocks are **already past**,
- for each past block, its late policy and what happens to it (see [Late starts](block-engine#late-starts)),
- any error from the playlist's checks.

Nothing is sent by the dry run. When you press the start button, the program starts. The sheet then lists what was done with the blocks that were already past. Press **Close** to close the sheet.

Escape closes the sheet without a change.

## Queue

The unit on the right shows what will play. Each row shows the cover, the title, the artist and the time the item is expected to start. On a computer, the unit ends where the left column ends. Scroll inside it to see the rest of the queue. The search box stays at the top.

On a phone, the Queue unit comes right after the keys, before the Program unit. You can change this order with [Layout](#layout). With a **Compact** queue, the unit shows only a few rows, and you scroll inside it.

- **Chips** show where the item came from (`rotation` is not shown, because it is the normal case): `liner`, `id`, `block`, `manual` and others. **pinned** marks an entry that you placed by hand. **gapless** marks an album track that follows its neighbor with no gap. A word in amber, for example `cut`, is a transition that you set for this item.
- **Drag** a row to move it. The planner cannot change the item that is already committed as next. That item is inside the lock-ahead time (`lock_ahead_s`, 20 seconds by default). You can still remove it, and the station then uses the item after it.
- **The ⋯ button** on a row opens the transition select and **Remove**.
  - The transition sets how this item starts after the one before it, for this one transition only. It is disabled on a gapless row, because a gapless run ignores every transition setting. A custom patch set through the API shows as `custom patch`. The options are `default` (follow the category, the item and the station), `cut` (no overlap) and one of the crossfade styles. See [Crossfades](crossfades#choosing-a-style).
  - **Remove** takes the entry out of the queue.
- **A row with a colored badge** is a block of the running program that acts soon. The badges are `FADE`, `STOP`, `PLAY`, `OSC`, `MIDI`, `HTTP`, `ID`, `WAIT`, `LOOP`, `GAIN` and `MUTE`.
  - The row shows the block's name and the time to go, in minutes and seconds. The time turns amber under 60 seconds and red under 10 seconds. A Wait for the operator shows `hold`.
  - A block row has no drag handle, no **⋯** and no transition.
  - The queue lists the blocks that act within the next hour, the actions of in-sequence blocks that wait for a song change, and the block where the program is now. See [The program unit](#program).
- The planner keeps the queue at least five items and at least thirty minutes deep. It plans the queue again each time something changes.

### Reshuffle

**Reshuffle** is in the header of the Queue unit. It makes a new random order for the rest of the current shuffle pass. It never changes these entries:

- the item on air,
- the item that is already committed as next,
- an entry that you placed by hand.

The audio does not change until the item after the committed one. You cannot reshuffle a playlist in sequential mode. The page tells you so.

You seldom need **Reshuffle**. A shuffle playlist already makes a new random order for every pass. See [Playlists](playlists#how-a-shuffle-playlist-chooses).

### Insert an item from the library {#insert-from-the-library}

Use this to play a request, a promo or a sound effect immediately. The search box is at the top of the Queue unit.

1. Type in the search box to find the item.
2. Press **Next** to put the item first in the queue, or **End** to put it last.

Both buttons create a pinned entry. On a phone, the **Add** key of the dock opens the same search on the whole screen. See [Add an item from a phone](#add).

## Box monitor {#display-view}

The `single`, `split`, `rotate` and `processing` buttons are in the header of the page, on the right. They set what the status display on the box's own monitor shows. They also set what a web display shows. They belong to the whole box, and the Stream Deck has the same setting. `single` and `processing` show one station: the station of the tab selected on this page. The button then names it, for example `single · Eve`. To show another station, select its tab and press the button again. `split` and `rotate` show every station. `processing` shows the gain-reduction meters of one station's chain. See [The status display](status-display#views).

## Processing {#processing}

The processing display shows what the station's [processing chain](processing) is doing now. It shows the gain reduction of each part of the chain. Choose one of three displays with [Layout](#layout), or none.

- **Meter bridge.** One thin row under both columns, the full width of the page. This is the default.
- **In the on-air unit.** One line under the levels of the on-air unit. It leaves out the safety limiter and the preset.
- **Rack unit.** A small unit at the bottom of the left column. It uses the parts of the rack on the [Processing](processing) page: lamps, centered gain bars and band bars.

Each display shows these parts:

- **AGC**: a lamp and the gain in dB. The lamp says `RIDING`, `GATED`, `FADE` or `CLAMPING`. `OFF` means the AGC is off.
- **Multiband**: one bar for each band of the multiband compressor. The meter bridge also shows the largest reduction, and the rack unit shows the value of each band.
- **WB**, **CLIP**, **LIM** and **SAFE**: the wideband compressor, the clipper, the true-peak limiter and the safety limiter, each with a bar and its reduction in dB.
- **Loudness**: a lamp for the loudness controller (`RIDING`, `ON TARGET`, `HOLDING`, `AT LIMIT` or `OFF`), its gain and its target. The loudness itself is in the on-air unit: M, S, W and I.

A gain-reduction bar runs from 0 to -12 dB. It moves at once when the reduction grows, and falls back at 15 dB per second. `--` means that the block is off, or that no meter reading has come for half a second. A red `bypassed` chip shows while the chain is bypassed. The name of the preset links to the [Processing](processing) page, and so does **Processing ›** (or **Open the rack ›**).

The display uses the meter readings that the page already receives. The meter bridge and the rack unit also ask the station for the AGC's state once a second, while the page is in view. This is how they know `FADE` and `CLAMPING`.

## Layout {#layout}

The **Layout** button is at the top of the page, beside **Box monitor**. It opens a small panel. Each change applies at once. The page remembers the layout in this browser only. Other browsers and other people keep their own layout.

- **Processing display**: **Meter bridge**, **In the on-air unit**, **Rack unit** or **Off**. See [Processing](#processing).
- **Queue**: **Full** fills the column, and the rows scroll inside it. **4**, **6**, **8** or **12** shows only that many rows. On a short window the unit can show fewer.
- **Program unit**: **Full** shows the whole rundown, and it scrolls inside the unit. **Compact** shows the status and the current block only. Press **Show the rundown** to see all the blocks.
- **Columns** (on a computer): the width of the left column (program) against the right column (queue): **40 / 60**, **50 / 50** or **60 / 40**.
- **On a phone or tablet**: **Queue first** or **Program first**. This sets which unit comes after the keys, when the page is one column.
- **Reset to default** goes back to the default: meter bridge, full queue, full program unit, 50 / 50, queue first.

On a computer, every layout fits the window. The units scroll inside. If the browser cannot store the layout, for example in a private window, the page uses the default layout.

## Meter ballistics {#meter-ballistics}

The setting for how the program bars rise and fall is in [Settings](settings), on the **This box** tab, under **Meter ballistics**. The card has a row for the box default and a row for each station. The **GR bars** fields set how the gain-reduction bars of the status display's processing view release. The **Meters** word in the settings line of the Program unit links to the card. See [The status display](status-display#meter-ballistics).

## Alerts

The unit at the bottom right lists the active alerts. Each alert shows its severity, its code and a message. When there is none, the unit says `none`.

- Press **Ack** to acknowledge an alert and remove it from the list. If the problem is still there, the alert can come back.

See [Alerts](alerts) for what each code means and what to do.

## Leave the page open

You can leave this page open for days, on a computer or on a phone. When the connection to the station breaks, for example when the box restarts, the page shows `reconnecting`, connects again and reads the state again. When a phone wakes up after it slept, the page reads the state again too. When you go to another page, the page closes its link to the station.
