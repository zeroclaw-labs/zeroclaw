#!/usr/bin/env bash
# deploy-sg-dev.sh — DEPRECATED. Use `./scripts/deploy.sh` instead.
#
# This script is kept as a shim so that old muscle memory + CI references
# continue to work. The real implementation now lives in
# videoclaw-ops/tooling/deploy.sh (single source of truth across all
# VideoClaw services).
#
# Key behaviour changes:
#   - The new flow opens a PR on videoclaw-ops (respects the pre-push
#     hook). Previously this script pushed directly to main.
#   - `--reason "<text>"` is now REQUIRED. Pass it through, e.g.:
#       ./scripts/deploy-sg-dev.sh --reason "hotfix CUD-1842"
#   - After PR merge, ArgoCD + image-updater sync the cluster (~3 min).
#
# See docs in videoclaw-ops/tooling/README.md.

set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

cat >&2 <<'BANNER'
┌────────────────────────────────────────────────────────────────────┐
│ NOTICE: scripts/deploy-sg-dev.sh is DEPRECATED.                    │
│                                                                    │
│ Forwarding to scripts/deploy.sh (new canonical wrapper, see        │
│ videoclaw-ops/tooling/README.md for details).                      │
│                                                                    │
│ Migrate your muscle memory: use ./scripts/deploy.sh directly.      │
└────────────────────────────────────────────────────────────────────┘
BANNER

# Forward all args; add --env dev default for back-compat if none given.
if ! printf '%s\n' "$@" | grep -q -- '--env' ; then
  set -- --env dev "$@"
fi

exec "${script_dir}/deploy.sh" "$@"
