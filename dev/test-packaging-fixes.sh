#!/bin/sh
# shellcheck disable=SC2016  # the checks are strings chk evaluates
# Tests of the shell side of three packaging fixes, with stand-ins for the package's commands (no package, no root):
#   - acceptance.sh's own_playlist: a second run empties and refills its playlists instead of creating them twice;
#   - install.sh's has_program: a station with no playlist in the library (and no state) is not written or started.
# The restore's ownership fix is tested in crates/hotclock/tests/backup_m18.rs (it needs root).
#   dev/test-packaging-fixes.sh
set -eu
cd "$(dirname "$0")/.."
t=$(mktemp -d); trap 'rm -rf "$t"' EXIT
fails=0
chk() { if eval "$1"; then echo "PASS  $2"; else echo "FAIL  $2"; fails=$((fails + 1)); fi; }

# ---- acceptance.sh: own_playlist against a fake `hc` that keeps playlists in a file and refuses a duplicate
eval "$(sed -n '/^own_playlist() {/,/^}/p' packaging/acceptance.sh)"
: > "$t/pl"
hc() {
    echo "$*" >> "$t/calls"
    case "$1 $2" in
    "playlist list") i=0; while IFS= read -r n; do i=$((i + 1)); printf '%3d  %s  (shuffle, no repeat 0 min, 1 entries)\n' "$i" "$n"; done < "$t/pl" ;;
    "playlist create") if grep -qxF "$3" "$t/pl"; then echo "UNIQUE constraint failed: playlists.name" >&2; return 1; fi; echo "$3" >> "$t/pl" ;;
    "playlist clear") grep -qxF "$3" "$t/pl" ;;
    esac
}
for run in 1 2; do
    : > "$t/calls"
    own_playlist Acceptance --no-repeat-minutes 0
    own_playlist "Acceptance emergency" --mode sequential --no-repeat-minutes 0
    cp "$t/calls" "$t/calls$run"
done
chk 'grep -qx "playlist create Acceptance --no-repeat-minutes 0" "$t/calls1" && grep -qx "playlist create Acceptance emergency --mode sequential --no-repeat-minutes 0" "$t/calls1"' "first run creates both playlists"
chk '! grep -q "playlist create" "$t/calls2"' "second run creates nothing"
chk 'grep -qx "playlist clear Acceptance" "$t/calls2" && grep -qx "playlist clear Acceptance emergency" "$t/calls2"' "second run clears both"
chk '[ "$(wc -l < "$t/pl")" -eq 2 ]' "still two playlists"
# "Acceptance emergency" existing alone does not count as "Acceptance".
echo "Acceptance emergency" > "$t/pl"; : > "$t/calls"
own_playlist Acceptance --no-repeat-minutes 0
chk 'grep -qx "playlist create Acceptance --no-repeat-minutes 0" "$t/calls"' "a name is matched whole"

# ---- install.sh: has_program against a fake runuser that runs `hotclock playlist list`
eval "$(sed -n '/^has_program() {/,/^}/p' packaging/install.sh)"
runuser() { cat "$t/list"; }
: > "$t/list"
chk '! has_program hc-test-no-such-station' "empty library: no program"
printf 'warning: something on stderr-like stdout\n' > "$t/list"
chk '! has_program hc-test-no-such-station' "other output is not a playlist"
printf '  1  Office  (shuffle, no repeat 120 min, 0 entries)\n' > "$t/list"
chk 'has_program hc-test-no-such-station' "a playlist: the station has a program"
# The station is neither written nor started when has_program fails: both paths go through it.
chk '[ "$(grep -c "has_program \"\$name\"" packaging/install.sh)" -eq 2 ]' "install.sh checks before writing and before starting"

echo "== $fails failure(s)"
[ "$fails" -eq 0 ]
