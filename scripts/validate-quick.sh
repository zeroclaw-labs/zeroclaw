#!/usr/bin/env bash
# scripts/validate-quick.sh — Phase 1.1 quick gate (file existence + git state)
# Fast: ~5-10s. Runs after every task commit during Phase 1.1 execution.
#
# Each phase fills in its labeled block with specific checks. Wave 0 creates
# the harness with placeholders; later waves uncomment their relevant blocks.
set -euo pipefail
cd "$(dirname "$0")/.."

PASS=0
FAIL=0
report() {
  if [ $1 -eq 0 ]; then echo "  ✓ $2"; PASS=$((PASS+1));
  else echo "  ✗ $2"; FAIL=$((FAIL+1)); fi
}

echo "=== Phase 1.1 validate-quick ==="

# PLAN-00 FILL — preflight artifacts
test -f preflight.env -a -f known-issues.md && report 0 "preflight artifacts present" || report 1 "preflight.env or known-issues.md missing"

# PLAN-01 FILL — fork relationship + NOTICE + Cargo.toml metadata
git remote -v | grep -qE "^upstream\s+https://github\.com/zeroclaw-labs/zeroclaw" && report 0 "upstream remote configured" || report 1 "upstream remote missing"
git remote -v | grep -qE "^origin\s+https://github\.com/andreas2301/osAgent" && report 0 "origin remote configured" || report 1 "origin remote missing"
git branch --list osagent-main | grep -q osagent-main && report 0 "osagent-main branch exists" || report 1 "osagent-main branch missing"
test -f NOTICE && grep -qF "osAgent is NOT ZeroClaw" NOTICE && report 0 "NOTICE has fork disambiguation" || report 1 "NOTICE missing or no disambiguation"
test -f LICENSE-APACHE -a -f LICENSE-MIT && report 0 "dual-license files present" || report 1 "LICENSE-APACHE or LICENSE-MIT missing"
grep -q "forked_from" Cargo.toml 2>/dev/null && report 0 "Cargo.toml has fork provenance metadata" || report 1 "Cargo.toml fork provenance missing"

# PLAN-02 FILL — deny.toml + CI workflow
test -f deny.toml && report 0 "deny.toml present" || report 1 "deny.toml missing"
grep -qF "AGPL-3.0" deny.toml 2>/dev/null && report 0 "deny.toml lists AGPL-3.0 banlist target" || report 1 "deny.toml missing AGPL ban"
test -f .github/workflows/ci.yml && grep -qF "EmbarkStudios/cargo-deny-action@" .github/workflows/ci.yml && report 0 "ci.yml uses cargo-deny-action" || report 1 "ci.yml missing or no cargo-deny-action"

# PLAN-03 FILL — UPSTREAM_SYNC.md exists in sibling repo
test -f ../sovereign-shield-backup/documentation/osAgent/UPSTREAM_SYNC.md && report 0 "UPSTREAM_SYNC.md present in sibling repo" || report 1 "UPSTREAM_SYNC.md missing"

echo "=== $PASS passed, $FAIL failed ==="
[ $FAIL -eq 0 ]
