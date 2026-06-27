#!/usr/bin/env bash
# Low-storage / MacBook Air friendly path.
# Runs the ESP32 simulator + visualizer directly on your host (no Docker required for the sim).
#
# This is the recommended way to test the vignette when you don't have 60-80+ GB free for Docker Desktop.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "=== ZeroClaw ESP32 Smart Room – Host (low storage) mode ==="
echo

if ! command -v socat >/dev/null 2>&1; then
  echo "socat is required. Install with: brew install socat"
  exit 1
fi

# Ensure .env exists and is not empty
if [[ ! -s demo/.env ]]; then
  echo "demo/.env is missing or empty."
  echo "Run these first:"
  echo "  cp demo/.env.template demo/.env"
  echo "  nano demo/.env     # or: code demo/.env   or: vim demo/.env"
  echo "Fill in OPENROUTER_API_KEY and TELEGRAM_BOT_TOKEN, then re-run this script."
  exit 1
fi

# Make sure a config exists for the agent later
mkdir -p demo/data/config
cp -n demo/zeroclaw.toml.example demo/data/config/config.toml 2>/dev/null || true

bind_addr="${1:-127.0.0.1:8080}"
if [[ $# -gt 0 ]]; then
  shift
fi

echo "Starting esp32_sim + visualizer directly on host..."
echo "  Frontend will be at http://${bind_addr} (localhost for safety in host mode)"
echo "  In another terminal run the agent with:"
echo "    ./demo/run-agent-host.sh"
echo
echo "Then send the printed /bind code to Telegram, paste the primer from demo/PROMPTS.md, and try natural language."
echo

# Default to localhost bind for host-mode safety (user can override with extra args if wanted).
exec cargo run -p zeroclaw-hardware --example esp32_sim --features "hardware dev-sim" -- "${bind_addr}" "$@"
