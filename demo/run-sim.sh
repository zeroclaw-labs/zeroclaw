#!/usr/bin/env bash
# Start the simulator container (runs esp32_sim as default CMD).
# Frontend will be reachable at http://127.0.0.1:8080
set -euo pipefail

cd "$(dirname "$0")"

# Pass env from .env if present (so MINIMAX_API_KEY reaches the container).
if [[ -f .env ]]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

exec docker compose up
