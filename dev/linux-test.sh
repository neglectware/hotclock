#!/bin/sh
# Run a cargo command in the Linux container against this checkout.
# Default: cargo test --workspace. Examples:
#   dev/linux-test.sh cargo clippy --workspace --all-targets -- -D warnings
#   dev/linux-test.sh cargo test -p engine -- --nocapture
# The container has its own target directory (target-linux, ignored by git) and a cached cargo
# registry volume, so it does not fight the macOS build.
set -eu
cd "$(dirname "$0")/.."
[ $# -eq 0 ] && set -- cargo test --workspace
# Keep target-linux bounded (it reached 21 GB by M1.7). The container build is not incremental (the
# incremental caches only pay off for interactive rebuilds and were a quarter of it). And every change of a
# workspace crate's dependency set left another full copy of each test binary (200 MB each) under a new hash:
# for the workspace's own crates and test targets, only the two newest variants are kept (third-party crates
# are left alone; cargo rebuilds anything removed that it still needs).
if [ -d target-linux/debug/deps ]; then
  rm -rf target-linux/debug/incremental target-linux/release/incremental
  names="$(ls crates) $(ls crates/*/tests/*.rs | sed 's|.*/||; s|\.rs$||')"
  for n in $names; do
    n=$(echo "$n" | tr - _)
    for f in "$n" "lib$n"; do
      ls -t target-linux/debug/deps/"$f"-* 2>/dev/null | sed -E 's/.*-([0-9a-f]{16}).*/\1/' | awk '!seen[$0]++' | tail -n +3 |
        while read -r h; do rm -f target-linux/debug/deps/"$f-$h" target-linux/debug/deps/"$f-$h".*; done
    done
  done
fi
# A small tmpfs at /small lets the disk-full test (M1.6 acceptance 8) hit a real ENOSPC.
exec docker run --rm \
  --tmpfs /small:rw,size=4m -e HOTCLOCK_SMALL_FS=/small \
  -v "$PWD":/src \
  -v hotclock-cargo:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/src/target-linux -e CARGO_INCREMENTAL=0 \
  -w /src hotclock-dev "$@"
