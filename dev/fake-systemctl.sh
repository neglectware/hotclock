#!/bin/sh
# A stand-in for systemctl in tests and UI checks (HOTCLOCK_SYSTEMCTL): `start`, `stop`, `restart` and `is-active` of
# hotclock-station@<name>.service run and stop a real `hotclock station <name>` process; other units are only logged.
#   FAKE_SC_DIR      state directory (pid files, log `systemctl.log`)           required
#   FAKE_SC_BIN      the hotclock binary                                        required
#   FAKE_SC_GLOBAL   the global config passed as --global-config                required
#   FAKE_SC_FAIL     space separated `verb:name` pairs that fail (e.g. `start:late`); also read from the file $FAKE_SC_DIR/fail
#   FAKE_SC_HANG     space separated names whose `start` succeeds but whose process never answers (a `sleep`); also from $FAKE_SC_DIR/hang
d="${FAKE_SC_DIR:?}"
mkdir -p "$d"
echo "$*" >> "$d/systemctl.log"
verb="$1"; unit="${2:-}"
name=$(printf %s "$unit" | sed -n 's/^hotclock-station@\(.*\)\.service$/\1/p')
[ -n "$name" ] || { [ "$verb" = is-active ] && { echo inactive; exit 3; }; exit 0; }
pidf="$d/$name.pid"
FAKE_SC_FAIL="${FAKE_SC_FAIL:-} $(cat "$d/fail" 2>/dev/null)"
FAKE_SC_HANG="${FAKE_SC_HANG:-} $(cat "$d/hang" 2>/dev/null)"
alive() { [ -f "$pidf" ] && kill -0 "$(cat "$pidf")" 2>/dev/null; }
case " ${FAKE_SC_FAIL:-} " in *" $verb:$name "*) echo "Job for $unit failed (fake)" >&2; exit 1 ;; esac
do_stop() { if alive; then p=$(cat "$pidf"); kill "$p" 2>/dev/null; for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do kill -0 "$p" 2>/dev/null || break; sleep 0.25; done; kill -9 "$p" 2>/dev/null; fi; rm -f "$pidf"; }
do_start() {
    alive && return 0
    case " ${FAKE_SC_HANG:-} " in
    *" $name "*) setsid sleep 1000 >/dev/null 2>&1 & echo $! > "$pidf" ;;
    *) setsid "${FAKE_SC_BIN:?}" --global-config "${FAKE_SC_GLOBAL:?}" station "$name" --rt-priority 0 >> "$d/$name.log" 2>&1 < /dev/null & echo $! > "$pidf" ;;
    esac
}
case "$verb" in
start) do_start ;;
stop) do_stop ;;
restart) do_stop; do_start ;;
is-active) if alive; then echo active; exit 0; else echo inactive; exit 3; fi ;;
esac
exit 0
