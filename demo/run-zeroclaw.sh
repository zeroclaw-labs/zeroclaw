#!/usr/bin/env bash
# Docker-packaged path.
# For low-storage machines use ./demo/run-agent-host.sh instead.
#
# Open an interactive ZeroClaw chat inside the running demo container.
# Requires: simulator container already up (see ./demo/run-sim.sh).
set -euo pipefail

cd "$(dirname "$0")"

if ! docker compose ps --status running --services 2>/dev/null | grep -q "^zeroclaw$"; then
  echo "error: simulator container not running." >&2
  echo "       start it first in another terminal:  ./demo/run-sim.sh" >&2
  exit 1
fi

# Wait for the pty to exist inside the container before launching zeroclaw.
for i in {1..40}; do
  if docker compose exec -T zeroclaw test -e /tmp/zc-sim-esp32 2>/dev/null; then
    break
  fi
  if [[ $i -eq 40 ]]; then
    echo "error: /tmp/zc-sim-esp32 never appeared inside container; is the simulator healthy?" >&2
    exit 1
  fi
  sleep 0.1
done

echo "Starting interactive agent session inside the demo container..."
echo "Agent has the smartroom tools (set_device / read_device) + gpio fallbacks."
echo "Try natural language after pasting the primer from demo/PROMPTS.md:"
echo "  'It's getting dark and chilly. I'm settling in to read for an hour.'"
echo
echo "Note: shell scripts in demo/ are English-only (demo harness)."
echo

exec docker compose exec zeroclaw \
  zeroclaw agent --config-dir /app/data/config --agent demo "$@"
