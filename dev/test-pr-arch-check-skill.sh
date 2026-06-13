#!/usr/bin/env bash
# TDD acceptance tests for the PR Architecture Check skill
# Run from repo root: bash dev/test-pr-arch-check-skill.sh

set -euo pipefail

PASS=0
FAIL=0
ERRORS=()

pass() { ((PASS++)); echo "  ✓ $1"; }
fail() { ((FAIL++)); ERRORS+=("$1"); echo "  ✗ $1"; }

echo "=== PR Architecture Check Skill — Acceptance Tests ==="
echo ""

# ─── 1. File existence ───────────────────────────────────────────────
echo "1. File existence"

SKILL=".claude/skills/pr-architecture-check/SKILL.md"
CHECKLIST=".claude/skills/pr-architecture-check/references/arch-checklist.md"

[ -f "$SKILL" ]     && pass "SKILL.md exists"          || fail "SKILL.md missing"
[ -f "$CHECKLIST" ] && pass "arch-checklist.md exists"  || fail "arch-checklist.md missing"

# ─── 2. SKILL.md frontmatter ─────────────────────────────────────────
echo "2. SKILL.md frontmatter"

if [ -f "$SKILL" ]; then
  head -5 "$SKILL" | grep -q '^---'             && pass "has frontmatter"            || fail "missing frontmatter delimiter"
  grep -q '^name:'        "$SKILL"               && pass "has name field"             || fail "missing name field"
  grep -q '^description:' "$SKILL"               && pass "has description field"      || fail "missing description field"
  grep -qi 'arch-check'   "$SKILL"               && pass "trigger phrase present"     || fail "trigger phrase 'arch-check' missing"
  grep -qi 'architecture check' "$SKILL"         && pass "alt trigger present"        || fail "trigger phrase 'architecture check' missing"
fi

# ─── 3. Advisory framing (FND-003 §6.4) ──────────────────────────────
echo "3. Advisory framing"

if [ -f "$SKILL" ]; then
  grep -qi 'advisory'       "$SKILL"             && pass "mentions advisory"          || fail "'advisory' not found in SKILL.md"
  grep -qi 'not.*merge.*gate\|never.*gate.*merge\|non-blocking' "$SKILL" \
                                                  && pass "non-blocking stated"        || fail "non-blocking/non-gate language missing"
  grep -qi 'FND-003'        "$SKILL"             && pass "references FND-003"         || fail "FND-003 reference missing"
fi

# ─── 4. Workflow steps ────────────────────────────────────────────────
echo "4. Workflow steps"

if [ -f "$SKILL" ]; then
  grep -q  'gh pr diff'     "$SKILL"             && pass "fetches PR diff"            || fail "gh pr diff step missing"
  grep -q  'gh pr view'     "$SKILL"             && pass "fetches PR metadata"        || fail "gh pr view step missing"
  grep -q  'AGENTS.md'      "$SKILL"             && pass "loads AGENTS.md"            || fail "AGENTS.md load missing"
  grep -qi 'FND-001\|fnd-001' "$SKILL"           && pass "loads FND-001"              || fail "FND-001 load missing"
  grep -q  'tmp/arch-review' "$SKILL"            && pass "writes tmp artifact"        || fail "tmp/arch-review artifact missing"
  grep -qi 'comment'        "$SKILL"             && pass "posts PR comment"           || fail "PR comment posting missing"
  grep -qi 'wait for.*approval\|explicit approval\|before posting' "$SKILL" \
                                                  && pass "human-approval checkpoint"  || fail "human-approval checkpoint before posting missing"
  grep -qi 'label'          "$SKILL"             && pass "label policy mentioned"     || fail "label policy not mentioned"
fi

# ─── 5. Checklist content ────────────────────────────────────────────
echo "5. Checklist content (arch-checklist.md)"

if [ -f "$CHECKLIST" ]; then
  grep -qi 'dependency direction\|dependencies flow' "$CHECKLIST" \
                                                  && pass "dependency direction"       || fail "dependency direction check missing"
  grep -qi 'trait boundar'  "$CHECKLIST"          && pass "trait boundary"             || fail "trait boundary check missing"
  grep -qi 'factory\|registration' "$CHECKLIST"   && pass "extension pattern"          || fail "extension/factory pattern check missing"
  grep -qi 'crate.*responsib\|crate.*placement'  "$CHECKLIST" \
                                                  && pass "crate placement"            || fail "crate placement check missing"
  grep -qi 'core.*constraint\|engineering.*constraint' "$CHECKLIST" \
                                                  && pass "core constraints"           || fail "core constraints section missing"
  # The 7 core constraints from AGENTS.md
  grep -qi 'single.*static.*binary\|static binary' "$CHECKLIST" \
                                                  && pass "constraint: static binary"  || fail "static binary constraint missing"
  grep -qi 'pluggab'        "$CHECKLIST"          && pass "constraint: pluggability"   || fail "pluggability constraint missing"
  grep -qi 'minimal.*footprint\|footprint'        "$CHECKLIST" \
                                                  && pass "constraint: footprint"      || fail "footprint constraint missing"
  grep -qi 'RPi\|raspberry\|edge\|runs on anything' "$CHECKLIST" \
                                                  && pass "constraint: hardware floor" || fail "hardware floor constraint missing"
  grep -qi 'secure.*default\|security'           "$CHECKLIST" \
                                                  && pass "constraint: secure default" || fail "secure default constraint missing"
  grep -qi 'vendor.*lock\|lock.in'               "$CHECKLIST" \
                                                  && pass "constraint: no vendor lock" || fail "vendor lock-in constraint missing"
  grep -qi 'external.*infra\|zero.*external'     "$CHECKLIST" \
                                                  && pass "constraint: no ext infra"   || fail "external infra constraint missing"
fi

# ─── 6. No new labels created ────────────────────────────────────────
echo "6. Guardrails"

if [ -f "$SKILL" ]; then
  # Must not add labels
  grep -qi 'does not.*label\|never.*label\|no.*label' "$SKILL" \
                                                  && pass "no-label policy"            || fail "no-label policy not stated"
fi

# ─── Summary ──────────────────────────────────────────────────────────
echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

if [ ${#ERRORS[@]} -gt 0 ]; then
  echo ""
  echo "Failures:"
  for e in "${ERRORS[@]}"; do
    echo "  - $e"
  done
  exit 1
fi

echo "All tests passed."
