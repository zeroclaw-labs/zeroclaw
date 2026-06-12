#!/usr/bin/env bash
# tests/test-01.3-mcp-boundary.sh — Tests for Phase 1.3 (MILESTONE-DEFINING).
#
# Properties under test (load-bearing for M1):
#   1. crates/osagent-tools-mcp/ exists as a workspace member.
#   2. bins/wizard/Cargo.toml has NO actual dependency declaration on
#      osagent-tools-mcp or osagent-mcp (comments mentioning the rule are fine).
#   3. bins/wizard/src/ has NO Rust imports of osagent_tools_mcp.
#   4. scripts/wizard-no-mcp-gate.sh exists and is executable; implements 4 layers.
#   5. CI workflow has the wizard-no-mcp-gate job with `needs: [workspace-build]`.
#   6. CI workflow installs cargo-bloat (Layer 3 dep).
#   7. Layers 1 of the gate run green locally (Layers 2/3/4 fire on CI).
#
# This is the MILESTONE-DEFINING test. M1 cannot close until these pass.

. "$(dirname "$0")/lib.sh"
start_suite "01.3 — MCP Boundary & 4-Layer CI Gate (MILESTONE-DEFINING)"

# Crate boundary
assert_dir_exists  "crates/osagent-tools-mcp"
assert_file_exists "crates/osagent-tools-mcp/Cargo.toml"
assert_file_exists "crates/osagent-tools-mcp/src/lib.rs"
assert_grep '^name = "osagent-tools-mcp"' "crates/osagent-tools-mcp/Cargo.toml" "crate package name correct"
assert_grep '"crates/osagent-tools-mcp"'   "Cargo.toml"                          "crate in workspace.members"

# Wizard binary has NO actual MCP dep declaration (TOML key-assignment line only;
# safety-prose comments mentioning the crate name are fine).
WIZARD_DEP_HITS=$(grep -nE '^(osagent-tools-mcp|osagent-mcp)\s*(=|\.)' bins/wizard/Cargo.toml 2>/dev/null | wc -l)
assert_eq "0" "${WIZARD_DEP_HITS:-0}" "wizard Cargo.toml has zero MCP dep declarations"

# Wizard source has NO Rust imports of MCP
WIZARD_IMPORT_HITS=$(grep -rnE "(use|extern crate)\s+osagent_tools_mcp|use\s+::osagent_tools_mcp" bins/wizard/src/ 2>/dev/null | wc -l)
assert_eq "0" "${WIZARD_IMPORT_HITS:-0}" "wizard src/ has zero MCP imports"

# Gate script
assert_file_exists "scripts/wizard-no-mcp-gate.sh"
assert_cmd_ok "test -x scripts/wizard-no-mcp-gate.sh" "wizard-no-mcp-gate.sh is executable"
assert_grep "Layer 1: source-grep"      "scripts/wizard-no-mcp-gate.sh" "gate implements Layer 1"
assert_grep "Layer 2: nm --defined-only" "scripts/wizard-no-mcp-gate.sh" "gate implements Layer 2"
assert_grep "Layer 3: cargo bloat"       "scripts/wizard-no-mcp-gate.sh" "gate implements Layer 3"
assert_grep "Layer 4: strings"           "scripts/wizard-no-mcp-gate.sh" "gate implements Layer 4"

# CI workflow wiring
assert_grep "wizard-no-mcp-gate:"                                 ".github/workflows/osagent-policy.yml" "CI wizard-no-mcp-gate job present"
assert_grep "needs: \\[workspace-build\\]"                        ".github/workflows/osagent-policy.yml" "wizard-no-mcp-gate runs AFTER workspace-build"
assert_grep "cargo install cargo-bloat"                           ".github/workflows/osagent-policy.yml" "CI installs cargo-bloat for Layer 3"
assert_grep "scripts/wizard-no-mcp-gate\\.sh"                     ".github/workflows/osagent-policy.yml" "CI invokes gate script"

# Live Layer 1 check (always available; Layers 2/3/4 skip on Windows hosts)
assert_cmd_ok "bash scripts/wizard-no-mcp-gate.sh 2>&1 | grep -q 'wizard-no-mcp-gate.*0 failed'" \
              "scripts/wizard-no-mcp-gate.sh runs green locally (Layer 1 must pass; Layers 2-4 skip allowed)"

summarise
