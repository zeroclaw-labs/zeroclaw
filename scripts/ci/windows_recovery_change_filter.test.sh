#!/usr/bin/env bash
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fixture="$(mktemp)"
trap 'rm -f "$fixture"' EXIT

check() {
    local expected="$1" event="$2"
    shift 2
    printf '%s\n' "$@" > "$fixture"
    actual="$(bash "$script_dir/windows_recovery_change_filter.sh" "$event" "$fixture")"
    [[ "$actual" == "windows_recovery=$expected" ]] || {
        echo "Unexpected recovery selection: $actual ($event: $*)" >&2
        exit 1
    }
}

check false pull_request docs/README.md crates/zeroclaw-config/src/policy.rs web/src/main.ts
for path in crates/zeroclaw-runtime/src/control_plane/authority.rs \
    crates/zeroclaw-runtime/src/control_plane/task_registry.rs \
    crates/zeroclaw-runtime/src/lib.rs Cargo.lock Cargo.toml \
    crates/zeroclaw-runtime/Cargo.toml crates/zeroclaw-runtime/build.rs \
    rust-toolchain.toml .cargo/config.toml .github/actions/rust-cache/action.yml \
    .github/workflows/ci.yml scripts/ci/windows_recovery_change_filter.sh \
    scripts/ci/windows_recovery_change_filter.test.sh; do
    check true pull_request "$path" docs/README.md
done
check true push docs/README.md
check true merge_group docs/README.md
: > "$fixture"
[[ "$(bash "$script_dir/windows_recovery_change_filter.sh" pull_request "$fixture")" == windows_recovery=true ]]
[[ "$(bash "$script_dir/windows_recovery_change_filter.sh" pull_request "$fixture.missing")" == windows_recovery=true ]]
echo 'Windows recovery selection: pass'
