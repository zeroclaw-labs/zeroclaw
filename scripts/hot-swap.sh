#!/usr/bin/env bash
# scripts/hot-swap.sh — thin wrapper around videoclaw-ops/tooling/hot-swap.sh.
#
# ── DO NOT EDIT LOGIC HERE ──
# Same pattern as scripts/deploy.sh. See videoclaw-ops/tooling/README.md.

set -euo pipefail

SERVICE_NAME="zeroclaw"

find_ops_dir() {
    if [[ -n "${VIDEOCLAW_OPS_DIR:-}" ]] ; then
        echo "${VIDEOCLAW_OPS_DIR}"; return
    fi
    local repo_root
    repo_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
    local candidate="${repo_root}/../videoclaw-ops"
    if [[ -d "$candidate/tooling" ]] ; then (cd "$candidate" && pwd); return; fi
    candidate="${HOME}/Documents/GitHub/videoclaw-ops"
    if [[ -d "$candidate/tooling" ]] ; then echo "$candidate"; return; fi
    echo ""
}

OPS_DIR="$(find_ops_dir)"
if [[ -z "$OPS_DIR" ]] ; then
    echo "[wrapper] ERROR: videoclaw-ops checkout not found." >&2
    echo "[wrapper] set VIDEOCLAW_OPS_DIR or clone alongside this repo." >&2
    exit 2
fi

exec "${OPS_DIR}/tooling/hot-swap.sh" "${SERVICE_NAME}" "$@"
