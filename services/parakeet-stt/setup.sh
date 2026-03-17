#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MODEL_DIR="$SCRIPT_DIR/models"
MODEL_NAME="sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"
MODEL_URL="https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/${MODEL_NAME}.tar.bz2"
VENV_DIR="$SCRIPT_DIR/.venv"

echo "=== Parakeet TDT STT Server Setup ==="
echo ""

# ── Prerequisites ──────────────────────────────────────────────

if ! command -v python3 &>/dev/null; then
    echo "ERROR: python3 is required."
    exit 1
fi

if ! command -v ffmpeg &>/dev/null; then
    echo "ERROR: ffmpeg is required for audio format conversion."
    echo "       Install with:  brew install ffmpeg"
    exit 1
fi

# ── Python venv ────────────────────────────────────────────────

if [ ! -d "$VENV_DIR" ]; then
    echo "Creating Python virtual environment..."
    python3 -m venv "$VENV_DIR"
fi

echo "Installing Python dependencies..."
"$VENV_DIR/bin/pip" install --quiet --upgrade pip
"$VENV_DIR/bin/pip" install --quiet -r "$SCRIPT_DIR/requirements.txt"

# ── Model download ─────────────────────────────────────────────

mkdir -p "$MODEL_DIR"

if [ ! -d "$MODEL_DIR/$MODEL_NAME" ]; then
    echo ""
    echo "Downloading Parakeet TDT 0.6B v3 INT8 model (~465 MB compressed)..."
    curl -fSL "$MODEL_URL" | tar xjf - -C "$MODEL_DIR"
    echo "Model extracted to $MODEL_DIR/$MODEL_NAME"
else
    echo "Model already present at $MODEL_DIR/$MODEL_NAME"
fi

# ── Verify ─────────────────────────────────────────────────────

echo ""
echo "Verifying model files..."
for f in encoder.int8.onnx decoder.int8.onnx joiner.int8.onnx tokens.txt; do
    if [ ! -f "$MODEL_DIR/$MODEL_NAME/$f" ]; then
        echo "ERROR: Missing $f in $MODEL_DIR/$MODEL_NAME"
        exit 1
    fi
done
echo "All model files present."

echo ""
echo "=== Setup complete ==="
echo ""
echo "Start the server:"
echo "  $SCRIPT_DIR/run.sh"
echo ""
echo "Test with:"
echo "  curl -X POST http://localhost:6008/v1/audio/transcriptions \\"
echo "    -F 'file=@test.wav' -F 'model=parakeet' -F 'response_format=json'"
