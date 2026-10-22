# Plex tools

The Plex tools copy playlists from a Plex Media Server into the library, and fill in missing cover pictures and tags from Plex. They only read from Plex, and each import is a one-time copy. The tools are the Plex card on the [Library](library) page.

Hotclock never needs Plex to play. The import copies the files into the box's own store, so playout does not depend on Plex or on a file server. The tools are for moving a library over, and for catching up later.

To open the tools, press **Plex tools** on the card.

## Connect to Plex

1. In **Server**, type the address of the Plex server, for example `http://plex-host:32400`.
2. In **Token file on this box**, type the path of a file that holds the Plex token, alone on its first line.
3. Press **Save**. Hotclock keeps both settings.

**Caution:** The token is a password. Make the file readable by the `hotclock` user only (`chmod 600`), and keep it out of the backups.

Hotclock sends the token as a request header. It never puts the token in an address, saves it in the database, prints it or shows it on this page. The box shows only whether a token is set and readable.

The chip beside the fields shows one of these:

- `token readable`: all is well.
- `token unreadable`: the file is there, but Hotclock cannot read it.
- `no token file`: there is no file at the path.

Type a path only when you want to change it.

## Import a Plex playlist

1. Select the playlist in the list. Each line shows how many tracks it has, whether it is `smart`, and whether it is already `imported`.
2. Select how the new library playlist plays: `sequential` keeps the order of Plex, and `shuffle` makes a rotation.
3. Press **Preview**. It shows what the import will do, and changes nothing: how many tracks are in Plex, how many are already here and how many it will download.
4. Press **Import**. The log under the buttons shows each step.

Hotclock downloads, analyzes and stores each track like any imported file. It then creates a static playlist with the same name, in the order of Plex. It fills in pictures that a file does not have from Plex.

When the import is done, a summary line tells you how many tracks it imported, how many were already in the library and how many failed.

An import never downloads or adds a track that is already here. So you can import the same playlist again after it changes in Plex.

## Fill missing artwork

1. Select a playlist, or **every Plex item**.
2. Press **Preview** to see what will change, or press **Fill missing artwork** to do it.

What the tool changes:

- It touches only items that have no picture. It never replaces a picture that came with the file.
- It fills in a blank title, artist, album or year from Plex too. It does not change a value that is already set.

The summary tells you how many items it checked, how many already had a picture, how many it filled, how many Plex has no picture for, and how many failed.

## Use the command line {#the-command-line}

The same functions run from a terminal.

1. Run `sudo -u hotclock hotclock import-plex --token-file /path/token --list`. It lists the playlists.
2. Add `--playlist "Name" --dry-run` to see what the import will do.
3. Run the command again without `--dry-run`. The import starts.

Other options are `--mode shuffle`, `--as` to give the new playlist a different name, `--category`, `--no-art` and `--fill-art`. See [Command line](command-line).
