#!/usr/bin/env bash
# Tests install.sh target-triple detection without running the installer.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
INSTALL_SH="$ROOT/install.sh"

if [[ ! -f "$INSTALL_SH" ]]; then
  echo "FAIL: install.sh not found at $INSTALL_SH"
  exit 1
fi

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

if ! awk '
  /^detect_target_triple\(\) \{/ { in_func = 1 }
  in_func { print }
  in_func && /^}$/ { found = 1; exit }
  END { if (!found) exit 1 }
' "$INSTALL_SH" >"$tmp"; then
  echo "FAIL: could not extract detect_target_triple() from install.sh"
  exit 1
fi

bash -n "$tmp"

# shellcheck disable=SC1090
source "$tmp"

mock_os=""
mock_arch=""
mock_libc="gnu"
mock_sysctl_arm64="0"

uname() {
  case "${1:-}" in
  -s) printf '%s\n' "$mock_os" ;;
  -m) printf '%s\n' "$mock_arch" ;;
  *)
    echo "FAIL: unexpected uname argument: ${1:-<empty>}" >&2
    return 1
    ;;
  esac
}

detect_libc() {
  printf '%s\n' "$mock_libc"
}

sysctl() {
  case "$*" in
  "-n hw.optional.arm64") printf '%s\n' "$mock_sysctl_arm64" ;;
  *)
    echo "FAIL: unexpected sysctl argument: $*" >&2
    return 1
    ;;
  esac
}

pass=0
fail=0
failures=()

assert_triple() {
  local os="$1" arch="$2" libc="$3" sysctl_arm64="$4" expected="$5" actual
  mock_os="$os"
  mock_arch="$arch"
  mock_libc="$libc"
  mock_sysctl_arm64="$sysctl_arm64"
  actual="$(detect_target_triple)"
  if [[ "$actual" == "$expected" ]]; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    failures+=("${os}/${arch}/${libc}/sysctl=${sysctl_arm64}: expected '${expected}', got '${actual}'")
  fi
}

assert_triple Darwin x86_64 gnu 0 x86_64-apple-darwin
assert_triple Darwin arm64 gnu 1 aarch64-apple-darwin
assert_triple Darwin x86_64 gnu 1 aarch64-apple-darwin

assert_triple Linux x86_64 gnu 0 x86_64-unknown-linux-gnu
assert_triple Linux x86_64 musl 0 x86_64-unknown-linux-musl
assert_triple Linux aarch64 gnu 0 aarch64-unknown-linux-gnu
assert_triple Linux aarch64 musl 0 aarch64-unknown-linux-musl
assert_triple Linux arm64 gnu 0 aarch64-unknown-linux-gnu
assert_triple Linux arm64 musl 0 aarch64-unknown-linux-musl
assert_triple Linux armv7l gnu 0 armv7-unknown-linux-gnueabihf
assert_triple Linux armv6l gnu 0 arm-unknown-linux-gnueabihf
assert_triple Linux armv5l gnu 0 arm-unknown-linux-gnueabihf
assert_triple Linux ppc gnu 0 ""

assert_triple FreeBSD x86_64 gnu 0 ""

echo "passed: $pass"
echo "failed: $fail"

if (( fail > 0 )); then
  echo
  echo "Failures:"
  for failure in "${failures[@]}"; do
    echo "  - $failure"
  done
  exit 1
fi
