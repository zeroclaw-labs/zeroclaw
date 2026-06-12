#!/usr/bin/env bash
# scripts/wizard-no-mcp-gate.sh — Phase 1.3 milestone-defining 4-layer CI gate
#
# Property under test: the wizard binary contains ZERO MCP code.
#
# Why 4 layers? `nm --defined-only` alone is not sufficient — LTO inlining,
# trait-object monomorphization, and #[no_mangle] survival can each defeat
# symbol-name grep alone. See .planning/research/PITFALLS.md Pitfall 17.
#
#   Layer 1 — source-grep:        Cargo.toml dep edge + bins/wizard/ source files
#                                  contain no `osagent-tools-mcp` reference (other
#                                  than the safety-prose comment block).
#   Layer 2 — nm --defined-only:  target/release/wizard ELF has no `mcp`-named
#                                  defined symbols.
#   Layer 3 — cargo-bloat --crates: `osagent-tools-mcp` does not appear in the
#                                    wizard binary's crate-size breakdown.
#   Layer 4 — strings:             target/release/wizard contains no `mcp_` or
#                                   `model.context.protocol` string constants.
#
# At Phase 1.3 (M1) the wizard binary is a hollow placeholder; all 4 layers
# pass trivially. The gate's real value materializes in M2+ when wizard's
# Cargo.toml grows real dependencies and when zeroclaw-runtime is split per
# the M3 prerequisite.

set -euo pipefail
cd "$(dirname "$0")/.."

PASS=0
FAIL=0
WIZARD_BIN="target/release/wizard"
WIZARD_CARGO="bins/wizard/Cargo.toml"

report() {
  if [ $1 -eq 0 ]; then echo "  ✓ $2"; PASS=$((PASS+1));
  else echo "  ✗ $2"; FAIL=$((FAIL+1)); fi
}

echo "=== wizard-no-mcp-gate (Phase 1.3) ==="

# ─────────────────────────────────────────────────────────────────────
# Layer 1 — source-grep against wizard's Cargo.toml + source files.
# Tolerates the prose comment block at the top that NAMES the rule.
# Catches an actual `[dependencies]` line referencing the crate.
# ─────────────────────────────────────────────────────────────────────
echo "--- Layer 1: source-grep ---"
if grep -nE '^(osagent-tools-mcp|osagent-mcp)\s*(=|\.)' "$WIZARD_CARGO" 2>/dev/null; then
  report 1 "Layer 1: wizard Cargo.toml has MCP dep declaration"
else
  report 0 "Layer 1: wizard Cargo.toml has no MCP dep declaration"
fi

# Also check bins/wizard/src/ — the source code must not import MCP symbols.
if grep -rnE "(use|extern crate)\s+osagent_tools_mcp|use\s+::osagent_tools_mcp" bins/wizard/src/ 2>/dev/null; then
  report 1 "Layer 1: bins/wizard/src/ imports osagent_tools_mcp"
else
  report 0 "Layer 1: bins/wizard/src/ has no MCP imports"
fi

# ─────────────────────────────────────────────────────────────────────
# Layer 2 — nm --defined-only on the wizard binary.
# Skipped on hosts without a release build (Phase 1.3 may run before
# the first CI build completes; CI always re-builds).
# ─────────────────────────────────────────────────────────────────────
echo "--- Layer 2: nm --defined-only ---"
if command -v nm >/dev/null 2>&1; then
  if [ -f "$WIZARD_BIN" ]; then
    MCP_SYMS=$(nm --defined-only "$WIZARD_BIN" 2>/dev/null | grep -iE "(\bmcp\b|McpClient|McpRegistry|McpTool|McpProtocol|McpTransport|McpDeferred)" | head -5 || true)
    if [ -n "$MCP_SYMS" ]; then
      echo "$MCP_SYMS"
      report 1 "Layer 2: wizard binary has MCP-named defined symbols"
    else
      report 0 "Layer 2: wizard binary has no MCP-named defined symbols"
    fi
  else
    echo "  ⊘ Layer 2 SKIPPED — $WIZARD_BIN not built yet (CI will build & verify)"
  fi
else
  echo "  ⊘ Layer 2 SKIPPED — nm not available"
fi

# ─────────────────────────────────────────────────────────────────────
# Layer 3 — cargo bloat --crates on the wizard binary.
# Requires cargo-bloat. CI installs it; local hosts may not.
# ─────────────────────────────────────────────────────────────────────
echo "--- Layer 3: cargo bloat --crates ---"
if command -v cargo >/dev/null 2>&1 && cargo bloat --version >/dev/null 2>&1; then
  if cargo bloat --crates --release -p osagent-wizard 2>/dev/null | grep -iE "osagent-tools-mcp|osagent_tools_mcp" > /dev/null; then
    report 1 "Layer 3: cargo bloat lists osagent-tools-mcp in wizard's crate breakdown"
  else
    report 0 "Layer 3: cargo bloat shows no osagent-tools-mcp in wizard"
  fi
else
  echo "  ⊘ Layer 3 SKIPPED — cargo-bloat not installed (CI installs it)"
fi

# ─────────────────────────────────────────────────────────────────────
# Layer 4 — strings on the wizard binary.
# Catches MCP-related string constants that survive symbol stripping
# (e.g., MCP protocol method names, error messages with "mcp_" prefix).
# ─────────────────────────────────────────────────────────────────────
echo "--- Layer 4: strings ---"
if command -v strings >/dev/null 2>&1; then
  if [ -f "$WIZARD_BIN" ]; then
    MCP_STRS=$(strings "$WIZARD_BIN" 2>/dev/null | grep -iE "(mcp_|model\.context\.protocol|McpClient|McpRegistry|McpToolWrapper)" | head -5 || true)
    if [ -n "$MCP_STRS" ]; then
      echo "$MCP_STRS"
      report 1 "Layer 4: wizard binary contains MCP-related string constants"
    else
      report 0 "Layer 4: wizard binary has no MCP-related string constants"
    fi
  else
    echo "  ⊘ Layer 4 SKIPPED — $WIZARD_BIN not built yet (CI will build & verify)"
  fi
else
  echo "  ⊘ Layer 4 SKIPPED — strings not available"
fi

echo "=== wizard-no-mcp-gate: $PASS passed, $FAIL failed ==="
[ $FAIL -eq 0 ]
