# Upgrading, backup and restore

This page tells you how to install a new version with no surprises, how the box protects its databases, and how to put a snapshot back.

## Where things are

| Path | What |
|---|---|
| `/usr/bin/hotclock` | The program. |
| `/usr/bin/hotclock-restore` | Puts the newest snapshots back. |
| `/etc/hotclock/hotclock.toml` | The global configuration. |
| `/etc/hotclock/stations/<name>.toml` | One file for each station. |
| `/var/lib/hotclock/library.db` | The library database. |
| `/var/lib/hotclock/media/` | The stored audio and artwork. Files are stored by content and never changed. |
| `/var/lib/hotclock/inbox/` | The drop folder for imports. |
| `/var/lib/hotclock/stations/<name>/state.db` | One station's queue, history, alerts and block log. |
| `/var/lib/hotclock/backup/` | The nightly snapshots. |
| `/var/lib/hotclock/secrets.toml` | The mail password (mode 0600). |
| `/var/lib/hotclock/debs/` | The packages that were installed, kept for rollback. |
| `/usr/share/hotclock/` | The install and upgrade scripts and example station files. |

The units are `hotclock-library.service`, one `hotclock-station@<name>.service` for each station, and `hotclock-display.service` for a screen. The configuration files are conffiles, so your edits stay after an upgrade. For the program's own help, run `hotclock --help`.

## Upgrade Hotclock {#upgrading}

Installing a package never restarts anything. A person decides when.

**Caution:** A restart of a station stops its audio for about three seconds. Do it between songs, not during a live show.

1. Copy the new package to the box.
2. Run `sudo /usr/share/hotclock/upgrade.sh install hotclock_<version>_amd64.deb`. The script takes a snapshot of the databases, keeps a copy of the package and installs it. Nothing restarts. The old program continues to run.
3. At a quiet moment, run `sudo /usr/share/hotclock/upgrade.sh restart`. The new program starts.
4. Open [System](system) and make sure that every station is `ok`.

```
sudo /usr/share/hotclock/upgrade.sh install hotclock_<version>_amd64.deb   # snapshots the databases, keeps the package, installs it; restarts nothing
sudo /usr/share/hotclock/upgrade.sh restart                                # the library service first, then each station in turn, each checked for health
sudo /usr/share/hotclock/upgrade.sh rollback                               # installs the previous kept package and restarts the same way
```

How `restart` works:

- It restarts the library service first, then each running station, one at a time. It waits up to 30 seconds for each one to answer its health check. Last, it restarts the status display, if it runs.
- It does not start a station that was stopped.
- A restarted station comes back on the same item at the same place when it can. See [Failures and recovery](failures-and-recovery#restarts).
- It stops at the first service that does not come back healthy. The stations after it continue to run the old program. Fix the problem, or run `sudo /usr/share/hotclock/upgrade.sh rollback`.

The restart buttons on the Settings page restart one unit in the same way. See [Settings](settings).

## Backups

### Snapshots {#snapshots}

Each process that owns a database writes a consistent snapshot of it every night at **02:30 local time**. It copies the database safely while the database is in use, then runs an integrity check on the copy.

- Hotclock keeps the newest seven snapshots in `/var/lib/hotclock/backup/`, as `library-YYYYMMDD.db` and `station-<name>-YYYYMMDD.db`.
- `/health` shows the time of the last snapshot and the result of its check.
- A snapshot that fails its check is kept as `*.db.bad`. A restore never uses it, and it raises the alert `backup_check_failed`.

To take a snapshot at any time, with the services running, run `sudo hotclock backup`.

### Off-box copies {#off-box-copies}

A snapshot on the same disk does not protect you if the disk fails. Copy these paths to another computer with the backup tool you use:

- `/etc/hotclock`
- `/var/lib/hotclock/backup`
- `/var/lib/hotclock/media`
- `/var/lib/hotclock/secrets.toml`

Leave these out:

- `library.db*` and `stations/*/state.db*`. The live databases are in write-ahead mode. Do not copy them while the services run.
- `upload-tmp` and `inbox`.

A tool that stores each file once, such as restic, copies the media only once, because the media store never changes a file after it writes it. The first copy of a large library takes a long time. Start it on a quiet night.

## Restore a backup {#restore}

**Warning:** A restore puts the box back to its state at the time of the snapshot. Anything done after it (new imports, edits, history) is gone from the databases. Imported media files stay.

1. Restore the files from your backup: `/etc/hotclock`, `/var/lib/hotclock/backup`, `/var/lib/hotclock/secrets.toml` and the media.
2. Run `sudo hotclock-restore --dry-run`. It lists what it will restore, and changes nothing.
3. Run `sudo hotclock-restore`. It restores the newest snapshot of each database. For the snapshot of another day, add `--date`.

```
sudo hotclock-restore --dry-run      # lists what would be restored, and changes nothing
sudo hotclock-restore                # restores the newest snapshot of each database
sudo hotclock-restore --date 20261105   # a snapshot of another day
```

The restore does these things:

1. It stops the units that are running.
2. It moves the live database files aside (`*.replaced-<time>`).
3. It copies the snapshots in.
4. It restores the counter that tells each station how many of the library's changes it already applied.
5. It starts the units again.

The stations resume with their playlists, queue position and quarantine list as they were at the time of the snapshot. Then they find that the library has changed, and they continue.

## Remove Hotclock {#removing}

`sudo apt remove hotclock` stops the units and removes the package's files. `purge` does not touch `/etc/hotclock` and `/var/lib/hotclock`, on purpose. If you want them gone, delete them by hand.
