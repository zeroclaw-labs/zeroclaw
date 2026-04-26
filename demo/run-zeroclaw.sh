#!/usr/bin/env bash
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

exec docker compose exec zeroclaw \
  zeroclaw agent --model MiniMax-M2.7 "$@"
