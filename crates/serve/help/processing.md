# The processing chain

The processing chain is the broadcast processor between the mix and the limiters. It is a fixed row of optional blocks. Each block has an on/off switch, all its settings in view and its own presets. You edit the chain on the **Processing** page.

## What it is, and what it is not

The chain is an **Optimod-style processor**. Each block works like a plug-in in a fixed broadcast order. The chain is a tool for an engineer: an aggressive setting (a hard limiter, a fast AGC) is a choice that you make, not a fault. The shipped presets are tuned for the stations they ship for. Their numbers are starting values. Adjust them by ear.

The order is fixed. You cannot move a block, with one exception: the equalizer can run before or after the multiband compressor (`eq.position`). These rules never change:

- The AGC comes before the multiband compressor.
- The clipper comes before the loudness gain and the true-peak limiter.
- The loudness controller comes before the true-peak limiter.
- The safety limiter is last.

```
mix
  -> input trim
  -> input high-pass            (optional)
  -> phase rotator              (optional)
  -> stereo enhancer            (optional)
  -> wideband AGC
  -> equalizer                  (when eq.position is pre)
  -> bass enhancer              (optional)
  -> multiband compressor
  -> equalizer                  (when eq.position is post)
  -> wideband compressor        (optional)
  -> output makeup
  -> clipper                    (optional)
  -> loudness controller's gain (when it is on)
  -> true-peak limiter
  -> safety limiter
  -> master gain, output trim
```

The chain has no **de-esser**. Remove sibilance in the studio, for each microphone, not on the program.

## The Processing page {#the-panel}

**Processing** in the menu opens the chain of one station at a time.

### Select a station {#select-a-station}

The top of the page has a tab for each station, as on the [On Air](on-air) page. Each tab shows the station's state (`ok`, `warning`, `critical` or `down`). Select a tab to change the station.

- The page keeps the station that you selected in this browser.
- The address names the station (`#/processing/<station>`), so a bookmark or a link opens that station's chain directly.
- A station that does not answer shows a notice in place of its chain.
- The [System](system) page has a link to the chain of each station.

Under the tabs are the station's **Processing chain** card and the blocks, in chain order. The [loudness controller](output-and-level#loudness-controller) is in its place, between the clipper and the limiter. The **View** switch on the chain card sets how the blocks look: **Rack**, **Faceplates** or **List**. The browser keeps your choice. **Rack** is the first choice on a new browser.

### The Rack view {#rack-view}

The **Rack** view shows one unit for each block, in signal order, like the units of a rack. Most units are 1U high (a thin strip). Four blocks are 2U high, which is two strips and the gap between them: the equalizer, the multiband compressor, the wideband AGC and the loudness controller. A 2U unit has a display. All units have the same width and the same columns, so you can read down the page.

From left to right, a strip has:

- the block's **number**
- the **power switch**: **ON**, **BYPASSED**, **ALWAYS ON**, or **OFF** for the loudness controller. Click it to switch the block. The strip does not open. **ALWAYS ON** is green like **ON**, but it is not a switch: the input trim, the output makeup and the safety limiter cannot be switched off.
- the block's **name**
- a **summary** of its main settings. The AGC and the loudness controller show their gain and their target. Their state is on the lights of the 2U display.
- the **meters**
- a small arrow: **▸** when the strip is closed, **▾** when it is open

A bypassed block is dim, with an amber mark at the left of its strip.

In a 2U unit, the number, the switch and the name stay on the first line. The summary is under the name. The level meter is at the right, in the same column as in the 1U units, in the middle of the unit. The display is between the summary and the meter. It ends where the gain-reduction bars and the meters of the 1U units begin.

The rack does not grow wider than 1440 pixels. On a wider window it stays at the left of the page.

**One block is open at a time.** Click a strip to open it. The block's full faceplate shows under the strip. Click the strip again to close it. When you open another strip, the first one closes. The strips below the open block move down. Nothing else moves. **Advanced** opens inside the open block. The browser remembers which block is open.

On the keyboard, Tab goes to a strip. Enter or Space opens and closes it. The Up and Down arrows go to the strip above and below.

On a tablet the strips are a little taller. On a phone each strip has two lines: the name and the switch above, the summary and the meters below. The summary shows its first two values.

#### What the meters show {#rack-meters}

Most strips show the level at the **output of the block**, left and right, as two thin bars made of small segments:

- The bars are green up to -12 dBFS, amber up to -3 dBFS, and red above that. The scale is -48 to 0 dBFS.
- A white mark holds the highest level for about one and a half seconds, and then falls back.
- The number is the peak of the louder channel, in dBFS.

So you can read down the rack and see the level at each stage of the chain. A block that is off passes its input on, so its bars show the level that goes in.

Some blocks show more:

- **Wideband compressor, clipper, true-peak limiter, safety limiter:** a **GR** bar at the left of the level bars. It shows the gain reduction, 0 to -12 dB, with the number.

The displays of the 2U units:

- **Equalizer:** the response curve, drawn large. When the **Analyzer** is on, the live spectrum is behind the curve, also while the unit is closed. You cannot drag the curve here. Open the unit to edit the bands and to set the analyzer (**off**, **in**, **out**).
- **Multiband compressor:** a gain-reduction bar for each band, with the name of the band and its reduction in dB under it. The bar hangs from the top. A longer bar means more reduction.
- **Wideband AGC and loudness controller:** a row of state lights with their names, and only the current state is lit. The AGC has **RIDING**, **GATED**, **FADE** and **CLAMPING**. The loudness controller has **ACQUIRING**, **HOLDING**, **RIDING**, **ON TARGET** and **AT LIMIT**. Under the lights is the gain bar, drawn around its center (up to 12 dB each way). Under that is a graph of the last 60 seconds of gain, with 0 dB in the middle.

On a phone, a 2U unit is two lines of the strip and the display under them. The display is smaller there, and only the lit state keeps its name.

### The grid of faceplates {#the-rack}

In the **Faceplates** view the cards are in a grid. The grid has four columns on a wide screen (from 1600 pixels), three from 1200, two from 800, and one on a phone.

- Each card takes as many columns as its controls need on one line, and no more.
- A card with a display takes the columns that are left in its row. The displays are the equalizer's curve, the history of the AGC and of the wideband compressor, and the strips of the multiband compressor. So the rows are full, and a card with nothing to show is not stretched.
- The cards always stay in signal order.

### The chain card {#the-chain-card}

The **Processing chain** card has these controls:

- the **chain preset** drop-down
- a **Bypass the chain** button
- a **View** switch: **Rack**, **Faceplates** or **List**
- a line that tells you which blocks are on

You do not have to select a preset. Every switch and every setting works at any time. After you change something, the drop-down reads **custom (based on radio)**, with the name of the shipped preset that the chain started from, and a **modified** mark.

### The faceplates {#the-faceplates}

Each block is a faceplate, numbered in signal order. All faceplates have the same layout:

- **First line of the nameplate:** a labeled **power switch** (a green **ON**, or an amber outlined **BYPASSED**) and the block's name.
- **Second line:** the block's **preset** drop-down, with **Presets** for the buttons that save, rename, delete, export and import. The **Advanced** switch is at the right.
- **Below:** the main controls on one line, the block's display if it has one, and the **meter dock** at the right edge.

### Use the controls {#use-the-controls}

- **Knobs.** Drag a round knob up or down to turn it. The whole range is 160 pixels. Hold Shift for ten times finer steps.
- **Keys.** The arrow keys move a knob one step. Shift, Page Up and Page Down move it ten steps. Home and End go to the ends.
- **Default.** Double-click the knob, or press Backspace, to set it back to its default.
- **Exact value.** Select the number under the knob, or press Enter, and type a value.
- Frequencies, times and ratios turn on a logarithmic scale.
- **Scrolling never changes a setting.** The mouse wheel and the trackpad only scroll the page, also when the pointer is over a knob, a fader, a graph or a number box. To change a value, drag it, use the keys, or type it.
- **Vertical faders** work in the same way. The multiband compressor uses them for threshold, ratio and makeup in each band.
- **Rows of buttons** are for choices with few values: the high-pass order, the band count, the equalizer position.

### Main and Advanced settings {#main-and-advanced}

A card shows its main controls. It keeps the other controls under **Advanced**.

- **Advanced** is closed until you open it. It stays open while you stay on the page.
- The switch is at the right of the second line of the nameplate. When it is open, the other settings appear under the main controls.
- For example, the AGC shows target, maximum gain, gate and the two rates. The fast detector and the fast clamp are under **Advanced**.

### Meters and displays {#meters-and-displays}

Every block with meters has the same **meter dock** at the right edge of its card:

- input and output level bars that rise from the bottom (-60 to 0 dB)
- the gain reduction
- a line of lamps: **gate** for the AGC, **clipping**, **limiting**

Gain reduction **hangs from the top**. The longer the bar, the more the signal is turned down. The number is under the bar. The AGC's bar is drawn around its center line: a boost goes up, and a cut hangs down.

The displays:

- **Equalizer:** its curve, with a point for each band that is on (up to 8). Drag a point to change its frequency and gain. Press Page Up or Page Down on a point to change its Q. Under the curve, a strip has a column for each band, and **+ Band** adds one. The **pre** and **post** switch is in the corner of the curve. The **Analyzer** switch (off, in, out) is in the other corner. It draws a live spectrum behind the curve.
- **AGC:** its gain over the last minute, around a center line.
- **Wideband compressor:** the last 20 seconds. The input level is gray from the bottom, the output level is a green line, and the gain reduction is amber from the top.
- **Multiband compressor:** a row of channel strips, one for each band. Each strip has the band's name and frequencies, its gain reduction hanging from the top, and faders for threshold, ratio and gain (makeup), with the unit under each fader's name. The band count is the stacked switch at the left. The live gain curve of all the bands runs under the strips, at their width.
- **Loudness controller:** its dock shows the short-term (S), window (W) and integrated (I) loudness against a white line at the target, and its gain.

### Bypassed blocks and the list view {#bypassed-blocks}

- **A bypassed block** shows an amber **BYPASSED** ribbon, and its meter dock and display are dim. The program goes through it unchanged. Its controls stay fully readable and they work. So you can adjust a block first, and then switch it in.
- **List** shows every block's settings as a plain list, with a slider and a number box in each row. The browser keeps this choice.

### How a change is applied {#applying-a-change}

- While you drag, the page sends the new value at most every 100 milliseconds, and once more when you release the control.
- The station checks each change, applies it and writes it to the station's config file.
- **A change that cannot be written** (a read-only file system, a wrong owner) still takes effect on the air. The card then says **Not saved** with the reason, because the station will not have the change at its next start. To clear the message, correct the file's permissions and make any change.
- **A change is refused** with a message when a value is out of range, a name is wrong, or a multiband change would make the lists of bands different in length. A refused change changes nothing.

The page is built from the running program, so every setting that the program has is on the card.

### How a change reaches the sound {#how-a-change-reaches-the-sound}

Every change, a switch too, builds a new chain. The station puts it in place with a **50 millisecond crossfade** between the old output and the new.

The new chain starts where the old one stopped. For every block that is in both chains, the station copies across the memory of the filters, the history of the detectors, the gain of the AGC and the reductions of the compressors. So a threshold that you move on the air does not restart the AGC and does not click. A block that you switch on starts fresh (the AGC at 0 dB). A block that you switch off disappears with the fade.

**No block adds delay.** So the timing of the program is the same with every combination of switches. In a test, all 1,024 combinations of ten blocks gave the same output frame. The 5 ms of the true-peak limiter and the 2 ms of the safety limiter are always in the path, whatever the chain does. The mixer starts everything that much early, so a start time is the time when the item is heard.

### Settings history {#settings-history}

The station keeps a history of its settings file. Each time the file is saved, from this page, from the loudness card or from the Settings page, the history keeps a version. A version has the time, the place that made the change, and a list of what changed, like `Equalizer · band 2 gain: −6.0 dB → −8.5 dB`.

Open it with the **History** button next to **Bypass the chain**. The Settings page has the same button on the tab of a station. The newest version is first.

- **Show** opens a version. It lists each setting by the name its control has, with its value before and after, in the unit the control shows. The key in the settings file is under the name in small type, for support. It also lists what a restore would change now.
- **Restore** puts the settings of that version back. The page asks first and names the time. The chain and the loudness controller change at once, with no restart. A setting that the station reads only when it starts is written to the file, and the page names it. It takes effect at the next restart.
- **Undo last change** restores the version before the newest. Use it when a change was a mistake.
- **A restore is a version too.** To undo a restore, restore the version above it, or press **Undo last change** again.

Changes in a row from the same place within 10 seconds are one version. A knob that you drag is one entry, not hundreds. The entry shows the value before the drag and the value after it. If you drag a knob back to where it started, no entry stays.

The station keeps the newest 200 versions and drops the older ones. When the station starts, the file as it is becomes a version if it differs from the newest one. So a hand edit of the file is kept too.

## Use presets {#presets}

Presets work in the same way for a block and for the whole chain.

- **Shipped presets** are read only. For the whole chain they are `bypass`, `gentle` and `radio`. The blocks have their own presets. For example, the equalizer has `flat`, `warm`, `presence` and `smile`, and the multiband compressor has `four band` and `two band` in addition to `radio` and `gentle`.
- **Your presets** are saved in the library, so every station on the box can use them.
  - **Save as…** keeps the current settings under a name.
  - **Update** replaces the selected preset of yours with the current settings.
  - **Rename…**, **Delete**, **Export** (a JSON file) and **Import…** do what their names say.
  - The page asks before it replaces a preset with the same name.
- A **modified** mark shows when the settings are different from the selected preset. The drop-down then reads **custom (based on <preset>)**. You never have to select a preset before you change a setting.
- A **chain preset** holds the settings and the switch of every block. A **block preset** holds the block's settings but not its switch. So you can try a preset on a block without switching the block on.

The station's config file names the shipped chain preset (`processing_preset`). After it, the file has only the settings that are different from that preset, in `[chain.<block>]` tables, with the names of the presets they came from. The [Settings](settings) page lists every key (see [Settings reference](settings-reference)). A station with no `[chain]` tables uses exactly the shipped preset. The default is `bypass`.

**Bypass the chain** takes the whole chain out of the path, and puts it back, with the same crossfade. It does not change the saved settings. At the next start, the station uses the configured chain again.

## The shipped chain presets {#shipped-presets}

| Block | `bypass` | `gentle` | `radio` |
|---|---|---|---|
| AGC | off | on, plus or minus 6 dB, rises 1.5 dB/s, fast level 800 ms, fast clamp 40 ms | on, +12 dB / -9 dB, rises 2.5 dB/s, fast level 400 ms, fast clamp 10 ms |
| Equalizer | off | on, flat | on, flat |
| Multiband | off | 3 bands at 200 Hz and 4 kHz, 2:1 | 5 bands at 120 Hz, 500 Hz, 2 kHz and 6 kHz, 3:1 (2.5:1 in the top band) |
| True-peak limiter | off | on, -1.0 dBTP | on, -1.0 dBTP |
| Every optional block (high-pass, rotator, stereo, bass, wideband compressor, clipper) | off | off | off |

With `bypass`, only the safety limiter acts. When the loudness controller is enabled, its gain and the limiter that it turns on act too. A flat equalizer does nothing and costs nothing.

**Which preset to select.** For slow music and ambient programs, select `gentle` or `bypass`. These programs do not want the tail of a quiet ending lifted, and an ambient start never needs a clamp. `radio` gives the overlap styles of [Crossfades](crossfades) their broadcast character (see [How the AGC handles a transition](#agc-and-joins)).

## The blocks {#blocks}

The [Settings reference](settings-reference) gives the exact ranges and defaults of every setting, group by group. The sections below tell you what each block does to the sound.

### Input trim {#trim}

A fixed gain at the start of the chain (`chain.trim.gain_db`, -24 to +24 dB). It has no switch. At 0 dB, the program goes through unchanged.

### Input high-pass {#highpass}

Removes subsonic content below `freq_hz` (default 25 Hz). It is a Butterworth filter of `order` 1 to 4 (6 to 24 dB per octave). Switch it on when rumble uses up the headroom of the blocks after it.

### Phase rotator {#rotator}

A chain of all-pass filters (`sections`, 1 to 6, around `freq_hz`). It makes an asymmetric waveform, mostly voice, more symmetric, so the peak stages after it work less. The sound changes little, but the peaks change. The benefit is largest for a transmitter and small for a stream.

### Stereo enhancer {#stereo}

Multiplies the difference between the channels by `width`. A value of 1 keeps the stereo image as it is, 0 is mono, and 2 doubles the difference.

### Wideband AGC {#agc}

A gain that holds the program at a target level. It is slow on purpose.

**What it measures.**

- The AGC K-weights the program (the BS.1770 filter, so a 1 kHz sine reads its RMS) and cuts it into 2 millisecond blocks.
- A block is *valid* when its level is above the **gate**, `target + gate_db`. With the defaults (-20 and -35), the gate is at -55 dBFS.
- From the valid blocks only, the AGC reads two levels as energy means: the **fast level** over `detector_ms` (400 ms) and the **program level** over `program_ms` (3 s).

Because only valid blocks count, the silence between songs does not dilute either level. After 20 ms or more of blocks under the gate, the fast level also forgets what came before the break. A playlist switch, for example, can start a song 100 to 200 ms after a fade-out. That song is judged by its own first blocks, not by the quiet tail of the fade, and the fast clamp acts immediately.

**What it does.** The wanted gain is `target_dbfs` (default -20 dBFS RMS) minus the lower of the two levels. It is limited to `max_gain_db` up and `max_reduction_db` down. The gain follows the wanted gain no faster than `down_db_per_s` (3) down and `up_db_per_s` (2.5 for `radio`) up. Because the AGC uses the lower of the two levels, it rides a fade up (the fast level falls with the fade while the program level lags) and stays slow on the way down.

**The gate.** The gain **holds** its value while the newest block is under the gate, or while the fast window has no valid block. It does not rise and does not fall. So silence, room tone and the noise floor between songs are not turned up. In the tests, 30 s of digital silence changed the gain by 0.0000 dB.

**The fast clamp.** This is for a loud song that starts after something quieter.

- The clamp acts when the fast level is `fast_trigger_db` (6) or more above the level that the gain was set for.
- The gain then falls to the new level in `fast_attack_ms`, not at 3 dB/s. This is the time to cover 95 percent of the way: 10 ms for `radio`.
- After the clamp, the AGC holds the rise of the gain for one fast window.
- The fast level is a 400 ms mean, so a drum hit inside a loud program does not trigger the clamp.
- `fast_attack_ms = 0` turns the clamp off.

**On the meters**, the reduction of the AGC is its gain, with a sign: plus is a boost. The gate chip comes on while the gain holds.

**Read the AGC card.** A pill shows what the AGC does. The lights of the Rack view use the same words:

- **RIDING**: the gain follows the program. This is the normal state. A quieter passage that the AGC turns up is also RIDING.
- **GATED**: the program is under the gate, so the gain holds.
- **FADE**: the program is fading out and the gain goes up to follow it. The fast level must fall steadily: at least 4 dB in 2 seconds, in 7 of the 8 quarter seconds, and it must still fall. A quieter bar or a step down in level is not a fade.
- **CLAMPING**: the fast clamp pulls the gain down on a loud onset. The state stays on while the gain may not rise again, for one fast window (400 ms).

The pill shows the state that lasted longest since the previous reading, four times a second. So a short dip under the gate does not light GATED. The pill has a fixed width, so the gain beside it does not move when the state changes. Next to the in and out bars are two more bars: **fast** (400 ms) and **prog** (3 s). They show the two detector levels, with a white line at the target. The gain bar has 0 dB at the center, and an amber tick shows where the gain goes. The number beside the pill is the gain now. The figure after the arrow is where the gain goes. The history shows the last 60 seconds, with a strip along the bottom in the colors of the pill.

### How the AGC handles a transition {#agc-and-joins}

The overlap styles do not fade songs. With `radio` active, the chain does that work.

In an offline test, song A ended with a 20 second natural fade to silence. Song B, 8 LU louder, started with no envelope while A was nearly silent. The results:

| What | Result with `radio` | Limit |
|---|---|---|
| Last 8 s of the fade, processed against unprocessed | 14.7 dB louder | at least 6 dB |
| AGC gain in 4.5 s or more of gated silence | changed 0.0000 dB | under 0.1 dB |
| B's onset: true peak | -1.44 dBTP | at most -1.0 dBTP |
| B's onset: AGC gain within 1 dB of its steady value | 6 ms | within 150 ms |
| B's onset: the multiband's mean reduction within 1 dB | 107 ms | within 150 ms |
| B's onset: output level above B's steady level (400 ms levels) | 0.09 dB | at most 1 dB |

With `gentle`, the fade is raised by 8.0 dB and the AGC settles in 26 ms.

### Equalizer {#eq}

A parametric equalizer with 1 to 8 bands in series. The default is four bands, flat at 0 dB: a low shelf at 100 Hz, bells at 250 Hz and 3 kHz, and a high shelf at 10 kHz. `bands` is the number of bands. Band *n* uses the keys `b<n>_on`, `b<n>_type`, `b<n>_hz`, `b<n>_db` and `b<n>_q`.

| Setting | Range | Step |
|---|---|---|
| On | on or off | |
| Type | `peak` (bell), `low_shelf`, `high_shelf`, `low_pass`, `high_pass`, `notch`, `band_pass` | |
| Frequency | 20 Hz to 20 kHz, on every band | 1 Hz |
| Gain | plus or minus 18 dB (bell and shelves only) | 0.1 dB |
| Q | 0.10 to 10.00 | 0.01 |

What each type does:

- **Bell:** raises or lowers the sound around the frequency. A higher Q is narrower.
- **Low shelf:** raises or lowers everything below the frequency. A Q of 0.71 is the gentlest step.
- **High shelf:** raises or lowers everything above the frequency.
- **Low-pass:** removes the sound above the frequency, at 12 dB per octave.
- **High-pass:** removes the sound below the frequency, at 12 dB per octave.
- **Notch:** removes a narrow band around the frequency.
- **Band-pass:** keeps a band around the frequency and removes the rest.

A bell or shelf at 0 dB is not run. A band that is off is not run, and it keeps its settings. A band above `bands` is not run.

To add a band:

1. Press **+ Band**. You can also double-click an empty place on the curve; the band starts there.
2. Raise or lower its gain to hear it.

To remove a band, press the cross on its column. The bands after it move up by one. The equalizer keeps at least one band.

To set an exact value:

1. Click a number in the band strip, or focus it and press Enter.
2. Type the value, for example 4087, -1.7 or 1.57.
3. Press Enter.

The page keeps what you type to the step in the table. A plain drag is coarse. Hold Shift while you drag for the last digit. The arrow keys step by about 1 percent of the frequency or of the Q, and by 0.5 dB of gain. Shift makes the step ten times larger. Alt makes it one step of the table. Double-click a number to put it back to its default.

On the curve, each band that is on has a numbered point:

- Drag the point to change the frequency and the gain. Hold Shift to move a tenth as far.
- Select the point and press Page Up or Page Down to change the Q.
- Double-click the point to set its gain to 0 and its Q to the default of its type.

`position` puts the equalizer before (`pre`) or after (`post`) the multiband compressor:

- Before it, the compressor works on the corrected sound.
- After it, the equalizer shapes the compressed sound, and the compressor does not undo the equalizer.

The choice is a matter of taste. The page shows the card at the position where the equalizer runs.

**The analyzer.** The analyzer draws a live spectrum behind the curve. It shows how loud each part of the sound is, from 20 Hz to 20 kHz. Use it to find a frequency that sounds wrong, and then to put a band on it.

- The switch **Analyzer** in the corner of the curve has three settings. **off** is the default. **in** shows the sound that enters the equalizer. **out** shows the sound that leaves it. Do not confuse it with **Position**, which says where the equalizer runs in the chain. The analyzer follows the equalizer, wherever it runs.
- The filled area is the level of each band of 1/12 octave. It rises fast and falls in about a third of a second. Its scale is its own: the faint numbers on the right edge are dBFS, from -90 to -10. It is not the scale of the curve on the left edge.
- The thin line above it is the peak line. It keeps the highest level of each band and falls 6 dB a second. Press **hold** and the line does not fall. It starts again from the levels at that moment. Press **hold** again and the line falls again.
- Move the pointer over the graph. A small box shows the frequency under the pointer, for example `2480 Hz`, and the analyzer level there. On a touch screen, touch the graph.
- The page keeps your choice of **off**, **in** or **out**, and of **hold**, in this browser.
- The analyzer costs nothing while it is off. The station makes the spectrum only while a page asks for it. When you close the page or switch the analyzer off, the work stops.

To find a problem frequency:

1. Set the analyzer to **in**, and play the item that has the problem. Look for a tall, narrow peak that stays, for example a whistle or a boom. Hold the pointer on its top and read the frequency. Press **hold** to keep the highest level in view.
2. Press **+ Band** (or double-click the graph at the peak), and set the frequency of the band to that number. Make it a bell with a Q of about 4. Lower the gain by 3 dB, and select the point and press Page Up to make the band narrower, or Page Down to make it wider.
3. Set the analyzer to **out**. The peak must be lower than before. Stop when the problem is gone and the sound is still natural.

The shipped equalizer preset **TV-era rolloff** has three bands: a bell of -3.6 dB at 2080 Hz (Q 1.57), a bell of -1.7 dB at 4087 Hz (Q 1.0) and a high shelf of -6.4 dB at 8043 Hz. Use it as a start, then change the numbers.

Older settings still work. A station file or a preset that names `low_hz`, `low_db`, `mid1_*`, `mid2_*` or `high_*` loads as bands 1 to 4 and sounds the same.

A change is a 50 ms crossfade to the new settings, so a drag does not click. Eight bands use about twice the processor time of four, and both are a small part of one core.

### Bass enhancer {#bass}

Makes the low end sound fuller on small speakers. It works in four steps:

1. It takes the bass below `freq_hz` (default 80 Hz).
2. It drives the bass into a saturating curve (`drive`).
3. It keeps only the harmonics that the curve adds above 1.5 times that frequency.
4. It mixes them in at `amount_pct`.

The block comes before the multiband compressor, so the compressor controls what it adds. Quiet bass makes few harmonics, and loud bass makes more.

### Multiband compressor {#multiband}

Two to six bands. **Linkwitz-Riley crossovers** of the 4th order (24 dB per octave) with all-pass compensation split the bands. So the bands add up to a flat response when no band reduces.

- The number of bands is the number of crossovers plus one. `crossovers_hz` lists them in ascending order, each at least 5 percent above the one before.
- Each band has its own `threshold_dbfs`, `ratio`, `knee_db`, `attack_ms`, `release_ms` and `makeup_db`.
- A list of any of these must have one value for each band. A list of the wrong length is refused.

The card shows one strip for each band. Below the strips is the **gain curve** that the compressor applies now: the sum of the bands at their current reduction, drawn from 20 Hz to 20 kHz, with the crossovers as dashed lines.

The **band count** buttons change the number of bands in one step. They put the new bands on the default crossovers for that count, and they resample the old values onto the new bands:

| Bands | Default crossovers |
|---|---|
| 2 | 300 Hz |
| 3 | 200 Hz and 4 kHz |
| 4 | 150 Hz, 1.5 kHz, 5 kHz |
| 5 | 120 Hz, 500 Hz, 2 kHz, 6 kHz |
| 6 | 80 Hz, 300 Hz, 1 kHz, 3 kHz, 9 kHz |

A change of the band count always replaces the whole block. It is never an edit in place.

**How a compressor measures.**

- The detector follows the louder channel. Both channels get the same reduction.
- It reads the level in dB, with a full-scale sine at 0 dB, and applies the usual soft-knee curve.
- The reduction (dB) is smoothed with the attack time constant while it grows, and with the release time constant while it falls.

The times are the time constants of the reduction itself. So a step into the compressing region shows an exponential with that time constant. In measurements, attack and release were within 3.3 percent of the settings. The static curve was within 0.12 dB of the configured threshold, ratio and knee.

### Wideband compressor {#wideband}

One compressor on the whole program, after the multiband compressor, for a slow leveling stage. Its `slow leveler` preset is a 2:1 stage with a 500 ms release. It has the same settings as one band of the multiband compressor.

### Output makeup {#makeup}

A fixed gain after the compressors and before the clipper (`chain.makeup.gain_db`, -24 to +24 dB). Not to be confused with the **Makeup** fader of each multiband band, a fixed gain after that band's compressor: the band makeups set how loud the chain's output is and how the bands balance, and this one lifts or lowers the whole chain's output. It has no switch: 0 dB leaves the program as it is. It is the one place to set the chain's output level, whichever blocks are on. A change glides at 3 dB a second instead of jumping.

**How it works with the loudness controller.** The [loudness controller](output-and-level#loudness-controller) adds whatever gain brings the program to the target. If the chain already lands near the target by itself, that gain stays close to 0 dB. When the gain stands still more than 3 dB from 0 for five minutes, the controller's card shows a tip with the numbers. It is not a fault: the controller is doing its job, but the chain is meant to do the coarse work and leave the controller the fine work.

Press **Move to makeup** on the controller's card to hand the gain over. After you confirm, the makeup goes up by the controller's gain (to 0.1 dB) while the controller's gain comes down by the same amount, both at 3 dB a second, so the program level does not change. The chain then shows as modified and the makeup is saved to the station's config. Because the makeup is before the clipper, the clipper sees the higher level, as it would if you had raised the multiband's makeup: listen for more clipping after a large move.

### Clipper {#clipper}

A soft clipper before the loudness gain and the limiter, so the limiter works less. It is linear up to `softness_db` under `threshold_dbfs`. Above that, it bends smoothly toward the threshold, and no sample goes past the threshold.

The clipper does not oversample, so hard clipping makes some aliasing. Keep the softness at a few dB, and keep the threshold a little under the ceiling of the limiter.

### True-peak limiter {#limiter}

A lookahead limiter (5 ms) that holds the **true peak** under `ceiling_dbtp` (default -1.0). The true peak is measured between samples, at 4 times oversampling.

- Its release depends on the program: 60 ms, stretching toward 500 ms under sustained reduction.
- The lookahead and the release are fixed.
- When the [loudness controller](output-and-level#loudness-controller) is enabled, it turns the limiter on by itself, at its own ceiling (`loudness.true_peak_dbtp`). When both ceilings are set, the lower one holds.
- The delay of the limiter is in the path whether the limiter is on or off.

In a measurement, an hour of pink noise at +11 dBFS RMS (peaks 27 dB over the ceiling) came out at a true peak of -1.10 dBTP. The largest sample was -1.50 dBFS, with up to 27 dB of reduction.

### Safety limiter {#safety-limiter}

Always on and last in the chain. It has a 2 ms lookahead, a ceiling of -1.0 dBFS on every sample and a 50 ms release. You cannot switch it off or change it here. In normal operation it almost never acts. Its card lights red when it does.

## Meters {#meters}

Each block that measures shows live levels on its card. The levels come from the same 30 Hz meter frame that the status display uses:

- the **peak level into the block** (`in`)
- the **peak level out of it** (`out`)
- its **gain reduction**: the largest reduction in the last 33 ms (for the AGC, its gain)

The multiband compressor shows a reduction for each band and a summary (the mean of the bands). A block that is off shows nothing. `GET /api/v1/chain/meters` gives the frame as JSON, with the same values as the WebSocket frame.

The frame's **stage mask** has one bit for each of these: the AGC, the multiband, the wideband compressor, the limiter, the clipper, the safety limiter and the loudness controller. The **bypassed** flag is set when no block is on. A change of the chain's configuration changes the frame's `chain_cfg_rev`, which tells the status display to read the configuration again. The `processing` view of the [status display](status-display#processing-view) draws these values as meters on the box's own monitor.

Against independent readings of the audio, the stage levels agreed to 0.004 dB, the reductions to 0.10 dB, and the AGC's gain in the frame to 0.05 dB.

## For other programs {#for-other-programs}

| Request | What it does |
|---|---|
| `GET /api/v1/chain` | The running chain: the preset, whether the chain is different from the preset, the stages, the multiband's band count and crossovers, the settings of every block, and the names of the presets they came from. `?schema=1` adds the description of every setting. |
| `PUT /api/v1/chain/{block}` | Changes one block: any of its settings, `enabled`, and `user_preset` (the name of the preset they came from). |
| `PUT /api/v1/chain` | `{"preset": "radio"}` starts a shipped chain preset. `{"chain": {...}, "user_preset": "name"}` starts a saved one. `{"blocks": {...}}` changes several blocks in one swap. |
| `POST /api/v1/processing/preset` | `{"name": "radio"}`: starts a shipped chain preset. |
| `POST /api/v1/processing/bypass` | `{"on": true}`: takes the whole chain out of the path, and puts it back, without a change to what is saved. |
| `GET /api/v1/chain/meters` | One meter frame as JSON. |
| `GET /api/v1/config-history` | The versions of the settings file, newest first: id, time, source and the list of changes. |
| `GET /api/v1/config-history/{id}` | One version with its file text, what it changed and what a restore would change now. |
| `POST /api/v1/config-history/{id}/restore` | Puts that version back. The answer says what took effect now and what waits for a restart. |
| `POST /api/v1/loudness/move-to-makeup` | Put the loudness controller's gain (to 0.1 dB) into the output makeup; both glide at 3 dB a second and the program level stays. Refused while the controller is off, the chain is bypassed by hand, or a move is still gliding. |

The presets of the library are at `/api/v1/processing-presets/{effect}`. The effect is `chain` or a block (`agc`, `eq`, `multiband` and so on).

## When something is wrong {#troubleshooting}

- **A block's card says that the change was refused.** Read the message. It names the setting and the limit. A change of the band count needs a list of the new length for every per-band setting. The **band count** buttons do that for you.
- **The sound got louder or quieter after you switched a block on.** The AGC steers to its target, and the multiband compressor adds makeup (3 dB for each band in `radio`, 2 dB in `gentle`). When the [loudness controller](output-and-level#loudness-controller) is on, it slowly corrects the level over minutes. If its gain stays more than 3 dB from 0 for five minutes, its card shows a tip: the output of the chain is that far from the target. Press **Move to makeup**, or change the [output makeup](#makeup).
- **The AGC does not move in a quiet passage.** The gate holds it when the program is more than `gate_db` under the target. That is what the gate is for.
- **The Processing page shows a notice in place of the chain.** The station does not answer. See [System](system).
