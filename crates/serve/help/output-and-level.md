# Output and level

This page follows the sound from the library to the sound card and the stream. It explains each control that sets the level: the loudness target, the safety limiter and the output trim. It also describes the loudness controller and gives a summary of the processing chain, which has its own page.

## The signal path

```
items (each at its own matched gain)
  -> mix
  -> processing chain            (the `bypass` preset, the default, does nothing)
  -> loudness controller's gain  (when it is on)
  -> true-peak limiter           (5 ms of delay, always in the path)
  -> safety limiter              (always on)
  -> master gain
  -> output trim
  -> conversion to the card's format
  -> sound card, and the network stream
```

- **Each item is matched to a target** before it is mixed. See [Markers and analysis](markers-and-analysis#normalization). A change of the target applies from the next item planned.
- **The mix** adds everything that plays at the same time: the two songs of a transition, a liner, an ID. The mix is in floating point, so nothing clips inside it.
- **The processing chain** is the row of optional blocks on the [processing chain](processing) page: a trim, a high-pass, a phase rotator, a stereo enhancer, the AGC, an equalizer, a bass enhancer, the multiband compressor, a wideband compressor, an output makeup and a clipper. With `processing_preset = "bypass"`, which is the default, no block is on.
- **The safety limiter** is last in the program path. It is always on, also when the chain is bypassed. It has a 2 millisecond lookahead, a ceiling of **-1.0 dBFS** and a 50 millisecond release. The ceiling holds for every sample, whatever the program does. In normal operation the limiter almost never acts. The service counts the time it spends reducing gain (`hotclock_limiter_active_seconds_total` in the metrics).
- **The master gain** is for fades that act on everything, for example a timed fade-and-stop.
- **The output trim** is one final gain. See [Set the output trim](#output-trim).
- **The stream** is taken after the trim, so it carries the same signal as the card.

## The loudness target

`target_lufs` (default -18) is the loudness that every item is brought to. It is an internal reference. It is not the level the listener hears at the end: the devices after the station set that level (the mixer, the receiver of the stream). It is a station setting, and it takes effect from the next item planned.

Two more settings control how far an item is changed:

- `max_boost_db` (default 12) limits how much a quiet item is raised.
- `peak_allowance_db` (default 6) limits how far the peaks of a raised item go past the ceiling of the safety limiter.

For very quiet material with high peaks, raise the allowance. The limiter then works a little harder. If you want the limiter to do almost nothing, keep the allowance low.

## Set the output trim {#output-trim}

`output_trim_db` is one gain, in dB, at the very end of the path, after the safety limiter. Its range is -40 to +6. Its default is 0, which is unity. It applies to the sound card and to the stream in the same way.

Use the trim, not the mixer of the sound server, to match the card to the next device. Sometimes the analog stage of a card sounds harsh at the mixer's 0 dB although the limiter does not act. Then the analog stage is out of headroom, not the digital path. To correct it:

1. Set the trim to -3.
2. If the sound is still harsh, set it to -6.
3. Raise the input of the next device to match.

- **Where to set it.** In the station file, on the [System](system) page (type the value in **Output trim (dB)**, then press **Set**), on [Settings](settings) (`output_trim_db`, live), or through the API. No restart is necessary. The change arrives over 50 milliseconds on a straight ramp, so there is no click.
- **The value is kept.** The station saves a value set at run time. After a restart, that value wins over the value in the file. To go back to the file's value, set that value again.
- **Above 0 dB**, the trim can push the output past the ceiling of the limiter, because the trim comes after the limiter. The page allows up to +6 for a quiet card. Prefer negative values.
- **What reads before and after the trim.** The silence detector and the health check read the signal **before** the trim, so a deep trim never causes a false silence alert. The meters and the stream carry the trimmed signal, which is what the listener hears.

## The meters

The program meters on [On Air](on-air#program-meters) and on the status display read the signal after the limiter. A second tap, before the processing chain, feeds the "before" spectrum of the full status display layout. Loudness numbers are in LUFS. To set how the display's meters move, see [The status display](status-display#meter-ballistics).

## The processing chain {#the-processing-chain}

The chain is an **Optimod-style processor** between the mix and the limiters. Each block works like a plug-in in a fixed broadcast order. Each has its own on/off switch, all its settings in view and its own presets. The whole chain has presets too: `bypass` (the default, nothing in the chain), `gentle` and `radio`.

The [processing chain](processing) page describes every block, the presets, the meters and how a change gets to the sound. You edit the chain on the **Processing** page (in the menu), one station at a time.

The important points:

- **A change is safe on the air.** Every change builds a new chain and crossfades to it over 50 ms. The state of the old chain goes to the new one. No block adds delay, so a block that you switch on or off does not click or move the timing.
- **The AGC and the limiters give a transition its radio character.** The crossfade styles other than `basic` do not fade songs. They overlap them. With `radio` active, the AGC raises the level during a fade and pulls it down at the next loud start. See [How the AGC handles a transition](processing#agc-and-joins) and [Crossfades](crossfades).
- **The order is fixed**, with one choice: the equalizer comes before or after the multiband compressor.
- **The loudness controller works with every preset**, also with `bypass`. It measures the signal that leaves the chain, so it also corrects the level that a block adds.

## The loudness controller {#loudness-controller}

The loudness controller is a slow gain. It brings the station's output to a target loudness, under a true-peak ceiling. A stream to YouTube or Twitch can use it to deliver about -14 LUFS with no one at a fader. A station that an operator sets by ear can leave it off. It is a station setting, and it works with the chain bypassed.

**Where it is.** It comes after the mix and the chain. After it come a lookahead true-peak limiter (5 ms), the safety limiter, the master gain and the output trim. The 5 ms delay of the true-peak limiter is always in the path. So when you switch the controller on or off, the program never moves in time. With the controller on, the true-peak limiter holds the ceiling (`true_peak_dbtp`, default -1 dBTP, measured between samples).

**How it works.**

- It measures the loudness of the program that enters its gain (ITU-R BS.1770, the same as the LUFS numbers).
- It leaves out what must not count: silence, a stopped or held station, a master-gain fade, and the categories in `gate_categories` (liners, IDs and promos by default).
- It moves the gain toward its target (`loudness.target_lufs`, default -14) minus the measured loudness.
- The gain never moves faster than `max_slew_db_per_min` (default 1 dB a minute). It does not move while it is within `deadband_lu` (0.25 LU) of the correct value.
- A slow trim corrects what the limiter takes off. Its time constant is `trim_tau_s` (10 minutes), and it moves at most `trim_range_db`.
- After a restart, the controller continues from the gain saved with the checkpoint. It does not start again from zero.

**Hold or follow.** The setting `within_item` selects how the controller reads the program level:

- `hold` judges each item as a whole. The program level is the median loudness of the last `robust_items` items, and each item counts for at most `item_cap_s`. A quiet passage, or one very quiet item, almost does not move the volume. Several quiet items in a row do. This keeps the dynamics of slow music.
- `follow` tracks the loudness of the last `window_s` continuously. It makes the level more even and suits fast programming.

**Limiting.** `max_limiter_gr_db` holds the gain down, so the loudest peak of the window never needs more than that much limiting. A value of 24 turns this off. `peak_allowance_db` is how far normalization can push the peaks of an item into the limiter while the controller is on. A low setting has a clear cost: an item whose peaks are high for its loudness stays quieter than the target. It is not limited up to the target.

**Chain makeup and this gain.** The [output makeup](processing#makeup) of the chain sets the level that the chain delivers. The controller adds the rest. When the chain is close to the target, the gain of the controller stays near 0 dB. When the gain stays more than 3 dB from 0 for five minutes, the controller card shows a tip with the numbers.

**Move to makeup.** The button on the tip raises the output makeup by the gain of the controller and lowers the gain by the same amount. Both change at 3 dB a second, so the level on the air does not change, and the gain ends near 0 dB. A dialog shows the numbers before anything changes. The makeup is before the clipper, so the clipper works a little harder after a large move. The change is saved like any other chain change.

**Presets.** Three presets ship with the program. You cannot change them.

| Preset | For | Hold or follow | Most limiting | Rate |
|---|---|---|---|---|
| `ambient` | slow music with long quiet passages (the default) | hold, median of 5 items | 5 dB | 1 dB a minute |
| `balanced` | mixed programming | hold, median of 3 items | 8 dB | 1 dB a minute |
| `aggressive` | Top-40 style | follow | no cap | 2 dB a minute, dead band 0.15 LU |

A station names a preset with `loudness.preset`. Any other `loudness.*` key overrides the preset, and the preset then reads `custom`.

**The Loudness controller card.** On the Processing page, the **Loudness controller** card edits every setting. Each setting has a line that tells you what it does to the sound.

- Each change applies to the running station at once, and the station writes it to its config. There is no Save button. A small `applied` mark shows when the station has taken the change. A change does not make the controller measure the program again.
- **Reset readings** resets the integrated reading, the measured window and the gain. It does not change the settings.
- The meters show the short-term (S), window (W) and integrated (I) loudness against a line at the target, and the gain.
- **Presets** keeps your own presets: **Save as…**, **Update**, **Rename…**, **Delete**, **Export** (a JSON file) and **Import…**. Your presets are stored in the library, so every station on the box can use them.
- A `modified` mark shows when the settings are different from the selected preset.
- A change to the gate categories applies to items loaded after the change.

**What you see.** The On Air page and the [status display](status-display) show the target (T) and the gain (G) beside W and I. They also show the state: `converging`, `on_target`, `frozen` (not enough program to measure) or `at_limit`. Two alerts belong to the controller: [`loudness_at_limit`](alerts#loudness-at-limit) and [`loudness_frozen`](alerts#loudness-frozen).

**Tuning.** In a test on 3 hours of mixed music normalized to -18 LUFS, `ambient` held every 10-minute stretch within 0.9 LU of -14 LUFS, with the true peak at -1.4 dBTP. Material that is not normalized (items 14 LU apart) needs `follow`, not `hold`. The preset numbers are starting values. Tune them by ear.

### Read the loudness card {#reading-the-card}

The card has the same layout as the Level Rider dock. A **pill** shows the state: **RIDING** (the gain goes toward the target), **ON TARGET**, **HOLDING** (silence or gated content, so the gain is frozen), **ACQUIRING** (the controller has not measured enough program to move yet, for example just after a start or a reset) or **AT LIMIT** (the gain is held at the end of its range). Next to it are the gain and, after an arrow, the gain it goes to. The **bars** show the loudness that enters the gain (In) and the short-term (S), window (W) and integrated (I) loudness of the output. Each bar has a white line at the target. The W and I numbers are green within 0.5 LU of the target, amber within 1.5 LU and red beyond that. S has no color because it changes with the music. The **gain bar** has 0 dB at the center, and an amber tick shows where the gain goes. The **history** shows the gain for the last minute or the last 15 minutes, with a strip along the bottom in the colors of the pill. One line shows the limiter reduction now, the slow trim and the share of the last minute that the limiter was working. The footer lists the settings in force.

The **status line** says in words what the controller does: *Holding*, *Raising* or *Lowering* (how far the program is from the target and how fast the gain moves), *Warming up*, *Frozen* or *At limit*. The controller moves a few dB a minute at most, so a quiet card means a controller that is on target.
