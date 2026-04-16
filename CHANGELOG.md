# Changelog

All notable changes to ZeroClaw are documented in this file.

## [0.7.0] - 2026-04-16

### Highlights

ZeroClaw 0.7.0 is a major milestone release consolidating months of community-driven development across the entire 0.6.x series. This release brings **140+ features** and **200+ fixes** from **70+ contributors**, spanning new channels, security hardening, streaming improvements, a desktop companion app, voice call support, and much more.

### Features

#### Agent & Core

- Multi-pass context compression with error-driven probing
- Context overflow recovery in interactive daemon loop and tool call loop
- Fast-path tool result trimming and truncation to prevent context blowout
- Preemptive context check before provider calls
- Graceful shutdown when max tool iterations reached
- Shared iteration budget for parent/subagent coordination
- Personality module for structured identity file loading
- Complexity-based eval and auto-classify routing
- Token efficiency analyzer layer
- Loop detection guardrail for repetitive tool calls
- Native `tool_calls_only` config to disable text fallback parsing
- Thinking/reasoning level control per message
- Consolidate multiple messages into single response
- Drop orphan `tool_results` when trimming history
- Treat `tool_use`/`tool_result` as atomic groups in history pruning

#### Channels

- **LINE** Messaging API channel with Reply/Push API support
- **WeChat iLink** channel with media support
- **Mattermost** WebSocket-based real-time listener
- **Feishu/Lark** transport media support and cron delivery
- Generic transport stall watchdog
- Inbound message debouncing for rapid senders
- Message chunker with per-platform character limits
- Message redaction API
- Room creation and user invite API
- Per-sender `/thinking` runtime command for reasoning toggle
- Reply intent precheck before typing or draft send
- Per-chat model switch persistence to `routes.json`
- Parse media attachment markers in WhatsApp Web send
- `mention_only` support for WhatsApp Web and Matrix group filtering
- `interrupt_on_new_message` support for WhatsApp
- `observe_group` flag and per-chat session keys
- Automatic media understanding pipeline
- `/new` session reset extended to all channels
- Notify user when provider fallback occurs
- Migrate inline transcription to TranscriptionManager

#### Discord

- Partial and MultiMessage streaming modes
- Provider-level token streaming for real-time delivery
- History logging and search tool with channel cache
- Image and video attachment downloads for agent processing

#### Slack

- `/config` command with Block Kit UI for model switching
- Progressive draft message streaming via `chat.update`
- DraftEvent enum and progressive draft streaming
- Reaction-based cancellation and `finalize_draft` thread fix
- Resolve Slack permalinks via API
- Audio file transcription
- `use_markdown_blocks` config for Slack block compatibility

#### Matrix

- Automatic E2EE recovery, multi-room listening, and masked secrets
- Partial and MultiMessage streaming modes
- Populate media attachments and outgoing media support
- `mention_only` config for group room filtering

#### Email & Messaging

- Email attachment download and sending with multipart MIME
- QQ audio attachment transcription and WebSocket reconnection
- WATI audio and voice message transcription
- Lark audio message transcription
- Mattermost audio transcription via TranscriptionManager
- Feishu/Lark handle list message type
- Support forwarded messages in Telegram

#### Providers

- Cost-optimized provider routing strategy
- Rate-limit cooldown and model compatibility filtering
- `merge_system_into_user` option for ModelProviderConfig
- ZhipuJwt auth style for Z.AI and GLM providers
- Bearer token API keys for Amazon Bedrock
- Vision support for kimi-code provider
- Alibaba Coding Plan support
- Upgrade claude-code provider to full agent mode
- Parse proxy tool events from SSE stream
- Configurable `max_tokens` per provider
- SSE streaming support for Anthropic chat responses
- Native tool-event streaming parity
- Delegate streaming to resolved provider

#### Security

- WebAuthn / hardware key authentication
- Ed25519 plugin signature verification
- 1Password secret resolution via `op://` references
- Per-channel DM pairing manager
- Per-sender rate limiting via PerSenderTracker
- SSRF validator with CIDR blocking and homograph detection
- Path-validation fallback sandbox
- macOS sandbox-exec (Seatbelt) profiles
- Harden native sandbox backends with seccomp and fail-closed fallback
- HMAC-SHA256 signing wired to audit trail
- LeakDetector wired into outbound message path
- Per-domain trust scoring and regression detection
- Auth rate limiting for brute-force protection on gateway
- Mutual TLS (mTLS) for high-security deployments

#### Memory & Knowledge

- pgvector support and Postgres knowledge graph
- BM25 keyword search mode for memory retrieval
- Namespace isolation for delegate agents
- Bulk JSON export for GDPR Art. 20 data portability
- Bulk memory deletion by namespace and session
- Client relationship node types and tool actions
- Memory continuity across all execution paths

#### Gateway & Web Dashboard

- Event-triggered automation (routines engine)
- Per-session actor queue for concurrent turn serialization
- Session state machine with idle/running/error tracking
- Buffer SSE events for dashboard log persistence
- Broadcast cron job results to WebSocket clients
- Cross-channel dashboard with tabbed interface
- Collapsible desktop sidebar with local state persistence
- Responsive mobile sidebar with hamburger toggle
- Form-based config editor with mode toggle
- Collapsible thinking/reasoning UI and markdown rendering
- Persist Agent Chat history across navigation and refresh
- Chat history with optimized scroll behavior
- 24 color theme palettes in settings
- ToolCallCard component for tool call display

#### Voice & Media

- Real-time voice call support (Twilio/Telnyx)
- Unified VoicePipeline facade for STT+TTS channels
- Configurable global `max_audio_bytes` for TranscriptionConfig
- `transcribe_non_ptt_audio` config for WhatsApp STT

#### Tools & Skills

- Escalate-to-human tool with urgency routing
- Built-in `ask_user` tool for interactive prompts
- Claude Code task runner with Slack progress and SSH handoff
- Report template engine as standalone agent tool
- SecretStore integration for `http_request` tool
- LLM task tool for structured JSON-only sub-calls
- Cross-channel poll creation tool
- Firecrawl fallback for JS-heavy sites
- Camera/screen/location node tool capabilities
- Proxy support for `web_search_tool`
- Wire session tools to composite backend for gateway visibility
- Skill self-improvement and pipeline tool
- `TEST.sh` validation/testing framework
- GitHub PR review skill for autonomous PR triage

#### Desktop & Installation

- Desktop companion app with device-aware installer and CI/CD
- macOS desktop menu bar app (Tauri)
- Windows setup batch file and guide
- TUI onboarding wizard (ratatui-based)
- Heartbeat enabled by default
- Browser tools enabled by default with auto-approve

#### Configuration & CLI

- `zeroclaw config reload` for hot-reloading config
- Configurable derive macro and zeroclaw props CLI
- MQTT channel configuration schema
- MultiMessage streaming mode and Matrix streaming config
- `provider_env` for injecting API keys from config
- Configurable context size for Ollama via `ZEROCLAW_OLLAMA_NUM_CTX`
- `--log-llm` flag to dump LLM provider message payloads
- Shell tool timeout configurable via `config.toml`
- Service logs command for systemd/launchd/Windows
- Streaming output and Ctrl+C cancellation to agent REPL
- Detect missing `loginctl linger` and prompt user

#### Integrations & Ecosystem

- ACP (Agent Communication Protocol) server mode over stdio
- SearXNG as a search provider
- Tavily as web search provider option
- GitHub Copilot added to onboarding
- AGENTS.md adopted as primary agent instruction format
- Movie extension with Douban and TMDB support
- `allowed_private_hosts` config for SSRF bypass
- Marketplace templates for Coolify, Dokploy, and EasyPanel

#### Internationalization

- All 31 languages added to web dashboard
- Tool descriptions translated for all 31 README languages
- Locale storage functionality

#### CI/CD & Observability

- GitHub deployment environments for release tracking
- Discord release announcements
- Per-component path labels and labeler workflow
- DORA metrics for deployment tracking
- Cron job delivery for Feishu/Lark channel
- Calendar-driven no-show detection triggers
- `message_sent` hook fired after successful channel delivery

#### Firmware

- Shared protocol crate, Pico Rust rewrite, Nucleo refactor

### Bug Fixes

Over **200 bug fixes** across the 0.6.x series, including:

- Streaming reliability improvements across all provider backends
- Context overflow and history pruning edge cases resolved
- Sandbox and security policy corrections for macOS Seatbelt and Bubblewrap
- Docker environment fixes (pairing code, workspace resolution, build targets)
- Cross-platform fixes for Windows (ACL, path sync, npm shims) and macOS (path canonicalization)
- WhatsApp personal mode detection and group message handling
- Matrix E2EE OTK conflict retry loop resolved
- Slack message splitting for API block limits
- Draft streaming hangs after tool loop completion
- Memory context injection closing tag fixes
- Cost tracking enabled by default with proper configuration
- WebSocket reconnection and session replay
- Provider fallback and non-streaming error handling
- Clippy lint and compilation error resolutions across the codebase
- Release pipeline hardening (checksum verification, arch matching, binary size limits)

### Contributors

Thank you to all **73 contributors** who made this release possible:

- **0668000448**
- **Abdul Sadath**
- **Adi Susilayasa** (@adisusilayasa)
- **aecs4u**
- **Aleksandr Prilipko** (@zverozabr)
- **Alix-007**
- **Argenis de la Rosa** (@theonlyhennygod)
- **awol2005ex**
- **B Kevin Anderson**
- **bennyzen**
- **Burnww** (@BurnWW)
- **ChenBo**
- **Cherilyn Buren** (@NiuBlibing)
- **Christian Pojoni** (@5queezer)
- **Dan Gilles**
- **Darren.Zeng**
- **Dong Shin**
- **Drew Lipiecki**
- **Eddie's AI Agent**
- **Eds Nody** (@RainbowXie)
- **ERROR404**
- **Fausto**
- **Giulio V**
- **gorlf**
- **guangmangbeijing**
- **Henrik Akselsen**
- **HoWon** (@hwc9169)
- **JamesYin**
- **Joe Hoyle**
- **JordanTheJet**
- **Keith_void**
- **khhjoe**
- **LaoChen** (@cftfc)
- **lif**
- **linyibin**
- **Loc Nguyen Huu**
- **m-tky**
- **Marcelo Correa**
- **Markus Bergholz**
- **Martin**
- **Matteo Pietro Dazzi**
- **Michael Lohr**
- **MZI13** (@mehmet-zahid)
- **Nero** (@www13059690)
- **Nim G** (@theredspoon)
- **Nisit Sirimarnkit**
- **oreofishcn**
- **Rafael Xavier de Souza**
- **rareba**
- **Richard Piacentini**
- **Roman Tataurov**
- **rrodri**
- **Ruikai Liu**
- **Ryan Tregea**
- **Shane Engelman**
- **shedwards**
- **simianastronaut** (@SimianAstronaut7)
- **smallwhite**
- **Tavily-Integrations**
- **Tenith Hasintha** (@Tenith01)
- **TJUEZ**
- **Tomas Migone**
- **Tyler Jennings**
- **Vast-stars**
- **Vlad** (@slayer)
- **windyboy**
- **Yijun Yu**
- **Zapan Gao**

---

[0.7.0]: https://github.com/zeroclaw-labs/zeroclaw/compare/v0.6.0...v0.7.0
