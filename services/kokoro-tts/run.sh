#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
VENV_DIR="${KOKORO_VENV:-$SCRIPT_DIR/../parakeet-stt/.venv}"

if [ ! -d "$VENV_DIR" ]; then
    echo "Venv not found. Run setup.sh first."
    exit 1
fi

export KOKORO_MODEL_DIR="${KOKORO_MODEL_DIR:-$SCRIPT_DIR/models/kokoro-en-v0_19}"
export KOKORO_PORT="${KOKORO_PORT:-6009}"
export KOKORO_THREADS="${KOKORO_THREADS:-4}"
export KOKORO_DEFAULT_SID="${KOKORO_DEFAULT_SID:-6}"  # am_michael

exec "$VENV_DIR/bin/python" "$SCRIPT_DIR/server.py"
