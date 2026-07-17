#!/usr/bin/env bash
# Verify target-specific feature selection for the bundled desktop kernel
# without compiling the kernel or invoking the Tauri bundler.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PREPARE_KERNEL="$ROOT/scripts/desktop/prepare-kernel.sh"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
extracted="$tmp/build_kernel.sh"
cargo_log="$tmp/cargo-args"

if ! awk '
  /^build_kernel\(\) \{/ { in_func = 1 }
  in_func { print }
  in_func && /^}$/ { found = 1; exit }
  END { if (!found) exit 1 }
' "$PREPARE_KERNEL" >"$extracted"; then
  echo "FAIL: could not extract build_kernel() from $PREPARE_KERNEL" >&2
  exit 1
fi

bash -n "$extracted"
# shellcheck disable=SC1090
source "$extracted"

REPO_ROOT="$tmp/repo"
export PROFILE="release"
mkdir -p "$REPO_ROOT"

cargo() {
  printf '%s\n' "$@" >"$cargo_log"
}

check_arch() {
  :
}

feature_count() {
  awk '
    $0 == "--features" {
      if (getline && $0 == "computer-use") count++
    }
    END { print count + 0 }
  ' "$cargo_log"
}

assert_feature_selection() {
  local triple="$1" expected="$2" actual
  : >"$cargo_log"
  unset ZEROCLAW_KERNEL_PATH
  build_kernel "$triple" >/dev/null
  actual="$(feature_count)"
  if [[ "$actual" != "$expected" ]]; then
    echo "FAIL: $triple expected computer-use feature count $expected, got $actual" >&2
    echo "cargo arguments:" >&2
    sed 's/^/  /' "$cargo_log" >&2
    exit 1
  fi
}

assert_feature_selection aarch64-apple-darwin 1
assert_feature_selection x86_64-apple-darwin 1
assert_feature_selection x86_64-unknown-linux-gnu 1
assert_feature_selection x86_64-pc-windows-msvc 1
assert_feature_selection aarch64-linux-android 0

# Supplying a prebuilt kernel must continue to bypass Cargo and its feature
# selection entirely.
: >"$cargo_log"
prebuilt="$tmp/prebuilt-zeroclaw"
export ZEROCLAW_KERNEL_PATH="$prebuilt"
actual_path="$(build_kernel aarch64-apple-darwin)"
if [[ "$actual_path" != "$prebuilt" ]]; then
  echo "FAIL: prebuilt path changed: expected $prebuilt, got $actual_path" >&2
  exit 1
fi
if [[ -s "$cargo_log" ]]; then
  echo "FAIL: prebuilt kernel path unexpectedly invoked cargo" >&2
  exit 1
fi

echo "desktop kernel feature selection: passed"
