#!/bin/sh
# Upgrade a running box without surprises. Installing a package never restarts a station; this script
# does it deliberately, in order, and stops at the first one that does not come back healthy.
#
#   sudo upgrade.sh install NEW.deb    keep the package, take snapshots, install it (nothing restarts)
#   sudo upgrade.sh restart            the library first, then each station one at a time, health-checked
#   sudo upgrade.sh rollback           install the previously kept package and restart the same way
#
# Every package installed through install.sh or this script is kept in /var/lib/hotclock/debs.
set -eu
[ "$(id -u)" -eq 0 ] || { echo "upgrade.sh: run it as root (sudo)" >&2; exit 1; }
debs=/var/lib/hotclock/debs

health_ok() { # port
    n=0
    while [ $n -lt 300 ]; do
        [ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$1/health" || true)" = "200" ] && return 0
        n=$((n + 1))
        sleep 0.1
    done
    return 1
}
station_port() { # name
    sed -n 's/^listen *= *"[^"]*:\([0-9]*\)".*/\1/p' "/etc/hotclock/stations/$1.toml" | head -n 1
}
restart_all() {
    lib_port=$(sed -n 's/^listen *= *"[^"]*:\([0-9]*\)".*/\1/p' /etc/hotclock/hotclock.toml | head -n 1)
    lib_port=${lib_port:-8300}
    if systemctl is-active --quiet hotclock-library.service; then
        echo "restarting the library service"
        systemctl restart hotclock-library.service
        health_ok "$lib_port" || { echo "the library service is not healthy: stopping here" >&2; exit 1; }
    fi
    for u in $(systemctl list-units --plain --no-legend --state=active 'hotclock-station@*.service' | awk '{print $1}'); do
        name=${u#hotclock-station@}
        name=${name%.service}
        port=$(station_port "$name")
        echo "restarting $u (port $port)"
        systemctl restart "$u"
        health_ok "$port" || { echo "$u is not healthy: stopping here; the other stations still run the old binary" >&2; exit 1; }
    done
    # the status display is a client of the stations: restart it last, so it runs the new binary too (it keeps its view)
    if systemctl is-active --quiet hotclock-display.service; then
        echo "restarting hotclock-display.service"
        systemctl restart hotclock-display.service || echo "warning: the display did not restart (the stations are unaffected)" >&2
    fi
    echo "done: hotclock $(dpkg-query -W -f='${Version}' hotclock) is running everywhere"
}

case "${1:-}" in
install)
    pkg="${2:-}"
    [ -f "$pkg" ] || { echo "usage: upgrade.sh install NEW.deb" >&2; exit 2; }
    mkdir -p "$debs"
    cp -n "$pkg" "$debs/" 2>/dev/null || true
    /usr/bin/hotclock backup || echo "warning: the pre-upgrade snapshot failed" >&2
    dpkg -i "$pkg"
    ;;
restart)
    restart_all
    ;;
rollback)
    cur=$(dpkg-query -W -f='${Version}' hotclock)
    prev=$(ls -1t "$debs"/hotclock_*.deb 2>/dev/null | while read -r f; do
        v=$(dpkg-deb -f "$f" Version)
        [ "$v" != "$cur" ] && { echo "$f"; break; }
    done)
    [ -n "$prev" ] || { echo "no earlier package in $debs" >&2; exit 1; }
    echo "installing $prev"
    dpkg -i "$prev"
    restart_all
    ;;
*)
    echo "usage: upgrade.sh install NEW.deb | restart | rollback" >&2
    exit 2
    ;;
esac
