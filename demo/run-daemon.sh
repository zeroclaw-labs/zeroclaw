#!/usr/bin/env bash
# Switch the running container from sim-only mode (default CMD) to daemon mode
# so the Telegram channel orchestrator runs alongside the simulator.
#
# Daemon mode wires the channel orchestrator → agent loop → gpio peripheral.
# Messages from Telegram → agent → tool calls → simulator → frontend updates.
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

echo "Starting zeroclaw daemon (Telegram + sim peripheral wired together)..."
echo "Press Ctrl-C to stop. Sim continues running independently."
exec docker compose exec zeroclaw zeroclaw daemon "$@"
