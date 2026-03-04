#!/usr/bin/env bash
set -euo pipefail

# Canonical remote installer entrypoint.
# Default behavior for no-arg interactive shells is TUI onboarding.

BOOTSTRAP_URL="${ZEROCLAW_BOOTSTRAP_URL:-https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/refs/heads/main/scripts/bootstrap.sh}"

have_cmd() {
  command -v "$1" >/dev/null 2>&1
}

run_remote_bootstrap() {
  local -a args=("$@")

  if have_cmd curl; then
    if [[ ${#args[@]} -eq 0 ]]; then
      curl -fsSL "$BOOTSTRAP_URL" | bash
    else
      curl -fsSL "$BOOTSTRAP_URL" | bash -s -- "${args[@]}"
    fi
    return 0
  fi

  if have_cmd wget; then
    if [[ ${#args[@]} -eq 0 ]]; then
      wget -qO- "$BOOTSTRAP_URL" | bash
    else
      wget -qO- "$BOOTSTRAP_URL" | bash -s -- "${args[@]}"
    fi
    return 0
  fi

  echo "error: curl or wget is required to run remote installer bootstrap." >&2
  return 1
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" >/dev/null 2>&1 && pwd || pwd)"
LOCAL_INSTALLER="$SCRIPT_DIR/zeroclaw_install.sh"

declare -a FORWARDED_ARGS=("$@")
# In piped one-liners (`curl ... | bash`) stdin is not a TTY; prefer the
# controlling terminal when available so interactive onboarding is still default.
if [[ $# -eq 0 && -t 1 ]] && (: </dev/tty) 2>/dev/null; then
  FORWARDED_ARGS=(--interactive-onboard)
fi

if [[ -x "$LOCAL_INSTALLER" ]]; then
  exec "$LOCAL_INSTALLER" "${FORWARDED_ARGS[@]}"
fi

run_remote_bootstrap "${FORWARDED_ARGS[@]}"
