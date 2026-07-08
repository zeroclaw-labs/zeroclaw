#!/usr/bin/env bash
# Launch the ZeroClaw Tauri desktop app in dev mode.
#
# Assumes a ZeroClaw daemon is reachable on 127.0.0.1:42617. Run
# `zeroclaw daemon` (NOT `zeroclaw gateway start`) — only the daemon attaches
# the supervisor that powers the in-place reload the Quickstart triggers; a
# standalone gateway returns 503 on /admin/reload. An SSH port-forward from a
# remote daemon works too.
#
# Usage: ./dev/run-tauri-dev.sh

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# If a previous instance is still alive, single-instance plugin will block us.
pkill -f 'target/debug/zeroclaw-desktop' 2>/dev/null || true
sleep 0.5

cd "$REPO/apps/tauri"
# Prefer the Tauri CLI (hot-reload, proper dev harness). Fall back to a plain
# cargo run when tauri-cli isn't installed — the app embeds the splash via
# frontendDist, so it launches fine either way.
if cargo tauri --version >/dev/null 2>&1; then
  exec cargo tauri dev
else
  echo "tauri-cli not found (cargo install tauri-cli); falling back to 'cargo run'." >&2
  exec cargo run -p zeroclaw-desktop
fi
