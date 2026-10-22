# Crossfades

This page explains how one item hands over to the next. It describes the five crossfade styles and tells you where to set a style for a station, a category, an item or one single transition.

Hotclock does not just fade one song out and the next one in. A radio-style transition starts the next song at the correct moment inside the end of the last one. The way it does that is the **crossfade style**. Each style is a set of parameters. You can change single parameters for one category, one item or one transition.

## The five crossfade styles {#the-styles}

The table below comes from the running system. A style that you made yourself is in it too.

{{generated:transitions}}

In plain words:

- **`basic`** is the plain crossfade of a desktop music player. The last five seconds of a song fade down on a straight line while the next song fades up. The style does not look at the music. So a loud ending is faded, and a quiet start arrives late in the overlap. It uses the whole file and ignores the markers.
- **`plexamp`** has no fades at all. The two songs overlap and are added together, each at its own level. The overlap is as long as the quiet end of the first song plus the quiet start of the second, up to 15 seconds. So the loud part of one song never lands on the loud part of the other, and two quiet ends meet in a long, gentle blend. The style uses the whole file. An album played in order gets no gap. The style is modeled on the crossfade of the Plexamp player, and it is the gentlest.
- **`sam`** is tight and radio-like. The next song starts so that its first strong sound lands exactly where the last song falls below its segue point, 12 LU under its own loudness. The last song is not faded. The only exception is a very long tail, which is cut at 12 seconds with a 2 second fade. The next song is not faded in. If it has a soft pickup, it starts up to 2 seconds early.
- **`fm`** is the aggressive style of a modern FM station. The next song hits while the last song is still within 8 LU of its loudness. The last song is gone about half a second later. A long tail is cut at 2 seconds with a half-second fade. A long quiet intro is skipped to its first strong sound. There is no air between songs.
- **`none`** has no overlap and no gap. The first item plays to its cue-out, and the second starts from its cue-in on the next sample.

**An important idea:** the styles other than `basic` do not fade songs. They overlap the songs and add them at their own levels. Because of this, much of the sound of a transition depends on the stages after it: the AGC and the limiter of the processing chain make it tighter or looser. See [How the AGC handles a transition](processing#agc-and-joins).

The other styles use fades only in these cases:

- the cap on a very long tail
- a `fade-out` marker that you set by hand
- the short fade at the end of a file that cuts its own fade-out short (see [Fades that were cut short](#truncated-fade))
- an operator action (**Fade to next**, **Stop**)
- a timed fade-and-stop block
- a 3 millisecond ramp on every start and stop, to prevent clicks

## Choose a crossfade style {#choosing-a-style}

You can set a style at four levels. Where two levels set the same parameter, the later level in this list wins.

1. **The station's default style** is `transition_preset` in the station file. Set it in one of three places: the **Crossfade style** drop-down on [On Air](on-air), the same drop-down on [System](system), or [Settings](settings). The change applies from the next transition that is not already committed. A committed transition keeps its plan. Only the five built-in styles can be a station's default.
2. **A category's style** and its patch. See [Category and item styles](#category-and-item-styles).
3. **An item's own style**, in the [item editor](item-editor#item-transition).
4. **The override of one transition.** A queue entry can carry an override for the transition into it. Set it with the **transition** drop-down on a row of the queue on [On Air](on-air#queue). Or use the API: `PATCH /api/v1/stations/<station>/queue/<entry id>` with one of these bodies:
   - `{"transition": "cut"}`
   - `{"transition": {"preset": "sam"}}` (a preset, a `patch`, or both)
   - `{"transition": null}` to clear the override

The **Cut** key and the `end` choice of **Change program…** use the same field. If a transition is already committed to the mixer, the station plans it again with the new setting. A transition that is in its last seconds before it starts is refused as busy.

Each parameter comes from the side that it belongs to:

- What the first item does at its end comes from the first item's category and item settings. This includes the exit point, the fades, the tail cap and how much of the next item's start it accepts.
- What the second item does at its start comes from the second item's settings. This includes how early it comes in, whether its intro is skipped and its fade-in.

So an item can say "I fade out slowly" or "skip my intro", and that is true for every neighbor it meets.

## Category and item styles {#category-and-item-styles}

On the [Settings](settings#category-crossfade-style) page, every category has a drop-down. Select `inherit (station)` or a crossfade style. A category can also have a **patch**, which changes single parameters. The shipped categories have these patches:

| Category | Patch | Why |
|---|---|---|
| `liner`, `id`, `promo` | accepts no lead from the next item; comes in at most 0.5 s early itself; tail at most 3 s (0.5 s fade) | The intro of a song must never cover the last word of a voice. |
| `ambient` | no early start, tail up to 8 s with a 4 s fade | Long soft tails need a longer fade. |
| `longform` | half a second of silence between items | A spoken show must not run together with the next item. |
| `music`, `ad` | none | |

The lead and tail values of a patch are **caps**: the smaller of the two values wins. For every other parameter, the value of the patch replaces the value below it.

**The voice rule.** After an item of the category `liner`, `id` or `promo`, the next item never comes in earlier than half a second before the voice ends. This is true for every crossfade style.

An item's own patch is JSON in the item editor, for example `{"tail_max_s": 6}`.

## The parameters {#parameters}

A style and a patch use these names. If a style does not name a parameter, the parameter keeps the value of the level below. If no level names it, it takes the model's default (the `sam` values).

| Parameter | Meaning |
|---|---|
| `exit.mode` | How the end point of the first item is found: `segue_marker` (the item's segue marker), `last_crossing` (the last moment the loudness is within `exit.threshold_lu` of the item's own loudness) or `fixed_before_end` (`exit.seconds` before the cue-out). |
| `exit.threshold_lu` | For `last_crossing`: LU below the item's loudness. |
| `out_fade.curve`, `out_fade.seconds` | The fade of the first item: `none`, `linear`, `cosine` or `log`, and its length. |
| `align.mode` | Where the second item starts: `at_cue_in` (at the exit point, from its cue-in), `attack_to_exit` (its first strong sound lands on the exit point), `fixed_overlap` (`align.overlap_s` before the end of the first item) or `ramp_overlap` (the overlap grows with how quiet the two ends are). |
| `align.overlap_s` | For `fixed_overlap`. |
| `align.threshold_db`, `align.overlap_max_s` | For `ramp_overlap`: the level at which the quiet ends are measured, and the longest overlap. |
| `in_fade.curve`, `in_fade.seconds`, `in_fade.skip_if_cold_start` | The fade-in of the second item. With `skip_if_cold_start`, an item that starts loud is not faded in. |
| `lead_max_s` | The most that the second item can come in before the exit point. |
| `accept_lead_s` | The most of the next item's start that the first item accepts under its end. |
| `tail_max_s`, `tail_fade_s` | When there is no out-fade: the first item stops this long after the exit point, with a fade of `tail_fade_s`. |
| `long_intro_policy` | `play` plays a long quiet intro under the end of the last item. `skip_to_attack` starts the item at its first strong sound. |
| `gap_s` | If above 0, the second item starts this long after the cue-out of the first, and the overlap parameters are ignored. |
| `bounds` | `cue` uses the analyzed cue-in and cue-out. `file` uses the whole file. A cue marker that you set by hand always wins. |

## A fade-out that you set by hand {#manual-fade-out}

Set a fade-out on one item in the [item editor](item-editor#fade-out). You give it in seconds before the end, for example `5`. The item then fades to silence over its last 5 seconds, from 5 seconds before its cue-out to its cue-out. Under a style that uses the whole file, such as `plexamp`, the fade ends at the end of the file.

A fade-out that you set by hand wins over the fade of the style and over the cap on a long tail.

## Fades that were cut short {#truncated-fade}

Some files stop while their own fade-out is still going. The analysis gives such an item the flag `truncated_fade`. See [Markers and analysis](markers-and-analysis#truncated-fade).

The station gives a flagged item a short fade to silence:

- The fade is 2 seconds long and ends at the item's cue-out.
- It never ends before the next item has started. If the next item starts later, the fade ends there.
- It applies only when nothing else fades the item to silence. A fade-out that you set by hand wins. The fade of a style, such as the 5 second fade of `basic`, or the cap on a long tail, also wins.
- It also applies to the last item, when nothing comes after it.

Normalization can raise a quiet item by up to 12 dB. The fade comes after that gain, so the item still reaches silence.

To change the fade, open [Settings](#/settings). In the **Loudness and crossfade** group, `truncated_fade` turns it on or off, and `truncated_fade_s` sets its length, from 0.5 to 10 seconds. A change applies from the next transition that is not committed. The **Preview transition** of the item editor uses the default, 2 seconds.

## Transitions you start by hand {#joins-you-start-by-hand}

| Action | The item on air | The next item |
|---|---|---|
| **Next** | Fades over 1 second. | Starts so that its first strong sound lands where the fade ends. |
| **Fade to next** | Fades over 2 seconds. | The same as **Next**. |
| **Cut** | A 30 millisecond fade. | Starts at the same instant. |
| **Stop** | Fades over about 1.5 seconds. | Nothing. |

These actions ignore the crossfade style. See [On Air](on-air#transport).

## Albums

Consecutive tracks of one album, played in order, have no gap and no overlap at all. This applies to an album entry, to **Play album (gapless)** and to a sequential playlist.

- The first track plays to its last decoded sample. The next track starts on the next sample, from the very start of its file.
- Markers and crossfade styles are ignored at that transition.
- The result is exact for FLAC, ALAC, WAV and AIFF.
- For MP3, the result is exact only when the file has a gapless header. Without one, the transition is correct to within a few hundred samples, and the item gets the flag `gapless_unknown`.

In a shuffle playlist the markers apply, as they do between songs of different albums.

## Hear a style before you select it {#hearing-a-style-before-you-choose-it}

Use **Preview transition** in the [item editor](item-editor#preview-transition). It renders the real transition between two items and draws it.

## Make your own crossfade style {#your-own-styles}

You can store custom styles through the API:

- `PUT /api/v1/transition-presets/<name>` with the parameters and a description stores a style.
- `DELETE` on the same address removes it.

You cannot change or remove the five built-in styles. To change one, make a copy under another name. A category or an item can use a custom style. The web UI has no editor for custom styles yet.
