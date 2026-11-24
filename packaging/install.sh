#!/bin/sh
# Fresh install on Ubuntu Server 24.04 (amd64): the package, the station configuration, the units, and a
# check that each station is on air. Run as root, from the directory that holds the package.
#
#   sudo ./install.sh hotclock_0.1.0-1_amd64.deb --station office=hw:CARD=PCH,DEV=0:8301 \
#                                                --station eve=hw:CARD=Device,DEV=0:8302
#
# `--station NAME=DEVICE:PORT` writes /etc/hotclock/stations/NAME.toml from the example (an existing file is
# left alone), where DEVICE is an ALSA name (`hw:CARD=<id>,DEV=0`) or `null`. Without any `--station` only the
# package is installed. `--stream NAME` adds a `[stream]` block (the network stream output, docs/obs-stream.md)
# to the station file it writes; use it with `NAME=null:PORT` for a station with no sound card. `--no-start`
# stops before enabling the units. On a box whose library has no playlist yet a station is neither written nor
# started (it would have nothing to play and restart until safe mode): import music, make the playlist, run it again.
set -eu
[ "$(id -u)" -eq 0 ] || { echo "install.sh: run it as root (sudo)" >&2; exit 1; }
pkg="${1:-}"
[ -f "$pkg" ] || { echo "usage: install.sh PACKAGE.deb [--station NAME=DEVICE:PORT]... [--stream NAME]... [--no-start]" >&2; exit 2; }
shift
stations=""
streams=""
start=1
while [ $# -gt 0 ]; do
    case "$1" in
    --station) stations="$stations $2"; shift 2 ;;
    --stream) streams="$streams $2"; shift 2 ;;
    --no-start) start=0; shift ;;
    *) echo "install.sh: unknown argument $1" >&2; exit 2 ;;
    esac
done

apt-get update -qq
apt-get install -y ./"$pkg"
mkdir -p /var/lib/hotclock/debs
cp -n "$pkg" /var/lib/hotclock/debs/ 2>/dev/null || true

# A station needs something to put on air: a playlist in the library, or a program it resumes from its own state.
# Without either it exits at start ("there is no playlist to run") and Restart=always runs it into safe mode. Its
# file is what starts it (the generator starts every station with a file, at boot too), so on such a box the file is
# not written and the station is not started; the message says what to do first.
has_program() { # station name
    [ -s "/var/lib/hotclock/stations/$1/state.db" ] && return 0
    runuser -u hotclock -- /usr/bin/hotclock playlist list 2>/dev/null | grep -qE '^ *[0-9]+  '
}
skipped=""
not_ready() { # station name, what was done
    skipped="$skipped $1"
    echo "$1: the library has no playlist yet, so the station would have nothing to play: $2." >&2
    echo "  Import music (http://<box>:8300/, Import), make a playlist (its default_playlist), then run this" >&2
    echo "  install.sh again with the same --station to write and start it (or make it on the Stations page)." >&2
}
for s in $stations; do
    name=${s%%=*}
    rest=${s#*=}
    port=${rest##*:}
    dev=${rest%:*}
    f=/etc/hotclock/stations/$name.toml
    if [ -e "$f" ]; then
        echo "$f exists: left alone"
        continue
    fi
    if ! has_program "$name"; then
        not_ready "$name" "$f not written, the station not started"
        continue
    fi
    ex=/usr/share/hotclock/examples/$name.toml
    [ -f "$ex" ] || ex=/usr/share/hotclock/examples/office.toml
    sed -e "s|^name = .*|name = \"$name\"|" \
        -e "s|^listen = .*|listen = \"0.0.0.0:$port\"|" \
        -e "s|^device = .*|device = \"$dev\"|" "$ex" > "$f"
    chgrp hotclock "$f" && chmod 664 "$f"
    case " $streams " in
    *" $name "*) printf '\n[stream]\nenabled = true\n' >> "$f"; echo "wrote $f (with [stream])" ;;
    *) echo "wrote $f" ;;
    esac
done
[ "$start" -eq 1 ] || { echo "installed; units not started (--no-start)"; exit 0; }

systemctl enable --now hotclock-library.service
# The status display: on by default (display.enabled), harmless without a monitor. Not `enable`: the generator starts
# it at boot while the switch is on, and its ExecCondition keeps it stopped while it is off.
systemctl start hotclock-display.service || echo "warning: the status display did not start (the stations are unaffected)" >&2
started=""
for s in $stations; do
    name=${s%%=*}
    port=${s##*:}
    case " $skipped " in *" $name "*) continue ;; esac
    # A file that was already there is left alone, and so is its station while it has nothing to play.
    if ! has_program "$name"; then
        not_ready "$name" "/etc/hotclock/stations/$name.toml is there but the station was not started"
        continue
    fi
    started="$started $name"
    t0=$(date +%s.%N)
    systemctl enable --now "hotclock-station@$name.service"
    n=0
    while [ $n -lt 300 ]; do
        code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port/health" || true)
        [ "$code" = "200" ] && break
        n=$((n + 1))
        sleep 0.1
    done
    t1=$(date +%s.%N)
    echo "$name: /health answered $code $(awk "BEGIN{printf \"%.1f\", $t1 - $t0}") s after the start"
done
systemctl --no-pager --lines=0 status hotclock-library.service $(for n in $started; do printf 'hotclock-station@%s.service ' "$n"; done) || true
