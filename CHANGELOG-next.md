# Changelog — v0.7.4 → v0.7.5

> v0.7.5 is a substantial follow-on to v0.7.4. The headline is in-browser
> onboarding and configuration: a schema-driven `/onboard` flow, a per-property
> gateway CRUD surface backed by an OpenAPI 3.1 spec and a typed CLI, a
> three-surface personality editor (CLI / TUI / web), and live drift detection
> for hand-edited config files. ACP picks up real cancellation and a
> tool-approval back-channel, per-provider pricing finally makes cost tracking
> accurate, the installer ships presets and a TTY feature picker, and the web
> dashboard learns hot model switching plus a stop button. Around 71 commits
> from 24 contributors.

---

## Highlights

- **Web onboarding, per-property config CRUD, and a personality editor — all
  schema-driven.** A new `/onboard` route renders the full first-run flow in
  the browser (Workspace, Providers, Channels, Memory, Hardware, Tunnel,
  Personality), driven by per-property `OPTIONS`/`GET`/`PUT`/`DELETE`/`PATCH`
  endpoints under `/api/config/*` that share their core (`Config::set_prop`)
  with the CLI and any third-party tool. The runtime emits an OpenAPI 3.1 spec
  at `/api/openapi.json` (Scalar explorer at `/api/docs`), the dashboard
  TypeScript client is generated from it via `openapi-typescript`, and the CLI
  picks up matching `config patch`, `config docs`, `config schema --path`, and
  `--json` envelopes on the existing `get`/`set`/`init`/`migrate` commands. A
  drift banner surfaces hand-edits to `config.toml` with per-row in-memory vs
  on-disk diffs and a one-click reload, and a personality system lets each of
  the seven runtime markdown files (`SOUL.md`, `IDENTITY.md`, `USER.md`,
  `AGENTS.md`, `TOOLS.md`, `HEARTBEAT.md`, `MEMORY.md`) be edited from CLI
  (`$EDITOR`), TUI, or the web UI's CodeMirror editor (#6179).

- **ACP gets cancellation, a tool-approval back-channel, and concurrency
  safety.** ACP protocol v1 now exposes `session/cancel` to abort an in-flight
  turn (#6374), the gateway WebSocket carries tool approvals over a dedicated
  back-channel (#6387), and concurrent `session/prompt` requests are rejected
  with a clear error instead of racing (#6408).

- **Live model switching and a real stop button in the web dashboard.** Pick a
  different model from the chat dropdown without losing context (#6101), open
  the agent chat directly from a memory row (#6217), trigger any cron job from
  the UI (#6164), and use the new chat-input lock plus running indicator and
  stop button while a turn is in flight (#6220). OpenRouter free models are
  flagged in the dropdown (#6218) and themes/sessions/CSS tokens behave
  consistently across pages (#5207).

- **Per-provider pricing makes cost tracking real.** `ModelProviderConfig` now
  carries pricing per provider profile (#6357), the gateway records cost and
  token usage on every turn (#6159), and missing pricing logs a single WARN per
  `(provider, model)` pair instead of spamming the log (#6356).

- **Installer overhaul.** `zeroclaw install` learns preset and gateway flags, a
  TTY-driven feature picker, web/dist building, and an onboarding gate (#6385);
  Linux MUSL static binaries are back in the release artifacts (#6411); the web
  dashboard installs to platform-correct data directories on macOS and Windows
  (78d2cd6c0); and the prebuilt path correctly extracts the dashboard again
  (821fbfcfc, a2c1e2bb2).

- **HMAC tool receipts wired up end-to-end.** The receipt-signing path stripped
  out of #5168 is now active (#6214), giving each tool result a verifiable HMAC
  trail when the feature is enabled.

---

## What's New

### Channels

- **ACP (Agent Client Protocol):** `session/cancel` aborts in-flight turns
  (#6374); concurrent `session/prompt` requests are rejected rather than
  raced (#6408); ACP protocol v1 picks up tool-call permission and a back-channel
  for structured prompts (#6167).
- **Matrix:** attachments are uploaded when finalizing partial drafts (#6200);
  device identity is derived from the `whoami` response when an access token is
  used directly (242ef2404), and access-token sessions now require an explicit
  device identity (21d0c5d6b).
- **WhatsApp:** `fromMe` replies are scoped to self-chat or trigger prefixes
  (#6353); LID→phone resolution failures are logged so debugging session
  drops is possible (#6354).
- **Discord:** inbound image attachments are preserved through the provider
  pipeline (#6184).
- **Jira:** API v2 server mode is supported alongside cloud (#6116).

### Web Dashboard

- **Schema-driven `/onboard` flow** — first-run users complete provider auth,
  channels, memory, hardware, tunnel, and personality entirely in the browser,
  with forms rendered from `GET /api/config/list`'s `kind` / `type_hint` /
  `enum_variants` (no value-sniffing). Fresh installs auto-redirect from `/`
  to `/onboard` (#6179).
- **Schema-driven config editor** at `/config` with per-section forms, drift
  banner, and per-row drift diff (in-memory vs on-disk; secrets surface only
  the *fact* of drift). One-click "Restart daemon to apply" reloads via the
  in-process watch channel (#6179).
- **Live model switching with chat context preserved on navigation** (#6101).
- **Chat input lock, running indicator, and stop button** while a turn is
  active (#6220), with `tool_call`/`tool_result` rendering off by default and a
  toolbar toggle to opt back in (#6388).
- **Open agent chat directly from a memory row** when `session_id` is present
  (#6217).
- **OpenRouter free models marked** in the default-model dropdown (#6218).
- **Manual cron trigger** from the web UI (#6164).
- **Theme switching, session crash, and CSS token consistency fixes** unify the
  visual surface across pages (#5207).
- **Agent tool button height** matches sibling controls (#6369).

### Providers

- **Anthropic:** `base_url` is honored for the default provider (#6314).
- **Bedrock:** `credential_process` support enables enterprise-friendly auth
  flows (#6168).
- **Groq:** per-profile `native_tools` override on `ModelProviderConfig`
  (#6380).
- **StepFun:** new `stepfun-intl` endpoint (#6310).
- **xAI:** model listing is restored (9bd95a0c9).
- **OpenAI-compatible:** `tool_call` `extra_content` is preserved so Gemini's
  `thoughtSignature` survives round-tripping (#6264); model id normalization
  has test coverage (cc8f0e7bd).

### Configuration

- **Per-property gateway CRUD** under `/api/config/*` (`GET`/`PUT`/`DELETE`/
  `PATCH`/`OPTIONS`/`list`/`init`/`migrate`/`drift`). Comment-preserving PATCH
  attaches a per-op `comment` field to the on-disk TOML key; PATCH/PUT are
  validator-gated with snapshot revert on failure; structured `ConfigApiError`
  surfaces stable codes for invalid range, enum variant, format, dangling
  reference, and required-field-empty (#6179).
- **Drift detection** compares in-memory state to on-disk `config.toml` via
  server-side SHA-256 hashing (secrets never leave the server). 409
  `config_changed_externally` blocks a write against a drifted path; explicit
  `X-ZeroClaw-Override-Drift: true` overrides (#6179).
- **OpenAPI 3.1 spec** at `/api/openapi.json`, Scalar explorer at `/api/docs`,
  committed snapshot at `crates/zeroclaw-gateway/openapi.json` regenerated by
  `cargo xtask gen-openapi` and CI-checked for staleness (#6179).
- **TypeScript client codegen** for the dashboard via `openapi-typescript`
  (`npm run gen-api`, wired into `npm run build`); tsc fails when the generated
  shape stops matching consumers (#6179).
- **CLI parity:** `zeroclaw config patch` (JSON Patch over the same core),
  `config docs`, `config schema --path <prop>`, `set --comment`, `--json`
  envelopes on `get`/`set`/`init`/`migrate` matching HTTP shapes (#6179).
- **Per-provider pricing** on `ModelProviderConfig` (#6357).
- **Dotted provider map keys preserved** across config round-trips (#6317).
- **Encrypted-secret mismatch surfaced clearly** when `.secret_key` doesn't
  match the encrypted blob (#6379).

### Agent & Runtime

- **Cost and token usage recorded on every gateway turn** (#6159).
- **Cost-pricing WARN once per `(provider, model)`** instead of per-turn
  (#6356).
- **Conversation memories excluded from `build_context` recall** to keep prior
  conversation chunks from leaking into recall results (#6316, follow-up to
  #5415).
- **Autosaved conversation memories recalled on subsequent turns** (#6363).
- **Reasoning content captured from streaming responses** (#6107).
- **Image markers stripped** from non-vision context-compression payloads
  (#6189).
- **Wildcard memory recall** is treated as a recent-memory query (#6296).
- **Daemon canvas store shared across subsystems** (gateway, channels, daemon)
  (#6221).
- **Cancel-tokens evicted when sessions are deleted mid-turn** to prevent stale
  approval state (#6216).
- **Session backend unified behind one factory** across runtime and channels
  (#6384).

### Tools

- **`zeroclaw memory reindex` CLI** for rebuilding the embedding index in place
  (#6046).
- **Tavily search backend** for `tools/web_search`, with bearer-header
  authentication and encrypted-key support (3205f0a, 46cb4510c, 3014e355c).
- **`tool_timeout_secs` honoured for HTTP SSE tool calls** (#5945) and used to
  derive the HTTP client timeout (#6397).
- **`allow_scripts` plumbed through `ReadSkillTool`** to the skill loader so
  declared scripts can actually run (#5981, closes #5697).
- **DockerSandbox bind-mount support** for workspace mounts (#5905, closes
  #5720).

### Personality

- **Three-surface personality editor** for the seven runtime markdown files
  (`SOUL.md`, `IDENTITY.md`, `USER.md`, `AGENTS.md`, `TOOLS.md`, `HEARTBEAT.md`,
  `MEMORY.md`) the runtime injects into the system prompt. CLI uses `$EDITOR`,
  TUI suspends and hands off to `$EDITOR`, web UI ships a CodeMirror 6 editor
  (one-dark theme + markdown grammar) with Edit/Preview toggle, per-tab Insert
  / Replace template buttons, char counter, and 409 `personality_disk_drift`
  resolution UX (`take theirs` / `keep mine`). Backed by a 7-file backend
  allowlist enforced in `crates/zeroclaw-gateway/src/api_personality.rs` —
  `BOOTSTRAP.md` is intentionally excluded as a first-run scaffold (#6179).

### Gateway

- **Tool-approval back-channel** via `WsApprovalChannel` so approvals don't
  fight the main message stream (#6387).
- **Fail-loud model resolution** at request time across gateway and channels
  (#6215, refined in #6493 to keep `/onboard` reachable on fresh installs).
  Misconfigured providers surface a clear error on the first chat call rather
  than silently substituting a vendor default.
- **Daemon boots without a configured model** so the browser onboarding flow
  at `/onboard` is reachable on fresh installs and partially-configured
  states. The gateway logs a `WARN` pointing at `/onboard` and chat dispatch
  refuses with a structured `needs_onboarding` marker until at least one
  `[providers.models.<name>] model = "..."` is set; the channels supervisor
  exits cleanly instead of restart-looping (#6493).
- **Connect-time `cwd` parameter** on the WebSocket pins the per-session
  security sandbox root (#6179, follow-on to #6167).

### Security

- **HMAC tool receipts activated** — the wiring stripped out of #5168 is now
  live and verifying receipts end-to-end (#6214).
- **`git -C` vs `git -c` distinction in the security policy** — case-preserved
  argument lists prevent legitimate `git -C <dir>` invocations from being
  blocked as `-c` config overrides (0bc0dc676, closes #5809).

### Installation & Distribution

- **Installer presets, gateway flags, TTY feature picker, web/dist build,
  onboarding gate** (#6385).
- **MUSL static binaries restored** for the Linux release artifacts (#6411).
- **Platform-correct web data directory** on macOS and Windows (78d2cd6c0).
- **Web dashboard extraction restored** in the prebuilt install path
  (821fbfcfc, a2c1e2bb2).
- **Workspace-member resolution** unbroken in `Dockerfile` and
  `Dockerfile.debian` (#6305).
- **`xtask:web` re-runs `npm install`** when `node_modules` is stale relative
  to the lockfile (#6355).

### Internationalization

- **Translations synced** for fr, ja, es, with new zh-CN coverage (#6170).
- **Chinese WeChat CLI strings** added (#6242).

### Documentation

- **Raspberry Pi setup guide** (25e77cc52, closes #4704).
- **Hardware page revisions:** Podman recommendation now justifies its memory
  budget (f595de532); macOS cross-compile recipe and install destination fixed
  per #6203 review (96a7f0018, dd535fb72).
- **Custom OpenAI-compatible provider syntax** clarified (#6300).
- **YOLO config examples** updated (#6194).
- **Philosophy page** links the RFCs and Fluent (#6232).

### Improvements

- **Integrations registry refactored** to a single schema-driven for-loop
  (#6386).
- **Doctor self-test** now reports both the configured host and the probed
  loopback so port-binding issues are easier to diagnose (#6219).
- **Workspace `default-run` set** to keep docs CI green (46235824e).

---

## Bug Fixes

| Area | Fix |
|---|---|
| ACP | Reject concurrent `session/prompt` requests instead of racing them (#6408). |
| Agent / runtime | Exclude Conversation memories from `build_context` recall (#6316). |
| Agent / runtime | Capture `reasoning_content` from streaming responses (#6107). |
| Agent / runtime | Strip image markers from non-vision context compression (#6189). |
| Agent / runtime | Treat bare-wildcard recall as a recent-memory query (#6296). |
| Channels | Recall autosaved conversation memories on subsequent turns (#6363). |
| Channels (Discord) | Preserve inbound image attachments for providers (#6184). |
| Channels (Jira) | Support API v2 server mode (#6116). |
| Channels (Matrix) | Require explicit device identity for access-token sessions (21d0c5d6b); derive identity from `whoami` (242ef2404). |
| Channels (WhatsApp) | Scope `fromMe` replies to self-chat or trigger prefixes (#6353); surface LID→phone resolution failures in logs (#6354). |
| CI / docs build | Track `lang-switcher.js.tpl`, generate `.js` at build time (#6395); set workspace `default-run` to unblock docs CI (46235824e); remove the obsolete `CHANGELOG-next.md` cleanup step (#6265). |
| Config | Preserve dotted provider map keys (#6317); surface `.secret_key` mismatch on enc2 decrypt (#6379). |
| Docker | Unbreak workspace-member resolution in `Dockerfile` and `Dockerfile.debian` (#6305). |
| Gateway | Record cost and token usage on every turn (#6159); evict `cancel_tokens` when a session is deleted mid-turn (#6216); fail-loud model resolution mirrored across gateway and channels (#6215); daemon boots without a configured model so `/onboard` stays reachable on fresh installs and partially-configured states (#6493). |
| Installation | Use platform-correct web data directory on macOS and Windows (78d2cd6c0); restore web-dashboard extraction in prebuilt install (821fbfcfc, a2c1e2bb2). |
| Providers (Anthropic) | Respect `base_url` config for the default provider (#6314). |
| Providers (compatible) | Preserve `tool_call` `extra_content` so Gemini `thoughtSignature` round-trips cleanly (#6264). |
| Providers (xAI) | Restore model listing (9bd95a0c9). |
| Runtime / channels | Unify session backend behind one factory (#6384); share canvas store across daemon subsystems (#6221). |
| Runtime / cost | WARN once per `(provider, model)` for missing pricing (#6356). |
| Security | Distinguish `git -C` from `git -c` in security policy (0bc0dc676). |
| Tools | Honour `tool_timeout_secs` for HTTP SSE tool calls (#5945); derive HTTP client timeout from `tool_timeout_secs` (#6397); pass `allow_scripts` through `ReadSkillTool` to the skill loader (#5981). |
| Tools (web_search) | Authenticate Tavily via Bearer header rather than body (46cb4510c). |
| Web | Fix theme switching, session crash, and CSS token consistency (#5207); agent tool button height (#6369); default `tool_call`/`tool_result` rendering off with toolbar toggle (#6388). |
| xtask:web | Re-run `npm install` when `node_modules` is stale vs lockfile (#6355). |
| Doctor | Self-test report shows configured host alongside probed loopback (#6219). |

---

## Contributors

- @abhinavmathur-atlan
- @aliasliao
- @aredridel
- @ArgenisDLR
- @Audacity88
- @dahungkee
- @donut-wenzhang
- @drbparadise
- @ilteoood
- @JordanTheJet
- @jscholz
- @MestreY0d4-Uninter
- @nanyuantingfeng
- @nebullii
- @nxajh
- @patrickzzz
- @perlowja
- @singlerider
- @songchao0421
- @theonlyhennygod
- @tidux
- @tredondo
- @WareWolf-MoonWall
- @xiongzubiao

---

*Full diff: `git log v0.7.4..v0.7.5 --oneline`*
