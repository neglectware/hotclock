# A box with systemd, for the package acceptance run (dev/systemd-smoke.sh): a fresh Ubuntu 24.04 with only
# what a minimal server has, plus the tools the acceptance script uses.
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive container=docker
RUN apt-get -qq update && apt-get -qq install -y --no-install-recommends \
      systemd systemd-sysv dbus udev curl ca-certificates sqlite3 procps iproute2 \
 && rm -rf /var/lib/apt/lists/* \
 && systemctl mask systemd-udevd.service systemd-udevd-kernel.socket systemd-udevd-control.socket \
      getty.target console-getty.service systemd-logind.service 2>/dev/null || true
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
