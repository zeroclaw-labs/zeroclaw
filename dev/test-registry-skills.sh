#!/bin/sh
# test-registry-skills.sh — End-to-end test for registry-based skill installation
# Installs every skill from zeroclaw-labs/zeroclaw-skills by bare name,
# verifies metadata, and cleans up.
# Must be run from repo root.
set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# ── Terminal-aware colors ────────────────────────────────────────
if [ -t 1 ]; then
  BOLD='\033[1m' GREEN='\033[32m' YELLOW='\033[33m' RED='\033[31m' DIM='\033[2m' RESET='\033[0m'
else
  BOLD='' GREEN='' YELLOW='' RED='' DIM='' RESET=''
fi

FAILURES=0
TESTS=0

pass() { TESTS=$((TESTS + 1)); printf "  ${GREEN}✓${RESET} %s\n" "$*"; }
fail() { TESTS=$((TESTS + 1)); FAILURES=$((FAILURES + 1)); printf "  ${RED}✗${RESET} %s\n" "$*"; }
info() { printf "\n${BOLD}%s${RESET}\n" "$*"; }
warn() { printf "  ${YELLOW}⚠${RESET} %s\n" "$*"; }

# ── Resolve zeroclaw binary ─────────────────────────────────────
ZEROCLAW=""
for candidate in \
  "$REPO_ROOT/target/debug/zeroclaw" \
  "$REPO_ROOT/target/release/zeroclaw" \
  "$(command -v zeroclaw 2>/dev/null || true)"; do
  if [ -n "$candidate" ] && [ -x "$candidate" ]; then
    ZEROCLAW="$candidate"
    break
  fi
done

if [ -z "$ZEROCLAW" ]; then
  printf "${RED}Error: No zeroclaw binary found. Run 'cargo build' first.${RESET}\n"
  exit 1
fi

printf "\n${BOLD}Registry Skills E2E Test${RESET}\n"
printf "${DIM}Binary:  %s${RESET}\n" "$ZEROCLAW"
printf "${DIM}Branch:  %s${RESET}\n" "$(git branch --show-current 2>/dev/null || echo 'unknown')"

# ── All 16 skills from zeroclaw-labs/zeroclaw-skills ─────────────
REGISTRY_SKILLS="
  auto-coder
  web-researcher
  telegram-assistant
  code-reviewer
  doc-writer
  knowledge-base
  git-assistant
  multi-agent-router
  slack-connector
  discord-moderator
  data-analyst
  security-scanner
  api-tester
  ci-helper
  email-responder
  self-improving-agent
"

SKILLS_DIR="$HOME/.zeroclaw/workspace/skills"

# ══════════════════════════════════════════════════════════════════
# Section 1: Install each skill by bare name
# ══════════════════════════════════════════════════════════════════
info "=== Install (bare name → registry) ==="

INSTALLED_SKILLS=""
for skill in $REGISTRY_SKILLS; do
  OUTPUT=$("$ZEROCLAW" skills install "$skill" 2>&1) || true
  if printf '%s' "$OUTPUT" | grep -q "✓"; then
    pass "install $skill"
    INSTALLED_SKILLS="$INSTALLED_SKILLS $skill"
  elif printf '%s' "$OUTPUT" | grep -q "already exists"; then
    warn "$skill already installed (skipped)"
    INSTALLED_SKILLS="$INSTALLED_SKILLS $skill"
  else
    fail "install $skill"
    printf "      %s\n" "$(printf '%s' "$OUTPUT" | tail -2)"
  fi
done

# ══════════════════════════════════════════════════════════════════
# Section 2: Verify skill directories and SKILL.md
# ══════════════════════════════════════════════════════════════════
info "=== Verify files ==="

for skill in $INSTALLED_SKILLS; do
  skill_dir="$SKILLS_DIR/$skill"
  if [ -d "$skill_dir" ]; then
    pass "directory exists: $skill"
  else
    fail "directory missing: $skill"
    continue
  fi

  if [ -f "$skill_dir/SKILL.md" ]; then
    if head -1 "$skill_dir/SKILL.md" | grep -q "^---"; then
      pass "SKILL.md has frontmatter: $skill"
    else
      fail "SKILL.md missing frontmatter: $skill"
    fi
  elif [ -f "$skill_dir/SKILL.toml" ]; then
    pass "SKILL.toml exists: $skill"
  else
    fail "no manifest (SKILL.md or SKILL.toml): $skill"
  fi
done

# ══════════════════════════════════════════════════════════════════
# Section 3: Verify skills list output
# ══════════════════════════════════════════════════════════════════
info "=== Verify skills list ==="

LIST_OUTPUT=$("$ZEROCLAW" skills list 2>&1)
INSTALLED_COUNT=$(printf '%s' "$LIST_OUTPUT" | grep -c "v[0-9]" || true)
INSTALLED_COUNT=$(printf '%s' "$INSTALLED_COUNT" | tr -d ' ')

if [ "$INSTALLED_COUNT" -ge 16 ]; then
  pass "skills list shows $INSTALLED_COUNT skills (expected ≥16)"
else
  fail "skills list shows $INSTALLED_COUNT skills (expected ≥16)"
fi

for skill in $INSTALLED_SKILLS; do
  if printf '%s' "$LIST_OUTPUT" | grep -q "$skill"; then
    pass "listed: $skill"
  else
    fail "not listed: $skill"
  fi
done

# ══════════════════════════════════════════════════════════════════
# Section 4: Error handling — nonexistent skill
# ══════════════════════════════════════════════════════════════════
info "=== Error handling ==="

ERR_OUTPUT=$("$ZEROCLAW" skills install nonexistent-skill-xyz 2>&1 || true)
if printf '%s' "$ERR_OUTPUT" | grep -q "not found in the registry"; then
  pass "nonexistent skill gives clear error"
else
  fail "nonexistent skill error message unclear"
fi

if printf '%s' "$ERR_OUTPUT" | grep -q "Available skills:"; then
  pass "error lists available skills"
else
  fail "error does not list available skills"
fi

# ══════════════════════════════════════════════════════════════════
# Section 5: Duplicate install prevention
# ══════════════════════════════════════════════════════════════════
info "=== Duplicate install ==="

DUP_OUTPUT=$("$ZEROCLAW" skills install auto-coder 2>&1 || true)
if printf '%s' "$DUP_OUTPUT" | grep -q "already exists"; then
  pass "duplicate install blocked"
else
  fail "duplicate install not blocked"
fi

# ══════════════════════════════════════════════════════════════════
# Section 6: Registry cache verification
# ══════════════════════════════════════════════════════════════════
info "=== Registry cache ==="

REGISTRY_DIR="$HOME/.zeroclaw/workspace/skills-registry"
if [ -d "$REGISTRY_DIR" ]; then
  pass "registry cache exists at $REGISTRY_DIR"
else
  fail "registry cache not found"
fi

if [ -f "$REGISTRY_DIR/.zeroclaw-skills-registry-sync" ]; then
  pass "sync marker present"
else
  fail "sync marker missing"
fi

CACHED_SKILLS=$(ls "$REGISTRY_DIR/skills/" 2>/dev/null | wc -l | tr -d ' ')
if [ "$CACHED_SKILLS" -ge 16 ]; then
  pass "registry cache has $CACHED_SKILLS skill directories"
else
  fail "registry cache has only $CACHED_SKILLS skill directories (expected ≥16)"
fi

# ══════════════════════════════════════════════════════════════════
# Section 7: Cleanup — remove installed skills
# ══════════════════════════════════════════════════════════════════
info "=== Cleanup ==="

for skill in $INSTALLED_SKILLS; do
  REMOVE_OUTPUT=$("$ZEROCLAW" skills remove "$skill" 2>&1) || true
  if printf '%s' "$REMOVE_OUTPUT" | grep -q "removed"; then
    pass "removed $skill"
  else
    fail "failed to remove $skill"
  fi
done

REMAINING=$(ls "$SKILLS_DIR" 2>/dev/null | wc -l | tr -d ' ')
if [ "$REMAINING" -eq 0 ]; then
  pass "skills directory empty after cleanup"
else
  warn "$REMAINING skill(s) remain after cleanup"
fi

# ══════════════════════════════════════════════════════════════════
# Summary
# ══════════════════════════════════════════════════════════════════
PASSED=$((TESTS - FAILURES))
printf "\n  %s tests, %s passed, %s failed\n" "$TESTS" "$PASSED" "$FAILURES"

if [ "$FAILURES" -eq 0 ]; then
  printf "${GREEN}${BOLD}  All tests passed!${RESET}\n\n"
else
  printf "${RED}${BOLD}  %d test(s) failed.${RESET}\n\n" "$FAILURES"
fi

exit "$FAILURES"
