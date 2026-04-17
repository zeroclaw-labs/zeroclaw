#!/usr/bin/env bash
# scripts/deploy.sh — thin wrapper around videoclaw-ops/tooling/deploy.sh.
#
# ── DO NOT EDIT LOGIC HERE ──
# All deploy logic lives in videoclaw-ops/tooling/deploy.sh so that we get
# one implementation / one bug-fix lane. This wrapper only:
#   1. locates videoclaw-ops on the filesystem
#   2. sets the service name (from zeroclaw placeholder)
#   3. forwards flags
#
# To regenerate: copy this file as `scripts/deploy.sh` in the service repo
# and replace `zeroclaw` with the service key in services.yaml.

set -euo pipefail

SERVICE_NAME="zeroclaw"

# ── Locate videoclaw-ops ──────────────────────────────────────────
# Priority:
#   1. $VIDEOCLAW_OPS_DIR env var (explicit override).
#   2. sibling checkout: ../videoclaw-ops from this repo's root.
#   3. ~/Documents/GitHub/videoclaw-ops (our macOS convention).
find_ops_dir() {
  if [[ -n "${VIDEOCLAW_OPS_DIR:-}" ]] ; then
    echo "${VIDEOCLAW_OPS_DIR}"; return
  fi
  local repo_root
  repo_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
  local candidate="${repo_root}/../videoclaw-ops"
  if [[ -d "$candidate/tooling" ]] ; then
    (cd "$candidate" && pwd); return
  fi
  candidate="${HOME}/Documents/GitHub/videoclaw-ops"
  if [[ -d "$candidate/tooling" ]] ; then
    echo "$candidate"; return
  fi
  echo "" # not found
}

OPS_DIR="$(find_ops_dir)"
if [[ -z "$OPS_DIR" ]] ; then
  echo "[wrapper] ERROR: videoclaw-ops checkout not found." >&2
  echo "[wrapper] set VIDEOCLAW_OPS_DIR or clone alongside this repo." >&2
  exit 2
fi

exec "${OPS_DIR}/tooling/deploy.sh" "${SERVICE_NAME}" "$@"
