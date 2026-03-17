#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MODEL_DIR="$SCRIPT_DIR/models"
MODEL_NAME="kokoro-en-v0_19"
MODEL_URL="https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/${MODEL_NAME}.tar.bz2"

# Reuse the Parakeet STT venv (sherpa-onnx already installed)
VENV_DIR="${KOKORO_VENV:-$SCRIPT_DIR/../parakeet-stt/.venv}"

echo "=== Kokoro TTS Server Setup ==="

if [ ! -d "$VENV_DIR" ]; then
    echo "ERROR: Parakeet venv not found at $VENV_DIR"
    echo "       Run parakeet-stt/setup.sh first, or set KOKORO_VENV"
    exit 1
fi

# Install additional deps (soundfile for WAV encoding)
echo "Installing additional dependencies..."
"$VENV_DIR/bin/pip" install --quiet soundfile

# Download model
mkdir -p "$MODEL_DIR"
if [ ! -d "$MODEL_DIR/$MODEL_NAME" ]; then
    echo "Downloading Kokoro v0.19 English model..."
    curl -fSL "$MODEL_URL" | tar xjf - -C "$MODEL_DIR"
    echo "Model extracted to $MODEL_DIR/$MODEL_NAME"
else
    echo "Model already present at $MODEL_DIR/$MODEL_NAME"
fi

echo ""
echo "=== Setup complete ==="
echo "Start: $SCRIPT_DIR/run.sh"
