#!/bin/sh
# Build the .deb for the target box (Ubuntu Server 24.04, amd64) in a container on this machine (M1.8).
#
#   dev/package.sh            amd64: dist/hotclock_<version>_amd64.deb, the binary at dist/hotclock-amd64
#   dev/package.sh arm64      the same for an arm64 Ubuntu (used for the systemd smoke test in Colima)
#
# The container is Ubuntu 24.04 (dev/release.Dockerfile) and cross-compiles the amd64 target at native speed;
# the libc and libasound the binary links against are Ubuntu's own. `HOTCLOCK_DEB_REVISION` (default 1) is the
# package revision; the API reports it with the commit as `0.1.0+build.<rev>.g<commit>`.
# `HOTCLOCK_ALLOC_COUNT=1` also builds dist/hotclock-<arch>-alloc-count: the same binary with the soaks' counting
# allocator (feature `alloc-count`, never packaged), for a station that logs its live heap during a hardware soak
# (`HOTCLOCK_LIVE_HEAP_LOG`; docs/hardware-tests.md).
set -eu
cd "$(dirname "$0")/.."
arch="${1:-amd64}"
case "$arch" in
amd64) triple=x86_64-unknown-linux-gnu ;;
arm64) triple=aarch64-unknown-linux-gnu ;;
*) echo "usage: dev/package.sh [amd64|arm64]" >&2; exit 2 ;;
esac
docker image inspect hotclock-release >/dev/null 2>&1 ||
    docker build -t hotclock-release -f dev/release.Dockerfile dev
rev="${HOTCLOCK_DEB_REVISION:-1}"
commit="$(git rev-parse --short HEAD 2>/dev/null || echo nogit)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
mkdir -p dist
# The registry is cached in a volume; the build directory is inside the checkout (ignored by git) so a
# rebuild is incremental across runs.
docker run --rm \
    -v "$PWD":/src \
    -v hotclock-release-cargo:/opt/cargo/registry \
    -e CARGO_TARGET_DIR=/src/target-release -e CARGO_INCREMENTAL=0 \
    -e "HOTCLOCK_BUILD=$rev.g$commit" \
    -w /src hotclock-release sh -eu -c "
        cargo build --release --locked -p hotclock --target $triple
        cargo deb -p hotclock --no-build --strip --target $triple --deb-version '$version-$rev' -o dist/
        cp target-release/$triple/release/hotclock dist/hotclock-$arch
        x86_64-linux-gnu-strip -o dist/hotclock-$arch.stripped dist/hotclock-$arch 2>/dev/null || strip -o dist/hotclock-$arch.stripped dist/hotclock-$arch
        mv dist/hotclock-$arch.stripped dist/hotclock-$arch
        if [ '${HOTCLOCK_ALLOC_COUNT:-0}' = 1 ]; then
            cargo build --release --locked -p hotclock --target $triple --features alloc-count
            cp target-release/$triple/release/hotclock dist/hotclock-$arch-alloc-count
            x86_64-linux-gnu-strip dist/hotclock-$arch-alloc-count 2>/dev/null || strip dist/hotclock-$arch-alloc-count
        fi
    "
ls -l dist/
