# Changelog: v0.7.5 → v0.8.0-beta-1

v0.8.0 turns ZeroClaw from a single-agent daemon into a true multi-agent host. One install now runs many named agents side by side, each with its own identity, workspace, memory, model provider, channels, and security profile, and they can talk to each other through peer groups or spawn scoped sub-agents. Delivering that meant a ground-up config rewrite (schema **V3**) plus a new on-disk layout, so this is a large, breaking release. Upgrades migrate automatically on first boot; **read the Breaking Changes section before upgrading a production install**, especially if you run Postgres or Qdrant memory.

## Highlights

- **Multi-agent, for real.** Declare any number of agents under `[agents.<alias>]`. Each gets its own identity files, its own `agents/<alias>/workspace/` (which is also its security boundary), its own memory scope, model provider, channels, and risk/runtime profiles. Old single-agent installs migrate into one `default` agent on first boot.
- **Per-agent memory isolation.** Every memory backend (SQLite, Postgres, Qdrant, markdown) is wrapped so an agent only sees its own rows by default. Controlled sharing is opt-in via `read_memory_from`. Memory is keyed by `(agent, key)`, so two agents can use the same key without collision.
- **Peer groups + sub-agents.** Agents bound to the same channel type can message each other in-process through a `[peer_groups.<alias>]` membership list; non-members are rejected with a reason. Agents can spawn depth-limited sub-agents (`spawn_subagent`) that can never escalate beyond the parent's risk profile, and cron jobs now run as agent-scoped sub-agents.
- **Agents know their own identity in shared channels.** Each bot's platform-native mention (Discord `<@id>`, Slack `<@id>`, Telegram `@user`) is injected into its system prompt, and the lookup is fixed for aliased channels. Co-resident agents that share a channel no longer mistake another agent's @-mention for their own or reply with the wrong user ID.
- **System prompt calibrated against over-refusal.** Agents previously erred toward silence and would decline a request because the answer was "already in memory." The prompt now skews toward replying and treats memory as supplementary context, not a gate on whether to respond; ACP sessions always produce a reply.
- **Schema V3 with automatic migration.** A typed V1→V2→V3 migration chain rewrites your config in memory on every load and writes a `.backup` when you commit it with `zeroclaw config migrate`. The install tree is split into `data/` (shared databases), `shared/` (host-wide skills), and `agents/<alias>/workspace/`, with timestamped backups.
- **Rebuilt observability.** A new `zeroclaw-log` crate and unified `record!` macro carry alias-bound, structured attribution through every log and trace. New `/api/logs` endpoint and a Web Logs page surface it live.
- **Reworked web dashboard.** A multi-agent dashboard with per-agent status/memory/cost/sessions, an in-browser agent workspace explorer, a tool-approval UI for supervised mode, skill-bundle editing, and a cross-section draft store with an unsaved-changes banner.
- **More providers and channels.** GitHub Models, Morph, Manifest, and atomic-chat join the provider lineup; ~36 providers gained typed-family config with OAuth refresh. ACP sessions now persist; Mattermost gained multi-channel polling and DM auto-discovery.
- **Provider fallback is gone.** No more fallback-provider chains. Each agent names one provider, retries handle transient failures, and routing is explicit. If your config carries `reliability.fallback_providers` or `reliability.model_fallbacks`, drop those keys before upgrading (see Breaking Changes).

## What's New

### Multi-Agent & Runtime

The multi-agent epic (#6272) is the spine of this release:

- **Agent aliasing**: agents are explicit, named map entries (`[agents.<alias>]`) with their own `model_provider`, `risk_profile`, `runtime_profile`, `channels`, and `identity`. The schema, migration, orchestrator, and onboarding all speak aliases end to end.
- **Per-agent identity & workspace**: each agent loads its own `IDENTITY.md`/`SOUL.md` and runs against `agents/<alias>/workspace/`, which doubles as its security boundary via path-subset enforcement (`SecurityPolicy::for_agent`).
- **Per-agent memory**: the `Memory` trait gained an agent-aware surface on every backend; `AgentScopedMemory<M>` enforces the cross-agent boundary on every method, and `AgentScopedMarkdownMemory` does the same for the markdown backend. Alias deletes purge the agent's rows.
- **Peer groups**: a peer-group resolver and `ResolvedPeers` type let agents on a shared channel *type* reach each other; external peers are matched case- and `@`-insensitively. Delivery between co-resident agents is in-process by design (the channel's bot identity is shared).
- **Sub-agents**: `spawn_subagent` agent-loop tool with a depth-1 cap and a `risk_profile` gate; `SecurityPolicy::ensure_no_escalation_beyond` enforces parent-subset authorization. Cron `JobType::Agent` dispatch now routes through the sub-agent spawn path; cron stays depth-0.
- **Lifecycle & CLI**: a per-agent lifecycle module + session registry, `DeleteReport` that surfaces active sessions and warns on force-delete, and new `zeroclaw agents create / delete / list` commands.
- **Tool authorization**: `Tool : Attributable` supertrait with per-tool role and alias; a policy-driven tool filter at the dispatch site; `SecurityPolicy.allowed_tools` / `excluded_tools` with `is_tool_allowed`.

### Configuration & Schema (V3)

- Typed migration chain rebuilt as partial lenses (`V1Config`, `V2Config`) covering every nested V1→V2 and V2→V3 transform, wired into runtime, CLI, and end-to-end tests.
- `zeroclaw config generate <version>` and `zeroclaw migrate generate <version>` produce a canonical config at any schema version from a comprehensive V1 fixture.
- RFC #5890 model-provider aliasing: nested `[providers.models.<type>.<alias>]`, with ~36 typed provider families and a `for_each_model_provider_slot!` macro driving factory dispatch and validation.
- TTS and transcription gained the same typed-family split (`[providers.tts.<type>.<alias>]`, `[providers.transcription.<type>.<alias>]`) with per-agent `tts_provider` / `transcription_provider` fields and cross-validation.
- Per-category typed alias-ref newtypes (`ModelProviderRef`, `TtsProviderRef`, `TranscriptionProviderRef`, `ChannelRef`) make dotted `<type>.<alias>` references first-class.
- New `[acp]` config section; risk/runtime/bundle profile synthesis on migration; `#[secret]`-driven `MaskSecrets`; pricing moved onto provider config as a `[costs.providers.models]` rate sheet.
- Defaulted fields are pruned from the saved `config.toml`; schema defaults render as ghost text in the TUI and the dashboard.

### Observability

- **OTel memory and RAG observability** — every agent turn now produces visible
  `memory.recall`, `memory.store`, and `rag.retrieve` spans plus five OTel meter
  instruments (`zeroclaw.memory.recall.{count,duration}`, `zeroclaw.memory.store.count`,
  `zeroclaw.rag.retrieve.{count,duration}`). The recall/retrieve spans set
  `gen_ai.operation.name = "retrieval"` and carry a scrubbed/truncated `query_summary`
  on `input.value`; the store span uses `db.system` / `db.operation = "INSERT"`. These
  are partial GenAI-compatible attributes — `SpanKind::Internal` is used (not `CLIENT`)
  and span names are `memory.recall` / `rag.retrieve` / `memory.store` (not the
  spec-suggested `{operation} {data_source.id}`) for compatibility with the existing
  ZeroClaw / Langfuse retrieval views. New spans and instruments live behind the
  existing `observability-otel` feature gate (#6190).

### Web Dashboard

- Multi-agent reframe: per-agent status, memory counts, cost, and session routing; RAM/CPU widgets for the ZeroClaw process.
- Cost moved into a Dashboard tab with time-range filters, daily-scoped rollups, per-agent/per-model token splits, cached-input tokens, and a schema-driven rate-sheet editor.
- Agent workspace explorer with jailed read/delete/move and mkdir/rmdir; lazy file browser for `shared/`.
- Tool-approval UI for supervised-mode execution; Memories tab; Skills drill-in with `SKILL.md` editor inside skill bundles.
- Cross-section draft store, unsaved-changes banner, tombstone unset, URL-driven alias routing (no modals), and alias pickers replacing `window.prompt`.
- Live Logs page over the new `/api/logs` endpoint; global reload banner driven by a `pending_reload` signal; version shown in the sidebar footer.

### Providers

- New providers: **GitHub Models** (#6445), **Morph** (#6440), **Manifest** open-source LLM router (#6268), **atomic-chat** local provider (#6513).
- MiniMax split into Global and China picker entries (#6758); llama.cpp promoted to a dedicated provider kind (#6417).
- OpenRouter prompt caching (#6008); Codex native Responses tool calls (#6117); Ollama `num_ctx`/`num_predict`/`temperature` tuning (#6178).
- Trait-driven provider dispatch with OAuth refresh on the per-alias schema; Azure rewired to typed config (and `AZURE_OPENAI_*` env vars retired); `models.dev` keys pre-populate the model picker.
- **Anthropic / Bedrock:** opt-in native extended thinking with `budget_tokens`
  and signed thinking-block round-trip across multi-turn tool use (#5652).
  Disabled by default — set `agent.thinking.native_thinking = true` to enable.
  When on, preserves the signed block bytes returned by the model (no trimming)
  so subsequent tool-use turns validate on the provider side; streaming with
  native thinking transparently falls back to a non-streaming request and
  carries the signed block through to conversation history via the stream's
  `reasoning` channel. Fixed-budget native thinking is gated off for Opus 4.7
  on both providers (which only supports adaptive thinking and rejects fixed
  `budget_tokens` with 400) — those models stay on prompt-based reasoning
  until adaptive thinking lands as a follow-up. Custom `budget_tokens`
  overrides are clamped to the provider's documented `[MIN, MAX]` range with
  a WARN, avoiding a 400 when a config value dips below the provider minimum.
  Full `thinking_delta` / `signature_delta` SSE handling remains a follow-up
  for token-by-token streaming of thinking text.

### Channels

- **Mattermost** multi-channel polling, DM auto-discovery, and `mention_only` bypass; **Nextcloud Talk** draft-update streaming (#6048).
- Bot self-mention injected into the per-channel system prompt (`self_addressed_mention()` in each channel's platform syntax), with the lookup fixed to resolve composite `<type>.<alias>` keys so aliased and multi-agent channels actually receive it; closure-resolver peer auth unified across all 24 channels; per-channel `self_handle` overrides; standardized inbound/outbound channel log shape.
- Reply-calibration nudge added to both the per-channel prompt and the base system prompt (so direct CLI chat gets it too): agents had been erring toward silence and treating memory as a reason not to answer, so the prompt now skews toward replying and frames memory as supplementary context rather than a gate on whether to respond.

### ACP (Agent Client Protocol)

ACP mode (the `zeroclaw acp` subprocess and the `zeroclaw-acp-bridge` editor bridge) got a substantial pass this release:

- **Sessions persist across restarts** (#6649). A SQLite-backed `AcpSessionStore` records each session on `session/new` and appends every successful prompt turn; on reconnect the agent's conversation history is restored full-fidelity, so a daemon restart no longer drops in-flight ACP conversations.
- **New `[acp]` config section.** `max_sessions` (default 10) and `session_timeout_secs` (default 3600) cap concurrent sessions and idle lifetime; `default_agent` names the agent to use when a client omits `agentAlias`. These wire into both the `zeroclaw acp` subprocess and the gateway WebSocket path; CLI flags still override the config for the subprocess.
- **Agent resolution when `agentAlias` is omitted.** Clients such as toad send `session/new` without an alias; resolution is now explicit alias, then `acp.default_agent`, then auto-select when exactly one agent is configured. With multiple agents and no hint, the server still requires an explicit alias.
- **`zeroclaw-acp-bridge --config-dir`.** Clients that spawn the bridge as a subprocess don't forward `ZEROCLAW_CONFIG_DIR`, so the bridge accepts `--config-dir <path>` (and `--config-dir=<path>`) to locate config off the default `~/.zeroclaw`.
- **Always-respond in ACP sessions.** The chatroom NoReply heuristic that suppresses bot chatter in broadcast channels is bypassed for ACP: every inbound is a direct request, so it always produces a reply.
- **Workspace access retained when the session cwd differs** (#6532). ACP sessions keep ZeroClaw workspace access even when the client opens the session in a different working directory.

### Skills

- New `SkillsService` with a canonical `SKILL.md` scaffold; bundle-aware `skills add/edit` and `bundle list/show`; `zeroclaw skills bundle add/remove` route through config CRUD.
- `shared/skills` default to read-only; tier banner on `zeroclaw skills install` (#6409); missing-capability suggestions (#6676); `timeout_secs` honored from `SKILL.toml` (#6054); install output localized with Fluent (#6674).

### Security

- `SecurityPolicy` read / read-write allowlist split; sub-agent escalation validator with path-subset matching; write-only roots stay unreadable; destructive session operations scoped.

### Installation & Distribution

- NixOS module + test for `services.zeroclaw.instances` (#6562).
- Desktop (Tauri): macOS onboarding wizard with permission primitives and capability sync (#6506), Linux/Windows permission onboarding (#6710), and `take_screenshot` / `run_applescript` commands (#6507).

### Internationalization

- CLI skill handlers and install output routed through Fluent; en/fr/ja catalog upkeep.

### Documentation

- New multi-agent architecture page and setup walkthrough (#6272 P14); SubAgents page derived from code; rewritten provider docs for the typed-family schema; fresh `[cost.rates.*]` cost-tracking guide; logging docs rewritten from source; V1/V2 holdovers scrubbed from operator docs.

### Improvements

- Every `anyhow!` site routed through `record!`; `tracing::*` / `log::*` macros banned workspace-wide in favor of the unified macro; one canonical, dependency-ordered config Section list driving every wizard surface.

## Bug Fixes

| Area | Fix |
|---|---|
| Config / V3 | V2→V3 migration correctness: provider-globals fold lands on the right V3 paths, channel `enabled` restored, dotted `model_provider` aliases resolve in routes and channel paths, Feishu V2 block migrates to `lark.feishu`, JSON-patch errors emitted for CLI patch (#6617) |
| Channels | Telegram tool calls; Matrix honors global `ack_reactions`; localized runtime command replies (#6550); media uploads routed into the owning agent's workspace |
| Providers | Skip unresolvable multimodal images (#6743); Ollama routed through `compatible.rs` at `/v1/chat/completions`; error source chain included in retry logs; GLM/compat fixes |
| Gateway / Web | Boot degrades (not crashes) when an enabled agent has an unresolved `risk_profile`; CronSchedule union restored; reload-banner and cron-modal fixes; OpenAPI re-export cleanup |
| Cron | Timezone preserved through the cron API (#6741, #6740); `cmd.exe` on Windows instead of hardcoded `sh` (#6713) |
| Memory | Composite `(agent_id, key)` uniqueness across SQL + Qdrant; tolerate concurrent SQLite schema migrations (#6432); residual isolation gaps closed |
| Skills | ClawHub install runs on the async runtime (#6682); strict `SkillMeta` + SkillForge provenance under `[forge]` (#6209); `__` separator in tool names for OpenAI-compat function calling (#6732) |
| Service / Update | Valid macOS launchd plist (#6738); release asset selection tightened (#6585); Windows snapshot TTL vs. polling interval (#6750) |
| Onboard | Interactive-flag compatibility restored (#6673); reachable model catalogs surfaced; SQLite-default storage help |

## Breaking Changes

v0.8.0 is a pre-1.0 breaking release. Your config is **auto-migrated in memory on every load**, and the install filesystem is migrated on first boot with timestamped backups. To commit the config migration to disk (preserving comments, writing a `.backup`):

```bash
zeroclaw config migrate          # add --json for a machine-readable report
```

**Back up first.** SQLite memory is backed up automatically (`brain.db.backup-<ts>`), and the install tree is copied to `backup-<ts>/` before the split. **Postgres and Qdrant are NOT backed up for you. Dump them yourself before upgrading.**

### Schema V3 and the install layout

- The on-disk tree is split: shared databases (memory, sessions, state) move to `<install>/data/`, host-wide skills to `<install>/shared/skills/`, and per-agent identity/markdown to `<install>/agents/default/workspace/`. `Config.workspace_dir` is renamed to `Config.data_dir` (`<config-dir>/workspace/` → `<config-dir>/data/`). (#6272)
- Single-agent installs are migrated into one `default` agent; `default` is the migration bridge alias and never appears on a fresh install.

### Agents are now explicit

- `[identity]` is demoted from a top-level block to per-agent `agents.<alias>.identity`; each agent picks its own format and source.
- Top-level provider maps collapse under one `[providers]` key: `[model_providers.*]` → `[providers.models.*]`, `[tts_providers.*]` → `[providers.tts.*]`, `[transcription_providers.*]` → `[providers.transcription.*]`.
- `MemoryEntry.agent_id` is renamed to `agent_alias` (a serde alias keeps existing Qdrant/JSON payloads readable, no data migration needed).

### Removed / relocated config

- **`[autonomy]` is gone.** Its fields move to per-agent `[risk_profiles.<alias>]` (authorization) and `[runtime_profiles.<alias>]` (budgets/timeouts); the active profile resolves via the agent's `risk_profile`.
- **`[security.sandbox]` and `[security.resources]` are gone.** Sandbox and resource fields now live flat on the risk profile.
- **Provider fallback is eradicated.** `reliability.fallback_providers` and `reliability.model_fallbacks` are removed; retries handle transient failures, routing is explicit.
- **Swarms are removed entirely**, deferred to a later release with a new shape; `[swarms.*]` tables are dropped on migration with a warning.
- **Channel-level peer-auth fields are removed** (`allowed_users`, `allowed_contacts`, `allowed_from`, `allowed_numbers`, `allowed_senders`, `allowed_pubkeys` across all 24 channels). Inbound authorization lives exclusively in `[peer_groups]` now; migration synthesizes `[peer_groups.<type>_default]` from your old allowlists (wildcard-only/empty lists are skipped).
- **Feishu folds into Lark.** `[channels.feishu]` migrates to `[channels.lark]` with `use_feishu = true`; conflicting `app_id`s drop the Feishu side with a warning.
- **`claude-code` is removed as a model provider** (folds under `anthropic.claude-code` on migration).
- Schema cleanups: `PeerExternal` flattened to `PeerUsername`; `RuntimeProfile.api_key` and `ModelProviderConfig.name` dropped.

### `ObserverEvent` is now `#[non_exhaustive]`

`zeroclaw_api::observability::ObserverEvent` is now annotated `#[non_exhaustive]` so
future additive variants do not break out-of-tree observer implementations. Out-of-tree
code that exhaustively matches on `ObserverEvent` (`match event { ObserverEvent::Foo
{ .. } => ..., ObserverEvent::Bar { .. } => ..., }`) must add a wildcard arm:

```rust
match event {
    ObserverEvent::Foo { .. } => { /* ... */ }
    ObserverEvent::Bar { .. } => { /* ... */ }
    _ => { /* future variants */ }
}
```

Three additive variants land in this release alongside the annotation: `MemoryRecall`,
`MemoryStore`, `RagRetrieve` (#6190).

### `MemoryLoader::load_context` gains an `observer` parameter

The `MemoryLoader` trait in `zeroclaw-runtime` (Beta tier) gains a required
`observer: &dyn Observer` parameter on `load_context` so the runtime can emit
`MemoryRecall` events at the boundary. Out-of-tree `MemoryLoader` implementors must
update their signature; impls that do not need instrumentation can ignore the parameter:

```rust
impl MemoryLoader for MyLoader {
    async fn load_context(
        &self,
        // ... existing params ...
        _observer: &dyn zeroclaw::observability::Observer,
    ) -> Result<...> { /* unchanged body */ }
}
```

The runtime always passes a valid reference (`&NoopObserver` when no observer is wired)
(#6190).

### Environment-variable grammar rewrite

Every legacy override is replaced by a single schema-mirror grammar (#6375):

```
ZEROCLAW_<lowercase_dotted_path_with_underscores>=<value>
```

The lowercase tail mirrors the dotted prop-path `config set` accepts (each `_` is either a path separator or a kebab joiner inside a snake-case field). Only the bootstrap vars `ZEROCLAW_WORKSPACE` and `ZEROCLAW_CONFIG_DIR` keep their uppercase form. All previous override names (including `AZURE_OPENAI_*`) are gone. See `docs/book/src/reference/env-vars.md` for per-variable recipes.

### Internals

- Observability: `runtime_trace::record_event` retired in favor of `record!` everywhere; the `record!` shape is locked and carries alias-bound attribution through spans.
- Cron: the `agent_alias` fallback to `DEFAULT_AGENT_ALIAS` is removed; jobs must name their agent.
- TTS/transcription `default_*_provider` fields are deleted; each agent declares its own.

## Known Issues

This is a beta. The following are known-broken or unfinished and will be addressed before the stable release:

- **Channel tool-approval timeouts default to 0s.** In practice the per-channel `approval_timeout_secs` resolves to `0` rather than the documented 300s (120s for Telegram), so an `always_ask` tool prompt can auto-deny immediately instead of waiting for an operator. Set `approval_timeout_secs` explicitly on each channel as a workaround.
- **Onboarding and agent-assignment UX is not final.** Expect rough edges in the onboarding flow and in how agents are assigned during setup.
- **Web gateway UI is not finalized.** The dashboard is functional but still changing; layout, controls, and routes may shift before stable.

## Contributors

- @0disoft
- @abhinavmathur-atlan
- @alexandme
- @aliasliao
- @Alix-007
- @Audacity88
- @drbparadise
- @flyin1600
- @fresh-fx59
- @FTDGRT
- @guitaripod
- @ilteoood
- @joe2643
- @johnrspeer83-png
- @JordanTheJet
- @kapelame
- @kmsquire
- @markuman
- @mminkus
- @mn13
- @nebullii
- @ninenox
- @NiuBlibing
- @ozzyfly
- @patrickzzz
- @plodsoft
- @Project516
- @rareba
- @roywong10
- @RyanHoldren
- @SebConejo
- @SimianAstronaut7
- @singlerider
- @SpectreMercury
- @TeoConnexioh
- @theonlyhennygod
- @thezillo
- @tidux
- @TJUEZ
- @WareWolf-MoonWall
- @whtiehack
- @xydigitLybnnnn
- @xydigit-sj
- @yanalialiuk
- @yijunyu
- @Yyukan
- @zwffff

---

*Full diff: `git log v0.7.5..v0.8.0-beta-1 --oneline`*
