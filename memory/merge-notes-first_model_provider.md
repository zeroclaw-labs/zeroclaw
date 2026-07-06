# Merge Note: first_model_provider must be NUKED

When merging upstream/master into integration/zerocode, after resolving conflicts,
destroy ALL traces of `first_model_provider` and its family.

## Definitions to delete (crates/zeroclaw-config/src/schema.rs)
- `fn first_model_provider()`
- `fn first_model_provider_mut()`
- `fn first_model_provider_type()`
- `fn first_model_provider_alias()`

## Call sites to fix
- `crates/zeroclaw-gateway/src/lib.rs` — 4 usages (lines ~489, ~490, ~647, ~2136)
- `crates/zeroclaw-providers/src/lib.rs` — fallback logic, multiple usages (~660, ~715, ~724, ~753)
- `crates/zeroclaw-runtime/src/agent/loop_.rs` — temperature lookup (~4598)
- `crates/zeroclaw-channels/src/orchestrator/mod.rs`

## What replaces it?
See commit `a1665774b` — "fix: delete first_model_provider*, enforce explicit provider alias resolution"

Root cause: `first_model_provider*` returned an arbitrary provider entry, causing
cross-agent provider contamination (e.g. the_writer getting clamps' max_tokens=128000
instead of its own 64000).

Replacements (per a1665774b):
- `provider_runtime_options_for_alias(config, family, alias)`
- `config.resolved_model_provider_for_agent(agent_alias)`
- `config.providers.models.find(family, alias)`
- `config.providers.models.iter_entries()` for enumeration

## Post-merge checklist
1. `git merge upstream/master` (resolve conflicts)
2. `grep -rn "first_model_provider" crates/` — find every hit
3. Cherry-pick or manually replay `a1665774b` changes for each call site
4. `cargo check` — confirm it compiles
5. Commit with message: "nuke: remove first_model_provider family entirely"

## Files touched in a1665774b (27 files)
- crates/zeroclaw-config/src/schema.rs
- crates/zeroclaw-config/src/providers.rs
- crates/zeroclaw-providers/src/lib.rs
- crates/zeroclaw-gateway/src/lib.rs, api.rs, api_onboard.rs, ws.rs
- crates/zeroclaw-channels/src/orchestrator/mod.rs, acp_server.rs
- crates/zeroclaw-runtime/src/agent/agent.rs, loop_.rs
- crates/zeroclaw-runtime/src/daemon/mod.rs, doctor/mod.rs, onboard/mod.rs, rpc/dispatch.rs, tools/mod.rs
- crates/zeroclaw-tools/src/model_routing_config.rs
- apps/tui/src/app.rs, chat.rs, client.rs
- src/commands/self_test.rs, main.rs, memory/cli.rs
- tests/component/config_persistence.rs, config_schema.rs
