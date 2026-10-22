#!/bin/sh
# The package acceptance run (M1.8 acceptance 1 and 2, and the M1.6 systemd-side checks), on a box with the
# package installed and the units not yet started: a container with systemd (dev/systemd-smoke.sh), or the
# target box. It writes a station, imports six generated tones, and measures on the real units:
#   1. time from `systemctl start` to the first item on air (limit 3 s)
#   2. `kill -9` of the station: time to sound again, and that systemd restarted it once
#   3. an engine stall (watchdog): time to the new process being on air again (limit 13 s)
#   4. a restore: delete both databases, `hotclock-restore`, the station resumes with its playlist
#   5. the units' sandboxing holds (no errors in the journal), the journal cap file, the udev rule parses
# DEVICE is `null` by default (a paced output with no card); on the target box give the real card: DEVICE=hw:CARD=PCH,DEV=0
# It leaves the box as it found it apart from the imported tones, the two "Acceptance" playlists and the
# snapshots: the station's toml has only the keys the test needs set (every other line, key and comment is kept)
# and is put back as it was at the end, and the station's state.db is put back too, so the station comes back
# on the playlist it was on (or, on a box that had no state yet, on its own `default_playlist`) and not on the
# test tones. Resuming the last playlist after a restart is deliberate (architecture 1.7); this script is what
# makes sure the last playlist is not one that only existed for the test.
#   RESTORE=0        leave the test toml and the test state in place (the old behavior)
#   LEAVE_STOPPED=1  leave the station stopped at the end (default: restarted, as it was left before)
set -eu
DEVICE="${DEVICE:-null}"
PORT="${PORT:-8301}"
NAME="${NAME:-office}"
RESTORE="${RESTORE:-1}"
LEAVE_STOPPED="${LEAVE_STOPPED:-0}"
CONF="/etc/hotclock/stations/$NAME.toml"
STATE="/var/lib/hotclock/stations/$NAME"
URL="http://127.0.0.1:$PORT"
fails=0
say() { printf '%s\n' "$*"; }
pass() { say "PASS  $*"; }
fail() { say "FAIL  $*"; fails=$((fails + 1)); }
now() { date +%s.%N; }
elapsed() { awk "BEGIN{printf \"%.2f\", $2 - $1}"; }
lt() { awk "BEGIN{exit !($1 < $2)}"; }
hc() { runuser -u hotclock -- /usr/bin/hotclock "$@"; }
on_air() { curl -fs "$URL/api/v1/now" 2>/dev/null | grep -q '"title"' ; }
# Five starts within 120 s put a station in safe mode on purpose (1.5); this script restarts it more often than
# that, so it clears the start counter before each deliberate restart.
reset_starts() { rm -f "/run/hotclock/$NAME.starts"; }
pid() { systemctl show -p MainPID --value "hotclock-station@$NAME.service"; }
wait_air() { # seconds limit -> prints elapsed since $1 or fails
    t0="$1"; limit="$2"
    while :; do
        if on_air; then elapsed "$t0" "$(now)"; return 0; fi
        lt "$(elapsed "$t0" "$(now)")" "$limit" || { elapsed "$t0" "$(now)"; return 1; }
        sleep 0.05
    done
}

say "== hotclock acceptance on $(hostname), $(uname -m), device $DEVICE, $(hotclock --version)"

# ---- configuration and content
# Set one top-level key in the station's toml, keeping every other line. A key already there (before the first
# [table]) is replaced in place; a missing one is added at the end of the top-level part. Commented examples
# and the tables are left alone. $3 is the TOML text of the value.
set_key() {
    awk -v key="$2" -v val="$3" '
        function emit() { if (!done) { print key " = " val; done = 1 } }
        /^[ \t]*\[/ { emit(); intable = 1 }
        !intable && $0 ~ "^[ \t]*" key "[ \t]*=" { if (!done) { print key " = " val; done = 1 }; next }
        { print }
        END { emit() }' "$1" > "$1.new" && cat "$1.new" > "$1" && rm -f "$1.new"
}
saved=$(mktemp -d)
had_conf=0; had_state=0
if systemctl is-active --quiet "hotclock-station@$NAME.service"; then
    echo "hotclock-station@$NAME is running; acceptance.sh starts and kills it, so it needs a station that is not on air." >&2
    echo "Stop it first (systemctl stop hotclock-station@$NAME), or set NAME= to another station." >&2
    exit 2
fi
if [ -f "$CONF" ]; then had_conf=1; cp -p "$CONF" "$saved/conf.toml"; fi
if [ -f "$STATE/state.db" ]; then
    had_state=1
    mkdir -p "$saved/state"
    cp -p "$STATE"/state.db* "$saved/state/"
fi
[ -f "$CONF" ] || : > "$CONF"
set_key "$CONF" name "\"$NAME\""
set_key "$CONF" listen "\"0.0.0.0:$PORT\""
set_key "$CONF" device "\"$DEVICE\""
set_key "$CONF" default_playlist '"Acceptance"'
set_key "$CONF" emergency_playlist '"Acceptance emergency"'
tmp=$(mktemp -d)
chmod 755 "$tmp"
i=0
for hz in 440 550 660 770 880 990; do
    i=$((i + 1))
    hotclock render --tone "$hz" --seconds 40 --level -20 --out "$tmp/tone$i.wav" > /dev/null
done
chmod 644 "$tmp"/*.wav
hc import "$tmp" > /dev/null
hc playlist create Acceptance --no-repeat-minutes 0 > /dev/null
hc playlist create "Acceptance emergency" --mode sequential --no-repeat-minutes 0 > /dev/null
sqlite3 /var/lib/hotclock/library.db "select id from items" > "$tmp/ids" 2>/dev/null || true
if [ -s "$tmp/ids" ]; then
    hc playlist add Acceptance $(cat "$tmp/ids") > /dev/null
    hc playlist add "Acceptance emergency" $(head -n 2 "$tmp/ids") > /dev/null
else
    # No sqlite3 on the box: item ids 1 to 6 are the ones just imported into a fresh library.
    hc playlist add Acceptance 1 2 3 4 5 6 > /dev/null
    hc playlist add "Acceptance emergency" 1 2 > /dev/null
fi
rm -rf "$tmp"

# ---- 1. audio within 3 s of the unit starting
systemctl enable --now hotclock-library.service
sleep 1
t0=$(now)
systemctl start "hotclock-station@$NAME.service"
if s=$(wait_air "$t0" 3); then pass "1. first item on air $s s after systemctl start (limit 3 s)"; else fail "1. no item on air after $s s (limit 3 s)"; fi
sleep 2
code=$(curl -s -o /dev/null -w '%{http_code}' "$URL/health" || true)
[ "$code" = "200" ] && pass "1. /health answers 200" || fail "1. /health answered $code"
[ -f "/run/hotclock/$NAME.starts" ] && pass "1. the start counter is in /run/hotclock (RuntimeDirectory works under ProtectSystem=strict)" || fail "1. /run/hotclock/$NAME.starts is missing"
if curl -fs "$URL/health" | grep -q '"snapshot_check"'; then pass "1. /health carries the db snapshot fields"; else fail "1. /health lacks db.snapshot_check"; fi

# ---- 2. kill -9
reset_starts
before=$(pid)
t0=$(now)
systemctl kill -s KILL --kill-whom=main "hotclock-station@$NAME.service"
n=0
while [ "$(pid)" = "$before" ] || [ "$(pid)" = "0" ]; do n=$((n + 1)); [ $n -gt 200 ] && break; sleep 0.05; done
if s=$(wait_air "$t0" 15); then pass "2. kill -9: sound again $s s later, pid $before -> $(pid)"; else fail "2. kill -9: not on air after $s s"; fi
nr=$(systemctl show -p NRestarts --value "hotclock-station@$NAME.service")
[ "$nr" -ge 1 ] && pass "2. systemd counted the restart (NRestarts $nr)" || fail "2. NRestarts is $nr"
sleep 2

# ---- 3. the watchdog
mkdir -p "/etc/systemd/system/hotclock-station@$NAME.service.d"
printf '[Service]\nEnvironment=HOTCLOCK_TEST_HOOKS=1\n' > "/etc/systemd/system/hotclock-station@$NAME.service.d/hooks.conf"
systemctl daemon-reload
reset_starts
systemctl restart "hotclock-station@$NAME.service"
wait_air "$(now)" 10 > /dev/null || true
sleep 3
reset_starts
before=$(pid)
t0=$(now)
hc ctl "$NAME" hook stall 60000
n=0
while [ "$(pid)" = "$before" ]; do n=$((n + 1)); [ $n -gt 400 ] && break; sleep 0.05; done
s_gone=$(elapsed "$t0" "$(now)")
if s=$(wait_air "$t0" 20); then
    if lt "$s" 13; then pass "3. watchdog: the stalled process was replaced after $s_gone s and sound was back $s s after the stall (limit 13 s)"; else fail "3. watchdog: sound back only $s s after the stall (limit 13 s)"; fi
else fail "3. watchdog: not on air after $s s"; fi
rm -rf "/etc/systemd/system/hotclock-station@$NAME.service.d"
systemctl daemon-reload
reset_starts
systemctl restart "hotclock-station@$NAME.service"
wait_air "$(now)" 10 > /dev/null || true
sleep 2

# ---- 4. restore
# The playlist on air now is what the restored station must resume: "Acceptance" on a station with no state of its
# own, or the playlist a station with a history was already on (it resumes that one, never default_playlist).
want=$(curl -fs "$URL/health" | sed -n 's/.*"program":{[^}]*"playlist":"\([^"]*\)".*/\1/p')
[ -n "$want" ] || want="Acceptance"
hc backup > /dev/null
ls /var/lib/hotclock/backup | grep -q '^library-.*\.db$' && pass "4. snapshots exist: $(ls /var/lib/hotclock/backup | grep '\.db$' | tr '\n' ' ')" || fail "4. no snapshots in /var/lib/hotclock/backup"
systemctl stop "hotclock-station@$NAME.service" hotclock-library.service
rm -f /var/lib/hotclock/library.db* "/var/lib/hotclock/stations/$NAME/state.db" "/var/lib/hotclock/stations/$NAME/state.db-wal" "/var/lib/hotclock/stations/$NAME/state.db-shm"
[ ! -e /var/lib/hotclock/library.db ] && pass "4. both databases deleted"
reset_starts
t0=$(now)
if hotclock-restore > /tmp/restore.log 2>&1; then pass "4. hotclock-restore ran"; else fail "4. hotclock-restore failed: $(tail -3 /tmp/restore.log)"; fi
systemctl is-active --quiet hotclock-library.service && pass "4. the library service is back" || systemctl start hotclock-library.service
systemctl is-active --quiet "hotclock-station@$NAME.service" || systemctl start "hotclock-station@$NAME.service"
if s=$(wait_air "$t0" 20); then pass "4. after the restore the station is on air ($s s)"; else fail "4. after the restore the station is not on air"; fi
items=$(curl -fs "http://127.0.0.1:8300/health" | sed -n 's/.*"items":\([0-9]*\).*/\1/p')
[ "${items:-0}" -ge 6 ] && pass "4. the restored library has $items items" || fail "4. the restored library has ${items:-0} items"
hcode=$(curl -s -o /tmp/health.json -w '%{http_code}' "$URL/health" || true)
[ "$hcode" = "200" ] || say "     /health answered $hcode right after the restore: $(sed -n 's/.*"alerts":\(\[.*\]\).*/\1/p' /tmp/health.json | cut -c1-500)"
pl=$(sed -n 's/.*"program":{[^}]*"playlist":"\([^"]*\)".*/\1/p' /tmp/health.json)
[ -n "$pl" ] || say "     health: $(cut -c1-600 /tmp/health.json)"
[ "$pl" = "$want" ] && pass "4. the station resumed the playlist \"$pl\"" || fail "4. the station's playlist is \"$pl\" (wanted \"$want\")"

# ---- 5. the rest
errs=$(journalctl -p err -u 'hotclock-*' --since '-20min' --no-pager -q 2>/dev/null | grep -v 'Watchdog\|watchdog\|killed\|Main process exited\|Failed with result' | wc -l)
[ "$errs" -eq 0 ] && pass "5. no error-level lines in the journal (apart from the injected faults)" || { fail "5. $errs error-level journal lines"; journalctl -p err -u 'hotclock-*' --since '-20min' --no-pager -q | tail -8; }
[ -f /etc/systemd/journald.conf.d/hotclock.conf ] && pass "5. the journal cap is installed" || fail "5. no journald drop-in"
if command -v udevadm > /dev/null 2>&1; then
    if udevadm verify /lib/udev/rules.d/60-hotclock-usb-audio.rules > /dev/null 2>&1; then pass "5. the udev rule verifies"; else fail "5. the udev rule does not verify"; fi
fi
own=$(stat -c '%U:%G %a' /var/lib/hotclock "/var/lib/hotclock/stations/$NAME" | tr '\n' ' ')
say "     ownership: $own"

# ---- put the box back
if [ "$RESTORE" = "1" ]; then
    systemctl stop "hotclock-station@$NAME.service" || true
    if [ "$had_conf" = "1" ]; then
        cp -p "$saved/conf.toml" "$CONF"
        say "     $CONF put back as it was ($(wc -l < "$CONF") lines)"
    fi
    rm -f "$STATE"/state.db "$STATE"/state.db-wal "$STATE"/state.db-shm
    if [ "$had_state" = "1" ]; then
        cp -p "$saved"/state/state.db* "$STATE/"
        say "     the station's state.db put back: it resumes the playlist it was on before the test"
    else
        say "     the station had no state before the test: it starts on its default_playlist"
    fi
    if [ "$LEAVE_STOPPED" != "1" ]; then
        reset_starts
        systemctl start "hotclock-station@$NAME.service" || true
    fi
fi
rm -rf "$saved"
say "== $fails failure(s)"
[ "$fails" -eq 0 ]
