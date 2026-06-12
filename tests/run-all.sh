#!/usr/bin/env bash
# tests/run-all.sh — Master test runner for the osAgent test suite.
#
# Usage:
#   bash tests/run-all.sh              # run every test
#   bash tests/run-all.sh 01.3         # run only the matching test(s)
#
# Each test file is sourced (so PASS/FAIL counters accumulate); final report
# at the end. Exit code is 0 iff every test passed.

set -uo pipefail
cd "$(dirname "$0")/.."

FILTER="${1:-}"
TOTAL_PASS=0
TOTAL_FAIL=0
SUITES_RUN=0
SUITES_FAILED=0

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo " osAgent test suite"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

for test_file in tests/test-*.sh; do
  [ -f "$test_file" ] || continue
  [ -n "$FILTER" ] && [[ "$test_file" != *"$FILTER"* ]] && continue

  # Run in subshell so counters reset between suites; capture output + exit code.
  if SUITE_OUTPUT=$(bash "$test_file" 2>&1); then
    SUITE_EXIT=0
  else
    SUITE_EXIT=$?
  fi
  echo "$SUITE_OUTPUT"

  # Extract this suite's pass/fail from the summarise line.
  SUITE_PASS=$(echo "$SUITE_OUTPUT" | tail -1 | sed -nE 's/.*: ([0-9]+) passed, ([0-9]+) failed/\1/p' || echo 0)
  SUITE_FAIL=$(echo "$SUITE_OUTPUT" | tail -1 | sed -nE 's/.*: ([0-9]+) passed, ([0-9]+) failed/\2/p' || echo 0)
  TOTAL_PASS=$((TOTAL_PASS + ${SUITE_PASS:-0}))
  TOTAL_FAIL=$((TOTAL_FAIL + ${SUITE_FAIL:-0}))
  SUITES_RUN=$((SUITES_RUN + 1))
  [ "$SUITE_EXIT" -ne 0 ] && SUITES_FAILED=$((SUITES_FAILED + 1))
done

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo " TOTAL: $TOTAL_PASS passed, $TOTAL_FAIL failed across $SUITES_RUN suites ($SUITES_FAILED with failures)"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

[ "$SUITES_FAILED" -eq 0 ]
