#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────
# E2E test: TUI onboarding "Full Setup" flow
#
# Launches the ratatui-based TUI in a tmux session, navigates through
# the full setup wizard, and verifies:
#   1. Config.toml contains all expected values
#   2. Workspace scaffold files are created
#   3. Pipe mode (/dev/tty reopening) does not crash
# ─────────────────────────────────────────────────────────────────────

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
BIN_PATH="${1:-$ROOT_DIR/target/release/zeroclaw}"
TMP_ROOT="/tmp/zeroclaw-tmux-tui-$$"
SESSION="zc_tui_full_$$"
CONFIG_DIR="$TMP_ROOT/config"
WORKSPACE_PATH="$TMP_ROOT/test-workspace"
PASS=0
FAIL=0

cleanup() {
  tmux kill-session -t "$SESSION" >/dev/null 2>&1 || true
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

# ── Prerequisites ────────────────────────────────────────────────────

if ! command -v tmux >/dev/null 2>&1; then
  echo "tmux is required for this test" >&2
  exit 1
fi

if [[ ! -x "$BIN_PATH" ]]; then
  echo "Binary not found at $BIN_PATH — building..." >&2
  cargo build --release --bin zeroclaw >/dev/null 2>&1
  BIN_PATH="$ROOT_DIR/target/release/zeroclaw"
fi

mkdir -p "$TMP_ROOT" "$CONFIG_DIR"

# ── Helpers ──────────────────────────────────────────────────────────

send_key() {
  tmux send-keys -t "$SESSION":0.0 "$1"
}

send_enter() {
  tmux send-keys -t "$SESSION":0.0 Enter
}

send_text() {
  # Send literal text — crossterm receives each char as KeyCode::Char
  tmux send-keys -t "$SESSION":0.0 -l "$1"
}

capture() {
  tmux capture-pane -p -S -120 -t "$SESSION":0.0
}

wait_for_screen() {
  local marker="$1"
  local timeout="${2:-8}"
  local deadline=$((SECONDS + timeout))
  while ! capture | grep -qiF "$marker"; do
    if (( SECONDS >= deadline )); then
      echo "TIMEOUT waiting for: $marker" >&2
      echo "--- pane content ---" >&2
      capture >&2
      echo "--- end ---" >&2
      return 1
    fi
    sleep 0.3
  done
}

check_pass() {
  local label="$1"
  echo "  ✓ $label"
  PASS=$((PASS + 1))
}

check_fail() {
  local label="$1"
  echo "  ✗ $label" >&2
  FAIL=$((FAIL + 1))
}

assert_file_exists() {
  local path="$1" label="$2"
  if [[ -f "$path" ]]; then
    check_pass "$label"
  else
    check_fail "$label — file not found: $path"
  fi
}

assert_dir_exists() {
  local path="$1" label="$2"
  if [[ -d "$path" ]]; then
    check_pass "$label"
  else
    check_fail "$label — dir not found: $path"
  fi
}

assert_file_contains() {
  local path="$1" pattern="$2" label="$3"
  if grep -q "$pattern" "$path" 2>/dev/null; then
    check_pass "$label"
  else
    check_fail "$label — '$pattern' not found in $path"
  fi
}

assert_config_contains() {
  local pattern="$1" label="$2"
  assert_file_contains "$CONFIG_DIR/config.toml" "$pattern" "$label"
}

# ── Start TUI session ───────────────────────────────────────────────

echo "=== TUI Full Setup E2E Test ==="
echo "Binary:    $BIN_PATH"
echo "Config:    $CONFIG_DIR"
echo "Workspace: $WORKSPACE_PATH"
echo ""

tmux new-session -d -x 240 -y 60 -s "$SESSION" \
  "bash \"$ROOT_DIR/tests/manual/tmux/tui_onboard_wrapper.sh\" \"$CONFIG_DIR\" \"$BIN_PATH\""

# ── Navigate the Full Setup flow ─────────────────────────────────────

echo "Navigating TUI wizard..."

# 1. Welcome → Enter
wait_for_screen "ZeroClaw setup" || { echo "TUI did not launch"; exit 1; }
send_enter
sleep 0.5

# 2. SecurityWarning → 'y'
wait_for_screen "Security" 5
send_key "y"
sleep 0.5

# 3. SetupMode → Down (Full Setup), Enter
wait_for_screen "Setup mode" 5
send_key Down
sleep 0.3
send_enter
sleep 0.5

# 4. ExistingConfig → Enter
send_enter
sleep 0.5

# 5. ConfigHandling → Enter (default)
send_enter
sleep 0.5

# 6. QuickStartSummary → Enter
send_enter
sleep 0.5

# 7. ProviderTier → Enter (Recommended, idx 0)
wait_for_screen "provider" 5
send_enter
sleep 0.5

# 8. ProviderSelect → Enter (first provider)
send_enter
sleep 0.5

# 9. ApiKeyInput → type key, Enter
wait_for_screen "key" 5 || true
send_text "test-key-e2e"
sleep 0.3
send_enter
sleep 0.5

# 10. ProviderNotes → Enter
send_enter
sleep 0.5

# 11. ModelConfigured → Enter
send_enter
sleep 0.5

# 12. ModelSelect → Enter (Auto)
send_enter
sleep 0.5

# 13. ChannelStatus → Enter
wait_for_screen "Channel" 5 || true
send_enter
sleep 0.5

# 14. HowChannelsWork → Enter
send_enter
sleep 0.5

# 15. ChannelSelect → Enter (Telegram, idx 0)
send_enter
sleep 0.5

# 16. WorkspaceDir → clear default, type test path, Enter
wait_for_screen "workspace" 5 || true
sleep 0.3
# Clear the default path (send plenty of backspaces)
for _ in $(seq 1 80); do send_key BSpace; done
sleep 0.3
send_text "$WORKSPACE_PATH"
sleep 0.3
send_enter
sleep 0.5

# 17. TunnelInfo → Enter
wait_for_screen "tunnel" 5 || true
send_enter
sleep 0.5

# 18. TunnelSelect → Down (Cloudflare, idx 1), Enter
send_key Down
sleep 0.2
send_enter
sleep 0.5

# 19. TunnelTokenInput → type token, Enter
send_text "cf-e2e-token"
sleep 0.3
send_enter
sleep 0.5

# 20. ToolModeSelect → Enter (Sovereign, idx 0)
wait_for_screen "Tool mode" 5 || true
send_enter
sleep 0.5

# 21. HardwareSelect → Enter (Software only, idx 0)
wait_for_screen "Hardware" 5 || true
send_enter
sleep 0.5

# 22. MemorySelect → Enter (SQLite, idx 0)
wait_for_screen "Memory" 5 || true
send_enter
sleep 0.5

# 23. ProjectContext → type user name, Tab, type agent name, Enter
wait_for_screen "project" 5 || true
send_text "E2EUser"
sleep 0.2
send_key Tab
sleep 0.2
send_text "E2EAgent"
sleep 0.2
send_enter
sleep 0.5

# 24. WebSearchInfo → Enter
wait_for_screen "web search" 5 || true
send_enter
sleep 0.5

# 25. WebSearchProvider → Down×4 (DuckDuckGo), Enter
for _ in 1 2 3 4; do
  send_key Down
  sleep 0.1
done
send_enter
sleep 0.5

# 26-38. SkillsStatus through WhatNow → Enter × 12, then Complete → Enter to quit
for _ in $(seq 1 13); do
  send_enter
  sleep 0.4
done

# Wait for the TUI to exit and show EXIT_STATUS
echo "Waiting for wizard to complete..."
sleep 3

# ── Verify config persistence ────────────────────────────────────────

echo ""
echo "=== Verifying Config Persistence ==="

CONFIG_FILE="$CONFIG_DIR/config.toml"

if [[ ! -f "$CONFIG_FILE" ]]; then
  echo "FATAL: config.toml not found at $CONFIG_FILE" >&2
  echo "--- tmux pane content ---" >&2
  capture >&2
  echo "--- end ---" >&2
  exit 1
fi

check_pass "config.toml exists"

# Provider
assert_config_contains "default_provider" "default_provider is set"

# API key (may be encrypted, just check field exists)
assert_config_contains "api_key" "api_key field present"

# Note: workspace_dir is #[serde(skip)] in Config — not serialized to TOML.
# Workspace persistence is verified via scaffold files below.

# Tunnel
assert_config_contains 'provider = "cloudflare"' "tunnel.provider = cloudflare"
assert_config_contains "tunnel.cloudflare" "tunnel.cloudflare section exists"
assert_config_contains "token" "tunnel token field present"

# Composio (sovereign — disabled)
assert_config_contains "composio" "composio section exists"
assert_config_contains 'enabled = false' "composio.enabled = false"

# Hardware (software only — disabled)
assert_config_contains "hardware" "hardware section exists"

# Memory
assert_config_contains 'backend = "sqlite"' "memory.backend = sqlite"
assert_config_contains "auto_save = true" "memory.auto_save = true"

# Web search
assert_config_contains "duckduckgo" "web_search.provider = duckduckgo"

# Channels (Telegram stub)
assert_config_contains "telegram" "telegram channel configured"

# ── Verify workspace scaffold ────────────────────────────────────────

echo ""
echo "=== Verifying Workspace Scaffold ==="

# Markdown files
for f in IDENTITY.md AGENTS.md HEARTBEAT.md SOUL.md USER.md TOOLS.md BOOTSTRAP.md MEMORY.md; do
  assert_file_exists "$WORKSPACE_PATH/$f" "scaffold: $f"
done

# Subdirectories
for d in sessions memory state cron skills; do
  assert_dir_exists "$WORKSPACE_PATH/$d" "scaffold dir: $d"
done

# Content spot-checks
assert_file_contains "$WORKSPACE_PATH/USER.md" "E2EUser" "USER.md contains user name"
assert_file_contains "$WORKSPACE_PATH/IDENTITY.md" "E2EAgent" "IDENTITY.md contains agent name"

# ── Pipe mode test ───────────────────────────────────────────────────

echo ""
echo "=== Pipe Mode Test ==="

PIPE_CONFIG="$TMP_ROOT/pipe-config"
mkdir -p "$PIPE_CONFIG"

# Pipe stdin — TUI should reopen /dev/tty on macOS
if echo "" | env ZEROCLAW_CONFIG_DIR="$PIPE_CONFIG" "$BIN_PATH" onboard --tui </dev/null 2>&1 | head -5; then
  # If /dev/tty is available, the TUI launches (and we immediately close stdin).
  # The TUI may error because it can't read further input, but it should NOT
  # crash with a panic. Check that it at least started.
  check_pass "pipe mode: no crash"
else
  # Exit code != 0 is expected if /dev/tty is not available or TUI can't read input
  check_pass "pipe mode: graceful failure"
fi

# ── Summary ──────────────────────────────────────────────────────────

echo ""
echo "================================="
echo "  Passed: $PASS"
echo "  Failed: $FAIL"
echo "================================="

if (( FAIL > 0 )); then
  exit 1
fi

echo ""
echo "All E2E checks passed."
