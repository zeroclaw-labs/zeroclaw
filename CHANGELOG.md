# Changelog

All notable changes to ZeroClaw will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-02-24

### Added
- **15 messaging channels**: Telegram, Discord, Slack, WhatsApp, Matrix, Signal, Email, IRC,
  CLI, DingTalk, Lark/Feishu, QQ, Mattermost, iMessage, and generic Webhook — all with
  deny-by-default allowlists, per-channel health checks, and automatic supervisor restart.
- **9 LLM providers + 19 aliases**: Anthropic, OpenAI, Gemini, Ollama, OpenRouter, Copilot,
  OpenAI Codex, GLM/Zhipu, and generic OpenAI-compatible (`custom:`) / Anthropic-compatible
  (`anthropic-custom:`) endpoints. Plus Groq, Mistral, xAI, DeepSeek, Together, Fireworks,
  Perplexity, Cohere, NVIDIA NIM, Venice, Vercel, Cloudflare AI, Moonshot/Kimi, MiniMax,
  Bedrock, Qianfan, Qwen/DashScope, and LM Studio.
- **ResilientProvider wrapper**: Automatic retry with exponential backoff, provider fallback
  chains, circuit breaker pattern, and per-provider rate limiting.
- **Subscription auth profiles**: Multi-account encrypted-at-rest auth for OpenAI Codex OAuth
  and Anthropic setup-token flows (`zeroclaw auth login/paste-token/refresh/use`).
- **Model routing**: `hint:<name>` routing via `[[model_routes]]` config for multi-provider
  model selection (e.g., `hint:reasoning` → Claude Opus, `hint:fast` → Groq Llama).
- **Telegram voice pipeline**: Full STT/TTS support via OpenAI-compatible Whisper/TTS APIs
  with configurable model, voice, language, and max duration.
- **Telegram mention_only mode**: Bot only responds to @-mentions in groups; DMs always work.
- **Discord mention_only mode**: Same behavior as Telegram mention_only for Discord guilds.
- **WhatsApp Business Cloud API**: Webhook-based integration with HMAC signature verification,
  E.164 phone number allowlists, and Meta Business Suite OAuth tokens.
- **Matrix E2EE support**: End-to-end encrypted room decryption via matrix-sdk, with automatic
  device/session restoration and key sharing.
- **Signal channel**: SSE-based listener with signal-cli HTTP daemon, group filtering, and
  attachment/story ignore options.
- **Lark/Feishu channel**: Dual-mode (WebSocket persistent + webhook callback), with Feishu
  (China) and Lark (International) endpoint toggle.
- **DingTalk channel**: Stream-mode integration with DingTalk Open Platform.
- **QQ channel**: Official QQ Bot SDK integration with C2C and group message support.
- **iMessage channel**: macOS-native AppleScript bridge for local iMessage integration.
- **Email channel**: IMAP polling + SMTP send with TLS, configurable poll interval and folder.
- **IRC channel**: TLS socket with NickServ, SASL, and bouncer password support.
- **Memory system**: SQLite hybrid search (FTS5 BM25 + vector cosine similarity), PostgreSQL
  backend, Lucid bridge, Markdown files, explicit `none` backend, snapshot/hydrate, and
  optional response cache. Custom embedding provider trait.
- **Browser tools**: Multi-backend (agent_browser, rust_native, computer_use) with domain
  allowlists and coordinate guardrails.
- **Composio integration**: Opt-in access to 1000+ OAuth apps via composio.dev.
- **AIEOS identity support**: AI Entity Object Specification v1.1 JSON identity format alongside
  default OpenClaw markdown identity files.
- **Tunnel system**: Cloudflare, Tailscale, ngrok, and custom tunnel support with automatic
  gateway bind safety.
- **Heartbeat engine**: Periodic task execution from HEARTBEAT.md with configurable interval.
- **Skills system**: TOML manifest + SKILL.md instruction loading for community skill packs.
- **Integrations registry**: 70+ integrations across 9 categories with plugin system.
- **Hardware peripherals**: STM32 and Raspberry Pi GPIO support via `Peripheral` trait.
- **Docker sandboxed runtime**: Optional Docker container execution with memory limits, CPU
  limits, read-only rootfs, and network isolation.
- **Service management**: `zeroclaw service install/status` for user-level background daemon.
- **Diagnostics**: `zeroclaw doctor` and `zeroclaw channel doctor` for system and channel health.
- **OpenClaw migration**: `zeroclaw migrate openclaw` with dry-run preview.
- **Python companion**: `zeroclaw-tools` pip package with LangGraph-based tool calling for
  providers with inconsistent native tool support.

### Changed
- **Project logo**: New SVG logo at `assets/logo.svg` replacing placeholder PNG.
- **Version bump**: 0.1.1 → 0.2.0 reflecting scope of channel/provider/docs additions.
- **Documentation overhaul**: Comprehensive setup guide, channels reference (all 15 channels
  with full config field documentation), providers reference (28+ providers with env vars),
  and interactive architecture overview HTML visualization.

### Security
- **Legacy XOR cipher migration**: `enc:` prefix deprecated; auto-migrates to `enc2:`
  (ChaCha20-Poly1305 AEAD) on decryption. Warning logged for legacy values.
- **Gateway bind safety**: Refuses `0.0.0.0` without active tunnel or explicit opt-in.
- **Channel allowlists**: Deny-by-default across all channels. Empty allowlist = deny all.
- **WhatsApp HMAC verification**: Optional `app_secret` for webhook signature validation.
- **Symlink escape detection**: Canonicalization + resolved-path workspace checks in file tools.

### Deprecated
- `enc:` prefix for encrypted secrets — use `enc2:` (ChaCha20-Poly1305) instead.

## [0.1.1] - 2026-02-18

### Added
- **Telegram voice/TTS test suite**: 19 new unit tests covering the full voice attachment
  pipeline in the Telegram channel (marker parsing, extension inference, path detection,
  send methods, caption handling, URL-based delivery, filename fallback, Audio vs Voice
  discrimination). Voice test coverage increased from 2 to 21 tests.

### Security
- **Legacy XOR cipher migration**: The `enc:` prefix (XOR cipher) is now deprecated.
  Secrets using this format will be automatically migrated to `enc2:` (ChaCha20-Poly1305 AEAD)
  when decrypted via `decrypt_and_migrate()`. A `tracing::warn!` is emitted when legacy
  values are encountered. The XOR cipher will be removed in a future release.

### Added
- `SecretStore::decrypt_and_migrate()` — Decrypts secrets and returns a migrated `enc2:`
  value if the input used the legacy `enc:` format
- `SecretStore::needs_migration()` — Check if a value uses the legacy `enc:` format
- `SecretStore::is_secure_encrypted()` — Check if a value uses the secure `enc2:` format
- **Telegram mention_only mode** — New config option `mention_only` for Telegram channel.
  When enabled, bot only responds to messages that @-mention the bot in group chats.
  Direct messages always work regardless of this setting. Default: `false`.

### Deprecated
- `enc:` prefix for encrypted secrets — Use `enc2:` (ChaCha20-Poly1305) instead.
  Legacy values are still decrypted for backward compatibility but should be migrated.

## [0.1.0] - 2026-02-13

### Added
- **Core Architecture**: Trait-based pluggable system for Provider, Channel, Observer, RuntimeAdapter, Tool
- **Provider**: OpenRouter implementation (access Claude, GPT-4, Llama, Gemini via single API)
- **Channels**: CLI channel with interactive and single-message modes
- **Observability**: NoopObserver (zero overhead), LogObserver (tracing), MultiObserver (fan-out)
- **Security**: Workspace sandboxing, command allowlisting, path traversal blocking, autonomy levels (ReadOnly/Supervised/Full), rate limiting
- **Tools**: Shell (sandboxed), FileRead (path-checked), FileWrite (path-checked)
- **Memory (Brain)**: SQLite persistent backend (searchable, survives restarts), Markdown backend (plain files, human-readable)
- **Heartbeat Engine**: Periodic task execution from HEARTBEAT.md
- **Runtime**: Native adapter for Mac/Linux/Raspberry Pi
- **Config**: TOML-based configuration with sensible defaults
- **Onboarding**: Interactive CLI wizard with workspace scaffolding
- **CLI Commands**: agent, gateway, status, cron, channel, tools, onboard
- **CI/CD**: GitHub Actions with cross-platform builds (Linux, macOS Intel/ARM, Windows)
- **Tests**: 159 inline tests covering all modules and edge cases
- **Binary**: 3.1MB optimized release build (includes bundled SQLite)

### Security
- Path traversal attack prevention
- Command injection blocking
- Workspace escape prevention
- Forbidden system path protection (`/etc`, `/root`, `~/.ssh`)

[0.2.0]: https://github.com/zeroclaw-labs/zeroclaw/releases/tag/v0.2.0
[0.1.1]: https://github.com/zeroclaw-labs/zeroclaw/releases/tag/v0.1.1
[0.1.0]: https://github.com/zeroclaw-labs/zeroclaw/releases/tag/v0.1.0
