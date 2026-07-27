#!/usr/bin/env bash

set -euo pipefail

while IFS= read -r path; do
    case "$path" in
        Cargo.toml|Cargo.lock|\
        .github/workflows/ci.yml|\
        crates/zeroclaw-runtime/*|\
        crates/zeroclaw-channels/*|\
        scripts/ci/parallel_runtime_test_*.sh)
            echo "parallel runtime test scope matched: $path"
            exit 0
            ;;
    esac
done

echo "parallel runtime test scope not matched"
exit 1
