# v0.8.0 Excision Pass — Incident Log

Working audit trail for the v0.8.0 excision pass. Each entry records a deletion candidate where the decision wasn't pure-delete: the surrounding test was real, the call-site couldn't be reached safely, or the suppression turned out to mark live code.

Format: site, decision (deleted / kept / kept-with-narrow), reason.

## Pre-existing test failures (not introduced by the excision pass)

Two failing onboard tests on the branch — fail with the excision pass applied AND reproduce on the pre-excision commit. The wizard's quick-mode prompts for the per-channel `excluded-tools` field, but neither test provides an answer for that prompt:

- `crates/zeroclaw-runtime/src/onboard/mod.rs::onboard::tests::channels_telegram_selection_writes_entry` (line 2138)
- `crates/zeroclaw-runtime/src/onboard/mod.rs::onboard::tests::channels_mochat_selection_persists_url_and_token` (line 2174)

Failure: `quick mode: no answer or default provided for prompt "excluded-tools"`.

Out of scope for the excision pass; the right fix is either (a) extend `QuickUi` answers in each test to include `with("excluded-tools", "")`, or (b) fix the wizard to skip prompting when a `Vec<String>` field carries `#[serde(default)]`. Belongs to the wizard maintainers, not this excision.

## Phase 1 — Orphaned files

- `v3.toml` (785 lines, repo root) — **deleted**. Zero references in code, docs, tests, scripts, .gitignore, CI. Residue from the scrapped `zeroclaw config generate` (commit 73f906474).
- `release-notes-notes.md` (32 lines, repo root) — **deleted**. Scratch TODOs accidentally committed; bullets belong in the runbook PR.

## Phase 2 — `#[allow(dead_code)]` sweep

### Skipped (test impact would force test edits per Q1 rule)

- `crates/zeroclaw-tools/src/google_workspace.rs:28` `rate_limit_per_minute: u32` — kept. Field is dead in production but constructor is invoked by ~14 legitimate tests of other tool methods. Removing would force test signature edits with no test-semantic gain.
- `crates/zeroclaw-providers/src/azure_openai.rs:14,16` `resource_name`, `deployment_name` — kept. Constructor is called from many tests (4-arg `new()`); a couple of tests assert on these fields specifically (lines 576-577) but most just construct. Removing forces test edits across the file.
- `crates/zeroclaw-runtime/src/agent/agent.rs:49` `allowed_tools: Option<Vec<String>>` — kept. Identically-named field exists in `crates/zeroclaw-gateway/src/api.rs:82` and `crates/zeroclaw-runtime/src/cron/types.rs:152,190`; verifying full disconnection is a multi-crate trace, deferred.
- `crates/zeroclaw-runtime/src/security/audit.rs:225` `buffer: Mutex<Vec<AuditEvent>>` — kept. Buffered batch-flush could be a half-wired feature; flushing a Mutex<Vec> as part of audit chain is load-bearing in a way that needs a deeper trace before deletion.
- `src/service/mod.rs:6`, `src/integrations/mod.rs:7`, `src/hardware/mod.rs:8`, `src/skills/mod.rs:23` — kept. The `handle_command` dispatchers are wired only on certain feature combinations; the `#[allow(dead_code)]` is a wrong-shape suppression but converting to `#[cfg(feature = "X")]` requires per-crate feature audit.
- `crates/zeroclaw-tools/src/browser.rs:66,68,70,2006` — kept. Fields/fns gated to the `browser-native` feature; the suppression marks the cfg-off path, which is a legitimate (if ugly) pattern.
- `crates/zeroclaw-plugins/src/host.rs:25` `verification: VerificationResult` — kept. Plugin trust audit field; deletion needs a security/trust review.
- `tests/integration/channel_matrix.rs:19`, `crates/zeroclaw-runtime/src/agent/tests.rs:63`, `crates/zeroclaw-runtime/src/sop/engine.rs:1002` — kept. Test file / `#[cfg(test)]` block content; user directive: don't touch tests.
- `crates/zeroclaw-channels/src/lark.rs:179` `event_id`, `crates/zeroclaw-channels/src/bluesky.rs:48,62`, `crates/zeroclaw-channels/src/reddit.rs:45` — kept. Deserializer struct fields. Serde reads and discards them; deletion would force a separate "manually skip in serde" change.
- `crates/zeroclaw-providers/src/bedrock.rs:532,555,571` — kept. Same shape as the lark/bluesky case (response-deserialize fields).

### Deleted

(see commits `chore(excision): drop WIP stubs in tools + gateway` and following)

## Phase 3 — Stale comment refs (PR / issue / phase numbers)

(populated)

## Phase 4 — Stale `#[serde(alias)]`

Three `#[serde(alias = ...)]` annotations in `crates/zeroclaw-config/src/schema.rs`. Reviewed; all kept:

- `:3646` `alias = "api_url"` on `TtsProviderConfig.uri` — the V1/V2-era field rename. The `migration.rs` walker doesn't currently rewrite this for TTS configs (only for `ModelProviderConfig`), so the serde alias is the operator-facing migration path for old TOMLs. **Kept** until the migration walker is extended.
- `:7718` `alias = "dbURL", "database_url", "databaseUrl"` on `PostgresStorageConfig.db_url` — same pattern. Multiple legacy/camelCase forms accepted; migration doesn't rewrite. **Kept**.
- `:11267` `alias = "sandbox-exec"` on `SandboxBackend::SandboxExec` — the parent enum carries `#[serde(rename_all = "lowercase")]` so the wire form is `sandboxexec`. The alias preserves the natural kebab `sandbox-exec` form for operators. **Kept** as UX, not legacy.

## Phase 5 — `channels_except_webhook` + `channels` hand-rolled lists

`crates/zeroclaw-config/src/schema.rs:9509-9628` — 120 lines of `(Box::new(ConfigWrapper::new(self.<field>.get("default"))), !self.<field>.is_empty())` per channel field.

Investigated for collapse but **kept**:

- The function `channels()` has 4 real callers (`crates/zeroclaw-gateway/src/api.rs:113,859`, `crates/zeroclaw-runtime/src/daemon/mod.rs:1055`, `crates/zeroclaw-runtime/src/integrations/registry.rs:82,194`).
- `channels_except_webhook` has exactly one caller: `channels()` itself.
- Collapsing into a Configurable-derive emit would net ~−80 lines of schema for ~+40 of macro work, but requires the macro to handle `#[cfg(feature = "channel-nostr")]` / `#[cfg(feature = "voice-wake")]` cfg-gated fields. That's risky on a release branch.
- Inlining `channels_except_webhook` into `channels()` is net-zero and only removes one level of indirection.

Defer to v0.9.0 along with the broader trait/macro consolidation of orchestrator dispatch matches discussed earlier in the excision pass.

## Phase 6 — FeishuConfig folded into LarkConfig

Followed the DiscordHistory fold pattern:

1. New V2→V3 migration step (split into `strip_feishu_block` pre-wrap + `inject_feishu_as_lark_alias` post-wrap) in `crates/zeroclaw-config/src/schema/v2.rs`. The V2 `[channels.feishu]` block lands as **`[channels.lark.feishu]`** (alias `feishu`, not `default`) with `use_feishu = true`. Two-bot V2 deployments with both `[channels.lark]` and `[channels.feishu]` survive as two distinct V3 aliases — `lark.default` and `lark.feishu` — without any merge, drop, or operator intervention. Three migration tests cover feishu-only, two-bot, and same-app_id scenarios.
2. `FeishuConfig` struct + `impl ChannelConfig for FeishuConfig` deleted from schema.
3. `pub feishu: HashMap<String, FeishuConfig>` field deleted from `ChannelsConfig`.
4. `"feishu"` removed from the V3_CHANNEL_TYPES alias-wrap list (the fold has already run by then).
5. `"channel.feishu"` removed from the schema's TYPE_NAMES const (parallel naming list).
6. `LarkChannel::from_feishu_config` deleted from the channel impl. `from_lark_config` also deleted — the dispatcher now uses `from_config` everywhere, which respects `use_feishu` correctly so the "explicit-lark, ignore use_feishu" override is redundant.
7. Orchestrator: removed the `"feishu" =>` dispatcher arm and the `for (alias, fs) in &config.channels.feishu` health-check loop. The lark health-check loop now picks `"Feishu"` vs `"Lark"` for `display_name` based on `lk.use_feishu`.
8. `channel-feishu = ["channel-lark"]` feature alias removed from root Cargo.toml.
9. Schema tests using `FeishuConfig` switched to `LarkConfig` with `use_feishu: true` (`channels.lark.feishu` instead of `channels.feishu.default`). Pure FeishuConfig serde/toml tests deleted (orphan after the struct's removal).
10. `lark_from_feishu_config_*` and `lark_from_lark_config_ignores_legacy_feishu_flag` tests deleted as orphans of the deleted methods. Replaced with one `lark_from_config_with_use_feishu_routes_to_feishu` test that pins the equivalent path.
11. `channels.feishu.is_empty()` assertion removed from `tests/component/config_schema.rs`.
12. Stale `channel-feishu` entry stripped from `docs/book/src/foundations/fnd-001-intentional-architecture.md` retire-to-plugin table.
