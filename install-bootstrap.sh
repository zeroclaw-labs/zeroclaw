#!/bin/sh
# install-bootstrap.sh — install the thin `zeroclaw-bootstrap` MCP launcher.
#
#   curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install-bootstrap.sh | sh
#
# The launcher is the small distribution client an MCP host (Claude Code,
# Codex, …) runs to reach — or first install — a configurable ZeroClaw. It is
# shipped as its OWN per-platform release asset because its whole job is to
# exist BEFORE the ZeroClaw binary does; it is NOT inside the zeroclaw archive.
#
# This installer only places the launcher on PATH. It never installs ZeroClaw
# itself (the launcher's `install` refuses without a human at a terminal), never
# edits config, and approves nothing.
#
# Windows is served by scoop, not this script.
set -eu

REPO="zeroclaw-labs/zeroclaw"
BIN_NAME="zeroclaw-bootstrap"
BIN_DIR="${ZEROCLAW_BIN_DIR:-$HOME/.local/bin}"

info() { printf '\033[0;34m==>\033[0m %s\n' "$1" >&2; }
warn() { printf '\033[0;33mwarning:\033[0m %s\n' "$1" >&2; }
die()  { printf '\033[0;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

detect_libc() {
  # musl if the loader reports musl (Alpine and friends); glibc otherwise.
  if ldd --version 2>&1 | grep -qi musl; then echo musl; else echo gnu; fi
}

# Mirror install.sh's detect_target_triple so a machine resolves to the same
# triple the main installer and the launcher's own registry use.
detect_triple() {
  os=$(uname -s)
  arch=$(uname -m)
  case "$os" in
  Darwin)
    # Rosetta reports x86_64 from `uname -m` on Apple Silicon; consult sysctl.
    if [ "$arch" = "arm64" ] || [ "$(sysctl -n hw.optional.arm64 2>/dev/null)" = "1" ]; then
      echo "aarch64-apple-darwin"
    else
      echo "x86_64-apple-darwin"
    fi
    ;;
  Linux)
    libc=$(detect_libc)
    case "$arch" in
    x86_64) echo "x86_64-unknown-linux-${libc}" ;;
    aarch64 | arm64) echo "aarch64-unknown-linux-${libc}" ;;
    armv7l) echo "armv7-unknown-linux-gnueabihf" ;;
    armv6l | arm*) echo "arm-unknown-linux-gnueabihf" ;;
    *) die "unsupported architecture: $arch" ;;
    esac
    ;;
  *) die "unsupported OS: $os (Windows: install the launcher via scoop)" ;;
  esac
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    die "no checksum tool (sha256sum/shasum) available"
  fi
}

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar >/dev/null 2>&1 || die "tar is required"

triple=$(detect_triple)
info "Detected platform: $triple"

version=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
  | grep -m1 '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')
[ -n "$version" ] || die "could not resolve the latest release tag from GitHub"
info "Latest release: $version"

asset="zeroclaw-bootstrap-${triple}.tar.gz"
base="https://github.com/${REPO}/releases/download/${version}"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

info "Downloading $asset"
curl -fsSL "${base}/${asset}" -o "$tmp/$asset" \
  || die "no prebuilt launcher for $triple in $version (build from source: cargo build --release -p zeroclaw-bootstrap)"

# Verify against the release SHA256SUMS, exactly as install.sh does for the main
# binary. A missing SHA256SUMS warns rather than aborts (best-effort releases).
if curl -fsSL "${base}/SHA256SUMS" -o "$tmp/SHA256SUMS" 2>/dev/null; then
  expected=$(awk -v f="$asset" '$2 == f {print $1}' "$tmp/SHA256SUMS")
  [ -n "$expected" ] || die "SHA256SUMS has no entry for $asset"
  actual=$(sha256_of "$tmp/$asset")
  [ "$expected" = "$actual" ] || die "checksum mismatch for $asset (expected $expected, got $actual)"
  info "Checksum verified"
else
  warn "could not fetch SHA256SUMS — skipping checksum verification"
fi

tar xzf "$tmp/$asset" -C "$tmp"
[ -f "$tmp/$BIN_NAME" ] || die "archive did not contain $BIN_NAME"
mkdir -p "$BIN_DIR"
if command -v install >/dev/null 2>&1; then
  install -m 0755 "$tmp/$BIN_NAME" "$BIN_DIR/$BIN_NAME"
else
  cp "$tmp/$BIN_NAME" "$BIN_DIR/$BIN_NAME" && chmod 0755 "$BIN_DIR/$BIN_NAME"
fi
info "Installed: $BIN_DIR/$BIN_NAME"

case ":$PATH:" in
*":$BIN_DIR:"*) : ;;
*) warn "$BIN_DIR is not on your PATH — add it: export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac

cat >&2 <<EOF

Register it as an MCP server in your harness (ONE server — it becomes the
ZeroClaw control surface itself after \`bootstrap.handoff\`, on the same pipe):

  Claude Code:
      claude mcp add zeroclaw-bootstrap -- zeroclaw-bootstrap mcp

  Codex  (~/.codex/config.toml):
      [mcp_servers.zeroclaw-bootstrap]
      command = "zeroclaw-bootstrap"
      args = ["mcp"]

Or drive it directly:  zeroclaw-bootstrap status
EOF
