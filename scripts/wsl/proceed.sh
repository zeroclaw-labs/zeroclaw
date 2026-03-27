#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." >/dev/null 2>&1 && pwd)"
cd "$ROOT"

echo "Repo: $ROOT"
echo "Step 1: bootstrap libs (dry-run)"
scripts/wsl/bootstrap-libs.sh --dry-run

echo "Step 2: sync from archive (dry-run with safety guard)"
if scripts/wsl/sync-from-win-archive.sh --dry-run; then
  echo "Sync check: source has newer content worth syncing."
else
  echo "Sync check: skipped by guard (archive source is older than WSL primary)."
  echo "This is expected once WSL becomes the active primary."
fi

echo "Step 3: current git status"
git status --short --branch

echo "Proceed checks completed successfully."
