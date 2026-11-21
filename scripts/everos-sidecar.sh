#!/usr/bin/env bash
# Start the EverOS memory sidecar for ZeroClaw (host process, not Docker).
set -euo pipefail

ROOT="${EVEROS_ROOT:-$HOME/.everos}"
HOST="${EVEROS_API__HOST:-127.0.0.1}"
PORT="${EVEROS_API__PORT:-8000}"

if ! command -v everos >/dev/null 2>&1; then
  echo "everos not found on PATH. Install with: uv pip install everos" >&2
  exit 1
fi

if [[ ! -f "$ROOT/everos.toml" ]]; then
  everos init --root "$ROOT"
  echo "Created $ROOT/everos.toml — set [llm].api_key before relying on extraction."
fi

echo "Starting EverOS at http://${HOST}:${PORT} (root=$ROOT)"
exec everos server start --host "$HOST" --port "$PORT" --root "$ROOT"
