#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Use the existing venv from the m4 benchmarks
VENV_DIR="${VIBEVOICE_VENV:-$HOME/code/m4_16GB_models/venv-vibetts}"

if [ ! -d "$VENV_DIR" ]; then
    echo "VibeVoice venv not found at $VENV_DIR"
    echo "Set VIBEVOICE_VENV to the correct path, or run setup.sh"
    exit 1
fi

export VIBEVOICE_DIR="${VIBEVOICE_DIR:-$HOME/code/m4_16GB_models/tools/VibeVoice}"
export VOICES_DIR="${VOICES_DIR:-$VIBEVOICE_DIR/demo/voices/streaming_model}"
export VIBEVOICE_MODEL="${VIBEVOICE_MODEL:-microsoft/VibeVoice-Realtime-0.5B}"
export VIBEVOICE_VOICE="${VIBEVOICE_VOICE:-en-Carter_man}"
export VIBEVOICE_PORT="${VIBEVOICE_PORT:-6009}"

# Ensure server deps are installed
"$VENV_DIR/bin/pip" install --quiet fastapi uvicorn soundfile 2>/dev/null || true

exec "$VENV_DIR/bin/python" "$SCRIPT_DIR/server.py"
