# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> **Note:** This changelog is automatically updated on each stable release via CI.

## [0.4.3] - 2026-03-17

### Added
- Two-tier response cache with multi-provider token tracking and cache analytics
- X/Twitter and Mochat channel integrations
- Configurable `initial_prompt` in transcription config for proper noun recognition
- Health metrics, adaptive intervals, and task history for heartbeat system
- Hands dashboard metrics and events for observability
- `VOLCENGINE_API_KEY` env var support for VolcEngine/ByteDance gateway
- AiHubMix, SiliconFlow, and Codex OAuth provider gaps closed
- Merkle hash-chain audit trail for security
- SQLite session backend with FTS5, trait abstraction, and migration
- Browser delegation tool
- WhatsApp Web voice message transcription

**Full Changelog**: [`v0.4.0...v0.4.3`](https://github.com/zeroclaw-labs/zeroclaw/compare/v0.4.0...v0.4.3)

## [0.4.0] - 2026-03-16

### Added
- Token-based context compaction, persistent sessions, and LLM consolidation
- Autonomous knowledge-accumulating agent packages (Hands)
- Secure HMAC-SHA256 node transport layer
- Notion database poller channel and API tool
- Capability-based tool access control
- MCSS security operations tool
- Nevis IAM integration for SSO/MFA authentication
- Multi-agent swarm orchestration with Mistral tool fix
- `allow_private_hosts` option for `http_request` tool
- Backup/restore and data management tools
- Cloud transformation accelerator tools
- Microsoft 365 integration via Graph API
- Project delivery intelligence tool
- OpenVPN tunnel provider
- Multi-client workspace isolation

### Changed
- Restored `--interactive` flag for swarm mode

### Fixed
- Tool call failure reasons now surfaced in chat progress messages

**Full Changelog**: [`v0.3.2...v0.4.0`](https://github.com/zeroclaw-labs/zeroclaw/compare/v0.3.2...v0.4.0)

## [0.3.2] - 2026-03-15

### Added
- Two-phase heartbeat execution with structured tasks and auto-routing

**Full Changelog**: [`v0.3.1...v0.3.2`](https://github.com/zeroclaw-labs/zeroclaw/compare/v0.3.1...v0.3.2)

## [0.3.1] - 2026-03-15

### Added
- Termux (aarch64-linux-android) release target in CI

**Full Changelog**: [`v0.3.0...v0.3.1`](https://github.com/zeroclaw-labs/zeroclaw/compare/v0.3.0...v0.3.1)

## [0.3.0] - 2026-03-14

### Added
- Comprehensive channel matrix tests
- Auto-sync README "What's New" and "Contributors" sections on release

**Full Changelog**: [`v0.2.1...v0.3.0`](https://github.com/zeroclaw-labs/zeroclaw/compare/v0.2.1...v0.3.0)

## [0.2.1] - 2026-03-14

### Added
- crates.io publish workflow and package config with auto-sync on version bump
- `tool_filter_groups` for per-turn MCP tool schema filtering
- Interactive session state persistence and recovery
- WeCom (WeChat Enterprise) Bot Webhook channel
- `ack_reactions` config to disable channel reactions
- `show_tool_calls` config to suppress tool notifications
- Debian-based Docker container variant with shell tools
- Cron run history API and dashboard panel
- Dynamic node discovery and capability advertisement
- Multi-turn chat for WebSocket gateway connections
- Branded one-click installer with secure pairing flow
- Read markers and typing notifications for Matrix
- Custom API path suffix for `custom:` provider endpoints
- 17 new providers (total now 61)
- Custom HTTP headers for LLM API requests
- On-demand MCP tool loading via `tool_search`
- MCP subsystem tools layer with multi-transport client
- Windows support for shell `tool_call` execution
- Electric blue dashboard restyle with animations and logo
- Message draft preservation in agent chat across view switches

**Full Changelog**: [`v0.1.9a...v0.2.1`](https://github.com/zeroclaw-labs/zeroclaw/compare/v0.1.9a...v0.2.1)

## [0.1.9a] - 2026-03-12

### Added
- Live tool call notifications with argument visibility
- Matrix reaction, threading, pin/unpin, file upload, voice message, and multi-room support
- Opencode-go provider
- `--reinit` flag to prevent accidental config overwrite
- Auto-expanding chat textarea and copy-on-hover for messages
- Azure OpenAI provider support
- 32-bit system support via feature gates
- `channel send` CLI command for outbound messages
- `tool_call_dedup_exempt` config to bypass within-turn dedup
- Configurable HTTP request timeout per provider
- Webhook-audit builtin hook

### Fixed
- Embedding `api_key` resolution from `embedding_provider` env var
- Channel secrets encryption/decryption on save/load
- Qwen `think` tag stripping for Ollama responses
- SIGTERM graceful shutdown for daemon
- WhatsApp Web session reconnect with QR on logout
- CJK input crash via byte-level stdin reads
- Brave API key lazy resolution with decryption support

**Full Changelog**: [`v0.1.7...v0.1.9a`](https://github.com/zeroclaw-labs/zeroclaw/compare/v0.1.7...v0.1.9a)

## [0.1.7] - 2026-02-24

### Added
- Lark and Feishu channel provider split
- Prompt injection defense and leak detection
- Novita AI as OpenAI-compatible provider
- WATI WhatsApp Business API channel
- Android target support (armv7 + aarch64)

### Fixed
- Non-image files no longer get `[IMAGE:]` markers in Telegram
- `reasoning_content` preserved in tool-call conversation history

**Full Changelog**: [`v0.1.6...v0.1.7`](https://github.com/zeroclaw-labs/zeroclaw/compare/v0.1.6...v0.1.7)

## [0.1.6] - 2026-02-22

### Changed
- Promotion release (dev to main pipeline stabilization)

**Full Changelog**: [`v0.1.5...v0.1.6`](https://github.com/zeroclaw-labs/zeroclaw/compare/v0.1.5...v0.1.6)

## [0.1.5] - 2026-02-22

### Changed
- Dependency bumps: `rppal` 0.22.1, `which` 8.0.0, `codeql-action` 4.32.4, and Rust crate updates

**Full Changelog**: [`v0.1.4...v0.1.5`](https://github.com/zeroclaw-labs/zeroclaw/compare/v0.1.4...v0.1.5)

## [0.1.4] - 2026-02-21

### Added
- Main/dev branch split with dev-to-main promotion gate

### Fixed
- Release publish robustness with dual-license file attachment
- Duplicate SHA256SUMS asset upload removed
- Docker images now publish only on `v*` tags

**Full Changelog**: [`v0.1.2...v0.1.4`](https://github.com/zeroclaw-labs/zeroclaw/compare/v0.1.2...v0.1.4)

## [0.1.2] - 2026-02-21

### Added
- `non_cli_excluded_tools` config for channel tool filtering
- Embedded web dashboard with React frontend
- Runtime trace diagnostics for tool-call and model replies
- Draft progress streaming for tool call execution
- Telnyx AI inference provider and ClawdTalk voice channel
- First-class SGLang provider
- Natural-language scenario model routing and delegate profile config
- `content_search` tool for regex-based file content search
- Randomized ack reactions for Telegram/Discord/Lark channels
- Built-in static security audit for skill packages
- OTP + estop phase 1 core
- Gemini OAuth with consolidated OAuth utilities
- Accelerated release build via cargo-slicer (27% faster fresh builds)

### Fixed
- Multimodal payload support for vision-capable OpenAI-compatible providers
- OTLP paths for OTel endpoints
- Matrix E2EE key persistence across daemon restarts
- Guided prompts when installer stdin is piped
- Wildcard `allowed_domains` support for `browser_open`/`http_request`
- OpenRouter vision with failed image turn rollback
- Shell path and variable expansion security hardening
- Cron tool autonomy and approval gates enforced

**Full Changelog**: [`v0.1.1...v0.1.2`](https://github.com/zeroclaw-labs/zeroclaw/compare/v0.1.1...v0.1.2)

[0.4.3]: https://github.com/zeroclaw-labs/zeroclaw/releases/tag/v0.4.3
[0.4.0]: https://github.com/zeroclaw-labs/zeroclaw/releases/tag/v0.4.0
[0.3.2]: https://github.com/zeroclaw-labs/zeroclaw/releases/tag/v0.3.2
[0.3.1]: https://github.com/zeroclaw-labs/zeroclaw/releases/tag/v0.3.1
[0.3.0]: https://github.com/zeroclaw-labs/zeroclaw/releases/tag/v0.3.0
[0.2.1]: https://github.com/zeroclaw-labs/zeroclaw/releases/tag/v0.2.1
[0.1.9a]: https://github.com/zeroclaw-labs/zeroclaw/releases/tag/v0.1.9a
[0.1.7]: https://github.com/zeroclaw-labs/zeroclaw/releases/tag/v0.1.7
[0.1.6]: https://github.com/zeroclaw-labs/zeroclaw/releases/tag/v0.1.6
[0.1.5]: https://github.com/zeroclaw-labs/zeroclaw/releases/tag/v0.1.5
[0.1.4]: https://github.com/zeroclaw-labs/zeroclaw/releases/tag/v0.1.4
[0.1.2]: https://github.com/zeroclaw-labs/zeroclaw/releases/tag/v0.1.2
