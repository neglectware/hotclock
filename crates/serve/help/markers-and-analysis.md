# Markers and analysis

This page explains what Hotclock measures in each file at import. It describes the six markers that tell the station where to start, join and end the item, the flags that the analysis can set, and how Hotclock matches the loudness of items.

## What is measured at import

Hotclock decodes each file once. In that one pass it measures:

- **Loudness** by the EBU R128 method: the integrated loudness in LUFS, the loudness range (LRA) in LU, the true peak in dBTP and the sample peak.
- **Level curves** over time. Hotclock stores them with the item. With them, it can calculate the markers again later, with other settings, and not decode the file again.
- **The markers** below, and the **flags**.

Hotclock stores the result with the item. It writes the result again only when a newer version of the analysis exists. The analysis never overwrites a marker that you set by hand.

## The markers {#markers}

Each marker is a time in milliseconds from the start of the file. The value in use is your override if you set one. If you did not, it is the value from the analysis.

| Marker | Meaning | How the analysis finds it |
|---|---|---|
| `cue-in` | Where playing starts. | The first moment the sound goes above a silence threshold and stays there for 30 ms, minus a 20 ms pad. |
| `attack` | The first strong sound. Hotclock uses it only to line up the overlap with the item before. Playing never starts here. | The first moment the level is within 8 dB of the song's loud level, within 60 seconds of the cue-in. If there is no such moment, the attack is the cue-in and the item gets `soft_start`. |
| `intro` | The end of a spoken intro, where the vocal starts. It is only for a voice laid over an intro. | Not found automatically. Set it by hand. |
| `segue` | The point where the next item can start: the last time the item is above its segue threshold on the way out. | The last moment the loudness is at or above its loudness minus 12 LU (for music). |
| `fade-out` | Optional. Where the item starts a fade to its cue-out. | Not found automatically. Set it by hand, in seconds before the end. See [Set a fade-out](item-editor#fade-out). |
| `cue-out` | Where playing ends: the audible end. | The end of the last sound above the silence threshold, plus a 50 ms pad. |

The markers always stay in this order: cue-in, attack, segue, cue-out. The playing window is always at least 250 ms. The editor does not let you save an order that breaks these rules.

Hotclock calculates the **silence threshold** for each file from its loudness. It is 36 dB under the loudness, but not above -45 dBFS and not below -60 dBFS. So a loud master gets a high threshold, and a very quiet recording gets a low one.

### Profiles

The **profile** of the item's category sets how the analysis reads the file:

| Profile | Used for | Difference |
|---|---|---|
| `music` | songs | The values above. |
| `liner` | liners, IDs and promos (and commercials) | The segue threshold is 15 LU under the loudness. No long-intro flag. |
| `ambient` | ambient music | A lower silence threshold (42 dB under the loudness, between -66 and -50 dBFS) and a segue threshold 24 LU under the loudness. The attack is ignored, so the item starts from its cue-in. |
| `longform` | shows and talks | The segue is the cue-out. The item plays to the end. |

### Long tails

If the gap from the segue to the cue-out is longer than 15 seconds, the analysis looks again with a threshold 20 LU under the loudness. It keeps the later result. This keeps a long fade audible, and the next item does not start half a minute early. The item gets `long_tail`.

### The hold-off rule

For an item longer than 30 seconds, the segue cannot be earlier than the middle of the playing window. If the analysis finds an "end" before the middle, the item gets the flag `segue_suspect`, and its segue goes 3 seconds before the cue-out.

### Fades that were cut short {#truncated-fade}

Some files stop before their fade-out is over. The fade is still going when the file ends, for example at -60 dBFS. This happens with tracks that were rendered at home. If the track is quiet, normalization raises it by up to 12 dB. Then the end of the fade is audible, and the sound stops dead.

The analysis looks at the last 12 seconds of the file. It removes any DC offset first, and it skips digital silence at the very end. The item gets `truncated_fade` when both of these are true:

- The last 500 ms with sound are louder than -70 dBFS. So is their last 100 ms, and it is less than 6 dB under their first 100 ms.
- The 10 seconds before that are a fade. The level falls by at least 6 dB, by at least 0.4 dB a second, and it does not rise in at least three of every four seconds.

A fade that reaches silence ends far below -70 dBFS, so it does not get the flag. A fade that reaches zero exactly at the last sample falls steeply in its last half second, so it does not get the flag either. A song that ends at full level does not fall, so it does not get the flag.

The station gives a flagged item a short fade to silence at its end. See [Crossfades](crossfades#truncated-fade). An item that you gave a fade-out by hand keeps your fade.

Items imported before this check existed do not have the flag yet. Analyze them again from the [Library](library#analyze-again) page.

## Override a marker {#overriding-a-marker}

1. Open the item in the [item editor](item-editor#markers).
2. Drag a marker, or type a number.

Hotclock keeps the marker that you set as an override. It calculates the markers after it again, inside the window that you set. It never reads anything outside the window. For example, if you set a later cue-in, the attack, the segue and the loudness for the next steps all follow it.

## Flags {#flags}

The library list and the editor show the flags. Flags mark things to check, and a few of them change what the station does.

| Flag | Meaning |
|---|---|
| `no_audio` | The file has no usable sound. Import refuses it. |
| `cold_start` | The attack is within 150 ms of the cue-in: the song starts loud. |
| `cold_end` | The cue-out is within 300 ms of the segue: the song ends suddenly, and the next item starts right against it. |
| `long_tail` | See [Long tails](#long-tails). |
| `soft_start` | The analysis found no strong sound in the first 60 seconds. |
| `long_intro` | The attack is more than 4 seconds after the cue-in (music). What the station does depends on the `long_intro_policy` of the crossfade style. The editor offers **Cue in at attack**. |
| `long_intro_review` | A long intro of more than 20 seconds, so Hotclock did not skip to the attack. It is more probably a sound effect or a hidden intro than a slow build. Look at it. |
| `override_outside_window` | A marker that you set is outside the current playing window. Hotclock ignores the value but does not delete it. |
| `gain_pending` | Hotclock has not yet measured the exact loudness of the playing window. It uses an estimate until then. |
| `gapless_unknown` | An MP3 with no gapless header, so a gapless album transition can be off by a few hundred samples. |
| `ambient_suspect` | The loudness range is over 15 LU, or the file has both a soft start and a long tail. It possibly belongs in the `ambient` category. This is only a suggestion. |
| `segue_suspect` | The hold-off rule moved the segue. Check it. |
| `clipped` | Sixteen or more samples in a row at full scale. |
| `short` | Shorter than 15 seconds. |
| `cue_override_invalid` | Your cue overrides left no usable window, so Hotclock ignored them. |
| `truncated_fade` | The file stops in the middle of its own fade-out. See [Fades that were cut short](#truncated-fade). |

The station adds three flags while it plays: `marker_invalid`, `rule_relaxed` and `hard_time`. See [Failures and recovery](failures-and-recovery) and [History](history).

## Loudness matching {#normalization}

Hotclock plays each item at a gain that brings its loudness to the station's target. The gain, in dB, is:

> target loudness, minus the item's loudness, plus the category's offset, plus the item's own gain trim

Two limits hold the gain back:

- **`max_boost_db`** (default 12): Hotclock never raises a quiet item by more than this.
- **`peak_allowance_db`** (default 6): the item's true peak, after the gain, goes at most this far past the -1 dBFS ceiling of the safety limiter. So a quiet item with high peaks goes into the limiter a little, and no further.

The target is **`target_lufs`** (default -18). The categories `liner` and `id` have an offset of -1 dB. Songs of one album played in order use the loudness of the whole run, so the album keeps its own dynamics between tracks.

The loudness of an item is the loudness of its playing window (cue-in to cue-out). So a long silent tail does not make the item seem louder than it is. To make a small change to one item, use the **Gain trim** field in the editor.

A change of `target_lufs`, `max_boost_db` or `peak_allowance_db` applies from the next item that Hotclock plans. See [Output and level](output-and-level) for the rest of the signal path.
