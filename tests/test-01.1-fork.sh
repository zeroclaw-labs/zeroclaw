#!/usr/bin/env bash
# tests/test-01.1-fork.sh — Tests for Phase 1.1 deliverables.
#
# Properties under test:
#   1. Git fork relationship (upstream + origin remotes; osagent-main exists).
#   2. NOTICE has osAgent attribution + "NOT ZeroClaw" disambiguation + verbatim
#      upstream NOTICE preserved below `---` separator (Apache-2.0 §4(d)).
#   3. Cargo.toml has fork provenance metadata + version 0.1.0.
#   4. LICENSE-APACHE + LICENSE-MIT both present.
#   5. deny.toml has license allowlist + AGPL/WTFPL/phone-home bans.
#   6. osagent-policy.yml wires cargo-deny-action at SHA-pinned version.
#   7. UPSTREAM_SYNC.md exists (on either main or feat branch of sibling repo)
#      and has all 8 required sections + quarterly cadence reference.

. "$(dirname "$0")/lib.sh"
start_suite "01.1 — Fork & Attribution & Sync Runbook"

# Git fork wiring
assert_cmd_ok "git remote -v | grep -qE '^upstream\s+https://github\.com/zeroclaw-labs/zeroclaw'" "upstream remote points at zeroclaw-labs/zeroclaw"
assert_cmd_ok "git remote -v | grep -qE '^origin\s+https://github\.com/andreas2301/osAgent'"  "origin remote points at andreas2301/osAgent"
assert_cmd_ok "git branch --list osagent-main | grep -q osagent-main"                          "osagent-main branch exists"

# NOTICE structure
assert_file_exists "NOTICE"
assert_grep "osAgent" "NOTICE"                                  "NOTICE mentions osAgent"
assert_grep "osAgent is NOT ZeroClaw" "NOTICE"                  "NOTICE has fork disambiguation"
assert_grep "Forked from" "NOTICE"                              "NOTICE has fork provenance block"
assert_grep "zeroclaw-labs/zeroclaw" "NOTICE"                   "NOTICE names upstream"
assert_grep "v0\.7\.5" "NOTICE"                                 "NOTICE pins upstream tag"
assert_grep "a2988f0dfffa0c14fa56e218c7bb9f28da494da4" "NOTICE" "NOTICE includes upstream SHA"
assert_grep "Upstream NOTICE \\(preserved verbatim" "NOTICE"    "NOTICE preserves upstream block per Apache-2.0 §4(d)"
assert_grep "Copyright 2025 ZeroClaw Labs" "NOTICE"             "NOTICE retains upstream copyright line"

# Cargo.toml fork metadata
assert_file_exists "Cargo.toml"
assert_grep '^version = "0\.1\.0"' "Cargo.toml"                                  "workspace.package.version = 0.1.0"
assert_grep 'forked_from = "zeroclaw-labs/zeroclaw"' "Cargo.toml"                "workspace.metadata.osagent.forked_from"
assert_grep 'forked_at_tag = "v0\.7\.5"' "Cargo.toml"                            "workspace.metadata.osagent.forked_at_tag"
assert_grep 'forked_at_sha = "a2988f0dfffa0c14fa56e218c7bb9f28da494da4"' "Cargo.toml" "workspace.metadata.osagent.forked_at_sha"

# License files
assert_file_exists "LICENSE-APACHE" "LICENSE-APACHE present (Apache-2.0 dual license)"
assert_file_exists "LICENSE-MIT"    "LICENSE-MIT present (MIT dual license)"

# deny.toml policy
assert_file_exists "deny.toml"
assert_grep 'all-features = false' "deny.toml"                  "deny.toml [graph].all-features = false (required for wizard-no-MCP)"
assert_grep '"MIT"' "deny.toml"                                  "deny.toml licenses.allow contains MIT"
assert_grep '"Apache-2\.0"' "deny.toml"                          "deny.toml licenses.allow contains Apache-2.0"
assert_grep 'wildcards = "deny"' "deny.toml"                     "deny.toml bans.wildcards = deny"
assert_grep 'name = "presage"' "deny.toml"                       "deny.toml bans presage (AGPL Signal SDK)"
assert_grep 'name = "libsignal' "deny.toml"                      "deny.toml bans libsignal-* (AGPL)"
assert_grep 'name = "frankenstein"' "deny.toml"                  "deny.toml bans frankenstein (WTFPL)"
assert_grep 'name = "sentry"' "deny.toml"                        "deny.toml bans sentry (phone-home)"
assert_grep 'name = "posthog' "deny.toml"                        "deny.toml bans posthog (phone-home)"
assert_grep 'name = "honeycomb' "deny.toml"                      "deny.toml bans honeycomb (phone-home)"
assert_grep 'name = "opentelemetry-otlp"' "deny.toml"            "deny.toml bans opentelemetry-otlp (phone-home transport)"

# osagent-policy.yml CI workflow
assert_file_exists ".github/workflows/osagent-policy.yml"
assert_grep "on:" ".github/workflows/osagent-policy.yml"
assert_grep "EmbarkStudios/cargo-deny-action@8f84122a46a358a27cb0625d85ad60ab436a1b87" ".github/workflows/osagent-policy.yml" \
            "cargo-deny-action pinned to v2.0.20 SHA"
assert_grep "cargo-deny-licenses-bans-sources:" ".github/workflows/osagent-policy.yml" "licenses-bans-sources job present"
assert_grep "cargo-deny-advisories:" ".github/workflows/osagent-policy.yml"            "advisories job present"

# UPSTREAM_SYNC.md (on either main or feat branch of sibling repo)
SS_BACKUP=../sovereign-shield-backup
SS_DOC=$SS_BACKUP/documentation/osAgent/UPSTREAM_SYNC.md
if [ -d "$SS_BACKUP/.git" ]; then
  if [ -f "$SS_DOC" ]; then
    SYNC_CONTENT=$(cat "$SS_DOC")
  else
    SYNC_CONTENT=$(cd "$SS_BACKUP" && git show feat/osagent-upstream-sync-runbook:documentation/osAgent/UPSTREAM_SYNC.md 2>/dev/null || echo "")
  fi
  if [ -n "$SYNC_CONTENT" ]; then
    _log_pass "UPSTREAM_SYNC.md present in sovereign-shield-backup (main or feat branch)"
    echo "$SYNC_CONTENT" | grep -qE "^## Purpose"                  && _log_pass "UPSTREAM_SYNC.md has §Purpose"                  || _log_fail "UPSTREAM_SYNC.md missing §Purpose"
    echo "$SYNC_CONTENT" | grep -qE "^## Branches"                 && _log_pass "UPSTREAM_SYNC.md has §Branches"                 || _log_fail "UPSTREAM_SYNC.md missing §Branches"
    echo "$SYNC_CONTENT" | grep -qE "^## Cadence"                  && _log_pass "UPSTREAM_SYNC.md has §Cadence"                  || _log_fail "UPSTREAM_SYNC.md missing §Cadence"
    echo "$SYNC_CONTENT" | grep -qE "^## Procedure"                && _log_pass "UPSTREAM_SYNC.md has §Procedure"                || _log_fail "UPSTREAM_SYNC.md missing §Procedure"
    echo "$SYNC_CONTENT" | grep -qE "^## Out-of-Cycle"             && _log_pass "UPSTREAM_SYNC.md has §Out-of-Cycle"             || _log_fail "UPSTREAM_SYNC.md missing §Out-of-Cycle"
    echo "$SYNC_CONTENT" | grep -qE "^## Refuse-to-Merge"          && _log_pass "UPSTREAM_SYNC.md has §Refuse-to-Merge"          || _log_fail "UPSTREAM_SYNC.md missing §Refuse-to-Merge"
    echo "$SYNC_CONTENT" | grep -qE "^## Append-Only Conflict Log" && _log_pass "UPSTREAM_SYNC.md has §Append-Only Conflict Log" || _log_fail "UPSTREAM_SYNC.md missing §Append-Only Conflict Log"
    echo "$SYNC_CONTENT" | grep -qE "^## Worked Example"           && _log_pass "UPSTREAM_SYNC.md has §Worked Example"           || _log_fail "UPSTREAM_SYNC.md missing §Worked Example"
    echo "$SYNC_CONTENT" | grep -qE "Q1.*Q2.*Q3.*Q4|quarterly"     && _log_pass "UPSTREAM_SYNC.md references quarterly cadence"  || _log_fail "UPSTREAM_SYNC.md missing quarterly cadence"
  else
    _log_fail "UPSTREAM_SYNC.md not found on main or feat branch"
  fi
else
  echo "  ⊘ sovereign-shield-backup sibling repo not available — skipping UPSTREAM_SYNC.md checks"
fi

summarise
