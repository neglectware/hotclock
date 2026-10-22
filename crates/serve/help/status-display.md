# The status display

The status display is a screen for the room where the station runs. It shows what plays, what is next, the meters and any alarm, in text large enough to read from across the room. You control what it shows from the web UI.

There are two displays. They show the same information:

- **The native display** is drawn by the program `hotclock display`, directly on the box's own monitor. It has no desktop, no window system and no browser, and it takes over the console of tty1. It talks only to the stations and the library service, and it cannot touch the audio.
- **The web display** is the page `http://<box>:8300/display`, for a browser on a screen of its own (a kiosk). The page `http://<box>:8300/display?station=<name>` shows one station. The web display draws the same layouts as the native display.

## Turn on the native display {#turn-on-the-native-display}

The package does not turn on the native display, because many boxes have no screen. To turn it on, type:

```
sudo systemctl enable --now hotclock-display
```

The display starts at once, and again at each boot.

## What the screen shows

- **The top bar** shows the station's name, its state as a colored label, an indicator for the worst alert, and the clock. The bar turns red for a critical alert. The label is one of these:
  - `PLAYING` (green);
  - `ASSIST` or `HELD` (amber);
  - `SAFE MODE` (red);
  - `NO CONNECTION` (red, when the display cannot reach the station).
- **Now playing** shows the cover, the title, the artist, and the time played and the time left. The time left turns amber and then red as it runs down. The segue line tells what comes next and when: `Segue in 0:14`, `ALBUM` for a gapless album transition, or `HELD` or `STOPPED` and how long ago the item ended. Nothing counts below zero.
- **The queue** shows the next items with their expected start times. A small badge tells where an item came from: `ROT` (rotation), `LINER`, `ID`, `BLK` (a block) or `MAN` (by hand).
- **Block rows in the queue.** A block of the running program that acts soon has a row of its own. The row shows the block's badge (`FADE`, `STOP`, `PLAY`, `OSC`, `MIDI`, `HTTP`, `ID`, `WAIT`, `LOOP`, `GAIN`, `MUTE`), its name, and the time to go in minutes and seconds. The time is amber under 60 seconds and red under 10 seconds. The small layouts hide the badge and keep the name and the time.
- **The END line** shows while a Play block is back-timed. It is above the queue and reads `END +3 s` and a state: `PLANNING`, `ON TARGET`, `WITHIN TOLERANCE` (amber) or `FALLBACK` (red). Plus means that the last song is planned to end that many seconds after the end time. It is the same line as on the [On Air](on-air#program) panel. See [The block engine](block-engine#back-timing).
- **The meters** show the left and right program bars with a peak mark, and the loudness numbers.
- **The loudness controller bar** (`LC`) is under the meter bars while the [loudness controller](output-and-level#loudness-controller) runs. It shows the gain the controller applies. The bar starts from a white mark in the middle: right is gain added, left is gain taken away. The ends of the bar are the controller's gain range, from `loudness.gain_min_db` and `loudness.gain_max_db`. The grey marks are those two limits. The color is the state:
  - green: `ON TARGET`;
  - blue: `CONVERGING`;
  - amber: `FROZEN` or `AT LIMIT`;
  - grey: `OFF`.

  The gain in dB is next to the loudness numbers, like `LC +3.2`, in the same color. There, it takes the place of the target. A `split` half has room only for the number. On the 800 x 480 fallback, `single` keeps the target, and its second line (`+3.2 dB ON TARGET`) takes the bar's color. When the controller is off, there is no bar and no number. The displays read the gain range from the station when they connect, and again every 30 seconds. The full layout of the web display draws the bar too. The full layout of the native display writes the gain and the state under the numbers, as before.
- **Alarms** shows `NO ALARMS PENDING` when all is well. Otherwise it shows the alerts: red for critical, amber for warning.
- **On a large screen only**, the display also shows the as-run list, the spectrum and the processing view (see below). The as-run list shows what played and how it ended: `END`, `SEG` for a song change, `CUT`, `HARD` or `ERR`.

## The layouts and the legibility rule {#layouts}

The display selects a layout from the screen's size, so that the smallest text is always easy to read from where you sit.

1. It calculates the smallest comfortable text size from the **screen's diagonal in inches** (`display.diagonal_in`) and the **viewing distance** (`display.view_distance_cm`). At a given distance, text needs a fixed physical size. So a screen with more pixels per inch needs larger text in pixels. For example, a 7-inch 1024 x 600 panel read from 70 cm needs 31 pixel text.
2. If the screen is tall enough for about 40 lines of that text, the display uses the **full layout**, with all areas side by side. Smaller screens use a **compact layout**, with fewer areas and bigger text. 1024 x 600 is the main compact layout, and 800 x 480 is a fallback.
3. All text on the screen is at least that size. The exception is the 800 x 480 fallback: there, the smallest text is 19 pixels, even when the rule asks for more.

If the text is too small from where you sit, increase `display.view_distance_cm`. If the layout does not fit, check the diagonal.

A different screen needs only new settings, not a new design. For example, a 15.6-inch 1920 x 1080 panel is `mode = "1920x1080@60"` and `diagonal_in = 15.6`.

## The views {#views}

A view sets which stations the screen shows. The box has one view, and every screen follows it within a second.

### Change the display view {#display-view}

To change the view, press one of the **Box monitor** buttons at the top of [On Air](on-air#display-view). `single` and `processing` show the station of the tab selected on On Air, and the button names it, for example `single · Eve`.

| View | What it shows |
|---|---|
| `single` | One station, full screen: the station that the view names. |
| `split` | Two stations side by side, under a shared [top bar](#split-top-bar). |
| `rotate` | One station at a time, changing every 20 seconds, with a thin time bar at the top. With `display.rotate_processing` on, each station shows two times in a row: its normal view, then its [processing view](#processing-view). For example: office, office processing, studio, studio processing. If `display.rotate_pin_on_alert` is on (the default), a **critical** alert on a station holds the view on that station's normal view until the alert clears. |
| `processing` | One station's [processing chain as meters](#processing-view): a gain-reduction bar for each band, the wideband compressor, the limiter and the clipper, the AGC, the level in and out, and the loudness controller. The `station` of the view selects the station. |

The display keeps its last view if the library service is down or restarts. With one station, every view shows that station, `processing` too.

### The top bar of split {#split-top-bar}

In `split`, a bar across the top of the screen belongs to the whole box. The native display and the web display show the same items, from left to right:

- **A network dot** (off by default). Green: the box has a network route out. Red: it has none. Grey: the box cannot tell.
- **The product name and version**, like `HOTCLOCK 0.1.0-25`. The number after the dash is the package revision.
- **The clock**, in the middle.
- **The date**, like `SAT 10 OCT`.
- **The CPU use** of the whole box, like `CPU 7%`. It is the average over the last 5 seconds, so it does not flicker. It turns amber at 85% and more. `CPU --` means the box cannot tell.

All the text is the same size as the clock, 31 pixels on the 7-inch panel.

Each item has its own setting on the **This box** tab of [Settings](settings), under the view setting:

| Setting | Item | Default |
|---|---|---|
| `display.bar_network` | the network dot | off |
| `display.bar_product` | the name and version | on |
| `display.bar_clock` | the clock | on |
| `display.bar_date` | the date | on |
| `display.bar_cpu` | the CPU use | on |

A change shows on the screen within 2 seconds. The display does not restart. Both displays read the items and the numbers from `GET /api/v1/display/bar` on the library service, every 2 seconds. While the library service does not answer, the native display shows the product name without the version, `CPU --` and a grey dot, with the items of the global file.

The bar has no room for more items at 31 pixels. The CPU temperature, the uptime and the free memory or disk space are not on it.

`single` and `rotate` have no shared bar. There, each station's own top bar shows the clock. On the 800 x 480 fallback, the bar is 32 pixels high and its text is 22 pixels, like the rest of that layout.

## The processing view {#processing-view}

The `processing` view shows what one station's [processing chain](processing) does, as meters. The layout is the same on all screens, from the 7-inch panel up to a large monitor.

To show it, press the `processing` button of the **Box monitor** buttons on [On Air](on-air#display-view). A remote control sends `PUT /api/v1/display` with `{"view": "processing", "station": "office"}`. On a box with two stations, the `station` selects which one is drawn. To see both stations, use `rotate` with `display.rotate_processing` on.

From the top of the screen down:

- **The header** shows the station's name, its state, the code of the newest open alert, the other station's indicator and the clock. The alert code is red for critical, with `+2` when there are more alerts. The header turns red for a critical alert.
- **The title strip** shows what is on air and the time left, or `HELD` or `STOPPED` and how long ago the item ended.
- **The gain-reduction bars** are the main part of the screen. See [the bars](#the-gain-reduction-bars) below.
- **The AGC** is the column at the left. Its bar starts from the middle: up is gain added, down is gain taken away, on a scale of 9 dB each way. The number under it has a sign. The LED next to its name lights amber while the AGC's **gate** holds the gain, because the program is too quiet to measure. The line below the bars then shows `AGC GATE`.
- **The clipper's LED** lights red while the clipper acts.
- **The limiter's ceiling** is written under the bars (`CEIL -1.0 dBTP`), and drawn as an amber mark on the output bars below. The same line lists the multiband's crossover frequencies (`XOVER 120 500 2k 6k`).
- **IN, L and R** are three level bars. `IN` is the level into the chain: the peak into the first block that is on, or the program's own peak when the chain is off. `L` and `R` are the level out of the chain. They follow the [meter ballistics](#meter-ballistics). The number at the end of each bar is the level in dBFS.
- **The loudness controller** is at the right of the level bars. It shows its state, the gain it applies, the `TARGET`, and the window (`W`), short-term (`S`) and integrated (`I`) loudness in LUFS. The state is one of these:
  - `OFF`;
  - `CONVERGING`;
  - `ON TARGET`;
  - `FROZEN`: there is not enough program to measure, and the gain is held;
  - `AT LIMIT`.

### The gain-reduction bars {#the-gain-reduction-bars}

There is one bar for each **band** of the multiband compressor (2 to 6). The bands are named `LOW` to `HIGH` when the names fit the columns, and `B1` to `B6` when they do not. There is also one bar for the **wideband** compressor (`WB`), one for the **limiter** (`LIM`) and one for the **clipper** (`CLP`).

- Each bar **grows down from the top** as its block takes gain away.
- All bars share one scale: 0 at the top, and `display.proc_range_db` (18 dB by default) at the bottom, with a line every 6 dB.
- The number under a bar is the reduction at this moment, in dB.
- A white mark on a bar is its peak. It falls back after a moment.

### Bypassed and missing blocks {#bypassed-and-missing-blocks}

A **bypassed** block is drawn grey, with `BYP` in place of its bar. A bypassed multiband is one grey `MB` column.

With `display.bypassed_stages` set to `hide`, bypassed blocks are left out, and the other columns get wider. A block that the chain does not have at all is not drawn.

When the whole chain is off, every column is grey. With `hide`, the screen shows `CHAIN BYPASSED` instead.

### Text size, spectrum and bar speed {#text-size-spectrum-and-bar-speed}

**Text size.** The legibility rule of [the layouts](#layouts) applies. On the 7-inch 1024 x 600 panel, all text is 31 pixels, the smallest that the rule allows, and nothing is smaller. Bars, lines and marks are as fine as the screen allows. On a larger screen, the same view has more room for the bars.

**The spectrum** is not drawn by default. With `display.proc_spectrum` on, the spectrum is drawn under the bars, if the screen has room after the bars get the height they need. The 7-inch 1024 x 600 panel has no room for it. `display.spectrum_source` selects the signal before the chain, after it, or both.

**The speed of the gain-reduction bars.** A gain-reduction bar goes down at once and comes back up at a set speed. You set the speed and the peak mark under **GR bars** in the meter ballistics (below). They do not depend on the meter preset.

### The processing view in a browser {#the-processing-view-in-a-browser}

On the web display, `http://<box>:8300/display?station=office&view=processing` shows the view for one station. The box page `http://<box>:8300/display` follows the box's view. Add `?rotate_processing=1` to it to turn on the rotation through the processing views.

You can add the options `proc_range_db`, `proc_spectrum`, `spectrum_source` and `bypassed_stages` to the address. Without them, the page uses the box's settings.

### The mini processing strip {#mini-processing}

The compact `single` and `split` views of the 7-inch panel have a small copy of the processing meters. With it, you can watch the chain without leaving the main view. It shows:

- the gain-reduction bars of the bands, `WB`, `LIM` and `CLP`, down from the top, with their peak marks, over a faint scale (`0`, `-6`, `-12` dB);
- the AGC's bar, from the middle.

In `single`, the strip is in the free corner to the right of the time left. In `split`, it is under the alarm text of each half. A half with many alarm lines, or an 800 x 480 half, sometimes has no room for it.

The strip is made for you to recognize by shape and position from across the room, and to read up close when necessary. **Its labels are the one exception to the legibility rule.** They are about 12 pixels, well under the 31 pixel minimum. They only name a bar (`LOW`, `LMID`, `MID`, `HMID`, `HIGH` or `B1` to `B6`, `WB`, `LIM`, `CLP`, `AGC`) and the scale. Nothing else on the screen is smaller than the minimum.

The strip uses the same bar speed and `display.proc_range_db` as the full view. It takes no room from the now-playing, time-left, next and alarm lines. The screenshots are in `docs/screenshots/proc-view/mini-*.png`.

`display.mini_processing` sets when the strip is drawn:

- `auto` (the default): while the processing chain is on;
- `on`: always (dark bars when the chain is off);
- `off`: never.

Only the native display draws the strip. The web display does not have it.

## The spectrum and the gain curve {#spectrum-and-curve}

On a large screen, the full layout can show what the processing chain does. It shows a **spectrum** of the program (64 bands from 20 Hz to 20 kHz) and the **gain curve** of the multiband compressor. These settings select what is drawn:

| Setting | Choice |
|---|---|
| `display.processing_view` | the `spectrum`, the `curve`, or `both` |
| `display.spectrum_source` | the signal `pre` (before the processing), `post` (after it) or `both` |
| `display.curve_range_db` | how far down the gain curve goes |
| `display.curve_include_makeup` | whether the bands' makeup gain is added to the curve |
| `display.bypassed_stages` | `show` a bypassed stage grey with `BYP`, or `hide` it |

When no block of the [processing chain](processing) is on, the screen shows `CHAIN BYPASSED` and the spectrum of the program.

When the chain is on, the screen draws the stage columns and the gain curve from the running chain. The stage columns show the level in, the level out and the gain reduction of each stage, and a bar for each multiband band. The band count and the crossovers come from `GET /api/v1/chain`, and the reductions come from the meter frame.

## Settings of the display {#display-settings}

The settings are in the `[display]` block of the global file, and on the **This box** tab of [Settings](settings). The full list, with ranges and defaults, is in the [Settings reference](settings-reference#global-status-display-the-box-s-own-monitor).

A change takes effect when the display restarts. To restart it from the Settings page, press **Restart display now**. The screen goes blank for a second. No station is touched.

- **`display.mode`** sets the screen's mode, for example `1024x600@60`. The display sets the mode explicitly through the kernel's display driver. It never trusts the panel's own description of itself (EDID) for the size. If the connector does not list the mode, the display adds it.
- **`display.connector`** and **`display.device`** select the video output. By default, the display uses the first connected output.

## Meter ballistics {#meter-ballistics}

The bars on the status display and on the web meters can behave like different kinds of meter. This setting never touches the audio.

The frame that carries the levels holds two readings: the measured PPM (a quasi-peak reading with a 5 ms integration time) and the sample peak. Each screen applies its own behavior to them.

### Change the meter ballistics {#change-the-meter-ballistics}

You change the ballistics on the **This box** tab of [Settings](settings), under **Meter ballistics**. [On Air](on-air#meter-ballistics) shows the preset in the settings line of its Program unit and links to it.

1. In the first drop-down, select **this station** or **box default**. This sets which setting you edit.
2. In the next drop-down, select a preset.

The change reaches the native display and every web meter within a second. It stays after a restart.

A station with no setting of its own follows the box default and shows `inherits box default`. To remove a station's own setting, press **Use box default**.

### The presets {#the-presets}

This list comes from the running system:

{{generated:ballistics}}

### A custom setting {#a-custom-setting}

When you select `custom`, or change any number, the setting becomes a custom one. Its fields are:

- **Source**: **sample peak** (`peak`, the sample peak with the attack and falloff below) or **PPM as measured** (`ppm`, the measured PPM as it is).
- **attack**: the time in milliseconds that the bar takes to rise 20 dB. 0 means at once.
- **falloff**: how fast the bar falls, in dB per second.
- **peak hold**, **hold** and **tick falls**: whether a mark stays at the highest level, for how many milliseconds, and the rate at which it then falls (0 means it drops to the bar at once).

### The GR bars {#the-gr-bars}

**GR bars** sets the gain-reduction bars of the [processing view](#processing-view). It has two fields:

- **release**: how fast a bar comes back up, in dB per second (1 to 400, 15 by default). At 15, a bar that was 12 dB down takes 0.8 s to come up.
- **tick**: how long the white peak mark waits before it falls with the bar, in milliseconds (0 to 10000, 1000 by default). 0 turns the mark off.

The attack of a gain-reduction bar is always at once. When you select a preset, these two fields do not change. A change to them does not make the preset `custom`.

## Read the display in a room {#reading-the-display-in-a-room}

- A **red top bar** means a critical alert on that station. Read the alarm line at the bottom, then see [Alerts](alerts).
- **`NO CONNECTION`** means that the display cannot reach the station. The station is possibly restarting. If the message stays, see [Troubleshooting](troubleshooting#display).
- **`HELD`** and **`ASSIST`** are amber because a person must act. See [On Air](on-air#transport).
