# Item editor

The item editor shows one item of the library. It has the item's details, its waveform with the markers, a player, and the item's own transition settings. The markers tell the station where to start the item, where the next item comes in, and where to end it.

To open the editor, click an item in the [Library](library). The **Library** link at the top takes you back to the list.

## Details

The top card has the cover, the fields that you can edit, and the buttons.

- **Title, Artist, Album, Disc, Track:** the tags. Tracks with the same album and artist make an album. You can play an album gapless.
- **Category:** the kind of item. The category sets how the station joins the item to its neighbors. See [Crossfades](crossfades#category-and-item-styles).
- **Tags:** free words, separated by commas, for your own filing.
- **Enabled:** a rotation or a block never selects an item that is not enabled. The item stays in the library.
- **Quiet piece:** select it for an item that is quiet on purpose. The silence detector then ignores the item. See [Failures and recovery](failures-and-recovery#silence).
- **Save details** saves all of these fields.
- **Replace artwork** takes a PNG or JPEG file and replaces the cover.
- **Delete** removes the item after you confirm.

## Release an item from quarantine {#quarantine}

When the item is quarantined, a red card above the details gives the reason. The card has a **Release from quarantine** button.

**Caution:** correct the cause first (restore or replace the file). If you do not, the item goes back into quarantine.

1. Correct the cause.
2. Press **Release from quarantine**. The box releases the item in the library and on every station of this box, and sets its error count to 0.

See [Failures and recovery](failures-and-recovery#quarantine).

## Waveform and markers {#markers}

The waveform card shows the whole file.

- The part outside the playing window is dark.
- A gray line shows the loudness over time.
- The dashed amber line is the **segue threshold**. The outgoing song must fall below this level before the next song can come in.

There are six markers. Each marker is a colored vertical line. [Markers and analysis](markers-and-analysis) explains what they mean.

### Move a marker {#move-a-marker}

1. Drag a line to move it. The editor takes the marker nearest the pointer, within a few pixels.

   The faint white lines (the "ghosts") are the values that the analysis found by itself. They show how far you moved each marker.
2. Or type a value in the number boxes below. They show each marker in milliseconds, with the automatic value beside it. The fade-out box is different: see [Set a fade-out](#fade-out).
3. Press **Save markers**. The editor keeps your changes as overrides. The analysis never overwrites an override.

The other controls:

- **Back to auto** removes your override for that one marker.
- **Gain trim (dB)** changes how loud this item plays, in addition to the normalization. Leave it empty for no trim.
- **Everything back to auto** removes every override of the item, after you confirm.
- **Cue in at attack** appears for an item with the flag `long_intro`. It moves the cue-in to the attack, so the song starts at its first strong sound and skips its quiet opening. Press **Save markers** to keep the change.

### Set a fade-out {#fade-out}

A fade-out makes the item fade to silence at its end. You give it as the number of seconds before the end.

1. Type the seconds in the **fade-out** box, for example `5`. The fade then starts 5 seconds before the cue-out and lasts until the cue-out.
2. Beside the box, the editor shows where the fade starts (as a time and in milliseconds) and how long it is. On the waveform, the fade-out line moves to that point, and a line falls from it to the cue-out.
3. Press **Save markers**.

You can also drag the fade-out line on the waveform. The box follows it.

Some rules apply:

- An empty box, or `0`, means no fade-out.
- If you move the cue-out in the editor while the box shows **s before the end**, the fade-out moves with it.
- The fade-out cannot start before the attack, or more than 30 seconds before the segue. If you type more, the editor moves the fade to the earliest allowed point and shows a message. The saved value is the moved one.

To type the start of the fade as a position in the file instead, select **ms from the start** beside the box. The box then shows milliseconds from the start of the file. Select **s before the end** to go back. The editor always opens with **s before the end**.

**Back to auto** removes the fade-out.

If the item has the flag `truncated_fade` and no fade-out of its own, the box says that the automatic fade applies, and a dashed line shows it on the waveform. See [Crossfades](crossfades#truncated-fade).

In the API, `PUT /api/v1/items/<id>/overrides` takes `{"fade_out_before_end_ms": 5000}` for 5 seconds before the end, or `{"fade_out": <ms from the start>}`. Send one of them, not both. `"fade_out_before_end_ms": 0` or `null` removes the fade-out, and so does `"fade_out": null`. The answer has `notes` when the fade was moved. The item's `effective` values include `fade_out_before_end_ms`.

### What the card shows {#what-the-card-shows}

The card also shows these values for the file:

- the loudness (LUFS),
- the loudness range (LRA),
- the true peak,
- the longest quiet part inside the playing window. The silence detector waits at least that long, plus five seconds, before it skips the item.

If a saved marker falls outside the playing window, the item gets the flag `override_outside_window`. The station ignores the value but does not delete it.

If the markers make no sense when the station loads the item, the station plays the whole file from its start. It then raises `item_marker_invalid`.

## Audition {#audition}

The player plays the stored file. Use it to hear the markers that you set. Each button starts playback at a marker:

- **From cue-in**
- **From attack**
- **From intro end** (when there is an intro marker)
- **Segue minus 8 s**
- **Cue-out minus 5 s**

## This item's own transition {#item-transition}

An item can have its own crossfade style. It applies to the transitions on both sides of the item, whatever the neighbor is. It overrides the category's style, and a single queue entry's own transition overrides it.

1. Select a **Preset**. `inherit` uses the category's style, or the station's style if the category has none. Or select one of the crossfade styles.
2. Optional: in the box under it, type a JSON patch that changes single parameters for this item, for example `{"tail_max_s": 6}`. The editor refuses a patch that is not valid JSON and shows a message.
3. Press **Save transition settings**. The editor keeps both.

See [Crossfades](crossfades) for the crossfade styles and their parameters.

## Preview a transition {#preview-transition}

The preview plays the real transition between two items. It uses the same code as the station, so you hear a crossfade style before you select it.

1. Select the direction: this item then another item, or another item then this item.
2. Select the other item. Search the library and select it. Or select a playlist that contains this item, then press **Previous in playlist** or **Next in playlist**.
3. Optional: type a JSON patch for this one transition, for example `{"overlap_max_s": 4}`.
4. Press **Preview transition** to hear the transition as the station would play it. Or press the button of one crossfade style (`basic`, `plexamp`, `sam`, `fm`, `none`) to hear that style.

The player starts immediately. The two waveforms show the transition:

- The lit band is what was rendered.
- The orange band is the overlap.
- The orange line is where the second item starts.
- The red line is where the first item is stopped.
- The coral lines are the fades.

The line above the player gives the crossfade style used, the kind of transition and the overlap in seconds.
