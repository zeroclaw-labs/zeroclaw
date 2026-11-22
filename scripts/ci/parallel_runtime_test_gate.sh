#!/usr/bin/env bash

set -euo pipefail

runs="${ZEROCLAW_PARALLEL_TEST_RUNS:-3}"
threads="${ZEROCLAW_PARALLEL_TEST_THREADS:-16}"
scope="${ZEROCLAW_PARALLEL_TEST_SCOPE:-all}"
pass_timeout="${ZEROCLAW_PARALLEL_TEST_PASS_TIMEOUT_SECONDS:-300}"

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

case "$pass_timeout" in
    ''|*[!0-9]*|0)
        echo "ZEROCLAW_PARALLEL_TEST_PASS_TIMEOUT_SECONDS must be a positive integer (got: $pass_timeout)."
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

# A stopped test must fail its pass, not sit until the job is cancelled. GNU
# timeout signals the whole process group, so the test binary goes with Cargo.
deadline=()
if command -v timeout >/dev/null 2>&1; then
    deadline=(timeout --signal=TERM --kill-after=10s "${pass_timeout}s")
    deadline_note="deadline ${pass_timeout}s"
else
    echo "timeout is not installed; passes run without a deadline."
    deadline_note="no deadline"
fi

for crate in "${crates[@]}"; do
    # Compile outside the deadline so a cold build is never mistaken for a hang.
    cargo test --locked --quiet -p "$crate" --lib --no-run
    for ((run = 1; run <= runs; run++)); do
        echo "==> parallel runtime regression: $crate run $run/$runs ($threads threads, $deadline_note)"
        status=0
        ${deadline[@]+"${deadline[@]}"} cargo test --locked --quiet -p "$crate" --lib -- --test-threads="$threads" || status=$?
        case "$status" in
            0) ;;
            124|137)
                echo "::error::$crate run $run/$runs did not finish within ${pass_timeout}s. The last 'has been running for over 60 seconds' line above names the test that stopped."
                exit "$status"
                ;;
            *) exit "$status" ;;
        esac
    done
done
