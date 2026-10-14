#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

# crates_preflight_trigger.sh: decide whether CI must run the crates.io
# package preflight for this change.
#
# Usage: scripts/ci/crates_preflight_trigger.sh <event> [<base-commit>]
#
# Prints run=true|false and, when true, tag=vX.Y.Z for $GITHUB_OUTPUT.
#
# The preflight runs when a pull request or merge-queue entry changes
# [workspace.package] version to a stable X.Y.Z. That change is the version
# bump that precedes a release, and it is the earliest point where the release
# version exists. Running there means a workspace whose crates cannot publish
# cannot merge the bump, well before the release run's own preflight. Other
# changes, and pushes to master, do not pay the cost: the release run verifies
# again before anything is published.

event="${1:-}"
base="${2:-}"

workspace_version() {
  # The first `version = "..."` line, which is [workspace.package]; the same
  # rule the release scripts use.
  sed -n 's/^version = "\([^"]*\)"/\1/p' | head -1
}

case "$event" in
  pull_request | merge_group) ;;
  *)
    echo "run=false"
    exit 0
    ;;
esac

if [[ -z "$base" ]] || ! git rev-parse -q --verify "${base}^{commit}" >/dev/null; then
  echo "::error::crates_preflight_trigger: base commit '${base}' is not available." >&2
  exit 1
fi

before="$(git show "${base}:Cargo.toml" | workspace_version)"
after="$(workspace_version <Cargo.toml)"

if [[ "$before" == "$after" ]]; then
  echo "run=false"
  exit 0
fi
if [[ ! "$after" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  # Only stable versions are published to crates.io.
  echo "Workspace version changed to '${after}', which is not a stable release; no crates.io preflight." >&2
  echo "run=false"
  exit 0
fi

echo "Workspace version changes from ${before} to ${after}; running the crates.io preflight." >&2
echo "run=true"
echo "tag=v${after}"
