#!/usr/bin/env bash
# test-tui-onboarding.sh — Build and launch the TUI onboarding wizard for manual QA.
#
# Usage:
#   ./dev/test-tui-onboarding.sh          # dev build (faster compile)
#   ./dev/test-tui-onboarding.sh release  # release build (optimized)
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

PROFILE="${1:-dev}"
BLUE='\033[0;34m'
GREEN='\033[0;32m'
RED='\033[0;31m'
BOLD='\033[1m'
DIM='\033[2m'
RESET='\033[0m'

echo -e "${BLUE}${BOLD}TUI Onboarding Test Script${RESET}"
echo -e "${DIM}Branch: $(git branch --show-current)${RESET}"
echo -e "${DIM}Profile: ${PROFILE}${RESET}"
echo

# ── Step 1: Build ────────────────────────────────────────────────────
echo -e "${BOLD}[1/3] Building zeroclaw (${PROFILE})...${RESET}"
if [[ "$PROFILE" == "release" ]]; then
  cargo build --release 2>&1
  BIN="$REPO_ROOT/target/release/zeroclaw"
else
  cargo build 2>&1
  BIN="$REPO_ROOT/target/debug/zeroclaw"
fi

if [[ ! -x "$BIN" ]]; then
  echo -e "${RED}Build failed — binary not found at ${BIN}${RESET}"
  exit 1
fi

echo -e "${GREEN}Build OK${RESET}"
echo

# ── Step 2: Verify --tui flag ────────────────────────────────────────
echo -e "${BOLD}[2/3] Verifying --tui flag...${RESET}"
if "$BIN" onboard --help 2>&1 | grep -q -- '--tui'; then
  echo -e "${GREEN}--tui flag present${RESET}"
else
  echo -e "${RED}--tui flag NOT found in onboard --help${RESET}"
  exit 1
fi
echo

# ── Step 3: Launch TUI ──────────────────────────────────────────────
echo -e "${BOLD}[3/3] Launching TUI onboarding wizard...${RESET}"
echo -e "${DIM}Navigate with arrow keys / j/k, Enter to select, Esc to go back, Ctrl+C to quit.${RESET}"
echo -e "${DIM}Walk through every screen to verify feature parity with OpenClaw.${RESET}"
echo
echo -e "${BOLD}Checklist:${RESET}"
echo "  [ ] Welcome screen renders with ZEROCLAW banner"
echo "  [ ] Security warning panel with full text + y/N prompt"
echo "  [ ] Setup mode selection (QuickStart / Full / Skip)"
echo "  [ ] Existing config detected panel"
echo "  [ ] Config handling (Use existing / Overwrite)"
echo "  [ ] QuickStart summary (gateway port, bind, auth, tailscale)"
echo "  [ ] Provider selection (8 providers)"
echo "  [ ] Auth method selection"
echo "  [ ] API key input (masked)"
echo "  [ ] Provider notes panel"
echo "  [ ] Model configured panel"
echo "  [ ] Default model selection (7 models)"
echo "  [ ] Channel status panel (24 channels with status)"
echo "  [ ] How channels work info panel"
echo "  [ ] Channel selection (22 channels + skip)"
echo "  [ ] Web search info panel"
echo "  [ ] Web search provider selection"
echo "  [ ] Web search API key input"
echo "  [ ] Skills status panel"
echo "  [ ] Skills install selection (28 skills)"
echo "  [ ] Hooks info panel"
echo "  [ ] Hooks enable/skip selection"
echo "  [ ] Gateway service runtime panel"
echo "  [ ] Health check result"
echo "  [ ] Optional apps panel"
echo "  [ ] Control UI panel (dashboard URL)"
echo "  [ ] Workspace backup panel"
echo "  [ ] Final security reminder panel"
echo "  [ ] Web search confirmation panel"
echo "  [ ] What now panel"
echo "  [ ] Complete screen with full summary"
echo
echo -e "${BOLD}Press Enter to launch the TUI...${RESET}"
read -r

"$BIN" onboard --tui

echo
echo -e "${GREEN}${BOLD}TUI test complete.${RESET}"
