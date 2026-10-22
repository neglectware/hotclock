#!/bin/sh
# A real-time soak of the real processes on this machine (Mac or Linux), no sound card and no systemd: the
# library service and a station on the paced `null` output run for hours, with the clients of a long soak
# against them (`hotclock soak-churn`: status polling, WebSocket sessions that come, stall and vanish, an
# operator pressing next) and a random fault schedule (an xrun-making stall, the card unplugged, the media
# store failing, state.db read-only, `kill -9` of the station and of the library, a plain restart). It is the
# hours-long counterpart of the accelerated soak (`hotclock soak`), which runs 30 virtual days on a virtual
# clock, and of the target box's 72-hour hardware soak (`dev/soak-hardware.sh`), which adds the real card.
#
#   dev/soak-realtime.sh start [HOURS] [SET_DIR]     default 8 hours on $HOTCLOCK_TESTDATA/office
#   dev/soak-realtime.sh status
#   dev/soak-realtime.sh stop                        ends it and writes the report
#
# Everything lives under $TMPDIR/hotclock-soak/rt/ (not under hotclock-tests/, whose old directories the test suites sweep) and is removed by `clean`. The library is a copy of the
# set (about the size of the set); logs are capped (the watch file grows by a line per process per minute).
set -eu
cd "$(dirname "$0")/.."
BIN="${HOTCLOCK_BIN:-$PWD/target/release/hotclock}"
BASE="${TMPDIR:-/tmp}/hotclock-soak/rt"
BASE="${BASE%/}"
D="$BASE"
ST_PORT="${ST_PORT:-28301}"
LIB_PORT="${LIB_PORT:-28300}"
FAULT_EVERY="${FAULT_EVERY:-1200}"     # mean seconds between injections

say() { printf '%s  %s\n' "$(date -u +%H:%M:%S)" "$*"; }
now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }
hc() { "$BIN" --global-config "$D/hotclock.toml" "$@"; }
on_air() { curl -fs "http://127.0.0.1:$ST_PORT/api/v1/now" 2>/dev/null | grep -q '"title"'; }
log_fault() { # kind phase [recovery_s] [detail]
    if [ -n "${3:-}" ]; then
        printf '{"t_ms":%s,"kind":"%s","station":"office","phase":"%s","recovery_s":%s,"detail":"%s"}\n' "$(now_ms)" "$1" "$2" "$3" "${4:-}" >> "$D/faults.log"
    else
        printf '{"t_ms":%s,"kind":"%s","station":"office","phase":"%s","detail":"%s"}\n' "$(now_ms)" "$1" "$2" "${4:-}" >> "$D/faults.log"
    fi
}
start_station() {
    HOTCLOCK_TEST_HOOKS=1 nohup "$BIN" --global-config "$D/hotclock.toml" station office --rt-priority 0 >> "$D/station.log" 2>&1 &
    echo $! > "$D/station.pid"
}
start_library() {
    nohup "$BIN" --global-config "$D/hotclock.toml" library >> "$D/library.log" 2>&1 &
    echo $! > "$D/library.pid"
}
wait_air() { # t0 -> seconds
    t0=$1
    n=0
    while [ $n -lt 1200 ]; do
        if on_air; then python3 -c "import time; print(round(time.time()-$t0,1))"; return 0; fi
        n=$((n + 1)); sleep 0.1
    done
    echo 999
}
# A fresh start counter (five starts in 120 s is safe mode, on purpose): the faults here restart the station
# far less often than that, but a run of bad luck must not end the soak.
clear_starts() { rm -f "$D/run/office.starts"; }

supervise() {
    end=$(python3 -c "import time; print(int(time.time() + $1*3600))")
    seed=$$
    while [ "$(date +%s)" -lt "$end" ]; do
        wait_s=$(python3 -c "import random; random.seed($seed); print(int(random.expovariate(1.0/$FAULT_EVERY)) + 60)")
        seed=$((seed + 1))
        i=0
        while [ $i -lt "$wait_s" ] && [ "$(date +%s)" -lt "$end" ]; do sleep 5; i=$((i + 5)); done
        [ "$(date +%s)" -lt "$end" ] || break
        pick=$(python3 -c "import random; random.seed($seed); print(random.randint(0,7))")
        seed=$((seed + 1))
        t0=$(python3 -c 'import time; print(time.time())')
        case "$pick" in
        0) log_fault stall inject "" "rt-out stalled 600 ms"; hc ctl office hook stall 600 > /dev/null; sleep 3
           log_fault stall recovered "$(wait_air "$t0")" "" ;;
        1|2) log_fault unplug inject "" "output card away 8 s"; hc ctl office hook unplug > /dev/null; sleep 8
           t1=$(python3 -c 'import time; print(time.time())'); hc ctl office hook plug > /dev/null
           log_fault unplug recovered "$(wait_air "$t1")" "seconds from plugging back to sound" ;;
        3) log_fault media_fail inject "" "media store failing 6 s"; hc ctl office hook media-fail on > /dev/null; sleep 6
           t1=$(python3 -c 'import time; print(time.time())'); hc ctl office hook media-fail off > /dev/null
           log_fault media_fail recovered "$(wait_air "$t1")" "" ;;
        4) log_fault db_fault inject "" "state.db read-only 40 s"; hc ctl office hook db-fault readonly > /dev/null; sleep 40
           hc ctl office hook db-fault none > /dev/null; log_fault db_fault recovered 0 "" ;;
        5|6) clear_starts; log_fault kill9 inject "" "kill -9 of the station"; kill -9 "$(cat "$D/station.pid")" 2>/dev/null || true
           sleep 1; start_station; log_fault kill9 recovered "$(wait_air "$t0")" "seconds from the kill to sound" ;;
        7) log_fault kill9_library inject "" "kill -9 of the library service"; kill -9 "$(cat "$D/library.pid")" 2>/dev/null || true
           sleep 1; start_library; sleep 3
           code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$LIB_PORT/health" || echo 0)
           if [ "$code" = "200" ]; then r=$(python3 -c "import time; print(round(time.time()-$t0,1))"); else r=999; fi
           log_fault kill9_library recovered "$r" "the playing station was not touched" ;;
        esac
    done
}

cmd="${1:-}"
case "$cmd" in
start)
    HOURS="${2:-8}"
    SET="${3:-${HOTCLOCK_TESTDATA:-$HOME/Developer/hotclock-testdata}/office}"
    [ -x "$BIN" ] || { echo "build first: cargo build --release -p hotclock" >&2; exit 1; }
    SRC_BIN="$BIN"
    [ -d "$SET" ] || { echo "no test set at $SET" >&2; exit 1; }
    rm -rf "$D"; mkdir -p "$D/data" "$D/stations" "$D/run" "$D/src"
    # The soak runs a copy of the binary, so a rebuild in the checkout does not change what is being soaked.
    cp "$SRC_BIN" "$D/hotclock"; BIN="$D/hotclock"
    cat > "$D/hotclock.toml" <<EOT
data_dir = "$D/data"
stations_dir = "$D/stations"
run_dir = "$D/run"
secrets_file = "$D/secrets.toml"
[library]
listen = "127.0.0.1:$LIB_PORT"
EOT
    cat > "$D/stations/office.toml" <<EOT
name = "office"
listen = "127.0.0.1:$ST_PORT"
device = "null"
default_playlist = "Office"
emergency_playlist = "Emergency"
EOT
    say "importing the set (copy under $D/data)"
    hc import "$SET" --workers 4 > "$D/import.log" 2>&1
    for i in 1 2 3; do "$BIN" render --tone $((900 + 100 * i)) --seconds 8 --out "$D/src/liner$i.wav" > /dev/null; done
    for i in 1 2; do "$BIN" render --tone $((400 + 100 * i)) --seconds 90 --out "$D/src/emerg$i.wav" > /dev/null; done
    hc import "$D/src/liner1.wav" "$D/src/liner2.wav" "$D/src/liner3.wav" --category liner > /dev/null
    hc import "$D/src/emerg1.wav" "$D/src/emerg2.wav" > /dev/null
    music=$(sqlite3 "$D/data/library.db" "select id from items where category_id=(select id from categories where name='music') order by id" | tr '\n' ' ')
    emerg=$(echo "$music" | awk '{print $(NF-1), $NF}')
    hc playlist create Office --no-repeat-minutes 60 > /dev/null
    office_ids=$(echo "$music" | awk '{for (i = 1; i <= NF - 2; i++) printf "%s ", $i}')
    hc playlist add Office $office_ids > /dev/null
    hc playlist insert-every Office --n 4 --category liner > /dev/null
    hc playlist create Emergency --mode sequential --no-repeat-minutes 0 > /dev/null
    hc playlist add Emergency $emerg > /dev/null
    start_library
    sleep 2
    start_station
    n=0; until on_air || [ $n -ge 100 ]; do n=$((n + 1)); sleep 0.2; done
    on_air || { echo "the station is not on air; see $D/station.log" >&2; exit 1; }
    start_ms=$(now_ms)
    printf '{"start_ms": %s, "hours": %s, "data_dir": "%s", "library_port": %s, "loopcheck": false, "required_faults": ["unplug", "kill9"], "stations": [{"name":"office","port":%s}]}\n' "$start_ms" "$HOURS" "$D/data" "$LIB_PORT" "$ST_PORT" > "$D/manifest.json"
    nohup "$BIN" soak-watch --out "$D" --station office=$ST_PORT --library "$LIB_PORT" --pidfile office="$D/station.pid" --pidfile library="$D/library.pid" --interval 60 --hours "$HOURS" > "$D/watch.log" 2>&1 &
    echo $! > "$D/watch.pid"
    nohup "$BIN" soak-churn --station "127.0.0.1:$ST_PORT" --library "127.0.0.1:$LIB_PORT" --report "$D/churn.jsonl" --seconds "$(python3 -c "print(int($HOURS*3600))")" > "$D/churn.log" 2>&1 &
    echo $! > "$D/churn.pid"
    ( supervise "$HOURS"
      say "the soak is over: stopping and writing the report"
      for f in churn watch station library; do [ -f "$D/$f.pid" ] && kill "$(cat "$D/$f.pid")" 2>/dev/null || true; done
      sleep 3
      "$BIN" soak-report --dir "$D" > "$D/report.stdout" 2>&1 || true
      say "report: $D/report.md" ) > "$D/soak.log" 2>&1 &
    echo $! > "$D/supervisor.pid"
    say "started for $HOURS h in $D; look with: $0 status"
    ;;
status)
    [ -d "$D" ] || { echo "no soak"; exit 1; }
    echo "dir: $D"
    tail -n 6 "$D/watch.csv" 2>/dev/null | cut -c1-180
    echo "--- churn"; tail -n 1 "$D/churn.jsonl" 2>/dev/null | cut -c1-400
    echo "--- faults"; cat "$D/faults.log" 2>/dev/null || echo "(none yet)"
    ;;
stop)
    [ -d "$D" ] || { echo "no soak"; exit 1; }
    for f in supervisor churn watch station library; do
        [ -f "$D/$f.pid" ] && kill "$(cat "$D/$f.pid")" 2>/dev/null || true
    done
    sleep 3
    "$BIN" soak-report --dir "$D" > "$D/report.stdout" 2>&1 || true
    tail -n +1 "$D/report.md" 2>/dev/null | head -n 30
    ;;
clean)
    rm -rf "$D"
    ;;
*)
    sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
