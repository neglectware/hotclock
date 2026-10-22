#!/bin/sh
# The clean-install, upgrade and rollback acceptance run in a container with systemd (M1.8 acceptance 1 and 2
# as far as a container can go: no sound card, so the output is the paced `null` device). It builds two arm64
# packages (revision 1 and 2; the Colima VM is arm64 and the amd64 build differs only in the target), starts
# a fresh Ubuntu 24.04 with systemd, installs revision 1 with packaging/install.sh, runs
# packaging/acceptance.sh, then upgrades to revision 2 and rolls back with packaging/upgrade.sh.
#   dev/systemd-smoke.sh
set -eu
cd "$(dirname "$0")/.."
v="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
d1="dist/hotclock_${v}-1_arm64.deb"
d2="dist/hotclock_${v}-2_arm64.deb"
[ -f "$d1" ] || HOTCLOCK_DEB_REVISION=1 dev/package.sh arm64 >/dev/null
[ -f "$d2" ] || HOTCLOCK_DEB_REVISION=2 dev/package.sh arm64 >/dev/null
docker image inspect hotclock-systemd >/dev/null 2>&1 || docker build -t hotclock-systemd -f dev/systemd.Dockerfile dev
name=hotclock-smoke
docker rm -f "$name" >/dev/null 2>&1 || true
docker run -d --name "$name" --privileged --cgroupns=host \
    -v /sys/fs/cgroup:/sys/fs/cgroup:rw --tmpfs /run --tmpfs /run/lock \
    hotclock-systemd >/dev/null
[ "${KEEP:-0}" = "1" ] || trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT
n=0
until docker exec "$name" systemctl is-system-running --wait >/dev/null 2>&1 || [ $n -ge 60 ]; do n=$((n + 1)); sleep 1; done
docker exec "$name" systemctl is-system-running || true
for f in "$d1" "$d2" packaging/install.sh packaging/upgrade.sh packaging/acceptance.sh; do
    docker cp "$f" "$name:/root/$(basename "$f")"
done
echo "== install.sh (revision 1, one station on the null device, not started)"
docker exec "$name" sh /root/install.sh "/root/$(basename "$d1")" --station office=null:8301 --no-start | tail -4
docker exec "$name" sh /root/acceptance.sh
echo "== upgrade.sh install (revision 2): nothing may restart"
docker exec "$name" sh -c "
    before=\$(systemctl show -p MainPID --value hotclock-station@office.service)
    sh /root/upgrade.sh install /root/$(basename "$d2") | tail -3
    after=\$(systemctl show -p MainPID --value hotclock-station@office.service)
    [ \"\$before\" = \"\$after\" ] && echo \"PASS  the station kept running through the install (pid \$before)\" || echo \"FAIL  the station restarted during the install\"
    dpkg-query -W -f='installed: \${Version}\n' hotclock
    curl -s http://127.0.0.1:8301/health | sed -n 's/.*\"version\":\"\\([^\"]*\\)\".*/  running: \\1\\n/p'
"
echo "== upgrade.sh restart"
docker exec "$name" sh -c "
    sh /root/upgrade.sh restart
    curl -s http://127.0.0.1:8301/health | sed -n 's/.*\"version\":\"\\([^\"]*\\)\".*/  running: \\1\\n/p'
    curl -s http://127.0.0.1:8300/health | sed -n 's/.*\"version\":\"\\([^\"]*\\)\".*/  library: \\1\\n/p'
"
echo "== upgrade.sh rollback"
docker exec "$name" sh -c "
    sh /root/upgrade.sh rollback
    dpkg-query -W -f='installed: \${Version}\n' hotclock
    curl -s http://127.0.0.1:8301/health | sed -n 's/.*\"version\":\"\\([^\"]*\\)\".*/  running: \\1\\n/p'
"
echo "== soak-hardware.sh dry run (a 2-minute soak: one station on the null device, a kill -9, no loopcheck)"
docker cp dev/soak-hardware.sh "$name:/root/soak-hardware.sh"
docker exec "$name" sh -c "
    export HOURS=0.03 STATIONS='office:8301' LOOP_IN_office=none FAULTS='kill9:office:0.008' WATCH_INTERVAL=5 DIR_BASE=/var/lib/hotclock-soak
    rm -f /run/hotclock/office.starts
    sh /root/soak-hardware.sh start
    sleep 150
    sh /root/soak-hardware.sh status | cut -c1-160
    d=\$(ls -1dt /var/lib/hotclock-soak/20* | head -n 1)
    ls \$d
    head -n 12 \$d/report.md
"
