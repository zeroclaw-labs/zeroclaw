#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
runner="${script_dir}/run_clippy.sh"
mock_dir="$(mktemp -d)"
trap 'rm -rf "$mock_dir"' EXIT

cargo_args="${mock_dir}/cargo.args"
summary_file="${mock_dir}/summary.md"
runner_temp="${mock_dir}/runner-temp"
mkdir -p "${mock_dir}/bin" "$runner_temp"

cat > "${mock_dir}/bin/cargo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$ZEROCLAW_CLIPPY_TEST_ARGS"
printf '%s\n' \
    'Downloaded demo-crate v1.0.0' \
    'Compiling zeroclaw-runtime v0.8.5 (/workspace/zeroclaw/crates/zeroclaw-runtime)' \
    'Compiling serde v1.0.0'
exit "${ZEROCLAW_CLIPPY_TEST_STATUS:-0}"
EOF
chmod +x "${mock_dir}/bin/cargo"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

assert_args() {
    local name="$1"
    shift
    local actual
    local expected

    actual="$(cat "$cargo_args")"
    expected="$(printf '%s\n' "$@")"
    if [ "$actual" != "$expected" ]; then
        printf 'FAIL: unexpected Cargo arguments for %s\nexpected:\n%s\nactual:\n%s\n' \
            "$name" "$expected" "$actual" >&2
        exit 1
    fi
}

assert_summary_line() {
    local expected="$1"
    grep -Fqx -- "$expected" "$summary_file" || \
        fail "missing summary line: $expected"
}

run_runner() {
    : > "$cargo_args"
    : > "$summary_file"
    PATH="${mock_dir}/bin:$PATH" \
        RUNNER_TEMP="$runner_temp" \
        GITHUB_STEP_SUMMARY="$summary_file" \
        RUNNER_OS=TestOS \
        RUST_CACHE_HIT=true \
        ZEROCLAW_CLIPPY_TEST_ARGS="$cargo_args" \
        bash "$runner" "$@" >/dev/null
}

run_runner \
    --scope workspace \
    --summary-title "Lint diagnostics" \
    --log-name cargo-clippy.log
assert_args "workspace" \
    clippy \
    --locked \
    --workspace \
    --exclude zeroclaw-desktop \
    --all-targets \
    --features ci-all \
    -- -D warnings
assert_summary_line '### Lint diagnostics'
assert_summary_line "| Runner OS | \`TestOS\` |"
assert_summary_line "| Rust cache exact hit | \`true\` |"
assert_summary_line "| Clippy status | \`0\` |"
assert_summary_line "| Workspace path compile lines | \`1\` |"
assert_summary_line "| Total compile lines | \`2\` |"
assert_summary_line "| Downloaded crate lines | \`1\` |"

run_runner \
    --scope workspace \
    --target aarch64-apple-darwin \
    --summary-title "Cross-platform Clippy diagnostics: aarch64-apple-darwin" \
    --log-name cargo-clippy-aarch64-apple-darwin.log
assert_args "targeted workspace" \
    clippy \
    --locked \
    --workspace \
    --exclude zeroclaw-desktop \
    --all-targets \
    --features ci-all \
    --target aarch64-apple-darwin \
    -- -D warnings
assert_summary_line "| Target | \`aarch64-apple-darwin\` |"

run_runner \
    --scope tools \
    --target x86_64-pc-windows-msvc \
    --summary-title "Targeted Windows Clippy diagnostics" \
    --log-name cargo-clippy-windows-tools.log
assert_args "targeted tools" \
    clippy \
    --locked \
    -p zeroclaw-tools \
    --all-targets \
    --all-features \
    --target x86_64-pc-windows-msvc \
    --no-deps \
    -- -D warnings

assert_rejected() {
    local name="$1"
    shift
    local status

    : > "$cargo_args"
    set +e
    PATH="${mock_dir}/bin:$PATH" \
        RUNNER_TEMP="$runner_temp" \
        GITHUB_STEP_SUMMARY="$summary_file" \
        ZEROCLAW_CLIPPY_TEST_ARGS="$cargo_args" \
        bash "$runner" "$@" >/dev/null 2>&1
    status=$?
    set -e

    if [ "$status" -ne 2 ]; then
        fail "$name should exit 2, got $status"
    fi
    if [ -s "$cargo_args" ]; then
        fail "$name invoked Cargo"
    fi
}

assert_rejected "invalid scope" \
    --scope invalid \
    --summary-title "Invalid scope" \
    --log-name invalid.log
assert_rejected "tools scope without target" \
    --scope tools \
    --summary-title "Missing target" \
    --log-name missing-target.log

: > "$summary_file"
set +e
PATH="${mock_dir}/bin:$PATH" \
    RUNNER_TEMP="$runner_temp" \
    GITHUB_STEP_SUMMARY="$summary_file" \
    RUNNER_OS=TestOS \
    RUST_CACHE_HIT=false \
    ZEROCLAW_CLIPPY_TEST_ARGS="$cargo_args" \
    ZEROCLAW_CLIPPY_TEST_STATUS=17 \
    bash "$runner" \
        --scope workspace \
        --summary-title "Failure diagnostics" \
        --log-name cargo-clippy-failure.log >/dev/null
status=$?
set -e

if [ "$status" -ne 17 ]; then
    fail "expected Cargo status 17, got $status"
fi
assert_summary_line "| Clippy status | \`17\` |"

echo "shared Clippy runner tests: pass"
