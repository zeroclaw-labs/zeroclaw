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
test -f .github/workflows/osagent-policy.yml && grep -qF "EmbarkStudios/cargo-deny-action@" .github/workflows/osagent-policy.yml && report 0 "osagent-policy.yml wires cargo-deny-action" || report 1 "osagent-policy.yml missing or no cargo-deny-action"

# PLAN-03 FILL — UPSTREAM_SYNC.md exists in sibling repo.
# This file lands on feat/osagent-upstream-sync-runbook in sovereign-shield-backup;
# it does NOT appear on that repo's main until the PR is merged. The check below
# explicitly tolerates the pre-merge gap so Phase 1.1 quick-gate doesn't flap.
SS_SYNC_BACKUP=../sovereign-shield-backup/documentation/osAgent/UPSTREAM_SYNC.md
SS_SYNC_BRANCH=$(cd ../sovereign-shield-backup 2>/dev/null && git show feat/osagent-upstream-sync-runbook:documentation/osAgent/UPSTREAM_SYNC.md 2>/dev/null | head -1)
if [ -f "$SS_SYNC_BACKUP" ]; then
  report 0 "UPSTREAM_SYNC.md present on sibling repo main"
elif [ -n "$SS_SYNC_BRANCH" ]; then
  report 0 "UPSTREAM_SYNC.md present on feat/osagent-upstream-sync-runbook (PR pending merge)"
else
  report 1 "UPSTREAM_SYNC.md missing on both main and feat branch"
fi

# PLAN-1.2 FILL — workspace skeleton + binary split
test -f bins/engineer/Cargo.toml -a -f bins/engineer/src/main.rs && report 0 "bins/engineer/ exists" || report 1 "bins/engineer/ missing"
test -f bins/wizard/Cargo.toml -a -f bins/wizard/src/main.rs && report 0 "bins/wizard/ exists" || report 1 "bins/wizard/ missing"
grep -q 'name = "osagent-engineer"' bins/engineer/Cargo.toml 2>/dev/null && report 0 "engineer package named correctly" || report 1 "engineer package name wrong"
grep -q 'name = "osagent-wizard"' bins/wizard/Cargo.toml 2>/dev/null && report 0 "wizard package named correctly" || report 1 "wizard package name wrong"
# WS-02 prep: wizard Cargo.toml must NOT have an actual dep declaration on osagent-tools-mcp
# (structural exclusion). Comments mentioning the crate name are fine — they document the rule.
# Match only TOML key-assignment lines: `^name =` or `^name.workspace = ...` etc.
if grep -nE '^(osagent-tools-mcp|osagent-mcp)\s*(=|\.)' bins/wizard/Cargo.toml 2>/dev/null; then
  report 1 "wizard Cargo.toml has an MCP dep declaration — violates wizard-no-MCP property"
else
  report 0 "wizard Cargo.toml has no MCP dep declaration"
fi
# WS-04: deny.toml bans inventory/linkme/ctor
grep -qE '"(inventory|linkme|ctor)"' deny.toml 2>/dev/null && report 0 "deny.toml bans distributed-slice crates" || report 1 "deny.toml missing inventory/linkme/ctor ban"
# WS-04 source-grep: no actual invocations in our tree
INV_HITS=$(grep -rnE "^[^/]*\b(inventory::submit!|linkme::distributed_slice|#\[ctor::ctor\])" --include="*.rs" crates/ src/ bins/ 2>/dev/null | grep -cv '//' || true)
[ "${INV_HITS:-0}" -eq 0 ] && report 0 "no distributed-slice invocations in source" || report 1 "found $INV_HITS distributed-slice invocations"
# bins/ added to workspace.members
grep -qE '"bins/(engineer|wizard)"' Cargo.toml && report 0 "bins/ in workspace.members" || report 1 "bins/ NOT in workspace.members"

echo "=== $PASS passed, $FAIL failed ==="
[ $FAIL -eq 0 ]
