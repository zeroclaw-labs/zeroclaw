#!/usr/bin/env bash
# Drive a rebase forward, resolving only the conflicts that are safe to
# resolve mechanically and stopping the moment one needs judgement.
#
# Two classes get handled automatically:
#   - Cargo.lock: a generated file. Taking upstream's and letting cargo
#     reconcile it is correct; hand-merging a lockfile is not a thing.
#   - Additive hunks: both sides added different things to the same place and
#     neither touched the other's lines. resolve-additive-conflicts.py proves
#     that from the diff3 base before rewriting anything, and exits non-zero
#     if the two sides actually compete.
#
# Everything else stops the loop with the conflict left in the tree. That is
# the point: a rebase that resolves semantic conflicts by rule silently drops
# one side's behaviour, and the tests may well still pass.
set -uo pipefail

readonly ROOT="${1:-$HOME/zc-rebase}"
readonly LOG="${2:-/tmp/rebase-loop.log}"
export GIT_EDITOR=true
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_INCREMENTAL=0

cd "$ROOT"
: > "$LOG"

step=0
while true; do
  step=$((step + 1))

  conflicts="$(git diff --name-only --diff-filter=U)"
  if [ -n "$conflicts" ]; then
    echo "[$step] conflicts: $(echo "$conflicts" | tr '\n' ' ')" >> "$LOG"

    # Lockfile: regenerated, never merged by hand.
    if echo "$conflicts" | grep -qx 'Cargo.lock'; then
      git checkout v0.8.4 -- Cargo.lock 2>/dev/null || true
      git add Cargo.lock 2>/dev/null || true
    fi

    others="$(echo "$conflicts" | grep -vx 'Cargo.lock' || true)"
    if [ -n "$others" ]; then
      # shellcheck disable=SC2086
      if ! python3 scripts/resolve-additive-conflicts.py $others >> "$LOG" 2>&1; then
        echo "STOP: conflict needs a human decision (step $step)" | tee -a "$LOG"
        exit 2
      fi
      # shellcheck disable=SC2086
      git add $others 2>/dev/null || true
    fi
  fi

  out="$(git rebase --continue 2>&1)"
  echo "$out" >> "$LOG"

  if echo "$out" | grep -q "Successfully rebased"; then
    echo "DONE: rebase complete after $step steps" | tee -a "$LOG"
    exit 0
  fi
  if echo "$out" | grep -q "No rebase in progress"; then
    echo "DONE: nothing in progress" | tee -a "$LOG"
    exit 0
  fi
  if echo "$out" | grep -qE "nothing to commit|patch is empty"; then
    # Commit already upstream in another form; drop it and move on.
    git rebase --skip >> "$LOG" 2>&1
    continue
  fi
  if ! echo "$out" | grep -qE "CONFLICT|Could not apply"; then
    echo "STOP: unexpected rebase state (step $step)" | tee -a "$LOG"
    echo "$out" | tail -5 | tee -a "$LOG"
    exit 3
  fi

  if [ "$step" -gt 60 ]; then
    echo "STOP: too many steps, something is looping" | tee -a "$LOG"
    exit 4
  fi
done
