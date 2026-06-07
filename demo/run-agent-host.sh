#!/usr/bin/env bash
# Companion to run-sim-host.sh for low-storage testing.
# Runs the agent binary directly on your host, talking to the simulator pty.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "Starting agent (host mode) talking to the simulator pty..."
echo "Make sure ./demo/run-sim-host.sh is already running in another terminal."
echo

# Ensure config
mkdir -p demo/data/config
cp -n demo/zeroclaw.toml.example demo/data/config/config.toml 2>/dev/null || true

# Load .env if present (for API keys)
if [[ -f demo/.env ]]; then
  set -a
  # shellcheck disable=SC1091
  source demo/.env
  set +a
fi

# Substitute Telegram bot token from env into the copied config (for easy test setup).
if [[ -n "${TELEGRAM_BOT_TOKEN:-}" ]]; then
  sed -i.bak "s/YOUR_TELEGRAM_BOT_TOKEN/${TELEGRAM_BOT_TOKEN}/g" demo/data/config/config.toml
fi

exec cargo run --bin zeroclaw --no-default-features --features "agent-runtime hardware dev-sim channel-telegram" \
  -- agent --config-dir demo/data/config --agent demo "$@"
