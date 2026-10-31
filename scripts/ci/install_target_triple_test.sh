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
diagnostic_tmp="$(mktemp)"
trap 'rm -f "$tmp" "$diagnostic_tmp"' EXIT

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
mock_termux_dir="0"
TERMUX_VERSION=""

function [ {
  if [[ "$#" -eq 3 && "$1" == "-d" && "$2" == "/data/data/com.termux" ]]; then
    [[ "$mock_termux_dir" == "1" ]]
  else
    builtin [ "$@"
  fi
}

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
  mock_termux_dir="0"
  TERMUX_VERSION=""
  actual="$(detect_target_triple)"
  if [[ "$actual" == "$expected" ]]; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    failures+=("${os}/${arch}/${libc}/sysctl=${sysctl_arm64}: expected '${expected}', got '${actual}'")
  fi
}

assert_termux_triple() {
  local arch="$1" expected="$2" actual
  mock_os="Linux"
  mock_arch="$arch"
  mock_libc="gnu"
  mock_sysctl_arm64="0"
  mock_termux_dir="0"
  TERMUX_VERSION="0.118"
  actual="$(detect_target_triple)"
  if [[ "$actual" == "$expected" ]]; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    failures+=("Termux/${arch}: expected '${expected}', got '${actual}'")
  fi
}

assert_termux_dir_triple() {
  local arch="$1" expected="$2" actual
  mock_os="Linux"
  mock_arch="$arch"
  mock_libc="gnu"
  mock_sysctl_arm64="0"
  mock_termux_dir="1"
  TERMUX_VERSION=""
  actual="$(detect_target_triple)"
  if [[ "$actual" == "$expected" ]]; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    failures+=("Termux directory/${arch}: expected '${expected}', got '${actual}'")
  fi
}

assert_unsupported_termux_arch() {
  local arch="$1" actual diagnostic
  mock_os="Linux"
  mock_arch="$arch"
  mock_libc="gnu"
  mock_sysctl_arm64="0"
  mock_termux_dir="0"
  TERMUX_VERSION="0.118"
  actual="$(detect_target_triple 2>"$diagnostic_tmp")"
  diagnostic=$(<"$diagnostic_tmp")
  if [[ -z "$actual" && "$diagnostic" == *"Unsupported Android architecture: $arch"* ]]; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    failures+=("Termux/${arch}: expected empty target and unsupported-architecture diagnostic, got '${actual}' / '${diagnostic}'")
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

assert_termux_triple aarch64 aarch64-linux-android
assert_termux_triple arm64 aarch64-linux-android
assert_termux_dir_triple aarch64 aarch64-linux-android
assert_unsupported_termux_arch x86_64

assert_triple FreeBSD x86_64 gnu 0 ""

# Exercise the production libc probe as well as target mapping. The fixtures
# replace host commands/filesystem facts, not detect_libc's decision logic.
if ! awk '
  /^detect_libc\(\) \{/ { in_func = 1 }
  in_func { print }
  in_func && /^}$/ { found = 1; exit }
  END { if (!found) exit 1 }
' "$INSTALL_SH" >"$tmp"; then
  echo "FAIL: could not extract detect_libc() from install.sh"
  exit 1
fi
# shellcheck disable=SC1090
source "$tmp"

assert_libc_probe() {
  local label="$1" getconf_status="$2" ldd_text="$3" ldd_status="$4"
  local loader_present="$5" os_release="$6" expected="$7" actual
  actual=$(
    # Keep the loader glob literal; the fixture owns this filesystem fact.
    set -f
    # install.sh uses POSIX sh with -eu, not this test runner's pipefail.
    # In particular, musl ldd can print its version and exit nonzero.
    set +o pipefail
    getconf() {
      [[ "$*" == GNU_LIBC_VERSION ]] || return 125
      return "$getconf_status"
    }
    ldd() {
      [[ "$*" == --version ]] || return 125
      printf '%s\n' "$ldd_text"
      return "$ldd_status"
    }
    function [ {
      if [[ "$#" -eq 3 && "$1" == "-e" && "$2" == /lib/ld-musl-*.so.1 ]]; then
        [[ "$loader_present" == 1 ]]
      elif [[ "$#" -eq 3 && "$1" == "-r" && "$2" == /etc/os-release ]]; then
        [[ -n "$os_release" ]]
      else
        builtin [ "$@"
      fi
    }
    grep() {
      if [[ "${!#}" == /etc/os-release ]]; then
        printf '%s\n' "$os_release" | command grep "${@:1:$#-1}"
      else
        command grep "$@"
      fi
    }
    detect_libc
    mock_os=Linux
    mock_termux_dir=0
    TERMUX_VERSION=""
    mock_arch=x86_64
    detect_target_triple
    mock_arch=aarch64
    detect_target_triple
  )
  if [[ "$actual" == "$expected"$'\n'"x86_64-unknown-linux-$expected"$'\n'"aarch64-unknown-linux-$expected" ]]; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    failures+=("$label: expected $expected libc and both Linux triples, got '$actual'")
  fi
}

assert_libc_probe 'glibc with musl loader' 0 'musl libc' 1 1 'ID=ubuntu' gnu
assert_libc_probe 'glibc without musl loader' 0 'ldd (Ubuntu GLIBC 2.39) 2.39' 0 0 'ID=ubuntu' gnu
assert_libc_probe 'glibc ldd without getconf' 127 'ldd (GNU libc) 2.39' 0 1 'ID=debian' gnu
assert_libc_probe 'Ubuntu ldd with failed getconf' 1 'ldd (Ubuntu GLIBC 2.39) 2.39' 0 1 'ID=ubuntu' gnu
assert_libc_probe 'musl ldd with nonzero exit' 1 'musl libc (x86_64)' 1 0 '' musl
assert_libc_probe 'musl loader without probes' 127 '' 127 1 '' musl
assert_libc_probe 'Alpine os-release fallback' 127 '' 127 0 'ID=alpine' musl
assert_libc_probe 'postmarketOS fallback' 127 '' 127 0 'ID=postmarketos' musl
assert_libc_probe 'unknown host keeps GNU fallback' 127 '' 127 0 '' gnu

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
