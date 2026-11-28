#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
classifier="${script_dir}/parallel_runtime_test_scope.sh"
gate="${script_dir}/parallel_runtime_test_gate.sh"

expect_run() {
    local name="$1"
    local expected="$2"
    local actual
    local status
    shift 2

    set +e
    actual="$(printf '%s\n' "$@" | bash "$classifier")"
    status=$?
    set -e

    if [ "$status" -ne 0 ]; then
        echo "FAIL: expected parallel runtime tests for $name" >&2
        exit 1
    fi

    if [ "$actual" != "$expected" ]; then
        echo "FAIL: expected '$expected' for $name, got '$actual'" >&2
        exit 1
    fi
}

expect_skip() {
    local name="$1"
    shift

    set +e
    printf '%s\n' "$@" | bash "$classifier" >/dev/null 2>&1
    status=$?
    set -e

    if [ "$status" -ne 1 ]; then
        echo "FAIL: expected status 1 while skipping $name, got $status" >&2
        exit 1
    fi
}

expect_run "runtime source" "all" \
    "crates/zeroclaw-runtime/src/agent/loop_.rs"
expect_run "channel source" "channels" \
    "crates/zeroclaw-channels/src/orchestrator/mod.rs"
expect_run "workspace manifest" "all" "Cargo.toml"
expect_run "workspace lockfile" "all" "Cargo.lock"
expect_run "quality workflow" "all" ".github/workflows/ci.yml"
expect_run "parallel gate" "all" \
    "scripts/ci/parallel_runtime_test_gate.sh"
expect_run "scope classifier" "all" \
    "scripts/ci/parallel_runtime_test_scope.sh"
expect_run "scope fixture" "all" \
    "scripts/ci/parallel_runtime_test_scope.test.sh"
expect_run "channel then runtime" "all" \
    "crates/zeroclaw-channels/src/orchestrator/mod.rs" \
    "crates/zeroclaw-runtime/src/lib.rs"
expect_run "runtime then channel" "all" \
    "crates/zeroclaw-runtime/src/lib.rs" \
    "crates/zeroclaw-channels/src/orchestrator/mod.rs"

expect_skip "ZeroCode-only changes" "apps/zerocode/src/app.rs"
expect_skip "web-only changes" "web/src/pages/AgentChat.tsx"
expect_skip "docs-only changes" "docs/book/src/contributing/testing.md"
expect_skip "unrelated crate changes" "crates/zeroclaw-providers/src/openai.rs"
expect_skip "empty input"

mock_dir="$(mktemp -d)"
cargo_log="${mock_dir}/cargo.log"
trap 'rm -rf "$mock_dir"' EXIT

cat > "${mock_dir}/cargo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$ZEROCLAW_PARALLEL_TEST_CARGO_LOG"
EOF
chmod +x "${mock_dir}/cargo"

expect_cargo_log() {
    local name="$1"
    local actual
    local expected
    shift

    actual="$(cat "$cargo_log")"
    expected="$(printf '%s\n' "$@")"
    if [ "$actual" != "$expected" ]; then
        echo "FAIL: unexpected Cargo calls for $name" >&2
        printf 'expected:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
        exit 1
    fi
}

PATH="${mock_dir}:$PATH" \
    ZEROCLAW_PARALLEL_TEST_CARGO_LOG="$cargo_log" \
    ZEROCLAW_PARALLEL_TEST_RUNS=1 \
    ZEROCLAW_PARALLEL_TEST_SCOPE=channels \
    bash "$gate" >/dev/null

expect_cargo_log "channels scope" \
    'test --locked --quiet -p zeroclaw-channels --lib --no-run' \
    'test --locked --quiet -p zeroclaw-channels --lib -- --test-threads=16'

: > "$cargo_log"
PATH="${mock_dir}:$PATH" \
    ZEROCLAW_PARALLEL_TEST_CARGO_LOG="$cargo_log" \
    ZEROCLAW_PARALLEL_TEST_RUNS=1 \
    bash "$gate" >/dev/null

expect_cargo_log "default scope" \
    'test --locked --quiet -p zeroclaw-runtime --lib --no-run' \
    'test --locked --quiet -p zeroclaw-runtime --lib -- --test-threads=16' \
    'test --locked --quiet -p zeroclaw-channels --lib --no-run' \
    'test --locked --quiet -p zeroclaw-channels --lib -- --test-threads=16'

: > "$cargo_log"
PATH="${mock_dir}:$PATH" \
    ZEROCLAW_PARALLEL_TEST_CARGO_LOG="$cargo_log" \
    ZEROCLAW_PARALLEL_TEST_RUNS=1 \
    ZEROCLAW_PARALLEL_TEST_SCOPE='' \
    bash "$gate" >/dev/null

expect_cargo_log "empty scope" \
    'test --locked --quiet -p zeroclaw-runtime --lib --no-run' \
    'test --locked --quiet -p zeroclaw-runtime --lib -- --test-threads=16' \
    'test --locked --quiet -p zeroclaw-channels --lib --no-run' \
    'test --locked --quiet -p zeroclaw-channels --lib -- --test-threads=16'

: > "$cargo_log"
set +e
PATH="${mock_dir}:$PATH" \
    ZEROCLAW_PARALLEL_TEST_CARGO_LOG="$cargo_log" \
    ZEROCLAW_PARALLEL_TEST_RUNS=1 \
    ZEROCLAW_PARALLEL_TEST_SCOPE=unsupported \
    bash "$gate" >/dev/null 2>&1
status=$?
set -e
if [ "$status" -ne 2 ]; then
    echo "FAIL: unsupported scope should exit 2, got $status" >&2
    exit 1
fi
if [ -s "$cargo_log" ]; then
    echo "FAIL: unsupported scope invoked Cargo" >&2
    exit 1
fi

# Zero and zero-padded values must be refused before Cargo runs: 00 would run no
# passes or turn the deadline off, and 08 is an error in bash arithmetic.
for setting in RUNS THREADS PASS_TIMEOUT_SECONDS; do
    for value in 0 00 08; do
        : > "$cargo_log"
        set +e
        env PATH="${mock_dir}:$PATH" \
            ZEROCLAW_PARALLEL_TEST_CARGO_LOG="$cargo_log" \
            "ZEROCLAW_PARALLEL_TEST_${setting}=${value}" \
            bash "$gate" >/dev/null 2>&1
        status=$?
        set -e
        if [ "$status" -ne 2 ] || [ -s "$cargo_log" ]; then
            echo "FAIL: ZEROCLAW_PARALLEL_TEST_${setting}=${value} should exit 2 before Cargo runs, got $status" >&2
            exit 1
        fi
    done
done

# A pass that stops must fail at its deadline and say which pass it was. The
# build call returns at once; only the test run sleeps past the deadline.
if command -v timeout >/dev/null 2>&1; then
    # Each test pass also starts a helper that ignores TERM, as the TTS
    # fixtures in zeroclaw-channels do. timeout exits as soon as Cargo does, so
    # the gate has to stop such a helper itself.
    leftover_pid_file="${mock_dir}/leftover.pid"
    expect_no_leftover() {
        local name="$1" leftover_pid i
        leftover_pid="$(cat "$leftover_pid_file")"
        # An orphan disappears once init reaps it, which can take a moment.
        for ((i = 0; i < 20; i++)); do
            kill -0 "$leftover_pid" 2>/dev/null || return 0
            sleep 0.1
        done
        kill -s KILL "$leftover_pid" 2>/dev/null || true
        echo "FAIL: $name left a helper that ignores TERM running" >&2
        exit 1
    }

    cat > "${mock_dir}/cargo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$ZEROCLAW_PARALLEL_TEST_CARGO_LOG"
case "$*" in
    *--no-run*) exit 0 ;;
esac
bash -c 'trap "" TERM; while :; do sleep 0.1; done' &
echo "$!" > "$ZEROCLAW_PARALLEL_TEST_LEFTOVER_PID"
sleep 30
EOF
    chmod +x "${mock_dir}/cargo"
    : > "$cargo_log"
    rm -f "$leftover_pid_file"
    hang_output="${mock_dir}/hang.out"
    set +e
    PATH="${mock_dir}:$PATH" \
        ZEROCLAW_PARALLEL_TEST_CARGO_LOG="$cargo_log" \
        ZEROCLAW_PARALLEL_TEST_LEFTOVER_PID="$leftover_pid_file" \
        ZEROCLAW_PARALLEL_TEST_RUNS=2 \
        ZEROCLAW_PARALLEL_TEST_SCOPE=channels \
        ZEROCLAW_PARALLEL_TEST_PASS_TIMEOUT_SECONDS=1 \
        bash "$gate" > "$hang_output" 2>&1
    status=$?
    set -e
    if [ "$status" -ne 124 ]; then
        echo "FAIL: a stopped pass should exit 124, got $status" >&2
        exit 1
    fi
    if ! grep -Fq "zeroclaw-channels run 1/2 did not finish within 1s" "$hang_output"; then
        echo "FAIL: a stopped pass should be reported by crate and run" >&2
        cat "$hang_output" >&2
        exit 1
    fi
    expect_no_leftover "a stopped pass"
    expect_cargo_log "stopped pass ends the gate" \
        'test --locked --quiet -p zeroclaw-channels --lib --no-run' \
        'test --locked --quiet -p zeroclaw-channels --lib -- --test-threads=16'

    # Ctrl-C in a terminal, or the terminal closing, signals the gate's process
    # group but not the one timeout makes for the pass. The gate must stop the
    # pass and end by that signal instead of going on to run 2. Job control
    # gives the gate a process group of its own, as a terminal would.
    cat > "${mock_dir}/cargo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$ZEROCLAW_PARALLEL_TEST_CARGO_LOG"
case "$*" in
    *--no-run*) exit 0 ;;
esac
bash -c 'trap "" TERM; while :; do sleep 0.1; done' &
echo "$!" > "$ZEROCLAW_PARALLEL_TEST_LEFTOVER_PID"
echo "$$" > "$ZEROCLAW_PARALLEL_TEST_PASS_PID"
exec sleep 30
EOF
    chmod +x "${mock_dir}/cargo"
    pass_pid_file="${mock_dir}/pass.pid"
    for case_spec in INT:130 HUP:129 TERM:143; do
        signal="${case_spec%%:*}"
        expected_status="${case_spec##*:}"
        : > "$cargo_log"
        rm -f "$pass_pid_file" "$leftover_pid_file"
        set -m
        PATH="${mock_dir}:$PATH" \
            ZEROCLAW_PARALLEL_TEST_CARGO_LOG="$cargo_log" \
            ZEROCLAW_PARALLEL_TEST_PASS_PID="$pass_pid_file" \
            ZEROCLAW_PARALLEL_TEST_LEFTOVER_PID="$leftover_pid_file" \
            ZEROCLAW_PARALLEL_TEST_RUNS=2 \
            ZEROCLAW_PARALLEL_TEST_SCOPE=channels \
            ZEROCLAW_PARALLEL_TEST_PASS_TIMEOUT_SECONDS=60 \
            bash "$gate" >/dev/null 2>&1 &
        gate_pid=$!
        set +m
        for ((i = 0; i < 100; i++)); do
            [ -s "$pass_pid_file" ] && break
            sleep 0.1
        done
        if [ ! -s "$pass_pid_file" ]; then
            kill -s KILL -- "-$gate_pid" 2>/dev/null || true
            echo "FAIL: the gate's first pass did not start within 10s" >&2
            exit 1
        fi
        pass_pid="$(cat "$pass_pid_file")"
        # Let the gate reach its wait for the pass.
        sleep 0.5
        kill -s "$signal" -- "-$gate_pid"
        # The shell reports a job that a signal ended; that notice is expected.
        for ((i = 0; i < 50; i++)); do
            kill -0 "$gate_pid" || break
            sleep 0.1
        done 2>/dev/null
        if kill -0 "$gate_pid" 2>/dev/null; then
            kill -s KILL -- "-$gate_pid" 2>/dev/null || true
            kill -s KILL "$pass_pid" 2>/dev/null || true
            echo "FAIL: SIG$signal did not stop the gate within 5s" >&2
            exit 1
        fi
        status=0
        wait "$gate_pid" 2>/dev/null || status=$?
        if kill -0 "$pass_pid" 2>/dev/null; then
            kill -s KILL "$pass_pid" 2>/dev/null || true
            echo "FAIL: SIG$signal stopped the gate but left its pass running" >&2
            exit 1
        fi
        expect_no_leftover "SIG$signal"
        if [ "$status" -ne "$expected_status" ]; then
            echo "FAIL: SIG$signal should end the gate with status $expected_status, got $status" >&2
            exit 1
        fi
        expect_cargo_log "SIG$signal stops the gate" \
            'test --locked --quiet -p zeroclaw-channels --lib --no-run' \
            'test --locked --quiet -p zeroclaw-channels --lib -- --test-threads=16'
    done
fi

echo "parallel runtime test scope tests: pass"
