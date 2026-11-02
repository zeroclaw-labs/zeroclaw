#!/usr/bin/env bash
# Recovery entrypoint for the shared required-Windows selector. The metadata
# input must be from cargo metadata --no-deps; missing evidence selects execution.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
python3 "$script_dir/windows_test_scope.py" --required-jobs \
    --event "${1:-}" --changed-paths-file "${2:-}" \
    --metadata-file "${3:-}" --repo-root "${4:-$PWD}" \
    | grep -E '^windows_recovery=(true|false)$'
