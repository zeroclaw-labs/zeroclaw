#!/usr/bin/env bash

set -euo pipefail

runs="${ZEROCLAW_PARALLEL_TEST_RUNS:-3}"
threads="${ZEROCLAW_PARALLEL_TEST_THREADS:-16}"
scope="${ZEROCLAW_PARALLEL_TEST_SCOPE:-all}"
pass_timeout="${ZEROCLAW_PARALLEL_TEST_PASS_TIMEOUT_SECONDS:-300}"

# A leading zero is refused: bash arithmetic reads 010 as octal 8 and 08 as an
# error, and GNU timeout reads 00 as no deadline at all.
case "$runs" in
    ''|*[!0-9]*|0*)
        echo "ZEROCLAW_PARALLEL_TEST_RUNS must be a positive integer with no leading zero (got: $runs)."
        exit 2
        ;;
esac

case "$threads" in
    ''|*[!0-9]*|0*)
        echo "ZEROCLAW_PARALLEL_TEST_THREADS must be a positive integer with no leading zero (got: $threads)."
        exit 2
        ;;
esac

case "$pass_timeout" in
    ''|*[!0-9]*|0*)
        echo "ZEROCLAW_PARALLEL_TEST_PASS_TIMEOUT_SECONDS must be a positive integer with no leading zero (got: $pass_timeout)."
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

# That process group is not the terminal's, so Ctrl-C or a hangup reaches only
# this shell, which would wait for the pass and then start the next one. A timed
# pass therefore runs in the background while this shell waits for it. On a
# signal, ask timeout to stop the pass, which it does for the whole group, then
# end this shell by the same signal so whatever called it stops too.
pass_pid=""
pending_signal=""

# timeout exits as soon as Cargo does, so a test helper that ignores TERM can
# outlive the pass with nothing left to stop it. Give whatever is still in the
# pass's process group a moment to exit, then kill it.
stop_leftovers() {
    local i
    for ((i = 0; i < 10; i++)); do
        kill -0 -- "-$pass_pid" 2>/dev/null || return 0
        sleep 0.1
    done
    kill -s KILL -- "-$pass_pid" 2>/dev/null || true
}

stop_pass() {
    # A TERM that reaches the pass before it has started timeout is lost, so
    # repeat it until the pass has ended. The loop is bounded in case it never
    # does; wait then blocks as before.
    local i
    for ((i = 0; i < 150; i++)); do
        kill -TERM "$pass_pid" 2>/dev/null || break
        sleep 0.1
    done
    wait "$pass_pid" 2>/dev/null || true
    stop_leftovers
    trap - "$1"
    kill -s "$1" "$$"
}

# The traps go in before the pass starts, so a signal that arrives before its
# pid is known is kept and acted on once it is.
on_signal() {
    if [[ -n "$pass_pid" ]]; then
        stop_pass "$1"
    else
        pending_signal="$1"
    fi
}

for crate in "${crates[@]}"; do
    # Compile outside the deadline so a cold build is never mistaken for a hang.
    cargo test --locked --quiet -p "$crate" --lib --no-run
    for ((run = 1; run <= runs; run++)); do
        echo "==> parallel runtime regression: $crate run $run/$runs ($threads threads, $deadline_note)"
        status=0
        if ((${#deadline[@]})); then
            pass_pid=""
            trap 'on_signal INT' INT
            trap 'on_signal HUP' HUP
            trap 'on_signal TERM' TERM
            "${deadline[@]}" cargo test --locked --quiet -p "$crate" --lib -- --test-threads="$threads" &
            pass_pid=$!
            if [[ -n "$pending_signal" ]]; then
                stop_pass "$pending_signal"
            fi
            wait "$pass_pid" || status=$?
            trap - INT HUP TERM
            if ((status == 124 || status == 137)); then
                stop_leftovers
            fi
        else
            cargo test --locked --quiet -p "$crate" --lib -- --test-threads="$threads" || status=$?
        fi
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
