#!/usr/bin/env bash
# tests/test-01.5-strips.sh — Tests for Phase 1.5 deliverables.
# Written test-first (TDD); each block goes RED first, then implementation
# moves it GREEN.

. "$(dirname "$0")/lib.sh"
start_suite "01.5 — Source Strips + MANIFEST Emission"

# ──────────────────────────────────────────────────────────────────────────
# STRIP-02: channels stripped to v1 set (6 kept).
# Keep: telegram, slack, mattermost (planner: in-house wrapper), matrix,
#       whatsapp-cloud (planner: in-house wrapper), signal (M4 via signal-cli
#       subprocess — not a Rust SDK; M1 just ensures no AGPL crate slips in).
# Drop everything else at source level.
# ──────────────────────────────────────────────────────────────────────────
echo "--- STRIP-02 (channels) ---"

KEPT_CHANNELS=(telegram slack matrix mattermost whatsapp_cloud signal)

# Channel directory under crates/zeroclaw-channels/src: every NON-kept .rs file
# (or directory) we stripped should be physically absent.
DROPPED_CHANNELS=(
  discord discord_history irc imessage matrix_history dingtalk qq bluesky twitter
  reddit notion linq wati nextcloud_talk mochat wecom clawdtalk voice_call
  voice_wake nostr feishu wechat lark email gmail_push line acp_server
)
for ch in "${DROPPED_CHANNELS[@]}"; do
  assert_file_absent "crates/zeroclaw-channels/src/$ch.rs" "STRIP-02 $ch.rs source absent"
done

# Feature definitions for dropped channels MUST be gone from both channels crate
# and root Cargo.toml.
for ch in "${DROPPED_CHANNELS[@]}"; do
  # convert underscore to hyphen for feature naming convention (channel-foo-bar)
  feature_name="channel-${ch//_/-}"
  assert_no_grep "^${feature_name}\\s*=" "crates/zeroclaw-channels/Cargo.toml" "STRIP-02 $feature_name feature absent in channels crate"
done

# ──────────────────────────────────────────────────────────────────────────
# STRIP-07: i18n non-en locales stripped.
# Mozilla Fluent pipeline retained; only en-US .ftl files survive.
# ──────────────────────────────────────────────────────────────────────────
# ──────────────────────────────────────────────────────────────────────────
# STRIP-03: providers stripped to v1 set.
# Keep: anthropic, gemini, openai (base for openai-compatible/openrouter),
#       ollama, openrouter, plus compatible/multimodal/router/reliable/traits/
#       models_dev support modules.
# ──────────────────────────────────────────────────────────────────────────
echo "--- STRIP-03 (providers) ---"
DROPPED_PROVIDERS=(azure_openai bedrock claude_code copilot gemini_cli glm kilocli openai_codex telnyx)
for p in "${DROPPED_PROVIDERS[@]}"; do
  assert_file_absent "crates/zeroclaw-providers/src/$p.rs"     "STRIP-03 $p.rs source absent"
  assert_no_grep    "^pub mod $p;"  "crates/zeroclaw-providers/src/lib.rs" "STRIP-03 $p mod declaration absent"
done

# ──────────────────────────────────────────────────────────────────────────
# STRIP-04: tools stripped — keep file ops + memory + git_operations + a few
# support modules; drop browser/web/hardware/social/desktop tools.
# ──────────────────────────────────────────────────────────────────────────
echo "--- STRIP-04 (tools) ---"
DROPPED_TOOLS=(
  browser browser_delegate browser_open canvas claude_code claude_code_runner
  codex_cli composio discord_search escalate gemini_cli google_workspace
  hardware_board_info hardware_memory_map hardware_memory_read http_request
  image_gen jira_tool linkedin linkedin_client llm_task microsoft365
  notion_tool opencode_cli project_intel pushover reaction screenshot swarm
  text_browser weather_tool web_fetch web_search_provider_routing
  web_search_tool cloud_ops cloud_patterns backup_tool
)
for t in "${DROPPED_TOOLS[@]}"; do
  assert_file_absent "crates/zeroclaw-tools/src/$t.rs"        "STRIP-04 $t.rs source absent"
  assert_no_grep    "^pub mod $t;" "crates/zeroclaw-tools/src/lib.rs" "STRIP-04 $t mod declaration absent"
done

echo "--- STRIP-07 (non-en locales) ---"
# Fluent locales live under crates/zeroclaw-runtime/locales/<lang>/ in v0.7.5.
LOCALES_DIR=crates/zeroclaw-runtime/locales
assert_dir_exists "$LOCALES_DIR/en" "STRIP-07 Fluent pipeline kept (en/ locale survives)"
NON_EN_LOCALES=$(find "$LOCALES_DIR" -maxdepth 1 -mindepth 1 -type d -not -name "en" 2>/dev/null | wc -l)
assert_eq "0" "${NON_EN_LOCALES:-0}" "STRIP-07 no non-en locale dirs remain (Fluent pipeline retained, non-en stripped)"

# ──────────────────────────────────────────────────────────────────────────
# MANIFEST-01: build emits MANIFEST.toml listing every compiled-in channel,
#              provider, and tool, with [declared] AND [detected] sections.
# At Phase 1.5 the framework lands; physical emission via build.rs.
# ──────────────────────────────────────────────────────────────────────────
echo "--- MANIFEST-01 ---"
assert_file_exists "MANIFEST.toml.template" "MANIFEST.toml template scaffold present"
# (Actual MANIFEST.toml is build-emitted; CI tests it post-build.)

# ──────────────────────────────────────────────────────────────────────────
# MANIFEST-02: `osagent manifest --diff <config.toml>` CLI subcommand
# (Phase 1.5 ships the subcommand; M2/M3 wire it into both binaries.)
# At M1 we test that the CLI handler exists in the shared crate.
# ──────────────────────────────────────────────────────────────────────────
echo "--- MANIFEST-02 ---"
assert_file_exists "crates/osagent-manifest/Cargo.toml" "osagent-manifest crate exists"
assert_file_exists "crates/osagent-manifest/src/lib.rs"  "osagent-manifest lib.rs exists"
assert_grep 'pub fn manifest_diff' "crates/osagent-manifest/src/lib.rs" "manifest_diff function defined"

# ──────────────────────────────────────────────────────────────────────────
# MANIFEST-03: reproducible-build profile pinned.
# codegen-units=1, lto=fat (already in some upstream profiles; verify),
# strip=symbols, panic=abort, CARGO_INCREMENTAL=0 in CI.
# ──────────────────────────────────────────────────────────────────────────
echo "--- MANIFEST-03 ---"
assert_grep '\[profile\.release\]' "Cargo.toml" "release profile defined"
assert_grep 'codegen-units = 1'    "Cargo.toml" "release codegen-units=1 (reproducibility)"
assert_grep 'lto = ' "Cargo.toml" "release lto pinned"
assert_grep 'CARGO_INCREMENTAL: 0' ".github/workflows/osagent-policy.yml" "CI sets CARGO_INCREMENTAL=0"

summarise
