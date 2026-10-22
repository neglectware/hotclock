# Insert rules

An insert rule plays an item from a category after every N songs. For example: a liner every four songs, or a station ID every three songs. This page lists the rules, and you add or remove rules here.

A rule belongs to one **playlist** or to the whole **station**.

- A playlist rule works only while that playlist is the program.
- A station rule works under every playlist, unless the playlist turns station blocks off. See [Playlists](playlists#station-blocks-switch).

## Playlist rules {#playlist-rules}

The first card lists the rules inside playlists. Each rule shows as `in <playlist>: every N songs from <source>, at least M min apart`. **Remove** deletes a rule.

To add a rule to a playlist:

1. Open the playlist on the [Playlists](playlists) page.
2. In the **Insert rule** box, type a name.
3. Select a category, and type the number of songs.
4. Press **Add rule**.

## Station rules {#station-rules}

The second card lists the station's blocks, including its timed blocks (for example, an ID at the top of the hour). Station rules apply to the station that is selected on the other pages.

To add a rule that applies to everything the station plays:

1. Type a name.
2. Select a category, and type the number of songs.
3. Press **Add station rule**.

## How a rule works

- **The count.** Each rule has its own counter of the songs that play. When the counter reaches N at a song change, the rule's item takes the next place in the queue. The counter then starts again.
- **Only songs count.** The liners and IDs themselves are not counted.
- **The rotation.** The item comes from the category's own rotation. The liners and IDs therefore take turns, and none repeats before the others have played.
- **Several rules at once.** When several rules are due at one song change, they play one at a time, in order of priority. The others wait for the next song change.
- **The limit.** There are never more than **two** items from rules in a row. A timed block, such as the hourly ID, can add a third.
- **IDs reset liners.** When an **ID** plays, it also resets the counters of the liner rules. A liner then does not follow an ID directly.
- **The minimum gap.** A rule can have a minimum gap in minutes between plays, for example a donation liner no more than once in twenty minutes. The page shows it as `at least M min apart`.
- **N below 1.** The checks refuse a rule with N below 1.

The station joins an item from a rule to its neighbors as its category says. A liner or ID never lets the next song cover its last word. See [Crossfades](crossfades#category-and-item-styles).
