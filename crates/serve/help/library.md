# Library

The Library page holds every audio file that the box can play. Here you add files, search and filter them, play an album, and open an item to change how it plays.

## Import files {#import}

The **Import** card at the top adds files to the library.

1. Select the **Category** before you add the files. Songs go in `music`. Station IDs, liners and promos have their own categories, because the station joins them to the songs around them differently. See [Crossfades](crossfades#category-and-item-styles).
2. Drop the audio files on the dashed box, or click **choose files**. You can add many files at once.

Some limits apply:

- A file must be 1 GB or smaller. The page skips a larger file and shows a message.
- The page sends a batch in parts of 4 GB or less.
- The box needs free disk space for the upload. If there is not enough, the box refuses the upload. The message tells you how much space is necessary.

The list under the card shows the latest imports and their status:

| Status | Meaning |
|---|---|
| `queued`, `running` | The file waits, or the box analyzes it. The box processes two files at a time. |
| `done` | The file is in the library. The item's link opens it. |
| `duplicate` | The same file is already in the library. Nothing was added. |
| `failed` | The file could not be used. The page shows the reason, for example that the file cannot be decoded or has no audio. Nothing is added. |

The box reads and decodes every file in full once, and stores the analysis with it. The box refuses a file that it cannot decode, so a damaged file never goes to air.

### The inbox folder

The box also imports files that you copy into `/var/lib/hotclock/inbox/`.

- A folder named after a category puts its files in that category: `inbox/liner/`, `inbox/id/`, `inbox/promo/`.
- Files directly in the inbox go to `music`.
- The box takes a file two seconds after it stops changing.
- The box removes a finished file from the inbox.
- A file that fails moves to `inbox/.failed/`, with a text file that gives the reason.

### Formats

The box reads FLAC, MP3, AAC and M4A (including ALAC), WAV, AIFF and Ogg Vorbis directly.

It converts Opus, WMA and AC-3 files to FLAC first. This needs `ffmpeg` on the box. Without `ffmpeg`, those files fail with a clear message.

The box finds the format from the content, not from the name. An MP4 file named `.aac` therefore works.

The box keeps each file as it arrived, in its own media store. Playout does not depend on the original location, on Plex or on a file server.

## Find items {#finding-items}

- **Search** looks in the title, artist and album as you type.
- **All categories** limits the list to one category. The number beside each name is how many items that category holds.
- **Any flag** shows only the items with one flag: `long_intro`, `ambient_suspect`, `truncated_fade`, `quarantined`, `disabled`, or no artwork. See [Markers and analysis](markers-and-analysis#flags).
- **Sort** by title, artist, album, newest or longest.
- **Albums** shows a list of albums. **Tracks** shows the tracks again.

Click a row to open the [item editor](item-editor). The **Flags** column shows `quarantined` (red), `disabled` (amber) and the analysis flags.

### Play an album {#play-an-album}

In the album list, two buttons add an album's tracks to the end of the station's queue, in disc and track order:

- **Play album (gapless)** joins the tracks with no gap and no overlap, as the artist made them.
- **Play album (normal)** joins the tracks with the station's usual transition.

Both buttons act on the station selected on the On Air page. They use only the tracks that the library has. A missing track number stops the run at that point.

## Analyze again {#analyze-again}

A newer version of Hotclock can check more things when it analyzes a file. For example, it now finds a fade-out that the file cuts short (`truncated_fade`). Items that you imported before do not have the new checks yet.

When the library has such items, the **Analysis** card appears at the top of the page. It says how many items an older version analyzed.

1. Press **Analyze them again**.
2. The box decodes the stored files again in the background. It does not import anything again. Your markers, your gain trim and your other overrides stay.
3. Reload the page later to see the new flags.

If an analysis fails, the item keeps its old analysis and stays playable.

You can do the same on the command line with `hotclock reanalyze --stale`. See [Command line](command-line). The API is `POST /api/v1/reanalyze` with `{"stale": true}`, `{"all": true}` or `{"item_ids": [...]}`. `GET /api/v1/reanalyze` gives the number of such items.

## Plex

The **Plex** card imports playlists from a Plex server and adds missing artwork. See [Plex tools](plex-tools).

## Quarantine

The box **quarantines** an item that fails again and again. For example, the item cannot be read, is cut short, decodes badly or is silent on air.

- Rotation, blocks and inserts never select a quarantined item.
- The station removes it from the queue.
- The item stays in the library, with its history and the reason.

The [System](system#quarantine) page lists every quarantined item and releases it. See [Failures and recovery](failures-and-recovery#quarantine).
