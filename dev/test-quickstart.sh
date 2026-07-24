#!/usr/bin/env bash
# test-quickstart.sh — Build and launch the Quickstart flow for manual QA.
#
# Usage:
#   ./dev/test-quickstart.sh          # dev build (faster compile)
#   ./dev/test-quickstart.sh release  # release build (optimized)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

PROFILE="${1:-dev}"
BOLD='\033[1m'
RESET='\033[0m'

case "$PROFILE" in
  release) cargo build --release --bin zeroclaw ;;
  dev|"")  cargo build --bin zeroclaw ;;
  *) echo "Usage: $0 [dev|release]" >&2; exit 2 ;;
esac

if [ "$PROFILE" = release ]; then
  BIN="$REPO_ROOT/target/release/zeroclaw"
else
  BIN="$REPO_ROOT/target/debug/zeroclaw"
fi

echo
echo -e "${BOLD}Checklist:${RESET}"
echo "  [ ] Quickstart prompts for provider type"
echo "  [ ] Quickstart accepts --model-provider / --model / --api-key / --agent flags non-interactively"
echo "  [ ] Quickstart writes a working config.toml with one [providers.models.<type>.<alias>] entry"
echo "  [ ] Quickstart writes one [agents.<alias>] entry bound to that provider"
echo "  [ ] Quickstart prints the next-step instructions (zeroclaw agent / zeroclaw daemon)"
echo "  [ ] Re-running quickstart on a configured install is idempotent (no destructive overwrite)"
echo
echo -e "${BOLD}Press Enter to launch quickstart...${RESET}"
read -r

"$BIN" quickstart
