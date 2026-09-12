#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
helper="${script_dir}/apt_install.sh"
test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

assert_contains() {
    local file="$1"
    local expected="$2"

    grep -Fq -- "$expected" "$file" || \
        fail "missing output: $expected"
}

assert_call_count() {
    local log_file="$1"
    local expected="$2"
    local call="$3"
    local actual

    actual="$(grep -Fxc -- "$call" "$log_file" || true)"
    if [[ "$actual" -ne "$expected" ]]; then
        fail "expected $expected '$call' calls, got $actual"
    fi
}

new_fixture() {
    local name="$1"

    fixture="${test_root}/${name}"
    mkdir -p "${fixture}/bin" "${fixture}/sources" "${fixture}/state"
    calls="${fixture}/calls.log"
    output="${fixture}/output.log"
    : > "$calls"

    touch \
        "${fixture}/sources/azure-cli.sources" \
        "${fixture}/sources/microsoft-prod.list" \
        "${fixture}/sources/keep.sources"

    cat > "${fixture}/bin/sudo" <<'EOF'
#!/usr/bin/env bash
printf 'sudo' >> "$ZEROCLAW_APT_TEST_CALLS"
printf '|%s' "$@" >> "$ZEROCLAW_APT_TEST_CALLS"
printf '\n' >> "$ZEROCLAW_APT_TEST_CALLS"
"$@"
EOF

    cat > "${fixture}/bin/timeout" <<'EOF'
#!/usr/bin/env bash
printf 'timeout' >> "$ZEROCLAW_APT_TEST_CALLS"
printf '|%s' "$@" >> "$ZEROCLAW_APT_TEST_CALLS"
printf '\n' >> "$ZEROCLAW_APT_TEST_CALLS"

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --signal=*|--kill-after=*) shift ;;
        *s) shift; break ;;
        *) break ;;
    esac
done

phase="${2:-unknown}"
count_file="${ZEROCLAW_APT_TEST_STATE}/timeout-${phase}"
count=0
if [[ -f "$count_file" ]]; then
    count="$(cat "$count_file")"
fi
count=$((count + 1))
printf '%s\n' "$count" > "$count_file"

if [[ "${ZEROCLAW_APT_TEST_TIMEOUT_PHASE:-}" == "$phase" ]] && \
   [[ "$count" -le "${ZEROCLAW_APT_TEST_TIMEOUT_ATTEMPTS:-0}" ]]; then
    exit 124
fi

"$@"
EOF

    cat > "${fixture}/bin/apt-get" <<'EOF'
#!/usr/bin/env bash
printf 'apt-get' >> "$ZEROCLAW_APT_TEST_CALLS"
printf '|%s' "$@" >> "$ZEROCLAW_APT_TEST_CALLS"
printf '\n' >> "$ZEROCLAW_APT_TEST_CALLS"

phase="${1:-unknown}"
count_file="${ZEROCLAW_APT_TEST_STATE}/apt-get-${phase}"
count=0
if [[ -f "$count_file" ]]; then
    count="$(cat "$count_file")"
fi
count=$((count + 1))
printf '%s\n' "$count" > "$count_file"

if [[ "${ZEROCLAW_APT_TEST_FAIL_PHASE:-}" == "$phase" ]] && \
   [[ "$count" -le "${ZEROCLAW_APT_TEST_FAIL_ATTEMPTS:-0}" ]]; then
    exit 42
fi
EOF

    cat > "${fixture}/bin/sleep" <<'EOF'
#!/usr/bin/env bash
printf 'sleep' >> "$ZEROCLAW_APT_TEST_CALLS"
printf '|%s' "$@" >> "$ZEROCLAW_APT_TEST_CALLS"
printf '\n' >> "$ZEROCLAW_APT_TEST_CALLS"
EOF

    chmod +x "${fixture}/bin/sudo" "${fixture}/bin/timeout" \
        "${fixture}/bin/apt-get" "${fixture}/bin/sleep"
}

run_helper() {
    PATH="${fixture}/bin:${PATH}" \
        ZEROCLAW_CI_APT_SOURCES_DIR="${fixture}/sources" \
        ZEROCLAW_APT_TEST_CALLS="$calls" \
        ZEROCLAW_APT_TEST_STATE="${fixture}/state" \
        ZEROCLAW_APT_TEST_FAIL_PHASE="${fail_phase:-}" \
        ZEROCLAW_APT_TEST_FAIL_ATTEMPTS="${fail_attempts:-0}" \
        ZEROCLAW_APT_TEST_TIMEOUT_PHASE="${timeout_phase:-}" \
        ZEROCLAW_APT_TEST_TIMEOUT_ATTEMPTS="${timeout_attempts:-0}" \
        bash "$helper" "$@" > "$output" 2>&1
}

new_fixture success
run_helper libudev-dev ripgrep
assert_call_count "$calls" 1 'apt-get|update|-qq'
assert_call_count "$calls" 1 'apt-get|install|-y|libudev-dev|ripgrep'
assert_contains "$output" 'apt refresh: attempt 1/2 started (deadline: 120s)'
assert_contains "$output" 'apt install: attempt 1/2 succeeded'
[[ ! -e "${fixture}/sources/azure-cli.sources" ]] || \
    fail "Azure source was not removed"
[[ ! -e "${fixture}/sources/microsoft-prod.list" ]] || \
    fail "Microsoft source was not removed"
[[ -e "${fixture}/sources/keep.sources" ]] || \
    fail "unrelated source was removed"

new_fixture transient_failure
fail_phase=update
fail_attempts=1
run_helper libudev-dev
assert_call_count "$calls" 2 'apt-get|update|-qq'
assert_call_count "$calls" 1 'sleep|5'
assert_contains "$output" 'apt refresh: attempt 1/2 failed (status: 42)'
assert_contains "$output" 'apt refresh: retrying after 5s'
assert_contains "$output" 'apt refresh: attempt 2/2 succeeded'
unset fail_phase fail_attempts

new_fixture transient_timeout
timeout_phase=install
timeout_attempts=1
run_helper libudev-dev
assert_call_count "$calls" 2 'timeout|--signal=TERM|--kill-after=10s|120s|apt-get|install|-y|libudev-dev'
assert_call_count "$calls" 1 'apt-get|install|-y|libudev-dev'
assert_contains "$output" 'apt install: attempt 1/2 timed out (status: 124)'
assert_contains "$output" 'apt install: attempt 2/2 succeeded'
unset timeout_phase timeout_attempts

new_fixture permanent_install_failure
fail_phase=install
fail_attempts=2
set +e
run_helper libudev-dev ripgrep
status=$?
set -e
if [[ "$status" -ne 42 ]]; then
    fail "permanent install failure should exit 42, got $status"
fi
assert_call_count "$calls" 2 'apt-get|install|-y|libudev-dev|ripgrep'
assert_contains "$output" 'apt install: attempt 2/2 failed (status: 42)'
assert_contains "$output" 'apt install: failed after 2 attempts (last status: 42)'
unset fail_phase fail_attempts

new_fixture permanent_timeout
timeout_phase=update
timeout_attempts=2
set +e
run_helper libudev-dev
status=$?
set -e
if [[ "$status" -ne 124 ]]; then
    fail "permanent timeout should exit 124, got $status"
fi
assert_call_count "$calls" 2 'timeout|--signal=TERM|--kill-after=10s|120s|apt-get|update|-qq'
assert_call_count "$calls" 0 'apt-get|update|-qq'
assert_contains "$output" 'apt refresh: failed after 2 attempts (last status: 124)'
unset timeout_phase timeout_attempts

echo "apt install timeout/retry tests: pass"
