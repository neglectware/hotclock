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
# stops before enabling the units.
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
for s in $stations; do
    name=${s%%=*}
    port=${s##*:}
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
systemctl --no-pager --lines=0 status hotclock-library.service $(for s in $stations; do printf 'hotclock-station@%s.service ' "${s%%=*}"; done) || true
