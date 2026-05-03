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
- **OpenAI Codex**: `Provider::chat_fast` override sends classifier and router-style
  calls with `reasoning.effort = "low"`, dropping ~1s per call versus inheriting the
  caller's higher effort. Default trait impl delegates to `chat_with_system` so
  non-Codex providers see no behavior change (#5979).

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
- **Reply-intent precheck opt-out**: new `[channels] precheck_reply_intent` flag (default
  `true`, preserving today's behavior). Single-bot DM operators can set it to `false`
  to skip the per-message classifier call; warm Telegram latency drops from ~7s to
  ~3.3s on a Codex backend (#5979).

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
| Skills (config) | Allow prompts inside `[skill]` TOML section (#5972) |
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
