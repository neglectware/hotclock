# Sourced by the dev/*-check.sh scripts: names and ports that belong to ONE worktree, so checks started from two
# worktrees at once do not kill each other's containers or fight over ports.
#   CHECK_TAG   a per-worktree suffix for container names (cksum of the worktree path)
#   CHECK_OFFSET  set by each script (ui 0, m2 2, m3 4, preset 6) so one worktree's scripts do not collide either
#   LP, SP      the host ports the library and the station are published on (container-internal ports stay fixed)
#   CHROME_DEBUG_PORT   the headless Chrome's debugging port (the .mjs files read it)
# The slot comes from the worktree path; `HOTCLOCK_CHECK_SLOT=N` (0 to 899) overrides it, and `HOTCLOCK_CHECK_LP`,
# `_SP` and `HOTCLOCK_CHECK_CHROME_PORT` override single values. A script only ever removes the container whose name
# carries its own tag.
_sum=$(printf %s "$PWD" | cksum | cut -d' ' -f1)
CHECK_TAG=$_sum
_slot=${HOTCLOCK_CHECK_SLOT:-$((_sum % 900))}
LP=${HOTCLOCK_CHECK_LP:-$((20000 + _slot * 10 + ${CHECK_OFFSET:-0}))}
SP=${HOTCLOCK_CHECK_SP:-$((LP + 1))}
CHROME_DEBUG_PORT=${HOTCLOCK_CHECK_CHROME_PORT:-$((31000 + _slot * 10 + ${CHECK_OFFSET:-0}))}
export CHROME_DEBUG_PORT
