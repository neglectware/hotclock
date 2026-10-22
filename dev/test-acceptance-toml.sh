#!/bin/sh
# Test of acceptance.sh's set_key: every key, comment and table of the station toml survives; only the keys the
# test needs change. Runs anywhere with sh and awk (no package needed): dev/test-acceptance-toml.sh
set -eu
cd "$(dirname "$0")/.."
eval "$(sed -n '/^set_key() {/,/^}/p' packaging/acceptance.sh)"
t=$(mktemp -d); trap 'rm -rf "$t"' EXIT
cp packaging/examples/eve.toml "$t/eve.toml"
cat >> "$t/eve.toml" <<'EOT'
#rate_trim_ppm = 0
[stream]
enabled = true
default_playlist = "must not be touched, it is in a table"
EOT
cp "$t/eve.toml" "$t/orig.toml"
set_key "$t/eve.toml" name '"eve"'
set_key "$t/eve.toml" listen '"0.0.0.0:8302"'
set_key "$t/eve.toml" device '"hw:CARD=PCH,DEV=0"'
set_key "$t/eve.toml" default_playlist '"Acceptance"'
set_key "$t/eve.toml" emergency_playlist '"Acceptance emergency"'
fails=0
chk() { if eval "$1"; then echo "PASS  $2"; else echo "FAIL  $2"; fails=$((fails + 1)); fi; }
chk 'grep -q "^target_lufs = -18.0$" "$t/eve.toml"' "target_lufs kept"
chk 'grep -q "^transition_preset = \"plexamp\"$" "$t/eve.toml"' "transition_preset kept"
chk 'grep -q "^default_playlist = \"Acceptance\"$" "$t/eve.toml"' "default_playlist overridden"
chk 'grep -q "^emergency_playlist = \"Acceptance emergency\"$" "$t/eve.toml"' "emergency_playlist overridden"
chk 'grep -q "^device = \"hw:CARD=PCH,DEV=0\"$" "$t/eve.toml"' "device overridden"
chk 'grep -q "^#rate_trim_ppm = 0$" "$t/eve.toml" && grep -q "^\[stream\]$" "$t/eve.toml"' "comment and table kept"
chk 'grep -q "must not be touched" "$t/eve.toml"' "same key inside a table untouched"
chk '[ "$(grep -c "^default_playlist" "$t/eve.toml")" = 2 ]' "no duplicate top-level key"
# Every original line is still there except the three replaced values.
lost=$(grep -vxF -f "$t/eve.toml" "$t/orig.toml" | sort | tr '\n' '|')
echo "lines of the original not in the result: $lost"
chk '[ "$lost" = "device = \"hw:CARD=Device,DEV=0\"|default_playlist = \"Eve elevator\"|emergency_playlist = \"Eve emergency\"|listen = \"0.0.0.0:8302\"|" ] || [ "$lost" = "default_playlist = \"Eve elevator\"|device = \"hw:CARD=Device,DEV=0\"|emergency_playlist = \"Eve emergency\"|" ]' "only the overridden lines differ"
# A missing key is added before the first table; an empty file gets all of them.
: > "$t/new.toml"
set_key "$t/new.toml" name '"x"'; set_key "$t/new.toml" device '"null"'
chk '[ "$(cat "$t/new.toml")" = "name = \"x\"
device = \"null\"" ]' "empty file gets the keys"
printf '[stream]\nenabled = true\n' > "$t/t.toml"; set_key "$t/t.toml" name '"x"'
chk '[ "$(head -n 1 "$t/t.toml")" = "name = \"x\"" ]' "a missing key lands before the first table"
echo "== $fails failure(s)"
[ "$fails" -eq 0 ]
