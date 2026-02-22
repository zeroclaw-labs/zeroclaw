#!/usr/bin/env bash
set -euo pipefail

REPO="zeroclaw-labs/zeroclaw"
API_URL="https://api.github.com/repos/${REPO}/releases/latest"
RELEASE_BASE="https://github.com/${REPO}/releases/latest/download"

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: required command not found: $1" >&2
    exit 1
  }
}

run_privileged() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    sudo "$@"
  else
    echo "error: sudo is required to install into $INSTALL_DIR" >&2
    exit 1
  fi
}

linux_triple() {
  local arch
  arch="$(uname -m)"
  case "$arch" in
    x86_64|amd64) echo "x86_64-unknown-linux-gnu" ;;
    aarch64|arm64) echo "aarch64-unknown-linux-gnu" ;;
    armv7l|armv7) echo "armv7-unknown-linux-gnueabihf" ;;
    *)
      echo "error: unsupported Linux architecture: $arch" >&2
      echo "supported: x86_64, aarch64, armv7" >&2
      exit 1
      ;;
  esac
}

pick_install_dir() {
  if [ -n "${ZEROCLAW_INSTALL_DIR:-}" ]; then
    echo "$ZEROCLAW_INSTALL_DIR"
    return 0
  fi

  if [ -d "$HOME/.cargo/bin" ]; then
    echo "$HOME/.cargo/bin"
    return 0
  fi

  echo "$HOME/.local/bin"
}

NO_ONBOARD=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --no-onboard)
      NO_ONBOARD=1
      ;;
    -h|--help)
      cat <<'EOF'
Usage: install-release.sh [--no-onboard]

Installs the latest Linux ZeroClaw binary from official GitHub releases.

Options:
  --no-onboard   Install only; do not run `zeroclaw onboard`

Environment:
  ZEROCLAW_INSTALL_DIR  Override install directory
EOF
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      exit 1
      ;;
  esac
  shift
done

if [ "$(uname -s)" != "Linux" ]; then
  echo "error: this installer currently supports Linux only." >&2
  exit 1
fi

need_cmd curl
need_cmd tar
need_cmd mktemp
need_cmd install

TRIPLE="$(linux_triple)"
ASSET="zeroclaw-${TRIPLE}.tar.gz"
DOWNLOAD_URL="${RELEASE_BASE}/${ASSET}"

TMP_DIR="$(mktemp -d)"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

echo "==> Checking latest release metadata from ${REPO}"
if ! curl -fsSL "$API_URL" >/dev/null; then
  echo "error: unable to reach GitHub release API" >&2
  exit 1
fi

echo "==> Downloading ${ASSET}"
curl -fL "$DOWNLOAD_URL" -o "$TMP_DIR/$ASSET"

echo "==> Extracting release archive"
tar -xzf "$TMP_DIR/$ASSET" -C "$TMP_DIR"
if [ ! -f "$TMP_DIR/zeroclaw" ]; then
  echo "error: release archive does not contain expected 'zeroclaw' binary" >&2
  exit 1
fi

INSTALL_DIR="$(pick_install_dir)"
BIN_PATH="${INSTALL_DIR}/zeroclaw"

if [ "${INSTALL_DIR#/usr/local/}" != "$INSTALL_DIR" ]; then
  run_privileged mkdir -p "$INSTALL_DIR"
  run_privileged install -m 0755 "$TMP_DIR/zeroclaw" "$BIN_PATH"
else
  mkdir -p "$INSTALL_DIR"
  install -m 0755 "$TMP_DIR/zeroclaw" "$BIN_PATH"
fi

echo "==> Installed: $BIN_PATH"
if ! command -v zeroclaw >/dev/null 2>&1; then
  echo "note: '$INSTALL_DIR' may not be in PATH for this shell yet." >&2
  echo "      run: export PATH=\"$INSTALL_DIR:\$PATH\"" >&2
fi

"$BIN_PATH" --version || true

if [ "$NO_ONBOARD" -eq 1 ]; then
  echo "==> Skipping onboarding (--no-onboard)"
  exit 0
fi

echo "==> Starting onboarding"
exec "$BIN_PATH" onboard
