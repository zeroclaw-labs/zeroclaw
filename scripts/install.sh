#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" >/dev/null 2>&1 && pwd || pwd)"
BOOTSTRAP_LOCAL="$SCRIPT_DIR/bootstrap.sh"
REPO_URL="https://github.com/zeroclaw-labs/zeroclaw.git"

echo "[deprecated] scripts/install.sh -> bootstrap.sh" >&2

if [[ -f "$BOOTSTRAP_LOCAL" ]]; then
  exec "$BOOTSTRAP_LOCAL" "$@"
fi

if ! command -v git >/dev/null 2>&1; then
  echo "error: git is required for legacy install.sh remote mode" >&2
  exit 1
fi

TEMP_DIR="$(mktemp -d -t zeroclaw-install-XXXXXX)"
cleanup() {
  rm -rf "$TEMP_DIR"
}
trap cleanup EXIT

git clone --depth 1 "$REPO_URL" "$TEMP_DIR" >/dev/null 2>&1

if [[ -x "$TEMP_DIR/scripts/bootstrap.sh" ]]; then
  "$TEMP_DIR/scripts/bootstrap.sh" "$@"
  exit 0
fi

echo "[deprecated] cloned revision has no bootstrap.sh; falling back to legacy source install flow" >&2

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  cat <<'USAGE'
Legacy install.sh fallback mode

Behavior:
  - Clone repository
  - cargo build --release --locked
  - cargo install --path <clone> --force --locked

For the new dual-mode installer, use:
  ./bootstrap.sh --help
USAGE
  exit 0
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is required for legacy install.sh fallback mode" >&2
  echo "Install Rust first: https://rustup.rs/" >&2
  exit 1
fi

cargo build --release --locked --manifest-path "$TEMP_DIR/Cargo.toml"
cargo install --path "$TEMP_DIR" --force --locked

echo "Legacy source install completed." >&2
