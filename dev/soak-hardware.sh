#!/bin/sh
# The 72-hour hardware soak on the target box (M1.8 acceptance 3): one command to start, one to look, one report.
#
#   sudo soak-hardware.sh start            start it (returns at once; it runs under systemd for 72 h)
#   sudo soak-hardware.sh status           where it is, the last samples, the faults so far
#   sudo soak-hardware.sh report           judge what has been collected (also runs by itself at the end)
#   sudo soak-hardware.sh stop             end it early and write the report
#
# The box has the package installed (packaging/install.sh) with two stations and the owner's office playlist (or a
# stand-in) with a liner every 4 songs. Never on radiobox.
#
# The target box as it is (2026-09-30): the onboard card (PCH) only, no USB dongle (the owner's one dongle is in use on
# radiobox). Eve plays on PCH, and a cable from PCH's line-out to PCH's line-in lets `loopcheck` record
# what actually leaves the box. The office station is card-less (`device = "null"`, the paced Null output),
# standing in for the stream-only office station until M2a's stream exists; no card, so nothing to record,
# and its evidence is its counters and its as-run log. The USB unplug and the second real card are DEFERRED
# until radiobox is replaced (the report lists them as deferred, not failed). With a dongle, the old wiring
# still works: set LOOP_IN_office, UNPLUG_CARD, add `unplug:...` to FAULTS and REQUIRED_FAULTS, and empty
# DEFERRED.
#
# Settings, all optional (environment; the defaults are that wiring):
#   HOURS=72                                   how long
#   STATIONS="office:8301 eve:8302"            NAME:PORT of the stations under test
#   LOOP_IN_office=none                        capture device that records the office station's output
#   LOOP_IN_ita=hw:CARD=PCH,DEV=0              ... and the Eve station's ("none" to skip one)
#   LOOP_MODEL_ita=-4.5,790,3                  what that loop does to the program, GAIN_DB[,HIGHPASS_HZ[,ORDER]]
#                                              (measure it; see docs/hardware-tests.md). Unset: a flat unity
#                                              loop. The report judges loopcheck's gaps through it.
#   LIVE_HEAP_STATIONS=""                      stations that log their live heap (run with the `alloc-count` binary and
#                                              HOTCLOCK_LIVE_HEAP_LOG=/var/lib/hotclock/stations/<name>/live-heap.log);
#                                              the report then judges their live-heap slope, and the end copies the logs
#   LIVE_HEAP_LIBRARY=0                        1: the library service logs its live heap too (the same binary, and
#                                              HOTCLOCK_LIVE_HEAP_LOG=/var/lib/hotclock/library-live-heap.log in a
#                                              drop-in of hotclock-library.service); judged as "library"
#   FAULTS="kill9:eve:20 kill9:office:44 clock_step:eve:68"      kind:station:hour (kinds: kill9, clock_step, unplug)
#   REQUIRED_FAULTS="kill9 clock_step"         kinds the report needs recovered
#   DEFERRED="..."                             what this box cannot run, one item per line (listed in the report)
#   UNPLUG_CARD=Device                         the USB card an `unplug` injection removes (ALSA card id)
#   UNPLUG_S=60                                how long the card stays away
#   DIR_BASE=/var/lib/hotclock-soak            where soak directories go
#   WATCH_INTERVAL=60                          seconds between samples (a dry run uses fewer)
#
# What it collects in <dir> (see `hotclock soak-report` and docs/hardware-tests.md): manifest.json,
# watch.csv (memory, threads, descriptors, CPU, health of every service every minute), one loopcheck-*.jsonl
# per station (gaps, glitches, overruns) with clips of the last 3 s of each gap, faults.log (every injection
# and how long recovery took) and, at the end, report.md.
set -eu

HOURS="${HOURS:-72}"
STATIONS="${STATIONS:-office:8301 eve:8302}"
LOOP_IN_office="${LOOP_IN_office:-none}"
LOOP_IN_ita="${LOOP_IN_ita:-hw:CARD=PCH,DEV=0}"
UNPLUG_CARD="${UNPLUG_CARD:-Device}"
LIVE_HEAP_STATIONS="${LIVE_HEAP_STATIONS:-}"
LIVE_HEAP_LIBRARY="${LIVE_HEAP_LIBRARY:-0}"
UNPLUG_S="${UNPLUG_S:-60}"
FAULTS="${FAULTS:-kill9:eve:20 kill9:office:44 clock_step:eve:68}"
REQUIRED_FAULTS="${REQUIRED_FAULTS:-kill9 clock_step}"
DEFERRED_DEFAULT="USB dongle unplug (device_lost and re-enumeration on a real card): deferred until radiobox is replaced (no spare dongle)
Second real card (a station on a USB card, and loopcheck of one card through another): deferred until radiobox is replaced
The office station runs card-less (the Null output) in place of the stream-only office station of M2a"
DEFERRED="${DEFERRED-$DEFERRED_DEFAULT}"
DIR_BASE="${DIR_BASE:-/var/lib/hotclock-soak}"
HC=/usr/bin/hotclock
SELF="$(readlink -f "$0")"

need_root() { [ "$(id -u)" -eq 0 ] || { echo "soak-hardware.sh: run it as root (sudo)" >&2; exit 1; }; }
now_ms() { date +%s%3N; }
ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }
say() { printf '%s  %s\n' "$(ts)" "$*"; }
latest_dir() { ls -1dt "$DIR_BASE"/20* 2>/dev/null | head -n 1; }
port_of() { # station -> port
    for s in $STATIONS; do [ "${s%%:*}" = "$1" ] && { echo "${s##*:}"; return; }; done
}
loop_in() { eval "echo \"\${LOOP_IN_$1:-none}\""; }
loop_model() { eval "echo \"\${LOOP_MODEL_$1:-}\""; }

on_air() { # port
    curl -fs "http://127.0.0.1:$1/api/v1/now" 2>/dev/null | grep -q '"title"'
}
health_code() { curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$1/health" 2>/dev/null || echo 0; }

# ---- fault log --------------------------------------------------------------------------------------------
log_fault() { # dir kind station phase [recovery_s] [detail]
    d="$1"; kind="$2"; st="$3"; phase="$4"; rec="${5:-}"; detail="${6:-}"
    if [ -n "$rec" ]; then
        printf '{"t_ms":%s,"kind":"%s","station":"%s","phase":"%s","recovery_s":%s,"detail":"%s"}\n' "$(now_ms)" "$kind" "$st" "$phase" "$rec" "$detail" >> "$d/faults.log"
    else
        printf '{"t_ms":%s,"kind":"%s","station":"%s","phase":"%s","detail":"%s"}\n' "$(now_ms)" "$kind" "$st" "$phase" "$detail" >> "$d/faults.log"
    fi
}
# Seconds from $1 (a `date +%s.%N`) until the station is on air again, or a large number after 120 s.
wait_recovered() { # port t0
    while :; do
        if on_air "$1" && [ "$(health_code "$1")" != "0" ]; then
            awk "BEGIN{printf \"%.1f\", $(date +%s.%N) - $2}"
            return 0
        fi
        elapsed=$(awk "BEGIN{print $(date +%s.%N) - $2}")
        awk "BEGIN{exit !($elapsed > 120)}" && { echo 999; return 1; }
        sleep 0.2
    done
}

usb_device_dir() { # ALSA card id -> the sysfs directory of its USB device (the one with `authorized`)
    n=$(sed -n "s/^ *\([0-9]*\) \[$1 *\].*/\1/p" /proc/asound/cards | head -n 1)
    [ -n "$n" ] || return 1
    p=$(readlink -f "/sys/class/sound/card$n/device") || return 1
    while [ "$p" != "/" ] && [ ! -f "$p/authorized" ]; do p=$(dirname "$p"); done
    [ -f "$p/authorized" ] && echo "$p"
}

inject_unplug() { # dir station
    d="$1"; st="$2"; port=$(port_of "$st")
    dev=$(usb_device_dir "$UNPLUG_CARD") || { say "cannot find the USB device of card $UNPLUG_CARD: unplug it by hand now and plug it back after $UNPLUG_S s"; dev=""; }
    log_fault "$d" unplug "$st" inject "" "card $UNPLUG_CARD for $UNPLUG_S s"
    if [ -n "$dev" ]; then
        say "unplug: deauthorizing $dev for $UNPLUG_S s"
        echo 0 > "$dev/authorized"
        sleep "$UNPLUG_S"
        t0=$(date +%s.%N)
        echo 1 > "$dev/authorized"
    else
        sleep "$UNPLUG_S"
        t0=$(date +%s.%N)
    fi
    r=$(wait_recovered "$port" "$t0") || true
    log_fault "$d" unplug "$st" recovered "$r" "seconds from plugging the card back to sound"
    say "unplug: $st recovered $r s after the card came back"
}
inject_kill9() { # dir station
    d="$1"; st="$2"; port=$(port_of "$st")
    log_fault "$d" kill9 "$st" inject "" "kill -9 of the station process"
    t0=$(date +%s.%N)
    systemctl kill -s KILL --kill-whom=main "hotclock-station@$st.service"
    sleep 1
    r=$(wait_recovered "$port" "$t0") || true
    log_fault "$d" kill9 "$st" recovered "$r" "seconds from the kill to sound"
    say "kill -9: $st recovered $r s later"
}
inject_clock_step() { # dir station
    d="$1"; st="$2"; port=$(port_of "$st")
    log_fault "$d" clock_step "$st" inject "" "+10 minutes, back after 2 minutes"
    timedatectl set-ntp false 2>/dev/null || true
    # Time the recovery on the monotonic clock (/proc/uptime): the wall clock is the thing being stepped, so
    # `date` would add the 600 s step to the recovery (the 2026-10-01 Box A soak reported 599.8 s that way).
    t0=$(cut -d' ' -f1 /proc/uptime)
    date -s "@$(( $(date +%s) + 600 ))" > /dev/null
    # Recovered: the station still plays and has raised clock_jump.
    n=0
    while [ $n -lt 100 ]; do
        curl -s "http://127.0.0.1:$port/health" | grep -q 'clock_jump' && break
        n=$((n + 1)); sleep 0.2
    done
    if on_air "$port"; then
        r=$(awk "BEGIN{printf \"%.1f\", $(cut -d' ' -f1 /proc/uptime) - $t0}")
    else
        r=999
    fi
    log_fault "$d" clock_step "$st" recovered "$r" "seconds until the station flagged clock_jump and was still on air"
    say "clock step: $st flagged it and stayed on air after $r s; stepping back in 2 minutes"
    sleep 120
    date -s "@$(( $(date +%s) - 600 ))" > /dev/null
    timedatectl set-ntp true 2>/dev/null || true
    sleep 10
}

# ---- the supervisor (runs under systemd-run for the whole soak) ---------------------------------------------
run() { # dir
    d="$1"
    start_ms=$(sed -n 's/.*"start_ms": *\([0-9]*\).*/\1/p' "$d/manifest.json")
    end_s=$(awk "BEGIN{printf \"%d\", $start_ms / 1000 + $HOURS * 3600}")
    pids=""
    cleanup() {
        for p in $pids; do kill "$p" 2>/dev/null || true; done
        wait 2>/dev/null || true
    }
    trap 'cleanup; finish "$d"; exit 0' TERM INT
    watch_args=""
    for s in $STATIONS; do watch_args="$watch_args --station ${s%%:*}=${s##*:}"; done
    "$HC" soak-watch --out "$d" $watch_args --interval "${WATCH_INTERVAL:-60}" &
    pids="$pids $!"
    for s in $STATIONS; do
        st=${s%%:*}
        in=$(loop_in "$st")
        [ "$in" = "none" ] && continue
        mkdir -p "$d/clips-$st"
        "$HC" loopcheck --input "$in" --report "$d/loopcheck-$st.jsonl" --clips-dir "$d/clips-$st" --status-s 60 &
        pids="$pids $!"
        say "loopcheck: $st recorded through $in"
    done
    # The faults, in time order.
    plan=$(for f in $FAULTS; do echo "${f##*:} $f"; done | sort -n | awk '{print $2}')
    for f in $plan; do
        kind=${f%%:*}; rest=${f#*:}; st=${rest%%:*}; hour=${rest##*:}
        at=$(awk "BEGIN{printf \"%d\", $start_ms / 1000 + $hour * 3600}")
        while [ "$(date +%s)" -lt "$at" ]; do sleep 20; done
        [ "$(date +%s)" -lt "$end_s" ] || break
        case "$kind" in
        unplug) inject_unplug "$d" "$st" ;;
        kill9) inject_kill9 "$d" "$st" ;;
        clock_step) inject_clock_step "$d" "$st" ;;
        *) say "unknown fault kind $kind" ;;
        esac
    done
    while [ "$(date +%s)" -lt "$end_s" ]; do sleep 30; done
    cleanup
    finish "$d"
}
finish() { # dir
    for st in $LIVE_HEAP_STATIONS; do
        cp "/var/lib/hotclock/stations/$st/live-heap.log" "$1/live-heap-$st.log" 2>/dev/null || say "no live-heap log for $st"
    done
    if [ "$LIVE_HEAP_LIBRARY" = 1 ]; then
        cp /var/lib/hotclock/library-live-heap.log "$1/live-heap-library.log" 2>/dev/null || say "no live-heap log for the library"
    fi
    say "the soak is over: writing the report"
    "$HC" soak-report --dir "$1" > "$1/report.stdout" 2>&1 || true
    say "report: $1/report.md"
}

# ---- commands -----------------------------------------------------------------------------------------------
cmd="${1:-}"
case "$cmd" in
start)
    need_root
    [ -x "$HC" ] || { echo "hotclock is not installed" >&2; exit 1; }
    for s in $STATIONS; do
        st=${s%%:*}; port=${s##*:}
        systemctl is-active --quiet "hotclock-station@$st.service" || { echo "hotclock-station@$st is not running: start the stations first" >&2; exit 1; }
        [ "$(health_code "$port")" = "200" ] || { echo "$st: /health does not answer 200" >&2; exit 1; }
        n=0
        until on_air "$port" || [ $n -ge 100 ]; do n=$((n + 1)); sleep 0.2; done
        on_air "$port" || { echo "$st: nothing on air after 20 s (is its playlist there?)" >&2; exit 1; }
        in=$(loop_in "$st")
        if [ "$in" != "none" ]; then
            "$HC" loopcheck --input "$in" --seconds 2 >/dev/null 2>&1 || { echo "$st: cannot record from $in (wrong card, or the card is held by something else)" >&2; exit 1; }
        fi
    done
    d="$DIR_BASE/$(date -u +%Y%m%dT%H%M%SZ)"
    mkdir -p "$d"
    stations_json=""
    for s in $STATIONS; do stations_json="$stations_json{\"name\":\"${s%%:*}\",\"port\":${s##*:}},"; done
    required_json=""
    for k in $REQUIRED_FAULTS; do required_json="$required_json\"$k\","; done
    loop_json=""
    for s in $STATIONS; do
        m=$(loop_model "${s%%:*}")
        if [ -n "$m" ]; then loop_json="$loop_json\"${s%%:*}\":\"$m\","; fi
    done
    heap_json=""
    for st in $LIVE_HEAP_STATIONS; do heap_json="$heap_json\"$st\":\"/var/lib/hotclock/stations/$st/live-heap.log\","; done
    if [ "$LIVE_HEAP_LIBRARY" = 1 ]; then heap_json="$heap_json\"library\":\"/var/lib/hotclock/library-live-heap.log\","; fi
    deferred_json=$(printf '%s\n' "$DEFERRED" | awk 'NF { gsub(/\\/, "\\\\"); gsub(/"/, "\\\""); printf "%s\"%s\"", (n++ ? "," : ""), $0 }')
    printf '{"start_ms": %s, "hours": %s, "data_dir": "/var/lib/hotclock", "library_port": 8300, "stations": [%s], "required_faults": [%s], "deferred": [%s], "loop_model": {%s}, "live_heap": {%s}}\n' \
        "$(now_ms)" "$HOURS" "${stations_json%,}" "${required_json%,}" "$deferred_json" "${loop_json%,}" "${heap_json%,}" > "$d/manifest.json"
    cp /etc/hotclock/hotclock.toml "$d/hotclock.toml" 2>/dev/null || true
    for s in $STATIONS; do cp "/etc/hotclock/stations/${s%%:*}.toml" "$d/" 2>/dev/null || true; done
    "$HC" --version > "$d/version.txt"
    uname -a >> "$d/version.txt"
    # The transient unit starts with a clean environment: every setting is handed over explicitly.
    systemd-run --quiet --collect --unit "hotclock-soak-$(basename "$d")" --description "Hotclock hardware soak" \
        -p KillMode=control-group -p StandardOutput="append:$d/soak.log" -p StandardError="append:$d/soak.log" \
        --setenv=WATCH_INTERVAL="${WATCH_INTERVAL:-60}" --setenv=HOURS="$HOURS" --setenv=STATIONS="$STATIONS" \
        --setenv=LOOP_IN_office="$LOOP_IN_office" --setenv=LOOP_IN_ita="$LOOP_IN_ita" \
        --setenv=UNPLUG_CARD="$UNPLUG_CARD" --setenv=UNPLUG_S="$UNPLUG_S" --setenv=FAULTS="$FAULTS" \
        --setenv=REQUIRED_FAULTS="$REQUIRED_FAULTS" --setenv=LIVE_HEAP_STATIONS="$LIVE_HEAP_STATIONS" --setenv=LIVE_HEAP_LIBRARY="$LIVE_HEAP_LIBRARY" \
        --setenv=DIR_BASE="$DIR_BASE" \
        "$SELF" run "$d"
    echo "started: $d"
    echo "  look at it:  sudo $0 status"
    echo "  it ends by itself in $HOURS hours and writes $d/report.md"
    ;;
run)
    need_root
    run "${2:?directory}"
    ;;
status)
    d=$(latest_dir)
    [ -n "$d" ] || { echo "no soak directory under $DIR_BASE"; exit 1; }
    echo "soak directory: $d"
    start_ms=$(sed -n 's/.*"start_ms": *\([0-9]*\).*/\1/p' "$d/manifest.json")
    mh=$(sed -n 's/.*"hours": *\([0-9.]*\).*/\1/p' "$d/manifest.json")
    echo "elapsed: $(awk "BEGIN{printf \"%.2f h of $mh\", ($(now_ms) - $start_ms) / 3600000}")"
    systemctl is-active "hotclock-soak-$(basename "$d")" 2>/dev/null | sed 's/^/supervisor: /' || true
    echo "--- last watch samples"
    tail -n 6 "$d/watch.csv" 2>/dev/null | cut -c1-200
    for f in "$d"/loopcheck-*.jsonl; do
        [ -f "$f" ] || continue
        echo "--- $(basename "$f"): $(grep -c '"type": *"gap"' "$f" || true) gap(s), $(grep -c 'tone_glitch' "$f" || true) glitch(es); last status:"
        grep '"type":"status"' "$f" | tail -n 1 | cut -c1-240
    done
    for f in /var/lib/hotclock/stations/*/live-heap.log; do
        [ -f "$f" ] && echo "--- $f (unix_ms pid live_heap_kb sqlite_kb): $(tail -n 1 "$f")"
    done
    echo "--- faults"
    cat "$d/faults.log" 2>/dev/null || echo "(none yet)"
    [ -f "$d/report.md" ] && echo "--- the report is done: $d/report.md"
    ;;
report)
    d=$(latest_dir)
    [ -n "$d" ] || { echo "no soak directory under $DIR_BASE"; exit 1; }
    for st in $(sed -n 's/.*"live_heap": *{\([^}]*\)}.*/\1/p' "$d/manifest.json" | tr ',' '\n' | sed -n 's/^ *"\([^"]*\)":.*/\1/p'); do
        cp "/var/lib/hotclock/stations/$st/live-heap.log" "$d/live-heap-$st.log" 2>/dev/null || true
    done
    "$HC" soak-report --dir "$d"
    ;;
stop)
    need_root
    d=$(latest_dir)
    [ -n "$d" ] || { echo "no soak directory"; exit 1; }
    systemctl stop "hotclock-soak-$(basename "$d")" 2>/dev/null || true
    sleep 2
    [ -f "$d/report.md" ] || "$HC" soak-report --dir "$d" || true
    ;;
*)
    sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
