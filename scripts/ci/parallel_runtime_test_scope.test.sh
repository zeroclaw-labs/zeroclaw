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
    'test --locked --quiet -p zeroclaw-channels --lib -- --test-threads=16'

: > "$cargo_log"
PATH="${mock_dir}:$PATH" \
    ZEROCLAW_PARALLEL_TEST_CARGO_LOG="$cargo_log" \
    ZEROCLAW_PARALLEL_TEST_RUNS=1 \
    bash "$gate" >/dev/null

expect_cargo_log "default scope" \
    'test --locked --quiet -p zeroclaw-runtime --lib -- --test-threads=16' \
    'test --locked --quiet -p zeroclaw-channels --lib -- --test-threads=16'

: > "$cargo_log"
PATH="${mock_dir}:$PATH" \
    ZEROCLAW_PARALLEL_TEST_CARGO_LOG="$cargo_log" \
    ZEROCLAW_PARALLEL_TEST_RUNS=1 \
    ZEROCLAW_PARALLEL_TEST_SCOPE='' \
    bash "$gate" >/dev/null

expect_cargo_log "empty scope" \
    'test --locked --quiet -p zeroclaw-runtime --lib -- --test-threads=16' \
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

echo "parallel runtime test scope tests: pass"
