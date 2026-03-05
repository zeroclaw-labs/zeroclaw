#!/usr/bin/env bash
# Compare PR binary size against the PR base commit and fail on large regressions.
#
# Usage:
#   check_binary_size_regression.sh <base_sha> <head_binary_path> [max_percent_increase]
#
# Behavior:
# - Builds base commit binary with the same release profile (`release-fast`)
# - Emits summary details to GITHUB_STEP_SUMMARY when available
# - Fails only when head binary grows above max_percent_increase
# - Fails open (warning-only) if base build cannot be produced for comparison

set -euo pipefail

BASE_SHA="${1:?Usage: check_binary_size_regression.sh <base_sha> <head_binary_path> [max_percent_increase]}"
HEAD_BIN="${2:?Usage: check_binary_size_regression.sh <base_sha> <head_binary_path> [max_percent_increase]}"
MAX_PERCENT="${3:-10}"

size_bytes() {
  local file="$1"
  stat -f%z "$file" 2>/dev/null || stat -c%s "$file"
}

if [ ! -f "$HEAD_BIN" ]; then
  echo "::error::Head binary not found: ${HEAD_BIN}"
  exit 1
fi

if ! git cat-file -e "${BASE_SHA}^{commit}" 2>/dev/null; then
  echo "::warning::Base SHA is not available in this checkout (${BASE_SHA}); skipping binary-size regression gate."
  exit 0
fi

HEAD_SIZE="$(size_bytes "$HEAD_BIN")"

tmp_root="${RUNNER_TEMP:-/tmp}"
worktree_dir="$(mktemp -d "${tmp_root%/}/binary-size-base.XXXXXX")"
cleanup() {
  git worktree remove --force "$worktree_dir" >/dev/null 2>&1 || true
  rm -rf "$worktree_dir" >/dev/null 2>&1 || true
}
trap cleanup EXIT

if ! git worktree add --detach "$worktree_dir" "$BASE_SHA" >/dev/null 2>&1; then
  echo "::warning::Failed to create base worktree at ${BASE_SHA}; skipping binary-size regression gate."
  exit 0
fi

BASE_TARGET_DIR="${worktree_dir}/target-base"
base_build_status="success"
if ! (
  cd "$worktree_dir"
  export CARGO_TARGET_DIR="$BASE_TARGET_DIR"
  cargo build --profile release-fast --locked --bin zeroclaw
); then
  base_build_status="failure"
fi

if [ "$base_build_status" != "success" ]; then
  echo "::warning::Base commit build failed at ${BASE_SHA}; skipping binary-size regression gate."
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    {
      echo "### Binary Size Regression"
      echo "- Base SHA: \`${BASE_SHA}\`"
      echo "- Result: skipped (base build failed)"
      echo "- Head size bytes: \`${HEAD_SIZE}\`"
    } >> "$GITHUB_STEP_SUMMARY"
  fi
  exit 0
fi

BASE_BIN="${BASE_TARGET_DIR}/release-fast/zeroclaw"
if [ ! -f "$BASE_BIN" ]; then
  echo "::warning::Base binary missing (${BASE_BIN}); skipping binary-size regression gate."
  exit 0
fi

BASE_SIZE="$(size_bytes "$BASE_BIN")"
DELTA_BYTES="$((HEAD_SIZE - BASE_SIZE))"

DELTA_PERCENT="$(
python3 - "$BASE_SIZE" "$HEAD_SIZE" <<'PY'
import sys
base = int(sys.argv[1])
head = int(sys.argv[2])
if base <= 0:
    print("0.00")
else:
    pct = ((head - base) / base) * 100.0
    print(f"{pct:.2f}")
PY
)"

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### Binary Size Regression"
    echo "- Base SHA: \`${BASE_SHA}\`"
    echo "- Base size bytes: \`${BASE_SIZE}\`"
    echo "- Head size bytes: \`${HEAD_SIZE}\`"
    echo "- Delta bytes: \`${DELTA_BYTES}\`"
    echo "- Delta percent: \`${DELTA_PERCENT}%\`"
    echo "- Max allowed increase: \`${MAX_PERCENT}%\`"
  } >> "$GITHUB_STEP_SUMMARY"
fi

if [ "$DELTA_BYTES" -le 0 ]; then
  echo "Binary size did not increase vs base (delta=${DELTA_BYTES} bytes)."
  exit 0
fi

if ! python3 - "$DELTA_PERCENT" "$MAX_PERCENT" <<'PY'
import sys
delta = float(sys.argv[1])
max_allowed = float(sys.argv[2])
if delta > max_allowed:
    sys.exit(1)
sys.exit(0)
PY
then
  echo "::error::Binary size regression ${DELTA_PERCENT}% exceeds threshold ${MAX_PERCENT}%."
  exit 1
fi

echo "::warning::Binary size increased by ${DELTA_PERCENT}% (within threshold ${MAX_PERCENT}%)."
exit 0
