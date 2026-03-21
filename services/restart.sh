#!/usr/bin/env bash
# restart.sh — detached zeroclaw daemon restart
#
# Called by the 'restart' Matrix command. Waits briefly so the confirmation
# message can be delivered, then kills the running daemon and re-execs it.
#
# Environment (set by the caller):
#   ZEROCLAW_BIN         — path to the zeroclaw binary
#   ZEROCLAW_CONFIG_DIR  — config directory

set -euo pipefail

ZC="${ZEROCLAW_BIN:-$(command -v zeroclaw 2>/dev/null || echo "")}"
if [[ -z "$ZC" || ! -x "$ZC" ]]; then
    echo "restart.sh: zeroclaw binary not found (ZEROCLAW_BIN=$ZC)" >&2
    exit 1
fi

# Give the reply message time to be sent before we kill the daemon.
sleep 2

pkill -f "zeroclaw daemon" 2>/dev/null || true
sleep 1

exec "$ZC" daemon
