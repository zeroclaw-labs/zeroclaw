#!/usr/bin/env bash
# tests/test-01.4-strips.sh — Tests for Phase 1.4 deliverables.
#
# Properties under test:
#   1. 5 dead crates physically absent (STRIP-01).
#   2. crates/zeroclaw-channels/src/webhook.rs absent (STRIP-06).
#   3. webhook channel feature definitions absent (STRIP-06).
#   4. opentelemetry-otlp / observability-otel absent (TELEMETRY-01).
#   5. No sentry/posthog/honeycomb/datadog deps anywhere (audit clean).
#   6. MCP files migrated: present in crates/osagent-tools-mcp/src/; absent
#      from crates/zeroclaw-tools/src/ and src/tools/.
#   7. zeroclaw-runtime now imports MCP from osagent_tools_mcp (not zeroclaw_tools).
#   8. Root-crate integration modules (src/hardware, src/peripherals, src/plugins) absent.
#   9. Workspace.members + workspace.dependencies are clean (no refs to dropped crates).

. "$(dirname "$0")/lib.sh"
start_suite "01.4 — Whole-Crate Drops + Telemetry + MCP Migration"

# STRIP-01: 5 dead crates physically gone
assert_file_absent "crates/zeroclaw-hardware"  "STRIP-01 zeroclaw-hardware crate dir absent"
assert_file_absent "crates/robot-kit"          "STRIP-01 robot-kit crate dir absent"
assert_file_absent "crates/aardvark-sys"       "STRIP-01 aardvark-sys crate dir absent"
assert_file_absent "crates/zeroclaw-plugins"   "STRIP-01 zeroclaw-plugins crate dir absent"
assert_file_absent "apps/tauri"                "STRIP-01 apps/tauri dir absent"

# STRIP-06: webhook channel triple-removed
assert_file_absent "crates/zeroclaw-channels/src/webhook.rs" "STRIP-06 webhook.rs absent"
assert_no_grep    "pub mod webhook;"           "crates/zeroclaw-channels/src/lib.rs" "STRIP-06 webhook mod decl absent"
assert_no_grep    "^channel-webhook = "        "Cargo.toml" "STRIP-06 channel-webhook feature absent in root Cargo.toml"

# TELEMETRY-01: OTLP and observability-otel gone
# Match an actual TOML dep declaration (line starts with the crate name and `=` follows),
# not the explanatory comment that names the crate as part of documenting the strip.
assert_no_grep "^opentelemetry-otlp\\s*=" "crates/zeroclaw-runtime/Cargo.toml" "TELEMETRY-01 opentelemetry-otlp dep absent in runtime"
assert_no_grep "^observability-otel = " "crates/zeroclaw-runtime/Cargo.toml" "TELEMETRY-01 observability-otel feature absent in runtime"
assert_no_grep "^observability-otel = " "Cargo.toml" "TELEMETRY-01 observability-otel feature absent in root"

# TELEMETRY-01: audit cleanliness — no phone-home crate references anywhere as actual deps
# (References inside comments and string literals are tolerated; deny.toml's deny list is fine.)
PHONE_HOME_HITS=$(grep -rnE '^[a-z][a-z0-9_-]*sentry|^sentry\s*=|^posthog-rust\s*=|^honeycomb-tracing\s*=|^datadog\s*=' \
                  --include="*.toml" -- crates/ bins/ 2>/dev/null | wc -l)
assert_eq "0" "${PHONE_HOME_HITS:-0}" "TELEMETRY-01 no phone-home crate deps in any workspace Cargo.toml"

# MCP migration: files moved to osagent-tools-mcp
for f in mcp_client mcp_deferred mcp_protocol mcp_tool mcp_transport; do
  assert_file_exists "crates/osagent-tools-mcp/src/$f.rs"     "MCP $f.rs migrated to osagent-tools-mcp"
  assert_file_absent "crates/zeroclaw-tools/src/$f.rs"        "MCP $f.rs removed from zeroclaw-tools (was duplicated)"
  assert_file_absent "src/tools/$f.rs"                        "MCP $f.rs removed from src/tools (was duplicated)"
done
assert_grep "pub mod mcp_client" "crates/osagent-tools-mcp/src/lib.rs" "osagent-tools-mcp lib.rs declares mcp_client mod"

# zeroclaw-tools lib.rs cleaned
assert_no_grep "^pub mod mcp_client" "crates/zeroclaw-tools/src/lib.rs" "zeroclaw-tools no longer exposes mcp_client mod"

# zeroclaw-runtime imports MCP from new location
assert_grep    "use osagent_tools_mcp::mcp_client" "crates/zeroclaw-runtime/src/tools/mod.rs" "runtime imports MCP from osagent_tools_mcp"
assert_no_grep "use zeroclaw_tools::mcp_client"    "crates/zeroclaw-runtime/src/tools/mod.rs" "runtime no longer imports MCP from zeroclaw_tools"
assert_grep    "osagent-tools-mcp"                  "crates/zeroclaw-runtime/Cargo.toml"       "runtime Cargo.toml lists osagent-tools-mcp dep"

# Root-crate integration modules gone
assert_file_absent "src/hardware"    "root-crate hardware module dir absent"
assert_file_absent "src/peripherals" "root-crate peripherals module dir absent"
assert_file_absent "src/plugins"     "root-crate plugins module dir absent"

assert_no_grep "^pub mod peripherals" "src/lib.rs"  "src/lib.rs no longer declares peripherals mod"
assert_no_grep "^pub mod plugins"     "src/lib.rs"  "src/lib.rs no longer declares plugins mod"
assert_no_grep "^mod hardware"        "src/main.rs" "src/main.rs no longer declares hardware mod"
assert_no_grep "^mod peripherals"     "src/main.rs" "src/main.rs no longer declares peripherals mod"
assert_no_grep "^mod plugins"         "src/main.rs" "src/main.rs no longer declares plugins mod"

# Workspace.members + workspace.dependencies hygiene
assert_no_grep '"crates/zeroclaw-hardware"' "Cargo.toml" "workspace.members no longer lists zeroclaw-hardware"
assert_no_grep '"crates/robot-kit"'         "Cargo.toml" "workspace.members no longer lists robot-kit"
assert_no_grep '"crates/aardvark-sys"'      "Cargo.toml" "workspace.members no longer lists aardvark-sys"
assert_no_grep '"crates/zeroclaw-plugins"'  "Cargo.toml" "workspace.members no longer lists zeroclaw-plugins"
assert_no_grep '"apps/tauri"'               "Cargo.toml" "workspace.members no longer lists apps/tauri"

assert_no_grep '^zeroclaw-hardware = '      "Cargo.toml" "workspace.dependencies no longer lists zeroclaw-hardware"
assert_no_grep '^zeroclaw-plugins = '       "Cargo.toml" "workspace.dependencies no longer lists zeroclaw-plugins"
assert_no_grep '^aardvark-sys = '           "Cargo.toml" "workspace.dependencies no longer lists aardvark-sys"

summarise
