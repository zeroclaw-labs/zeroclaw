#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
VENV_DIR="$SCRIPT_DIR/.venv"

if [ ! -d "$VENV_DIR" ]; then
    echo "Virtual environment not found. Run setup.sh first:"
    echo "  $SCRIPT_DIR/setup.sh"
    exit 1
fi

export PARAKEET_MODEL_DIR="${PARAKEET_MODEL_DIR:-$SCRIPT_DIR/models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8}"
export PARAKEET_PORT="${PARAKEET_PORT:-6008}"
export PARAKEET_THREADS="${PARAKEET_THREADS:-4}"

exec "$VENV_DIR/bin/python" "$SCRIPT_DIR/server.py"
