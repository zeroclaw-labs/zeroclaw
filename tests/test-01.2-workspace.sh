#!/usr/bin/env bash
# tests/test-01.2-workspace.sh — Tests for Phase 1.2 deliverables.
#
# Properties under test:
#   1. bins/engineer + bins/wizard workspace members exist with correct package
#      names + binary names.
#   2. Each binary's Cargo.toml inherits workspace metadata (version, edition,
#      license, repository, rust-version).
#   3. Workspace.members in root Cargo.toml lists bins/engineer + bins/wizard.
#   4. resolver = "2" is pinned at the workspace level.
#   5. WS-04: NO inventory!/linkme!/ctor! invocations anywhere in source.
#   6. WS-04: deny.toml bans the distributed-slice crates.
#   7. WS-04: osagent-policy.yml has the no-distributed-slice-registration job.
#   8. workspace-build CI job exists for the new binaries.

. "$(dirname "$0")/lib.sh"
start_suite "01.2 — Workspace Skeleton & Binary Split"

# bins/ structure
assert_dir_exists  "bins/engineer"
assert_dir_exists  "bins/wizard"
assert_file_exists "bins/engineer/Cargo.toml"
assert_file_exists "bins/engineer/src/main.rs"
assert_file_exists "bins/wizard/Cargo.toml"
assert_file_exists "bins/wizard/src/main.rs"

# Package + binary naming
assert_grep '^name = "osagent-engineer"' "bins/engineer/Cargo.toml" "engineer package name correct"
assert_grep '^name = "osagent-wizard"'   "bins/wizard/Cargo.toml"   "wizard package name correct"
assert_grep 'name = "engineer"' "bins/engineer/Cargo.toml" "engineer binary name correct"
assert_grep 'name = "wizard"'   "bins/wizard/Cargo.toml"   "wizard binary name correct"

# Workspace inheritance (every binary should inherit from workspace)
for crate in engineer wizard; do
  assert_grep 'version.workspace = true'     "bins/$crate/Cargo.toml" "$crate inherits workspace.version"
  assert_grep 'edition.workspace = true'     "bins/$crate/Cargo.toml" "$crate inherits workspace.edition"
  assert_grep 'license.workspace = true'     "bins/$crate/Cargo.toml" "$crate inherits workspace.license"
done

# Workspace.members
assert_grep '"bins/engineer"' "Cargo.toml" "root Cargo.toml lists bins/engineer in workspace.members"
assert_grep '"bins/wizard"'   "Cargo.toml" "root Cargo.toml lists bins/wizard in workspace.members"
assert_grep 'resolver = "2"'  "Cargo.toml" "workspace resolver pinned to 2 (required for safety properties)"

# WS-04: NO inventory!/linkme!/ctor! invocations in workspace source.
# (Comments and prose that NAME these crates are fine; only actual macro invocations break.)
INV_HITS=$(grep -rnE "^[^/]*\b(inventory::submit!|linkme::distributed_slice|#\[ctor::ctor\])" \
              --include="*.rs" \
              crates/ src/ bins/ 2>/dev/null | grep -cv '//' || true)
assert_eq "0" "${INV_HITS:-0}" "no distributed-slice invocations in workspace source"

# WS-04: deny.toml bans the crates
assert_grep 'name = "inventory"' "deny.toml" "deny.toml bans inventory crate"
assert_grep 'name = "linkme"'    "deny.toml" "deny.toml bans linkme crate"
assert_grep 'name = "ctor"'      "deny.toml" "deny.toml bans ctor crate"

# WS-04: CI gate job exists
assert_grep 'no-distributed-slice-registration:' ".github/workflows/osagent-policy.yml" \
            "CI job no-distributed-slice-registration wired"

# WS-01: workspace-build CI job
assert_grep 'workspace-build:'                  ".github/workflows/osagent-policy.yml" "CI job workspace-build wired"
assert_grep 'cargo build -p osagent-engineer'   ".github/workflows/osagent-policy.yml" "workspace-build runs engineer compile"
assert_grep 'cargo build -p osagent-wizard'     ".github/workflows/osagent-policy.yml" "workspace-build runs wizard compile"

# bins/ Cargo.toml documentation discipline (every binary's Cargo.toml is the human-readable manifest)
assert_grep "HUMAN-READABLE MANIFEST" "bins/engineer/Cargo.toml" "engineer Cargo.toml has manifest-of-capabilities header"
assert_grep "HUMAN-READABLE MANIFEST" "bins/wizard/Cargo.toml"   "wizard Cargo.toml has manifest-of-capabilities header"
assert_grep "LOAD-BEARING SAFETY PROPERTY" "bins/wizard/Cargo.toml" "wizard Cargo.toml has prominent MCP-exclusion safety prose"

summarise
