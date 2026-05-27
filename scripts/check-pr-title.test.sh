#!/usr/bin/env bash
# Tests for scripts/check-pr-title.sh
# Asserts pass/fail behavior against representative PR titles.

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECKER="$SCRIPT_DIR/check-pr-title.sh"

if [[ ! -x "$CHECKER" ]]; then
  echo "FAIL: $CHECKER is missing or not executable"
  exit 1
fi

pass=0
fail=0
failures=()

assert_accept() {
  local title="$1"
  if "$CHECKER" "$title" >/dev/null 2>&1; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    failures+=("expected ACCEPT, got REJECT: $title")
  fi
}

assert_reject() {
  local title="$1"
  if "$CHECKER" "$title" >/dev/null 2>&1; then
    fail=$((fail + 1))
    failures+=("expected REJECT, got ACCEPT: $title")
  else
    pass=$((pass + 1))
  fi
}

# --- ACCEPT: valid conventional commits with scope ---
assert_accept "fix(ci): unblock pr-title workflow"
assert_accept "feat(channel:rocketchat): add Rocket.Chat channel"
assert_accept "fix(providers/compatible): normalize image markers"
assert_accept "refactor(config): decompose schema.rs into sub-modules"
assert_accept "fix(scope)!: breaking change indicator"
assert_accept "chore(deps): bump foo to 1.2.3"
assert_accept "docs(security): point sandboxing example at default image"
assert_accept "feat(channels/acp): persist ACP sessions"
assert_accept "test(tools): cover Tavily search routing aliases"
assert_accept "perf(memory): reduce allocations in hot path"
assert_accept "build(docker): add arm64 target"
assert_accept "style(rust): apply rustfmt"
assert_accept "revert(api): roll back endpoint rename"
assert_accept "fix(a): single-char scope"
assert_accept "fix(channels.email): scope with dot"
assert_accept "fix(scope_with_underscore): underscore in scope"
assert_accept "fix(ci): description with (#1234) PR number suffix"
assert_accept "feat(skills): 🎉 description with unicode emoji"
assert_accept "fix(my-scope): scope with hyphen"
assert_accept "fix(ci):  description with double space after colon"
assert_accept "feat(providers/compatible/openai): deeply nested scope"

# --- REJECT: invalid format ---
assert_reject ""
assert_reject "fix something"
assert_reject "fix: no scope provided"
assert_reject "v0.8.0: Multi-Agent Runtime and Schema V3"
assert_reject "Fix(ci): capitalized type"
assert_reject "FIX(ci): all caps type"
assert_reject "wip(ci): wip is not an allowed type"
assert_reject "fix(): empty scope"
assert_reject "fix(ci):no space after colon"
assert_reject "fix(ci) : space before colon"
assert_reject "refactor!(api): bang in wrong place"
assert_reject "fix(ci):"
assert_reject "fix(ci): "
assert_reject "  fix(ci): leading whitespace"
assert_reject "fix(CI): uppercase scope"
assert_reject "fix(ci with space): space in scope"
assert_reject "feat(scope): "
assert_reject "fix-something: dash in type"
assert_reject "fix (ci): space between type and scope"
assert_reject "(ci): missing type"
assert_reject "fix(ci) description without colon"

echo "passed: $pass"
echo "failed: $fail"
if (( fail > 0 )); then
  echo
  echo "Failures:"
  for f in "${failures[@]}"; do
    echo "  - $f"
  done
  exit 1
fi
