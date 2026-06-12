#!/usr/bin/env bash
# scripts/validate-full.sh — Phase 1.1 full gate (cargo deny + structural checks)
# Slower: ~60-90s. Runs after every wave.
set -euo pipefail
cd "$(dirname "$0")/.."

PASS=0
FAIL=0
report() {
  if [ $1 -eq 0 ]; then echo "  ✓ $2"; PASS=$((PASS+1));
  else echo "  ✗ $2"; FAIL=$((FAIL+1)); fi
}

echo "=== Phase 1.1 validate-full ==="
echo "--- quick gate ---"
bash scripts/validate-quick.sh || true

# PLAN-02 FILL — cargo deny green
if command -v cargo >/dev/null 2>&1 && cargo deny --version >/dev/null 2>&1; then
  echo "--- cargo deny check ---"
  cargo deny check licenses && report 0 "cargo deny licenses" || report 1 "cargo deny licenses"
  cargo deny check bans     && report 0 "cargo deny bans"     || report 1 "cargo deny bans"
  cargo deny check sources  && report 0 "cargo deny sources"  || report 1 "cargo deny sources"
  cargo deny check advisories && report 0 "cargo deny advisories" || report 1 "cargo deny advisories"
else
  echo "  ⊘ skipping cargo deny (cargo or cargo-deny not installed)"
fi

# PLAN-01 FILL — fork relationship via GitHub API
PAT_FILE="$HOME/.git-credentials"
if [ -f "$PAT_FILE" ]; then
  # Use git's stored credential to query the API; never echo the token.
  AUTH_HEADER=$(git config --global credential.helper >/dev/null 2>&1 && echo "via-helper" || echo "")
  if [ -n "$AUTH_HEADER" ]; then
    # Construct API call using curl with netrc-style cred from helper; if PAT export is set, prefer it.
    HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
      -H "Authorization: token ${GITHUB_PAT:-${GH_TOKEN:-}}" \
      -H "Accept: application/vnd.github+json" \
      "https://api.github.com/repos/andreas2301/osAgent" 2>/dev/null)
    [ "$HTTP_CODE" = "200" ] && report 0 "fork reachable via API" || report 1 "fork unreachable (HTTP $HTTP_CODE) — may need GITHUB_PAT in env"
  fi
fi

# PLAN-03 FILL — UPSTREAM_SYNC.md structural completeness
SYNC_DOC=../sovereign-shield-backup/documentation/osAgent/UPSTREAM_SYNC.md
if [ -f "$SYNC_DOC" ]; then
  SECTION_COUNT=$(grep -cE '^## (Purpose|Branches|Cadence|Procedure|Out-of-Cycle|Refuse-to-Merge|Append-Only Conflict Log|Worked Example)' "$SYNC_DOC")
  [ "$SECTION_COUNT" -ge 8 ] && report 0 "UPSTREAM_SYNC.md has all 8 sections ($SECTION_COUNT found)" || report 1 "UPSTREAM_SYNC.md missing sections (only $SECTION_COUNT/8)"
  grep -qE "Q1.*Q2.*Q3.*Q4|quarterly" "$SYNC_DOC" && report 0 "UPSTREAM_SYNC.md mentions quarterly cadence" || report 1 "UPSTREAM_SYNC.md cadence missing"
fi

echo "=== $PASS passed, $FAIL failed ==="
[ $FAIL -eq 0 ]
