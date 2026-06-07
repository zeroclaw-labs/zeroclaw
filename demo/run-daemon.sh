#!/usr/bin/env bash
# Switch the running container from sim-only mode (default CMD) to daemon mode.
#
# Daemon mode wires the channel orchestrator → agent loop → gpio peripheral.
# This Docker image is intentionally simulator/local-chat focused; use
# run-agent-host.sh for the Telegram channel demo.
set -euo pipefail

cd "$(dirname "$0")"

if ! docker compose ps --status running --services 2>/dev/null | grep -q "^zeroclaw$"; then
  echo "error: simulator container not running. Start it first:" >&2
  echo "       ./demo/run-sim.sh" >&2
  exit 1
fi

# Wait for pty to exist
for i in {1..40}; do
  if docker compose exec -T zeroclaw test -e /tmp/zc-sim-esp32 2>/dev/null; then break; fi
  if [[ $i -eq 40 ]]; then
    echo "error: /tmp/zc-sim-esp32 never appeared inside container" >&2
    exit 1
  fi
  sleep 0.1
done

echo "Starting zeroclaw daemon inside the demo container..."
echo "This Docker image does not include the Telegram channel; use ./demo/run-agent-host.sh for Telegram."
echo "Press Ctrl-C to stop the daemon. The simulator will keep running."
echo
echo "Note: shell scripts in demo/ are English-only (demo harness)."
echo
exec docker compose exec zeroclaw \
  zeroclaw daemon --config-dir /app/data/config "$@"
