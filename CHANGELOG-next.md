# Changelog — v0.7.3 → v0.7.4

> The first patch release on top of the v0.7.x workspace foundation. v0.7.4 lands a
> clean-room Matrix rewrite, a Mozilla Fluent i18n pipeline with multi-locale docs, a
> ground-up rewrite of the CLI/TUI onboarding flow, recovers the WeChat iLink Bot channel. Around 110 commits from 36 contributors covering
> channels, providers, web dashboard, security, and developer experience.

---

## Highlights

- **ACP v1 — full IDE integration protocol** — ZeroClaw's Agent Client Protocol has been upgraded to schema v1. The initialize response now carries `protocolVersion`, `agentCapabilities`, `agentInfo`, and `_meta.zeroclaw` extension fields. Session/update notifications use a `sessionUpdate` discriminant with four variants (`agent_message_chunk`, `tool_call`, `tool_call_update`, `agent_thought_chunk`). Tool-call approval now flows via an outbound `session/request_permission` JSON-RPC request from agent to IDE — the IDE acknowledges with `allow-once`, `allow-always`, or `reject-once`. A new ACP back-channel (`AcpChannel`) lets structured-choice `ask_user` prompts and non-blocking escalation messages reach the connected IDE client; ACP reactions and free-form/waiting escalation replies remain unsupported until the protocol grows those primitives. The gateway WebSocket gains a connect-time `cwd` parameter that pins the per-session security sandbox root. Clients still on v0 must migrate; see the [ACP migration guide](docs/book/src/channels/acp.md#version-compatibility).

- **`escalate_to_human` tool** — New agent-callable tool for urgency-aware human escalation. `high`/`critical` urgency additionally notifies any channels listed in `[escalation] alert_channels` (best-effort, non-fatal). On ACP, non-blocking escalation messages are rendered into the connected client; `wait_for_response: true` fails fast because ACP has no free-form elicitation primitive yet.

- **Per-session security sandbox root** — Both ACP (`session/new`) and the gateway WebSocket (connect-time `cwd`) now pin an independent workspace boundary per session. The daemon's data directory (memory, cron, identity) remains separate from the per-session sandbox, enabling multi-project setups where each IDE window gets its own file-access scope.

- **Multi-locale docs and i18n pipeline** — A Mozilla Fluent-based i18n pipeline now
  drives a multi-locale mdBook, alongside a comprehensive docs overhaul (#5788). Header
  links point at the upstream repo (#6124) and the CNAME is preserved on every Pages
  deploy (#6142).

- **Onboarding clean-slate rewrite** — `zeroclaw onboard` is now schema-driven,
  idempotent, and DRY (#5960). It picks up a generic OpenAI-compatible `/v1/models`
  fallback for unknown providers (#6056) and uses container-aware URLs for local AI
  providers running in Docker (#5552).

- **Session management surface** — New `SessionResetTool`, `SessionDeleteTool`, and
  `SessionsCurrentTool` give the agent first-class control over its own sessions
  (#5696, #6033). The gateway gained a session abort endpoint with incremental streaming
  persistence (#5705).

- **WeChat iLink Bot channel recovered** — The previously reverted iLink Bot
  integration is back, ported to current trait surfaces (#6130). `request_approval()`
  is now implemented across Discord, Slack, Signal, Matrix, and WhatsApp.

- **Voice foundation** — A new `Vad` trait and `VoiceEvent` protocol land behind a
  `gateway-voice-duplex` feature flag, paving the way for live voice channels (#5942).

- **PostgreSQL memory backend** — Memory can now be persisted to PostgreSQL via a new
  `memory-postgres` backend.

- **Matrix channel rewritten** — A clean-room reimplementation on `matrix-rust-sdk 0.16`
  replaces the long-running patch pile. E2EE auto-verification of `allowed_users` is
  preserved, and the channel is markedly simpler to operate (#6112).

---

## Breaking changes

### zeroclaw-runtime (Beta)

- `IntegrationStatus::ComingSoon` removed. Callers that match on it must drop that arm. Hand-written "planned" entries are gone; if a channel or tool is not in the schema or not a real runtime built-in, it does not appear in the integrations registry.
- `IntegrationCategory` variants `Productivity`, `MusicAudio`, `SmartHome`, `MediaCreative`, `Social` removed. Downstream `match` exhaustiveness will break at compile time. These categories had no live entries (only the now-removed `ComingSoon` placeholders).
- `Google Workspace` recategorised from `Productivity` (removed) to `ToolsAutomation`.
- `IntegrationEntry.status_fn: fn(&Config) -> IntegrationStatus` replaced by `IntegrationEntry.status: IntegrationStatus`. The catalog is now evaluated eagerly inside `all_integrations(&Config)` rather than carrying a per-entry closure.
- `all_integrations()` signature changed from `() -> Vec<IntegrationEntry>` to `(&Config) -> Vec<IntegrationEntry>`. Callers must thread a `Config` reference.

---

## What's New

### Architecture & Workspace

- Decoupled `gateway` and `tui-onboarding` from `agent-runtime`, so each can be
  compiled without dragging in the full agent loop (#5735).
- `SessionBackend` trait gained `clear_messages()` for O(1) session reset (#5900) and
  `get_session_metadata(key)` for typed metadata access (#6043).
- Hardware crate: wizard UI moved from `main.rs` into `zeroclaw_hardware::wizard` for
  reuse outside the binary (#6041).
- Web router refactor for clearer route ownership (#6176).
- Tools: rate-limiting delegated to wrappers for `glob_search` and `content_search`
  (#5772); session validation now uses typed errors (#6135).

### Agent & Runtime

- `prune_history` Phase 1 now treats mixed-protection tool groups as atomic, preventing
  partial pruning that left the conversation in an invalid state (#5828).
- Self-heals orphaned `tool_result` blocks on session load and on compaction (#5853).
- Sandbox auto-detection now respects `runtime.kind = "native"` (#5904).
- `runtime.kind` is detected for memcg availability at daemon startup (#5906).

### Providers

- **OpenRouter**: `extra_body` passthrough for arbitrary request params (#5623); the
  upstream stream task is now aborted when the consumer drops the stream (#5830).
- **MiniMax** native tool calling is now enabled (#6027).
- **Bedrock** omits `temperature` for Opus 4.7, matching the model's API contract
  (#6144).
- **Gemini / OpenRouter** tool-call compatibility fixes plus clearer
  `google_workspace` schema (#5975).
- **Groq**: native tool calling is now disabled where it was misbehaving (#5848).
- `strip_native_tool_messages` now coalesces adjacent assistant turns (#5829).

### Channels

- **Matrix**: clean-room rewrite on `matrix-rust-sdk 0.16` replacing the prior
  long-running patch series (#6112).
- **WeChat iLink Bot**: channel recovered from the bulk revert in PR #4221 (#6130).
- **Slack**: `strict_mention_in_thread` option lets you require an @-mention even in
  threads where the agent has previously replied (#5992).
- **IRC**: `mention_only` config option for IRC channels (#5998).
- **Telegram**: bot command list updated (#5691); `request_approval` now forwards the
  `message_thread_id` (#5970); auto-injected topic-root reply context is skipped in
  forum topics (#5969).
- **IMAP**: polling fallback for servers that don't support IDLE (#5712).
- **ACP**: `defaultModel` resolves from config and is null when unconfigured (#6013);
  tool output formatting corrected (#6035); INFO logs suppressed and missing ACP spec
  protocol implemented (#5c81d4e).
- **Discord, Slack, Signal, Matrix, WhatsApp**: `request_approval()` implemented across
  the channel set, unblocking approval-gated tool flows on every supported chat
  platform.
- **Feishu**: `mention_only` config wired through (#5848).

### Tools & Skills

- `SessionResetTool` and `SessionDeleteTool` for in-agent session management (#5696).
- `SessionsCurrentTool` exposes the active session identity (#6033).

### Plugins

- Extism WASM execution bridge wired up (Phase 2 D2 plumbing) (#5913).
- `image-gen-fal` WASM plugin added as the fal.ai Flux reference plugin (#5921).
- Markdown-only plugin bundles can now declare a `skill` capability (#6141).

### Voice

- New `Vad` trait and `VoiceEvent` protocol behind the `gateway-voice-duplex` feature
  flag (#5942).

### Memory

- PostgreSQL backend re-introduced as `memory-postgres`.
- `is_user_autosave_key` detector identifies per-turn user message keys (#5631), and
  these keys are now skipped in every memory context path (#5632).

### Web Dashboard

- Chat message deletion, clear-all, and a compact mode (#6083).
- Cron job configuration UI (#5936).
- Embedded web build for the `pack` bin (#6181).
- Bug-fix bundle: Overview crash, model save, editor caret, chat CPU usage (#6161).
- Array-returning API helpers now guard against non-array responses (#6162).
- WebSocket session ID persists in `localStorage` across page reloads (#5641).

### Configuration

- `Vec<String>` fields are now exposed via `zeroclaw config get/set/list` (#5950),
  including JSON-array syntax in `config set` (#0e9b9c2).
- User-supplied `providers.fallback` is preserved through load/save (#6099) and
  mirrored under the canonical fallback key (#321e96f).
- WebSocket buffer is preserved in the non-proxy `ws_connect_with_proxy` path (#5794).
- `[skill]` TOML sections may now contain prompts (#5972).

### Onboarding

- Clean-slate rewrite: schema-driven, idempotent, DRY (#5960).
- Generic OpenAI-compatible `/v1/models` fallback for unknown providers (#6056).
- Container-aware URLs for local AI providers (#5552).
- Windows: `setup.bat` issues fixed (#6137).

### Gateway & Runtime

- Session abort endpoint plus incremental streaming persistence (#5705).
- Tool support enabled in the webhook endpoint (#6080).
- Token usage emitted from the webhook handler (#5793).
- Missing `/api/channels` route added (#6069).

### Cron

- Memory snowball accumulation in agent jobs prevented (#5817).
- `deliver_announcement` returns `Err` when no delivery handler is registered (#5827).
- Closing tag added to the memory context block in cron and daemon paths (#3b24f81).

### Documentation

- Mozilla Fluent i18n pipeline + multi-locale mdBook + full docs overhaul (#5788).
- ZeroClaw Maturity Framework ratified and committed (#5911).
- Manual release runbook (#5920).
- AGENTS code-style rules clarified (#6163).

### Installation & Distribution

- OpenShift / Kubernetes deployment manifests (#5880).
- Docker images now include the web dashboard (release image #5996, debian local-dev
  image #6025).
- Install script prompts for pre-built vs source, defaulting to pre-built on
  `curl | bash` (#5968).
- Windows `cargo test` unbroken; self-update target triples added (#6050).

### Improvements

- Refactor: web router (#6176); rate-limiting wrappers for filesystem tools (#5772);
  typed session validation errors (#6135); hardware wizard relocation (#6041).

### Security & Dependencies

- `cargo update` and `deny.toml` audit (2026-04-27) (#6152).
- `rustls-webpki` updated to v0.103.13; unfixable v0.102.8 copy ignored (#6011).
- Patches applied for `rand`; `picomatch` ReDoS fixed; `wasmtime` and `glib` ignores
  documented (#5971).
- Daily advisory scan workflow added (#5928).
- `rand` bumped from 0.10.0 to 0.10.1 (#5713).
- `postcss` bumped from 8.5.6 to 8.5.10 in `/web` (#6084).

---

## Bug Fixes

| Area | Fix |
|---|---|
| Tauri desktop | Install rustls crypto provider to prevent crash (#5997); replace PNG-as-ICO with a real Windows ICO to unblock Win11 builds (#5966) |
| Telegram | Forward `message_thread_id` in `request_approval` (#5970); skip auto-injected topic-root reply context in forum topics (#5969) |
| Skills (config) | Allow prompts inside `[skill]` TOML section (#5972); reject unknown fields in `[skill]` block to surface silent typo drops (#6128); relocate SkillForge provenance to a sibling `[forge]` table and surface `SKILL.toml` parse failures via `tracing::warn` instead of swallowing them silently (#6210) |
| Providers | Gemini/OpenRouter tool-call compatibility + `google_workspace` schema clarity (#5975); MiniMax native tool calling enabled (#6027); Bedrock omits temperature for Opus 4.7 (#6144); Groq native tools disabled where misbehaving (#5848); coalesce adjacent assistant turns in `strip_native_tool_messages` (#5829); abort OpenRouter stream task when consumer drops (#5830) |
| CI | `nextest` now runs across all workspace crates (#6197); CNAME persisted on every Pages deploy (#6142) |
| Bulk revert recovery | Recover 4 small fixes lost in bulk revert c3ff635 (#6169) |
| Runtime | Align tool-call text preservation test (#6204); detect memcg availability at daemon startup (#5906); self-heal orphaned tool_result blocks on load + compact (#5853); register skill tools and apply excluded filter in gateway path (#5774); drop redundant narration push before AssistantToolCalls (#6093); unbreak pre-existing test failures on master (#6108); respect `runtime.kind = "native"` in sandbox auto-detection (#5904) |
| Infrastructure | SQLite FTS UPDATE trigger for `sessions_fts` (#5985) |
| xtask | Resolve real `mdbook` binary, avoid xtask self-spawn (#6171) |
| Web | Dashboard bug-fix bundle (#6161); guard array-returning API helpers (#6162); persist WebSocket session ID across reloads (#5641) |
| Memory | Add closing tag to memory context in cron and daemon (#3b24f81); skip user autosave keys in all memory context paths (#5632) |
| Gateway | Enable tool support in webhook endpoint (#6080); add missing `/api/channels` route (#6069); emit token usage from webhook handler (#5793) |
| Channels (ACP) | Resolve `defaultModel` from config (#6013); correct tool output formatting (#6035); suppress INFO logs and implement missing ACP spec protocol (#5c81d4e) |
| Channels (Feishu) | Wire `mention_only` config (#5848) |
| Config | Preserve `providers.fallback` through load/save (#6099); mirror provider entry under canonical fallback key (#321e96f); preserve WebSocket buffer in non-proxy path (#5794); parse JSON array syntax in `config set` for `Vec<String>` fields (#0e9b9c2) |
| Cron | Prevent memory snowball accumulation in agent jobs (#5817); return Err when no delivery handler registered (#5827) |
| Multimodal | Harden image-marker parser against non-path payloads (#5864) |
| Tools | Multiply embedding score by 100 before percent formatting (#5857) |
| Shell | Skip expansion guard when all commands allowed (#5773) |
| Onboarding | Use container-aware URLs for local AI providers (#5552) |
| Docs | mdBook header links point to upstream repo (#6124) |
| Install | Prompt for pre-built vs source, default to pre-built on `curl | bash` (#5968) |
| Docker | Include web dashboard in release image (#5996) and Dockerfile.debian local-dev image (#6025) |
| Windows | Fix `setup.bat` issues (#6137); unbreak `cargo test` and add self-update target triples (#6050) |
| rag-pdf | Unbreak `--features rag-pdf` end-to-end and restore Windows tests (#6076) |
| Security | `rustls-webpki` v0.103.13 (#6011); `rand` patches + `picomatch` ReDoS (#5971); cargo update + deny.toml audit (#6152) |

---

## Breaking Changes

### Config schema (V1 → V2)

The provider section of `config.toml` has a new layout. V1 configs are still loaded and
automatically understood, but the recommended path is to run the migration:

```sh
zeroclaw config migrate
```

This rewrites your config to V2 in-place. The old format will continue to work in this
release but will not be supported indefinitely.

### `zeroclaw props` deprecated

Use `zeroclaw config` instead. The `props` subcommand still works and will not be
removed in this release, but it will emit a deprecation notice.

### `zeroclaw onboard` defaults to TUI

`zeroclaw onboard` now launches the ratatui TUI backend by default. Users who were
relying on the old terminal-prompt behavior should pass `--cli` explicitly. The
previous `--tui` flag is accepted for one release as a deprecated no-op and emits a
warning pointing at the new default. In non-TTY environments (piped output, CI without
`--quick`) onboarding automatically falls back to the terminal-prompt backend.

### Slack `channel_id` deprecated

Use `channel_ids` (a list) in the Slack config block. `channel_id` (singular) still
works but is deprecated in V2.

### `[skill]` block in SKILL.toml rejects unknown fields; SkillForge provenance moves to `[forge]`

Three coordinated changes that ship together so the strictness in #6128 is
safe for users with `auto_integrate = true`:

**1. `[skill]` is strict.** The `SkillMeta` struct backing the `[skill]` block of
`SKILL.toml` now carries `#[serde(deny_unknown_fields)]`. Previously, unrecognised
keys inside `[skill]` were silently dropped during deserialization — a typo in a
field name would be accepted without error while the intended value was ignored.
This is the bug class tracked in #6128, identified as a follow-up to the
`SkillManifest` parsing refactor in #5972.

Any `SKILL.toml` whose `[skill]` block contains a key not defined in the current
schema will now fail to load with a descriptive serde error. Example:

```toml
[skill]
name = "my-skill"
descriptin = "Fixes a common issue"  # typo — was silently ignored, now an error
```

There is no per-field opt-out. The strictness is intentional: a skill that loads
with a silently-dropped typo is harder to debug than one that fails loudly.
Operators must correct or remove unrecognised fields from the `[skill]` block of
their `SKILL.toml` before upgrading.

**2. SkillForge provenance moves to a top-level `[forge]` table.** The SkillForge
integrator (`auto_integrate = true`) previously emitted `source`, `owner`,
`language`, `license`, `stars`, `updated_at` and the sub-tables
`[skill.requirements]` / `[skill.metadata]` directly inside `[skill]`. With
`SkillMeta` now strict, those keys would be rejected — every auto-integrated
skill would fail to load. The integrator now emits a sibling top-level `[forge]`
table instead, with `[forge.requirements]` and `[forge.metadata]` underneath.
This keeps the runtime's canonical skill identity contract (`SkillMeta`) decoupled
from the integrator's emit format (FND-001 §4.2 dependency rule). `[forge]` is
optional on hand-authored skills and also strict (`deny_unknown_fields`) so
typos in the provenance namespace surface the same way typos in `[skill]` do.

**3. `SKILL.toml` parse failures are now logged.** The skill loader at both
`load_skills_from_directory` and `load_open_skills_from_directory` previously
swallowed every error from `load_skill_toml` silently. With `[skill]` now strict,
that swallow would have flipped the failure mode from "field value silently
ignored" to "skill silently never loads." Both sites now emit a structured
`tracing::warn!` with `path` and `err` fields when a `SKILL.toml` fails to
deserialize, so an operator running `RUST_LOG=warn zeroclaw` can identify the
offending file.

#### Migration for users with `auto_integrate = true`

If you have `SKILL.toml` files on disk that were generated by SkillForge before
this release, run the bundled migration script to move the provenance fields
from `[skill]` into the new `[forge]` table layout. The script is idempotent —
re-running it on already-migrated files is a no-op. Dry-run by default; pass
`--apply` to write changes:

```sh
# Inspect the planned changes (dry-run):
python3 scripts/migrate-skill-toml.py ~/.zeroclaw/workspace/skills

# Apply the migration in place:
python3 scripts/migrate-skill-toml.py ~/.zeroclaw/workspace/skills --apply
```

The script can also target a single file (`scripts/migrate-skill-toml.py
path/to/skill/SKILL.toml --apply`) and uses standard library only (Python 3.8+,
no `tomli` / `tomli_w` required). Hand-authored `SKILL.toml` files without
SkillForge provenance are left untouched.

If you do not run the migration before upgrading, auto-integrated skills will
fail to load and the failure will surface as a `tracing::warn` line naming the
offending path.

### Workspace crate boundaries

If you have any code that depends directly on internal ZeroClaw crate paths (e.g. for
embedding or testing), the crate structure has changed significantly. Refer to
`AGENTS.md` for the current crate map and stability tiers. `zeroclaw-api` is the stable
extension point — all other crates are Beta or Experimental.

---

## Contributors

- @akhilesharora
- @Audacity88
- @david1gp
- @DengHaoke
- @flyin1600
- @fresh-fx59
- @hurtdidit
- @ilteoood
- @itripn
- @jokemanfire
- @JordanTheJet
- @justjuangui
- @kmsquire
- @MGSE97
- @nanookclaw
- @ninenox
- @NiuBlibing
- @OmkumarSolanki
- @pavelanni
- @perlowja
- @rareba
- @rpodgorny
- @RyanHoldren
- @RyanSquared
- @shaun0927
- @singlerider
- @theonlyhennygod
- @tidux
- @tonsiasy
- @vernonstinebaker
- @WareWolf-MoonWall
- @xydigit-sj
- @yijunyu
- @yusufsyaifudin
- @zavertiaev
- @zuyopme

---

*Full diff: `git log v0.7.3..v0.7.4 --oneline`*
