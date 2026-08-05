#!/usr/bin/env bash

set -euo pipefail

scope=none

while IFS= read -r path; do
    case "$path" in
        Cargo.toml|Cargo.lock|\
        .github/workflows/ci.yml|\
        scripts/ci/parallel_runtime_test_*.sh)
            scope=all
            ;;
        crates/zeroclaw-runtime/*)
            scope=all
            ;;
        crates/zeroclaw-channels/*)
            if [ "$scope" = none ]; then
                scope=channels
            fi
            ;;
    esac
done

if [ "$scope" = none ]; then
    echo "parallel runtime test scope not matched" >&2
    exit 1
fi

echo "$scope"
