#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" >/dev/null 2>&1 && pwd || pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." >/dev/null 2>&1 && pwd || pwd)"

info() {
  echo "==> $*"
}

warn() {
  echo "warning: $*" >&2
}

error() {
  echo "error: $*" >&2
  exit 1
}

usage() {
  cat <<'USAGE'
One-click local refresh for ZeroClaw development.

Usage:
  scripts/dev-restart.sh [--release|--debug] [--skip-build]

Behavior:
  1) Build current repo source (debug or release)
  2) Create/update user-level zeroclaw command symlink
  3) Ensure local config exists (first run only)

Options:
  --release      Build release binary (default)
  --debug        Build debug binary
  --skip-build   Re-link and config-check only
  -h, --help     Show this help

Notes:
  - This script never overwrites existing config.toml.
  - After script completes, run `zeroclaw gateway` directly.
USAGE
}

BUILD_PROFILE="release"
SKIP_BUILD=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release)
      BUILD_PROFILE="release"
      ;;
    --debug)
      BUILD_PROFILE="debug"
      ;;
    --skip-build)
      SKIP_BUILD=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      error "unknown argument: $1"
      ;;
  esac
  shift
done

if ! command -v cargo >/dev/null 2>&1; then
  error "cargo is required but not found"
fi

cd "$REPO_ROOT"

TARGET_BIN="$REPO_ROOT/target/$BUILD_PROFILE/zeroclaw"

if [[ "$SKIP_BUILD" -eq 0 ]]; then
  info "Building ZeroClaw ($BUILD_PROFILE)"
  if [[ "$BUILD_PROFILE" == "release" ]]; then
    cargo build --locked --release
  else
    cargo build --locked
  fi
else
  info "Skipping build by request"
fi

if [[ ! -x "$TARGET_BIN" ]]; then
  error "build output not found: $TARGET_BIN"
fi

LINK_DIR="${ZEROCLAW_DEV_BIN_DIR:-$HOME/.local/bin}"
LINK_PATH="$LINK_DIR/zeroclaw"
mkdir -p "$LINK_DIR"
ln -sfn "$TARGET_BIN" "$LINK_PATH"
info "Linked command: $LINK_PATH -> $TARGET_BIN"

if [[ ":$PATH:" != *":$LINK_DIR:"* ]]; then
  warn "$LINK_DIR is not in PATH for current shell"
  warn "run: export PATH=\"$LINK_DIR:\$PATH\""
fi

CONFIG_DIR="${ZEROCLAW_CONFIG_DIR:-$HOME/.zeroclaw}"
CONFIG_PATH="$CONFIG_DIR/config.toml"
TEMPLATE_PATH="$REPO_ROOT/dev/config.template.toml"

mkdir -p "$CONFIG_DIR"
if [[ ! -f "$CONFIG_PATH" ]]; then
  if [[ -f "$TEMPLATE_PATH" ]]; then
    cp "$TEMPLATE_PATH" "$CONFIG_PATH"
    info "Initialized config: $CONFIG_PATH"
  else
    warn "template missing: $TEMPLATE_PATH"
    warn "create config manually at: $CONFIG_PATH"
  fi
else
  info "Keeping existing config: $CONFIG_PATH"
fi

info "Done. You can now run: zeroclaw gateway"
"$TARGET_BIN" --version || true
