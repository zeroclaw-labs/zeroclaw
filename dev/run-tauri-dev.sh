#!/usr/bin/env bash
# Launch the ZeroClaw Tauri desktop app in dev mode.
#
# Assumes the gateway is reachable on 127.0.0.1:42617 (locally running
# `zeroclaw gateway`, or an SSH port-forward from a remote host).
#
# Usage: ./dev/run-tauri-dev.sh

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# If a previous instance is still alive, single-instance plugin will block us.
pkill -f 'target/debug/zeroclaw-desktop' 2>/dev/null || true
sleep 0.5

cd "$REPO/apps/tauri"
exec cargo tauri dev
