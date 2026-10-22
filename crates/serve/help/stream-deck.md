# The Stream Deck plugin

This page explains how to put a Stream Deck in front of a station. It tells you how to install the plugin, how to tell it where the box is, and how to set up the keys.

The plugin is a separate download, built from the `streamdeck` folder of the source (see [Build and install](#build-and-install)). It talks to the box over the network, through the same API as the web UI.

The plugin keeps no state of its own, other than its settings. What a key shows is what the box says.

## Build and install the plugin {#build-and-install}

The plugin needs the Stream Deck app 6.9 or later. Nothing is installed on the box for it.

1. On a Mac or PC with Node.js 20 or later, open the `streamdeck` folder.
2. Run `npm install`.
3. Run `npm run pack`. This writes `dist/com.neglectware.hotclock.streamDeckPlugin`.
4. Double-click that file. The Stream Deck app installs the plugin and adds a **Hotclock** category to the action list.

## Tell the plugin where the box is {#box-address}

1. Drag a Hotclock action onto a key. The settings for the key open under it.
2. In **Box address**, type the host name or address of the box, for example `hotclock.local`. The plugin saves it once for the whole plugin, so every key uses it.
3. Press **Test connection**. The plugin asks the library service for its stations, and shows them: `Connected to the library: 2 stations (office, eve)`. If the port is a station's port, it says so: `8301 is a station port (station office), not the library. The library is usually 8300.`
4. In **Station**, select the station that this key controls. The plugin reads the list from the box, and saves the station's name and its port. **Refresh** reads the stations and playlists again. The lists are also read again each time the settings open, so a playlist made since shows up.

Until a box address is set, every key shows `SET UP` in amber. A key that needs a playlist, an item or a block shows `SET UP` and what it needs.

A key that controls a station must have one. A key with no station selected shows a red `NO STATION`, and a press sends nothing. It never falls back to the first station.

## Set a key's face {#key-face}

Under **Key face**, you can change what any key looks like:

- **Label**: your own text, in place of the key's main text (for example, the playlist name). The small status line stays, for example `RUNNING` or `expected 18:45`.
- **Label color**: the color of that text. **Default** goes back to the key's own colors.
- **Image**: your own picture for the key. The plugin makes it 144 x 144 pixels.
- **Image when lit**: a second picture for when the key is lit (the program runs, the playlist is on air, a hold is on).
- **Tint the image green when lit**: with no second picture, the first picture turns green while the key is lit. So one picture is enough.

With nothing set, the key keeps its own face. The faces for a problem (`OFFLINE`, `NO STATION`, `SET UP`) do not change.

Under **More** there are two more settings:

- **Library port** is the port of the library service (default 8300).
- **This key only** points one key at a different box. Use it when two stations run on different boxes.

## The actions {#actions}

| Action | A press does | The key shows |
|---|---|---|
| Now Playing | nothing | The album art (dimmed), the title, the artist, and the time left to the segue as `-2:31`. Amber at 30 s and less, red at 10 s and less. Inside a gapless album run, it counts to the end of the run and shows `ALBUM`. An item with no art shows a plain placeholder. |
| Elapsed / Remaining | changes between the two | the time as large digits |
| Next Up | nothing | the next title and when it starts |
| Next (Segue Now) | segues now | `NEXT` |
| Fade to Next | fades out and starts the next item (the fade length is a setting; blank means the station's default) | `FADE` |
| Hold / Resume | holds after the item on air, or resumes | lit amber while held |
| Stop | **hold the key** for half a second | `STOP`; a short press shows `HOLD` |
| Cut | **hold the key** for half a second | `CUT` |
| Start / Switch Playlist | switches to the selected playlist (now, at the next segue, or at the end of this item), or starts it when the station is stopped | Lit green while that playlist is the station's active playlist. A sequential playlist plays through once. The station then goes back to its default playlist, and the key goes dark. |
| Schedule Change | schedules a one-time switch at a minute of the hour (for example `:00`) or after a delay; a second press cancels it | `SCHEDULED` and the time, while a switch is pending |
| Reshuffle | reshuffles the active playlist | the new next title, for 2 s |
| Fire Item (Cart) | plays the selected library item over the audio on air | lit green while it plays |
| Loudness | nothing; **hold** to reset the integrated reading | The window loudness against the target, for example `-14.2 / -14`. Green within 1 LU, amber outside it. With the loudness controller off, it shows the reading and `no target set`. |
| Station Status | opens the box's web UI | green, amber (a warning alert) or red (a critical alert, or the station does not answer) |
| Start Playlist as Program | starts the playlist as the program now (T0 = now), from its first row, whatever the station is doing. The picker lists only the programs of the key's station, and those of no station. The box refuses a program of another station: a stopped station starts playing, and anything else on air fades out over 2 s (see [Start a program](block-engine#start-a-program)); **hold** cancels the fallback start | `expected 18:45`, `ARMED`, or lit while the program runs |
| Arm / Disarm Program | arms or disarms the fallback start of a program that is started by hand | Lit amber while armed. A Start block with no fallback cannot be armed (the Monday example has none). The box answers 409 with what to set, and the key shows its alert. |
| Next Block | nothing; **hold** fires the block now, if that is turned on in the key's settings | the next block's countdown (amber under 60 s, red under 10 s), and `END +3 s` while back-timing is on |
| Fire Block | fires the selected block now | lit green after it fires |
| Continue | releases a Wait block that waits for the operator | lit amber while a Wait holds |
| Skip Next Block | skips the next block | `SKIP` and the block's name |
| Abort Program | **hold the key**; ends the program, and also stops the audio if that is turned on | `ABORT` |
| Back-time End | back-times the program to end at a time of day or after a delay | the countdown to the end, and `END +3 s` |
| Display View | sets the box's display | the view that shows now |

Actions that stop audio (Stop, Cut, Abort Program) act only when you hold the key for 500 ms. A stray touch does nothing. All other actions act when you release the key.

## When the box does not answer {#offline}

When a station does not answer, every key of that station shows a grey `OFFLINE` within 5 seconds. The Station Status key turns red.

A press while the station is offline does nothing, and the plugin does not keep it. The press does not fire when the station comes back.

The plugin connects again by itself after 1, 2 and 5 seconds, and then every 10 seconds. So the keys recover within 15 seconds after the station comes back.

The plugin forgets what it showed when the connection drops, and when the computer wakes from sleep. So a key never shows a song that stopped while the computer slept.

## Set the Display View key {#display-view}

The Display View key sets what the box's own display shows (see [The status display](status-display)). In the key's settings, use **A press** to select what a press does:

- **Steps through the views**: each station alone, then both stations, then rotating. Select **Include the processing view** to add the processing view to the steps.
- **Shows one station**, **Shows both stations**, **Rotates between stations** or **Shows the processing view**: the key sets that view, and lights while that view shows.

The key talks to the library service, so it needs the library port, not a station. The processing view exists only on boxes whose version has it. An older box refuses it, and the key shows its alert.

## How the keys are drawn {#drawing}

The plugin draws each key as a picture. It sends a maximum of two pictures a second to a key, and only when the picture changes.

The countdowns run on the computer's own clock between the box's `timing` events, which come once a second. Each event corrects the countdown, so the countdowns agree with the box to within half a second.

## What the plugin uses {#api}

Each station's WebSocket (`/api/v1/ws`) gives the snapshot and the events. The plugin also reads these:

- the program state (`/api/v1/program/state`);
- the schedule (`/api/v1/schedule/upcoming`);
- the album art (`/api/v1/items/<id>/art?size=144`);
- the library's lists of stations, playlists and items, for the settings.

Presses call the transport, playlist, cart, program, end-at and loudness endpoints, and `PUT /api/v1/display` on the library service.

The station's `state` also lists the carts that play, as `carts: [{item_id, started_at}]`. So the Fire Item key lights from the box's own information.

## If a key does not work {#troubleshooting}

- **`SET UP`**: there is no box address. Set it in the settings of any key.
- **`OFFLINE`**: the plugin cannot reach the station. Open the box's web UI from the same computer. Check the station port in the key's settings (**Station port** is under the station list).
- **The station list is empty**: the box address is wrong, or the library service is not running. Press **Test connection**.
- **A press shows a yellow triangle**: the box refused the press (for example, Continue when nothing waits). The On Air page of the web UI shows the same refusal in words.
