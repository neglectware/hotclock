# Sourced by the dev/*-check.sh scripts (through dev/check-env.sh; ui-check-m6a.sh sources it itself): the check's node
# runner and its headless Chrome never outlive the script, however it ends (a failed step, Ctrl-C, `timeout`, a closed
# terminal). A Chrome left running for a day fills a Mac's disk (its code_sign_clone copies in the temp folder).
#   run_node ARGS     runs `node ARGS` in the background and waits for it, so a signal to the script runs its trap at once
#                     (a foreground command would hold the trap until it ended)
#   stop_check_chrome the first thing in each script's EXIT trap: TERM to the runner (dev/chrome.mjs then removes its Chrome
#                     and the Chrome's profile), then KILL to any Chrome still on this check's debugging port
#   check_traps       EXIT runs the script's clean-up; INT, TERM and HUP exit (130, 143, 129) through it
run_node() { node "$@" & wait $!; }
stop_check_chrome() {
  if pkill -TERM -P $$ -f 'node ' 2>/dev/null; then sleep 1; fi
  pkill -KILL -f "remote-debugging-port=${CHROME_DEBUG_PORT:-none}( |\$)" 2>/dev/null || true
}
check_traps() {
  trap "stop_check_chrome; $1" EXIT
  trap 'exit 130' INT; trap 'exit 143' TERM; trap 'exit 129' HUP
}
