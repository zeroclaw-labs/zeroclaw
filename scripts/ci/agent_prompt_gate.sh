#!/usr/bin/env bash
# Agent prompt + runtime/hardware regression gate
#
# Runs focused Rust tests (name prefix `agent_prompt_`) that bound system prompt size
# and runtime/hardware sections. Skips when no prompt-related sources changed (faster
# local/CI runs), unless FORCE_AGENT_PROMPT_GATE=1.
#
# Usage: from repo root:
#   bash scripts/ci/agent_prompt_gate.sh
#
# Environment:
#   BASE_SHA                  Merge base for diff (default: merge-base origin/master HEAD)
#   FORCE_AGENT_PROMPT_GATE   If set to 1, always run tests
#   AGENT_PROMPT_TEST_FILTER  Extra cargo test filter (default: agent_prompt_)
#   GITHUB_STEP_SUMMARY       When set, append a short summary (GitHub Actions)
#
# Requires: bash, git, cargo (same as other Rust CI scripts).

set -euo pipefail

FORCE="${FORCE_AGENT_PROMPT_GATE:-0}"
FILTER="${AGENT_PROMPT_TEST_FILTER:-agent_prompt_}"
BASE_SHA="${BASE_SHA:-}"

if [ "$FORCE" != "1" ]; then
  if [ -z "$BASE_SHA" ] && git rev-parse --verify origin/master >/dev/null 2>&1; then
    BASE_SHA="$(git merge-base origin/master HEAD)"
  fi

  if [ -n "$BASE_SHA" ] && git cat-file -e "$BASE_SHA^{commit}" 2>/dev/null; then
    CHANGED="$(
      git diff --name-only "$BASE_SHA" HEAD | awk '
        $0 ~ /^src\/channels\/mod\.rs$/ ||
        $0 ~ /^src\/agent\/prompt\.rs$/ ||
        $0 ~ /^src\/agent\// ||
        $0 ~ /^tests\/component\/agent_prompt_gate\.rs$/ {
          print
        }
      '
    )"
    if [ -z "$CHANGED" ]; then
      echo "No agent prompt source changes vs ${BASE_SHA}; skipping agent prompt gate."
      exit 0
    fi
    echo "Agent prompt gate: changed files:"
    echo "$CHANGED" | sed 's/^/  - /'
  else
    echo "BASE_SHA missing or invalid; running agent prompt gate (full test filter)."
  fi
else
  echo "FORCE_AGENT_PROMPT_GATE=1: running agent prompt gate."
fi

echo "==> cargo test -p zeroclawlabs --locked ${FILTER}"
set +e
OUT="$(mktemp)"
cargo test -p zeroclawlabs --locked "$FILTER" -- --nocapture 2>&1 | tee "$OUT"
STATUS=${PIPESTATUS[0]}
set -e

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### Agent prompt gate"
    echo "- Filter: \`${FILTER}\`"
    if [ "$STATUS" -eq 0 ]; then
      echo "- Result: passed"
    else
      echo "- Result: failed"
    fi
  } >>"$GITHUB_STEP_SUMMARY"
fi

if [ "$STATUS" -ne 0 ]; then
  echo "::error::agent_prompt_gate: cargo test failed (exit $STATUS)"
  rm -f "$OUT"
  exit "$STATUS"
fi

rm -f "$OUT"
echo "Agent prompt gate: OK"
