# Settings

The Settings page changes every setting of a station and of the box from the web UI. No one has to edit a file by hand.

To create, rename or delete a station, use the [Stations](stations) page. For that reason, the `name` key is read only here.

The page has one tab for each station (**Station** and its name) and one tab called **This box**:

- A station's tab edits that station's file, `/etc/hotclock/stations/<name>.toml`.
- **This box** edits the global file, `/etc/hotclock/hotclock.toml`. It also has the status display and meter settings.

The [Settings reference](settings-reference) gives every key, its range, its default and what it does. The reference is built from the same list that this page uses.

## Read a row {#reading-a-row}

- **The key** is the name in the file. Below it, a chip tells you when a change takes effect. The word `default` appears when the file does not set the key.
- **The control** shows the value in force. The **default** button beside it removes the key from the file, so the default applies again.
- **The help line** tells you what the key does, the default and the allowed range.
- A row turns yellow when you change it and do not save it yet.

## When a change takes effect

| Chip | Meaning |
|---|---|
| `live` | Takes effect immediately, with no restart. |
| `next boundary` | Takes effect from the next item the station plans. The item on air and the item already committed do not change. |
| `restart station` | The station reads this key when it starts. The page can restart the station for you. |
| `restart library` | The library service reads this key when it starts. |
| `restart display` | The status display reads this key when it starts. |

When you save, the page checks every value with the same code that the services use. The page refuses a value that is out of range and tells you why. A change that would break the file changes nothing.

## Go back to earlier settings {#settings-history}

The **History** button on a station's tab lists every saved version of its settings file. You can see what each save changed and restore any version. See [Settings history](processing#settings-history).

## Save a change {#saving}

1. Change the values on the tab.
2. Press **Save**.

The page changes only the keys that you edited. It writes the file again with all its comments, its order and the keys that the page does not know. The previous file is kept beside it as `<name>.toml.bak`. The write is atomic, so the file is never half written.

The result card lists each key that changed and what happened to it:

- `applied now`
- `waits for a restart`
- `not applied (station not answering)`

A key that takes effect live or at the next boundary goes to the running station immediately. The station also reads the file again at its next start.

If a change needs a restart, a button appears:

- **Restart station now** restarts the station and waits until it is healthy. The station is silent for a few seconds. It restarts immediately, not at a song boundary, so do it between songs of a show you care about. When it can, the station comes back on the same item at the same place.
- **Restart display now** restarts the status display. The screen goes blank for a second. No station is touched.
- **Restart library service now** restarts the service that runs the web UI. The page connects again when the service is back. The stations continue to play.

A change to `library.listen` moves the web UI to a new address. After the restart, open the new address.

### If the page says the file is read only

The service that runs the web UI needs write access to `/etc/hotclock` and to the station files. The package sets this up. If a file shows a `read only` chip, someone changed its owner or its mode.

To repair it, give the file back to the group `hotclock` with write access:

```
sudo chgrp hotclock <file> && sudo chmod 664 <file>
```

See also [Troubleshooting](troubleshooting#settings-read-only). A key that takes effect live still applies when the file cannot be written. The page warns you that the change is lost at the next start.

## Pickers

- **Sound card** (`device`) lists the cards that the system found, and `null` for a station with no card. Select **Other** to type a device yourself: `hw:CARD=...,DEV=0`, or `file:/path.wav` for testing. A `file:` device writes a WAV file as fast as it can.
- **Playlists** (`default_playlist`, `emergency_playlist`) list your playlists. Select **Other** to name a playlist that does not exist yet.
- **Crossfade style** (`transition_preset`) lists the built-in crossfade styles, with one line about each. The same picker is on [On Air](on-air#crossfade-style) (press **Crossfade** in the settings line of the Program unit) and on [System](system).

## Change a category's crossfade style {#category-crossfade-style}

On a station's tab, each category has a drop-down. Select a crossfade style for the category, or select `inherit (station)` to use the station's style.

A `has a patch` chip tells you that the category also changes single parameters of the style. You cannot edit patches on this page. See [Crossfades](crossfades#category-and-item-styles).

## Meter ballistics

On the **This box** tab, one row sets the default for the box. One more row for each station sets that station's own ballistics. See [The status display](status-display#meter-ballistics).

## Keys that arrive later

A card at the bottom of a tab can list keys that are designed but not in this build. Each key appears on this page when its code is installed. See [Overview](overview#what-is-built-and-what-is-coming).
