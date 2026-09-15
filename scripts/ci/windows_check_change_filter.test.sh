#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
classifier="${script_dir}/windows_check_change_filter.sh"

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

# Runs the classifier from the fixture checkout with an explicit BASE_SHA
# (empty means unset) so an ambient BASE_SHA cannot change the result.
classify() {
    local base="$1"
    shift
    if [ -n "$base" ]; then
        (cd "$fixture" && printf '%s\n' "$@" | BASE_SHA="$base" bash "$classifier")
    else
        (cd "$fixture" && printf '%s\n' "$@" | env -u BASE_SHA bash "$classifier")
    fi
}

check() {
    local name="$1"
    local expected="$2"
    local base="$3"
    local actual
    shift 3

    actual="$(classify "$base" "$@")"

    if [ "$actual" != "$expected" ]; then
        echo "FAIL: expected '$expected' for $name, got '$actual'" >&2
        exit 1
    fi
}

expect() {
    local name="$1"
    local expected="$2"
    shift 2
    check "$name" "$expected" "" "$@"
}

write() {
    local path="$1"
    shift
    mkdir -p "$(dirname "${fixture}/${path}")"
    printf '%s\n' "$@" > "${fixture}/${path}"
}

git -C "$fixture" init -q
git -C "$fixture" config user.name fixture
git -C "$fixture" config user.email fixture@example.invalid
write crates/demo/src/dropped_branch.rs \
    '#[cfg(windows)]' \
    'fn console_handle() {}'
git -C "$fixture" add .
git -C "$fixture" commit -q -m base
base_sha="$(git -C "$fixture" rev-parse HEAD)"

write crates/demo/src/dropped_branch.rs 'fn console_handle() {}'
write crates/demo/src/neutral.rs 'pub fn add(a: u32, b: u32) -> u32 { a + b }'
write crates/demo/src/context_windows.rs \
    '// Sliding context windows keep the prompt within budget.' \
    'pub struct ContextWindows { windows: Vec<u32> }'
write crates/demo/src/console_cfg.rs '#[cfg(windows)]' 'fn console() {}'
write crates/demo/src/permissions_cfg.rs '#[cfg(unix)]' 'fn set_mode() {}'
write crates/demo/src/cfg_not_unix.rs '#[cfg(not(unix))]' 'fn fallback() {}'
write crates/demo/src/cfg_macro.rs 'let shell = if cfg!(target_os = "windows") { "cmd" } else { "sh" };'
write crates/demo/src/cfg_multiline.rs \
    '#[cfg(any(' \
    '    unix,' \
    '    target_arch = "wasm32"' \
    '))]' \
    'fn permissions() {}'
write crates/demo/src/unix_permissions.rs 'use std::os::unix::fs::PermissionsExt;'
write crates/demo/src/libc_call.rs 'let uid = unsafe { libc::getuid() };'
write crates/demo/src/windows_api.rs 'use windows_sys::Win32::Foundation::HANDLE;'

# Rust sources with platform-conditional code must run the Windows leg: Linux
# jobs never compile their Windows side.
expect "cfg windows attribute" "true" "crates/demo/src/console_cfg.rs"
expect "cfg unix attribute" "true" "crates/demo/src/permissions_cfg.rs"
expect "cfg not unix attribute" "true" "crates/demo/src/cfg_not_unix.rs"
expect "cfg macro on target_os" "true" "crates/demo/src/cfg_macro.rs"
expect "multi-line cfg predicate" "true" "crates/demo/src/cfg_multiline.rs"
expect "std::os::unix import" "true" "crates/demo/src/unix_permissions.rs"
expect "libc call" "true" "crates/demo/src/libc_call.rs"
expect "windows-sys import" "true" "crates/demo/src/windows_api.rs"
expect "mixed neutral then platform source" "true" \
    "crates/demo/src/neutral.rs" \
    "crates/demo/src/console_cfg.rs"

# A deleted Rust source cannot be inspected, so it must run the leg.
expect "deleted rust source" "true" "crates/demo/src/removed.rs"

# Dropping the last Windows branch from a file must still run the leg when the
# base revision is known, and a base that cannot be read must fail open.
expect "dropped branch without base" "false" "crates/demo/src/dropped_branch.rs"
check "dropped branch with base" "true" "$base_sha" "crates/demo/src/dropped_branch.rs"
check "neutral source with base" "false" "$base_sha" "crates/demo/src/neutral.rs"
check "unreadable base" "true" "0000000000000000000000000000000000000000" \
    "crates/demo/src/neutral.rs"

# Build inputs can break one target alone.
expect "workspace manifest" "true" "Cargo.toml"
expect "crate manifest" "true" "crates/zeroclaw-runtime/Cargo.toml"
expect "workspace lockfile" "true" "Cargo.lock"
expect "root build script" "true" "build.rs"
expect "crate build script" "true" "crates/zeroclaw-runtime/build.rs"
expect "toolchain file" "true" "rust-toolchain.toml"
expect "cargo config" "true" ".cargo/config.toml"
expect "mixed docs then lockfile" "true" \
    "docs/book/src/contributing/testing.md" \
    "Cargo.lock"

# The leg's own targets and controls.
expect "recovery test module" "true" \
    "crates/zeroclaw-runtime/src/control_plane/task_registry.rs"
expect "voice-wake module" "true" "crates/zeroclaw-channels/src/voice_wake.rs"
expect "ci workflow" "true" ".github/workflows/ci.yml"
expect "local action" "true" ".github/actions/rust-cache/action.yml"
expect "the filter itself" "true" "scripts/ci/windows_check_change_filter.sh"
expect "the filter fixture" "true" "scripts/ci/windows_check_change_filter.test.sh"

# Windows-named paths and assets.
expect "windows asset directory" "true" "apps/tauri/windows/app.manifest"
expect "windows module file" "true" "crates/demo/src/platform/windows.rs"
expect "powershell script" "true" "scripts/install.ps1"

# Unrelated changes must keep the leg skipped.
expect "platform-neutral rust source" "false" "crates/demo/src/neutral.rs"
expect "context windows prose and identifiers" "false" \
    "crates/demo/src/context_windows.rs"
expect "docs-only changes" "false" "docs/book/src/contributing/testing.md"
expect "web-only changes" "false" "web/src/pages/AgentChat.tsx"
expect "other workflow changes" "false" ".github/workflows/release.yml"
expect "other ci script" "false" "scripts/ci/plugin_backend_change_filter.sh"
expect "empty input" "false"

echo "windows check change filter tests: pass"
