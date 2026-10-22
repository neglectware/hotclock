# Command line

This page lists the commands of the `hotclock` program, what each one does, and the options you need most. The API can do everything that the web pages do, and a few jobs are done only here.

## Get help on a command {#get-help}

- For the list of commands, run `hotclock --help`.
- For the options of one command, run `hotclock <command> --help`.
- To use a different global file, add the global option `--global-config <file>`. The default is `/etc/hotclock/hotclock.toml`.
- Run commands that touch the databases of a running box as the `hotclock` user: `sudo -u hotclock hotclock <command>`.

## Run the services {#running-services}

| Command | What it does |
|---|---|
| `hotclock library` | The library service: the web pages and the API, the notifier and the inbox watcher. `hotclock-library.service` usually starts it. |
| `hotclock station <name>` | One station, from `/etc/hotclock/stations/<name>.toml`. `hotclock-station@<name>.service` usually starts it. `--device` overrides the output. `--stream-test` runs the station with a timing tone in place of its music, to measure the network stream. It needs `[stream]`. |
| `hotclock display` | The native status display. `--render-png out.png --size 1024x600 --view split --feed demo:both` draws one frame of a built-in feed into a picture instead, with no screen. |

## Daily jobs

| Command | What it does |
|---|---|
| `hotclock ctl <station> ack <code>` | Acknowledges an alert, for example `safe_mode`. |
| `hotclock ctl <station> release <item id>` | Releases an item from quarantine. |
| `hotclock ctl <station> quarantine <item id> <reason>` | Puts an item in quarantine by hand. |
| `hotclock backup` | Writes a snapshot of the library database and of every station database now. It is safe to run while the services run. |
| `hotclock-restore [--date YYYYMMDD] [--dry-run]` | Stops the units, puts snapshots back and starts the units. Run it as root. See [Upgrading, backup and restore](upgrading-backup). |
| `hotclock notify --event <code> [--unit <unit>] [--severity <level>] [--message <text>]` | Sends one event through the notification channels, with no library service. systemd runs it when the library service fails. |

## Library and playlists

| Command | What it does |
|---|---|
| `hotclock import <files or folders...>` | Imports files, folders (with all their subfolders) or `.m3u` lists. `--category <name>` sets the category. `--workers <n>` sets the threads (default 2). |
| `hotclock import-plex` | Copies Plex playlists into the library. Options: `--list`, `--playlist "Name"` (repeat it for several), `--as`, `--mode shuffle`, `--category`, `--dry-run`, `--no-art`, and `--fill-art` with `--all-plex-items` or `--playlist`. `--token-file` names the file with the token. See [Plex tools](plex-tools). |
| `hotclock playlist list` | Lists the playlists, with the no-repeat window that each one can keep. |
| `hotclock playlist create <name> [--mode shuffle\|sequential] [--no-repeat-minutes N]` | Creates a playlist. |
| `hotclock playlist add <name> <item ids...>` | Adds items to a playlist. |
| `hotclock playlist insert-every <name> --n <N> --category <cat> [--min-gap-minutes M]` | Adds an insert rule. |
| `hotclock analyze <file> [--profile music\|liner\|ambient\|longform]` | Analyzes one file and prints its loudness, markers and flags as JSON. It stores nothing. |
| `hotclock reanalyze --stale` | Analyzes again every item that an older version of the analysis made, from its stored file. Your markers and other overrides stay. It does not import anything again. `--all` analyzes every item. Or give item ids. See [Library](library#analyze-again). |
| `hotclock census <folder or file> [--json]` | Lists the container, codec, rate, channels, decode result, tags and artwork of every file in a folder. Use it before you import the folder. |

## Sound card checks

| Command | What it does |
|---|---|
| `hotclock station test --tone [--device hw:CARD=<id>,DEV=0] [--seconds N]` | Plays a test tone to a card. Use it to check a card before you make a station with it. |
| `hotclock loopcheck --input hw:CARD=<id>,DEV=0 --tone-out hw:CARD=<id>,DEV=0 --seconds N` | Records the output of one card through the input of another card, and reports dropouts. It needs a cable from the output to the input. |
| `hotclock render --tone <hz> --seconds N --out file.wav` | Renders a test tone to a WAV file. |
| `hotclock play <files...>` | Plays files one after another through an output, with no transitions. It is a harness to check an output. |

## Test tools

`soak`, `soak-churn`, `soak-watch` and `soak-report` run and judge the long acceptance soaks. `plex-replay` and `ramp-fit` check the transition model against recorded data. These commands are for testing the program, not for running a station.
