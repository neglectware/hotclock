#!/bin/sh
# The status display's switch (`display.enabled`, Settings) against REAL systemd 255 and polkit in a container, no fake:
# install the package (the display is on by default and the generator wants it), switch it off and on through the web
# API (real `systemctl stop`/`start` through polkit as user hotclock), check polkit allows exactly start/stop/restart of
# the display and no enable/disable, and check that off survives a "reboot" (container restart), also on a box where an
# operator ran `systemctl enable hotclock-display` by hand, with never a second display process. Then the postinst's
# one-time migration of a box upgraded from before the switch, both ways. The container has no monitor (/dev/dri), so
# the display runs and reports "no monitor connected", as on a box without a screen.
#   dev/display-systemd-smoke.sh                       builds dist/hotclock_<v>-9_<arch>.deb with dev/package.sh if missing
#   DEB=dist/hotclock_0.1.0-9_amd64.deb dev/display-systemd-smoke.sh
set -eu
cd "$(dirname "$0")/.."
v="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
arch="$(uname -m | sed 's/x86_64/amd64/; s/aarch64/arm64/')"
d="${DEB:-dist/hotclock_${v}-9_${arch}.deb}"
[ -f "$d" ] || HOTCLOCK_DEB_REVISION=9 dev/package.sh "$arch" >/dev/null
docker image inspect hotclock-systemd >/dev/null 2>&1 || docker build -t hotclock-systemd -f dev/systemd.Dockerfile dev
name=hotclock-display-smoke
docker rm -f "$name" >/dev/null 2>&1 || true
docker run -d --name "$name" --privileged --cgroupns=host \
    -v /sys/fs/cgroup:/sys/fs/cgroup:rw --tmpfs /run --tmpfs /run/lock hotclock-systemd >/dev/null
[ "${KEEP:-0}" = "1" ] || trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT
wait_boot() { n=0; until docker exec "$name" systemctl is-system-running --wait >/dev/null 2>&1 || [ $n -ge 60 ]; do n=$((n + 1)); sleep 1; done; }
reboot() { docker restart "$name" >/dev/null; wait_boot; sleep 5; }
wait_boot
docker cp "$d" "$name:/root/h.deb"
fails=0
pass() { echo "PASS  $*"; }
fail() { echo "FAIL  $*"; fails=$((fails + 1)); }
check() { # description, command
    if docker exec "$name" sh -c "$2" >/dev/null 2>&1; then pass "$1"; else fail "$1"; fi
}
L=http://127.0.0.1:8300
J='content-type: application/json'
api() { docker exec "$name" curl -s -H "$J" "$@"; }
switch() { api -X PUT $L/api/v1/settings/global -d "{\"values\":{\"display.enabled\":$1}}"; }
# the answer's display_switch: `stop ok` when the unit was stopped through systemd
did() { echo "$1" | grep -q "\"action\":\"$2\"" && echo "$1" | grep -q '"display_switch":{[^}]*"ok":true'; }
G=/etc/hotclock/hotclock.toml
ONE="test \"\$(pgrep -fc '^/usr/bin/hotclock display\$')\" = 1"
NONE="! pgrep -f '^/usr/bin/hotclock display\$'"
WANTED="test -L /run/systemd/generator/multi-user.target.wants/hotclock-display.service"

echo "== fresh install: the switch is on by default"
docker exec "$name" sh -c 'apt-get -qq update >/dev/null && DEBIAN_FRONTEND=noninteractive apt-get -qq install -y --no-install-recommends polkitd curl /root/h.deb >/dev/null 2>&1; systemctl reload dbus; systemctl restart polkit'
check "the shipped global file does not set display.enabled" "! grep -q '^[[:space:]]*enabled' $G"
check "the package does not enable the unit" "test \"\$(systemctl is-enabled hotclock-display)\" != enabled"
check "postinst marked the box as migrated (a fresh install has nothing to keep)" "test -e /var/lib/hotclock/.display-switch"
check "the generator wants the display (default on)" "$WANTED"
check "the unit has the switch as its ExecCondition" "systemctl cat hotclock-display | grep -qx 'ExecCondition=/usr/bin/hotclock display --check-enabled'"
docker exec "$name" sh -c 'systemctl enable --now hotclock-library && systemctl start hotclock-display'   # what install.sh does
sleep 4
check "the library answers" "curl -sf $L/health"
check "the display runs" "systemctl is-active hotclock-display && $ONE"
j=$(api $L/api/v1/display/status)
echo "$j" | grep -q '"running":true' && echo "$j" | grep -q '"text":"waiting for a station' && pass "with no station yet the page sees: running, waiting for a station" || fail "status: $j"
r=$(api -X POST $L/api/v1/station-admin/demo -d '{}')
echo "$r" | grep -q '"created":"demo"' && pass "a station made on the web page (the demo)" || fail "demo: $r"
sleep 4
j=$(api $L/api/v1/display/status)
echo "$j" | grep -q '"running":true' && echo "$j" | grep -q '"text":"no monitor connected"' && pass "the display found the station; the page sees: running, no monitor connected" || fail "status: $j"
check "the display reported its monitor in its state directory" "grep -q '\"connected\":false' /var/lib/hotclock-display/output.json"
check "the station plays on, untouched by the display" "systemctl is-active hotclock-station@demo"

echo "== polkit: start, stop and restart of the display; no enable, no disable"
check "hotclock may stop the display" "runuser -u hotclock -- systemctl stop hotclock-display && ! systemctl is-active hotclock-display"
check "hotclock may start the display" "runuser -u hotclock -- systemctl start hotclock-display && systemctl is-active hotclock-display"
check "hotclock may restart the display" "runuser -u hotclock -- systemctl restart hotclock-display && systemctl is-active hotclock-display"
check "hotclock may NOT enable the display" "! runuser -u hotclock -- systemctl enable hotclock-display.service"
check "hotclock may NOT disable the display" "! runuser -u hotclock -- systemctl disable hotclock-display.service"
check "hotclock may NOT start another unit (ssh)" "! runuser -u hotclock -- systemctl start ssh.service"
check "hotclock may NOT stop the library (restart only)" "! runuser -u hotclock -- systemctl stop hotclock-library.service"
check "hotclock may NOT start the kiosk example or a look-alike" "! runuser -u hotclock -- systemctl start hotclock-display.socket && ! runuser -u hotclock -- systemctl start xhotclock-display.service"

echo "== off through the web API"
r=$(switch false)
did "$r" stop && pass "the switch stopped the unit through polkit" || fail "off: $r"
check "the display is stopped, no process left" "! systemctl is-active hotclock-display && $NONE"
check "the file says enabled = false in [display]" "sed -n '/^\[display\]/,/^\[/p' $G | grep -qx 'enabled = false'"
j=$(api $L/api/v1/display/status)
echo "$j" | grep -q '"text":"stopped"' && pass "the page sees: stopped" || fail "status: $j"
docker exec "$name" systemctl start hotclock-display || true
sleep 5
check "a start by root is skipped by the condition, and nothing restarts it" "! systemctl is-active hotclock-display && $NONE && systemctl show -p NRestarts --value hotclock-display | grep -qx 0"
check "the journal says why" "journalctl -u hotclock-display --no-pager | grep -q 'display.enabled is off'"
check "daemon-reload: the generator no longer wants it" "systemctl daemon-reload && ! $WANTED"
reboot
check "after a reboot the library runs" "systemctl is-active hotclock-library"
check "after a reboot the display stays off" "! systemctl is-active hotclock-display && $NONE && ! $WANTED"

echo "== a box where an operator enabled the display by hand"
docker exec "$name" systemctl enable hotclock-display.service >/dev/null 2>&1
reboot
check "switch off + enabled by hand: still off after a reboot (the condition skips it)" "systemctl is-enabled hotclock-display | grep -qx enabled && ! systemctl is-active hotclock-display && $NONE"
r=$(switch true)
did "$r" start && pass "the switch started it" || fail "on: $r"
check "one display process" "systemctl is-active hotclock-display && $ONE"
reboot
check "switch on + enabled by hand + generator: one unit, one process after a reboot" "systemctl is-active hotclock-display && $ONE && $WANTED"

echo "== postinst on an upgrade: the generator takes over a hand-made enable"
check "postinst disables the hand-made enable and leaves the display running" "/var/lib/dpkg/info/hotclock.postinst configure 0.1.0-30 && test \"\$(systemctl is-enabled hotclock-display)\" != enabled && systemctl is-active hotclock-display && $ONE"
reboot
check "after a reboot the generator alone starts it" "systemctl is-active hotclock-display && $ONE && $WANTED"

echo "== postinst on the first upgrade from before the switch: the box keeps what it had"
# a box that had the display enabled by hand: no key written, the display stays on
docker exec "$name" sh -c "rm -f /var/lib/hotclock/.display-switch; sed -i '/^enabled = /d' $G; systemctl enable hotclock-display.service" >/dev/null 2>&1
check "enabled by hand before: no key written, the unit handed to the generator, still running" "/var/lib/dpkg/info/hotclock.postinst configure 0.1.0-30 && ! grep -q '^enabled' $G && test \"\$(systemctl is-enabled hotclock-display)\" != enabled && systemctl is-active hotclock-display && test -e /var/lib/hotclock/.display-switch"
# a box that did not have it: enabled = false written, and it holds across a reboot
docker exec "$name" sh -c "systemctl stop hotclock-display; rm -f /var/lib/hotclock/.display-switch $G.pre-display-switch; sed -i '/^enabled = /d' $G"
check "not enabled before: enabled = false written into [display], with a copy of the old file" "/var/lib/dpkg/info/hotclock.postinst configure 0.1.0-30 && sed -n '/^\[display\]/,/^\[/p' $G | grep -q '^enabled = false' && test -f $G.pre-display-switch && ! runuser -u hotclock -- /usr/bin/hotclock display --check-enabled"
check "the generator no longer wants it" "! $WANTED"
check "a second run changes nothing (the marker)" "cp $G /root/g1 && /var/lib/dpkg/info/hotclock.postinst configure 0.1.0-30 && cmp -s $G /root/g1"
reboot
check "after a reboot the display stays off" "! systemctl is-active hotclock-display && $NONE"
j=$(api $L/api/v1/settings)
echo "$j" | grep -q '"display.enabled":false' && pass "Settings shows the switch off" || fail "settings: $(echo "$j" | head -c 300)"
r=$(switch true)
did "$r" start && pass "and turns it on again" || fail "on: $r"
check "running" "systemctl is-active hotclock-display && $ONE"

echo "== journal: no sandbox or permission errors"
check "no 'Permission denied' or 'Read-only file system' from the library or the display" "! journalctl --no-pager -u hotclock-library -u hotclock-display | grep -i 'read-only file system\|permission denied'"
check "no polkit refusal of the switch logged by the library" "! journalctl --no-pager -u hotclock-library | grep -q 'display switch:.*failed'"
check "the station runs at the end (the generator brought it back after each reboot)" "systemctl is-active hotclock-station@demo"
[ $fails -eq 0 ] && echo "ALL PASS" || { echo "$fails FAILED"; exit 1; }
