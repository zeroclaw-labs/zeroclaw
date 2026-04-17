# Changelog — v0.6.9 → next

> Changes since the **v0.6.9** stable release. This release represents the largest
> structural overhaul in ZeroClaw's history: the entire codebase has been split into a
> proper Cargo workspace of focused crates, a new config schema has shipped with a live
> migration path, and a wave of channel, provider, and security improvements have landed
> on top of that foundation.

---

## Highlights

- **Workspace split complete** — ZeroClaw is now a multi-crate Cargo workspace. The
  monolithic source tree has been decomposed into 12+ focused crates
  (`zeroclaw-api`, `zeroclaw-runtime`, `zeroclaw-gateway`, `zeroclaw-channels`,
  `zeroclaw-tools`, `zeroclaw-memory`, `zeroclaw-providers`, `zeroclaw-infra`,
  `zeroclaw-config`, `zeroclaw-tui`, `zeroclaw-plugins`, `zeroclaw-hardware`).
  The foundation binary now builds at **6.6 MB** with `--no-default-features`.

- **Config V2 schema with automatic migration** — Provider config has moved to a cleaner
  layout. Running `zeroclaw config migrate` upgrades your existing config in-place,
  preserving comments. The old `props` subcommand still works but is now deprecated in
  favour of `zeroclaw config`.

- **OpenRouter streaming** — OpenRouterProvider now streams responses token-by-token
  instead of waiting for the full response, matching the experience of native providers.

- **Web dashboard decoupled from the binary** — The dashboard is now built separately
  and embedded at release time. `cargo install` and AUR/Homebrew packages include it.
  A new **voice mode** and **plugins page** have been added to the dashboard.

- **LINE channel** — LINE Messaging API is now a supported channel.

- **Matrix improvements** — Mention-only filtering (the agent only responds when
  mentioned), encrypted media download restored, outbound attachment support added, and
  onboarding wizard preservation.

---

## What's New

### Architecture & Workspace

- Extracted 12 workspace crates from the monolith, implementing the microkernel RFC
  roadmap (RFC D1–D5). Every subsystem — providers, channels, tools, memory, infra,
  config, gateway, TUI, plugins, hardware — now lives in its own crate with explicit
  dependency boundaries enforced by the compiler.
- Foundation binary (`--no-default-features`) compiles clean at 6.6 MB.
- `agent-runtime` feature flag gates the full agent loop; the kernel binary builds
  without it.
- Switched TLS from `aws-lc-rs` to `ring` and stripped `.eh_frame` sections, reducing
  binary size further.
- `schemars` is now optional behind a `schema-export` feature flag — no longer a
  mandatory compile dependency.
- 28 per-channel feature flags with forwarding chains so unused channels add zero
  compile time.
- Workspace-wide `[workspace.dependencies]` and `[workspace.package]` inheritance
  eliminates version duplication across `Cargo.toml` files.
- RFC Rev 2 compliance: stability tiers, versioning policy, and release profile are now
  wired into the workspace.

### Providers

- **OpenRouterProvider** now supports streaming (#5717). Responses appear token-by-token
  instead of arriving all at once.
- Fixed: native tool-call messages are now stripped before sending to providers that
  have `native_tool_calling = false`, preventing provider errors (#5762).
- Fixed: DeepSeek V3.2 system prompt escaping and token estimation corrected (#5454).

### Channels

- **LINE Messaging API** channel added (#5642).
- **Matrix**: mention-only filtering — the agent can be configured to respond only when
  directly mentioned. Encrypted media download restored. Outbound attachment support
  added. Onboarding wizard settings now preserved across restarts (#5166, #5727).
- Sender user ID is now propagated into the channel system prompt, giving the agent
  context about who it is talking to (#5526).
- Email and VoiceCall channels now have an `enabled` field and are correctly wired into
  the orchestrator (#5659).
- `<think>` tags are stripped from streaming draft updates before they reach the client
  (#5505).
- Fixed: missing channels in `build_channel_by_id` caused `sessions_send` to silently
  fail for some channel types (#5506).
- Telegram and Matrix implementations moved out of the orchestrator into their own
  modules (#5639).

### Configuration

- **Config V2 schema** with a new provider layout (`providers.models`,
  `providers.fallback`, `model_routes`, `embedding_routes`).
- `zeroclaw config migrate` upgrades a V1 config to V2 in-place, preserving comments
  and formatting.
- `zeroclaw config` replaces `zeroclaw props`. The old `props` subcommand is deprecated
  but still functional.
- Onboarding wizard updated to write V2 provider format directly.
- Fixed: false "Unknown config key" warnings for `Option<T>` fields and config aliases
  (#5510).
- Fixed: `providers.fallback` now emits a warning if it references a key that does not
  exist in `providers.models`.
- Fixed: temperature validation restored in the `providers.models` loop.
- Slack config: `channel_id` deprecated in favour of `channel_ids` (plural) for V2.
- Nostr, WhatsApp Web, and hardware wizard sections wired into the onboarding flow
  (#5640).

### Web Dashboard

- Voice mode added to the dashboard.
- Plugins management page added.
- Dashboard is now decoupled from the main binary — built separately and embedded at
  release time. Included in binary releases, AUR, Homebrew, and `cargo install` (#5675,
  #5665).
- Web build logic moved into the gateway crate; no-op recompiles (previously ~1 minute)
  eliminated (#5ec5f2a6).

### Agent & Runtime

- CLI channel factory now registered for interactive mode — `zeroclaw` interactive
  sessions work again after the workspace split (#5802).
- Duplicate `ToolCall` events in `turn_streamed` deduplicated; clients no longer see the
  same tool call reported twice (#5746).
- Session integrity improvements: streaming refactor and history pruning for long
  conversations (#5167).
- Cron agent jobs no longer trigger `auto_save`, preventing runaway memory consolidation
  on scheduled tasks (#5664).
- Windows: the shell console window is now hidden when running as a background process
  (#5563).

### Skills (Claude Code)

- `github-issue-triage` skill added — automates structured triage of GitHub issues using
  Claude Code (#5780).
- `squash-merge` skill added — preserves clean commit history when merging upstream
  changes (#5782).

### Security

- Dangerous interpreter arguments (e.g. `-e`, `--eval`, `-c` on interpreters) are now
  blocked by the command security policy (#5702).
- Heredocs and safe shell redirects (`<<EOF`, `>`, `>>`) are explicitly allowed (#5160).

### Installation & Distribution

- `install.sh` rewritten from scratch for the workspace split — correctly handles the
  new crate layout and binary paths (#5666).
- AUR package migrated from `zeroclaw` to `zeroclawlabs` (#5544).
- Daemon supervisor and onboarding launch checks now include the webhook channel (#5799).

### Dependencies & Security Advisories

- `rustls-webpki` and `rumqttc` bumped to resolve RUSTSEC-2026-0098 and
  RUSTSEC-2026-0099 (#5786).

---

## Bug Fixes (summary)

| Area | Fix |
|------|-----|
| Provider | Strip native tool messages for non-native-tool-calling providers |
| Provider | DeepSeek V3.2 system prompt escaping and token estimation |
| Agent | CLI channel factory missing in interactive mode |
| Agent | Duplicate ToolCall events in streaming turns |
| Matrix | Encrypted media download; outbound attachments |
| Channels | Missing Arc Provider forwarding methods |
| Channels | `<think>` tag leaking into streaming draft updates |
| Config | False "Unknown config key" warnings on Option fields |
| Config | Temperature validation missing from providers loop |
| Config | Fallback key references nonexistent provider — now warns |
| Session | Integrity, streaming refactor, history pruning |
| Cron | auto_save causing recursive memory bloat on scheduled jobs |
| Security | Dangerous interpreter flags not blocked |
| Install | install.sh broken after workspace split |
| Runtime | Windows console window visible in background mode |
| Distribution | Web dashboard missing from AUR and cargo install builds |

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

### Slack `channel_id` deprecated

Use `channel_ids` (a list) in the Slack config block. `channel_id` (singular) still
works but is deprecated in V2.

### Workspace crate boundaries

If you have any code that depends directly on internal ZeroClaw crate paths (e.g. for
embedding or testing), the crate structure has changed significantly. Refer to
`AGENTS.md` for the current crate map and stability tiers. `zeroclaw-api` is the stable
extension point — all other crates are Beta or Experimental.

---

## Contributors

Thank you to everyone who contributed to this release:

- @aliasliao
- @ArgenisDLR
- @Audacity88
- @c98
- @DaBlitzStein
- @freeekanayaka
- @guitaripod
- @ilteoood
- @JordanTheJet
- @kunalk16
- @markuman
- @nayrosk
- @ninenox
- @singlerider
- @theonlyhennygod
- @titulus
- @UtopiaX
- @vernonstinebaker
- @WareWolf-MoonWall
- @wlh320

---

*Full diff: `git log v0.6.9..HEAD --oneline`*