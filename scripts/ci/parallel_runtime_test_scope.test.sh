#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
classifier="${script_dir}/parallel_runtime_test_scope.sh"

expect_run() {
    local name="$1"
    shift

    if ! printf '%s\n' "$@" | bash "$classifier" >/dev/null; then
        echo "FAIL: expected parallel runtime tests for $name" >&2
        exit 1
    fi
}

expect_skip() {
    local name="$1"
    shift

    set +e
    printf '%s\n' "$@" | bash "$classifier" >/dev/null
    status=$?
    set -e

    if [ "$status" -ne 1 ]; then
        echo "FAIL: expected status 1 while skipping $name, got $status" >&2
        exit 1
    fi
}

expect_run "runtime source" "crates/zeroclaw-runtime/src/agent/loop_.rs"
expect_run "channel source" "crates/zeroclaw-channels/src/orchestrator/mod.rs"
expect_run "workspace manifest" "Cargo.toml"
expect_run "workspace lockfile" "Cargo.lock"
expect_run "quality workflow" ".github/workflows/ci.yml"
expect_run "parallel gate" "scripts/ci/parallel_runtime_test_gate.sh"
expect_run "scope classifier" "scripts/ci/parallel_runtime_test_scope.sh"
expect_run "mixed paths" "apps/zerocode/src/app.rs" "crates/zeroclaw-runtime/src/lib.rs"

expect_skip "ZeroCode-only changes" "apps/zerocode/src/app.rs"
expect_skip "web-only changes" "web/src/pages/AgentChat.tsx"
expect_skip "docs-only changes" "docs/book/src/contributing/testing.md"
expect_skip "unrelated crate changes" "crates/zeroclaw-providers/src/openai.rs"
expect_skip "empty input"

echo "parallel runtime test scope tests: pass"
