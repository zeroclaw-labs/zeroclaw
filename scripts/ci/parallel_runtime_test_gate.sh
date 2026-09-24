#!/usr/bin/env bash

set -euo pipefail

runs="${ZEROCLAW_PARALLEL_TEST_RUNS:-3}"
threads="${ZEROCLAW_PARALLEL_TEST_THREADS:-16}"
scope="${ZEROCLAW_PARALLEL_TEST_SCOPE:-all}"

# Runtime binaries use 8 MiB Tokio worker stacks. Keep this full-path stress
# gate on the same stack contract while still allowing callers to override it.
export RUST_MIN_STACK="${RUST_MIN_STACK:-8388608}"

case "$runs" in
    ''|*[!0-9]*|0)
        echo "ZEROCLAW_PARALLEL_TEST_RUNS must be a positive integer (got: $runs)."
        exit 2
        ;;
esac

case "$threads" in
    ''|*[!0-9]*|0)
        echo "ZEROCLAW_PARALLEL_TEST_THREADS must be a positive integer (got: $threads)."
        exit 2
        ;;
esac

case "$scope" in
    all)
        crates=(zeroclaw-runtime zeroclaw-channels)
        ;;
    channels)
        crates=(zeroclaw-channels)
        ;;
    *)
        echo "ZEROCLAW_PARALLEL_TEST_SCOPE must be 'channels' or 'all' (got: $scope)."
        exit 2
        ;;
esac

for crate in "${crates[@]}"; do
    for ((run = 1; run <= runs; run++)); do
        echo "==> parallel runtime regression: $crate run $run/$runs ($threads threads)"
        cargo test --locked --quiet -p "$crate" --lib -- --test-threads="$threads"
    done
done
