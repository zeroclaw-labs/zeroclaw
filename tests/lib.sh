#!/usr/bin/env bash
# tests/lib.sh — Shared assertion library for the osAgent test suite.
#
# Source from each test file:  . "$(dirname "$0")/lib.sh"
# Then use:
#   assert_file_exists <path> ["context"]
#   assert_dir_exists  <path> ["context"]
#   assert_file_absent <path> ["context"]
#   assert_grep        <pattern> <file> ["context"]
#   assert_no_grep     <pattern> <file> ["context"]
#   assert_eq          <expected> <actual> ["context"]
#   assert_cmd_ok      <cmd> ["context"]
#   assert_cmd_fails   <cmd> ["context"]
#
# Each assertion increments PASS or FAIL counters and prints a per-line result.
# At end-of-suite, `summarise` prints the totals and exits non-zero on any fail.

set -uo pipefail

PASS=${PASS:-0}
FAIL=${FAIL:-0}
SUITE=${SUITE:-"(unnamed)"}

_log_pass() { printf "  ✓ %s\n" "$1"; PASS=$((PASS+1)); }
_log_fail() { printf "  ✗ %s\n" "$1"; FAIL=$((FAIL+1)); }

assert_file_exists() {
  local path="$1" ctx="${2:-file $1 exists}"
  if [ -f "$path" ]; then _log_pass "$ctx"; else _log_fail "$ctx (missing: $path)"; fi
}

assert_dir_exists() {
  local path="$1" ctx="${2:-dir $1 exists}"
  if [ -d "$path" ]; then _log_pass "$ctx"; else _log_fail "$ctx (missing: $path)"; fi
}

assert_file_absent() {
  local path="$1" ctx="${2:-file $1 is absent}"
  if [ ! -e "$path" ]; then _log_pass "$ctx"; else _log_fail "$ctx (still present: $path)"; fi
}

assert_grep() {
  local pattern="$1"
  local file="$2"
  local ctx="${3:-$file contains /$pattern/}"
  if [ -f "$file" ] && grep -qE "$pattern" "$file"; then
    _log_pass "$ctx"
  else
    _log_fail "$ctx"
  fi
}

assert_no_grep() {
  local pattern="$1"
  local file="$2"
  local ctx="${3:-$file does NOT contain /$pattern/}"
  if [ ! -f "$file" ]; then
    _log_fail "$ctx (file missing: $file)"
  elif grep -qE "$pattern" "$file"; then
    _log_fail "$ctx (pattern found)"
  else
    _log_pass "$ctx"
  fi
}

assert_eq() {
  local expected="$1" actual="$2" ctx="${3:-assert_eq}"
  if [ "$expected" = "$actual" ]; then
    _log_pass "$ctx"
  else
    _log_fail "$ctx (expected '$expected', got '$actual')"
  fi
}

assert_cmd_ok() {
  local cmd="$1" ctx="${2:-$1 exits 0}"
  if eval "$cmd" >/dev/null 2>&1; then _log_pass "$ctx"; else _log_fail "$ctx"; fi
}

assert_cmd_fails() {
  local cmd="$1" ctx="${2:-$1 exits non-zero}"
  if eval "$cmd" >/dev/null 2>&1; then _log_fail "$ctx (unexpectedly succeeded)"; else _log_pass "$ctx"; fi
}

start_suite() { SUITE="$1"; PASS=0; FAIL=0; printf "\n━━━ %s ━━━\n" "$SUITE"; }

summarise() {
  printf "  %s: %d passed, %d failed\n" "$SUITE" "$PASS" "$FAIL"
  [ "$FAIL" -eq 0 ]
}
