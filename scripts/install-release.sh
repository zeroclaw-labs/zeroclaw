#!/usr/bin/env sh
# Backward-compatible entrypoint for older install docs that still fetch
# scripts/install-release.sh directly.
set -eu

repo="${ZEROCLAW_INSTALL_REPO:-zeroclaw-labs/zeroclaw}"
ref="${ZEROCLAW_INSTALL_REF:-main}"
script_url="${ZEROCLAW_INSTALL_SH_URL:-https://raw.githubusercontent.com/${repo}/${ref}/install.sh}"

if command -v curl >/dev/null 2>&1; then
  curl -fsSL "$script_url" | bash -s -- "$@"
  exit $?
fi

if command -v wget >/dev/null 2>&1; then
  wget -qO- "$script_url" | bash -s -- "$@"
  exit $?
fi

echo "error: curl or wget is required to download $script_url" >&2
exit 1
