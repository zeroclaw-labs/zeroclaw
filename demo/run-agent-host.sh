#!/usr/bin/env bash
# Companion to run-sim-host.sh for low-storage testing.
# Runs the agent binary directly on your host, talking to the simulator pty.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "Starting channel agent (host mode) talking to the simulator pty..."
echo "Make sure ./demo/run-sim-host.sh is already running in another terminal."
echo

# Ensure config
mkdir -p demo/data/config
cp -n demo/zeroclaw.toml.example demo/data/config/config.toml 2>/dev/null || true

# Load .env if present (for API keys).
if [[ -f demo/.env ]]; then
  set -a
  # shellcheck disable=SC1091
  source demo/.env
  set +a
fi

if [[ -z "${OPENROUTER_API_KEY:-}" ]]; then
  echo "OPENROUTER_API_KEY is missing. Set it in demo/.env and re-run this script."
  exit 1
fi

if [[ -z "${TELEGRAM_BOT_TOKEN:-}" ]]; then
  echo "TELEGRAM_BOT_TOKEN is missing. Set it in demo/.env and re-run this script."
  exit 1
fi

if [[ "${TELEGRAM_BOT_TOKEN}" != *:* ]]; then
  echo "TELEGRAM_BOT_TOKEN is set but does not look like a BotFather token (missing ':')."
  exit 1
fi

echo "Credential checks:"
echo "  OPENROUTER_API_KEY: set (${#OPENROUTER_API_KEY} chars)"
echo "  TELEGRAM_BOT_TOKEN: set (${#TELEGRAM_BOT_TOKEN} chars, format ok)"
echo

# Keep demo/.env as the source of truth for secrets. These schema-mirror env
# overrides feed the current runtime config without persisting secrets to TOML.
export ZEROCLAW_providers__models__openrouter__agent_demo__api_key="${OPENROUTER_API_KEY}"
export ZEROCLAW_channels__telegram__default__bot_token="${TELEGRAM_BOT_TOKEN}"

exec cargo run --bin zeroclaw --no-default-features --features "agent-runtime hardware dev-sim channel-telegram" \
  -- channel start --config-dir demo/data/config "$@"
