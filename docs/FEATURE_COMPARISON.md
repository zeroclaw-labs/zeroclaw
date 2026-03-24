# Feature Comparison: OpenClaw vs ZeroClaw

> Generated 2026-03-23. **Legend:** ✅ implemented · 🟡 partial · ❌ missing · 🔄 portable from RustyClaw
>
> Sources: ZeroClaw `src/`, RustyClaw `src/` + `PARITY_PLAN.md` (rustyclaw.org), OpenClaw `src/` + `extensions/`

---

## 1. Provider Integrations

| Provider | OpenClaw | ZeroClaw | RustyClaw | Notes |
|----------|----------|----------|-----------|-------|
| Anthropic (Claude) | ✅ | ✅ | ✅ | |
| OpenAI (GPT-4/5) | ✅ | ✅ | ✅ | |
| Google Gemini | ✅ | ✅ | ✅ | |
| Ollama (local) | ✅ | ✅ | ✅ | |
| OpenRouter | ✅ | ✅ | ✅ | |
| GitHub Copilot | ✅ | ✅ | ✅ | |
| Azure OpenAI | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| AWS Bedrock | ✅ | ❌ | ❌ | OpenClaw only |
| X.AI (Grok) | ✅ | ❌ | ✅ | 🔄 Port from RustyClaw |
| Qwen | ✅ | ❌ | ❌ | |
| Minimax | ✅ | ❌ | ❌ | |
| Moonshot | ✅ | ❌ | ❌ | |
| Hugging Face | ✅ | ❌ | ❌ | |
| vLLM | ✅ | ❌ | ❌ | |
| GLM (Zhipu) | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Telnyx | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| KiloCLI | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| OpenAI Codex | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Claude Code Provider | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| OpenAI-compatible (generic) | ❌ | ✅ | ✅ | |
| Gemini CLI auth | ✅ (ext) | ✅ | ❌ | |
| Multi-provider failover | ✅ | ✅ (ReliableProvider) | ✅ | |
| Router/model-based routing | ❌ | ✅ (RouterProvider) | ✅ | |
| Cost-optimized selection | ❌ | ✅ | ✅ | 🔄 Port from RustyClaw |
| Prompt caching | ✅ | ✅ | ❌ | |
| Streaming responses | ✅ | ✅ (partial) | ✅ | |
| Vision/multimodal | ✅ | ✅ | ✅ | |
| Thinking/reasoning preservation | ❌ | ✅ | ❌ | ZeroClaw-exclusive |

**Gap summary:** ZeroClaw leads with 16+ providers vs OpenClaw's 13+. Missing: Bedrock, Qwen, Minimax, Moonshot, HuggingFace, vLLM. Grok can be ported from RustyClaw.

---

## 2. Channel Integrations

| Channel | OpenClaw | ZeroClaw | RustyClaw | Notes |
|---------|----------|----------|-----------|-------|
| Telegram | ✅ | ✅ | ✅ | |
| Discord | ✅ | ✅ | ✅ | |
| Slack | ✅ | ✅ | ✅ | |
| WhatsApp | ✅ | ✅ | ✅ | |
| Matrix | ✅ (ext) | ✅ (feature) | ✅ (feature) | |
| Signal | ✅ (ext) | ✅ | ✅ (feature) | |
| iMessage | ✅ (ext) | ✅ | ❌ | |
| Google Chat | ✅ (ext) | ❌ | ✅ | 🔄 Port from RustyClaw |
| Microsoft Teams | ✅ (ext) | ❌ | ✅ | 🔄 Port from RustyClaw |
| IRC | ✅ (ext) | ✅ | ✅ | |
| Mattermost | ✅ (ext) | ✅ | ✅ | |
| Line | ✅ (ext) | ❌ | ✅ | 🔄 Port from RustyClaw |
| Lark/Feishu | ✅ (ext) | ✅ (feature) | ✅ | |
| Nextcloud Talk | ✅ (ext) | ✅ | ❌ | |
| Nostr | ✅ (ext) | ✅ (feature) | ❌ | |
| Twitch | ✅ (ext) | ❌ | ❌ | |
| Zalo | ✅ (ext) | ❌ | ❌ | |
| Tlon | ✅ (ext) | ❌ | ❌ | |
| Email (SMTP/IMAP) | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Gmail Push | ❌ | ✅ | ✅ | |
| Reddit | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Bluesky | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Twitter/X | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| LinkedIn | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| QQ | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| DingTalk | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| WeChat Enterprise | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| MoChat | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| MQTT | ❌ | ✅ | ❌ | ZeroClaw-exclusive (IoT) |
| WATI | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| LINQ | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| ClawdTalk | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Webhook (generic) | ✅ | ✅ | ✅ | |
| CLI | ✅ | ✅ | ✅ | |
| WebChat (browser) | ✅ | ✅ (static) | ❌ | |
| XMPP | ❌ | ❌ | ✅ | 🔄 Port from RustyClaw |
| Notion | ❌ | ✅ | ❌ | ZeroClaw-exclusive |

**Gap summary:** ZeroClaw leads with 40+ channels vs OpenClaw's ~25. Missing: Google Chat, MS Teams, Line (all in RustyClaw), Twitch, Zalo, Tlon. ZeroClaw has many exclusive social/enterprise channels.

---

## 3. Tools

| Tool | OpenClaw | ZeroClaw | RustyClaw | Notes |
|------|----------|----------|-----------|-------|
| Shell/execute | ✅ | ✅ | ✅ | |
| File read/write/edit | ✅ | ✅ | ✅ | |
| Browser (CDP/Playwright) | ✅ (Playwright) | ✅ (fantoccini) | ✅ (CDP) | |
| Web fetch | ✅ | ✅ | ✅ | |
| Web search | ✅ | ✅ | ✅ | |
| Screenshot | ✅ | ✅ | ✅ | |
| Memory store/recall/forget | ✅ | ✅ | ✅ | |
| Cron add/list/remove/update | ✅ | ✅ | ✅ | |
| Git operations | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| HTTP request | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Glob/file search | ❌ | ✅ | ✅ | |
| PDF read | ❌ | ✅ | ✅ | 🔄 Port from RustyClaw |
| Image generation | ❌ | ✅ | ✅ | |
| Calculator | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Canvas | ✅ (A2UI) | ✅ | ✅ | |
| Session management | ✅ | ❌ | ✅ | 🔄 Port from RustyClaw |
| Secrets management | ❌ | ❌ | ✅ | 🔄 Port from RustyClaw |
| TTS tool | ❌ | ❌ | ✅ | 🔄 Port from RustyClaw |
| Image manipulation | ❌ | ✅ | ✅ | |
| Jira | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Notion | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| LinkedIn | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Cloud ops (AWS) | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Weather | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Pushover notifications | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| MCP tools | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| LLM task delegation | ✅ (ext) | ✅ | ❌ | |
| Swarm (multi-agent) | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Report templates | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Skill HTTP/tool | ✅ | ✅ | ✅ | |
| Approval workflows | ✅ | ✅ | ❌ | |
| Nodes (camera/screen/location) | ✅ | ❌ | ✅ | 🔄 Port from RustyClaw |
| Content search | ❌ | ✅ | ✅ | |
| Knowledge tool/RAG | ❌ | ✅ | ❌ | ZeroClaw-exclusive |

**Gap summary:** ZeroClaw has the richest tool surface (80+). Missing from OpenClaw: Git, HTTP, Jira, Notion, MCP, swarm, RAG. Session management and secrets tools can be ported from RustyClaw.

---

## 4. Memory

| Feature | OpenClaw | ZeroClaw | RustyClaw | Notes |
|---------|----------|----------|-----------|-------|
| Markdown/file-based | ✅ | ✅ | ✅ | |
| SQLite | ❌ | ✅ | ✅ | |
| PostgreSQL | ❌ | ✅ (feature) | ❌ | ZeroClaw-exclusive |
| Vector DB (LanceDB) | ✅ (ext) | ❌ | ❌ | OpenClaw-exclusive |
| Vector DB (Qdrant) | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Knowledge graph | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Importance scoring | ❌ | ✅ | ✅ | |
| Time-scoped queries | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Session-scoped memory | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Procedural memory | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Response caching | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Memory consolidation | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Memory decay | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Context compaction | ❌ | ✅ (summarize) | ✅ (4 strategies) | ZeroClaw has basic; RustyClaw has sliding-window, importance, hybrid |
| Local embeddings (fastembed) | ❌ | ✅ (vector) | ✅ | |
| OpenAI embeddings | ✅ | ✅ | ✅ | |
| Namespace isolation | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Structured memory (facts DB) | ❌ | ❌ | ✅ | 🔄 Port from RustyClaw |
| BM25 keyword search | ❌ | ❌ | ✅ | 🔄 Port from RustyClaw |

**Gap summary:** ZeroClaw has the most advanced memory system. Context compaction strategies and structured fact DB from RustyClaw are worth porting.

---

## 5. Security

| Feature | OpenClaw | ZeroClaw | RustyClaw | Notes |
|---------|----------|----------|-----------|-------|
| Autonomy levels/policy | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| IAM/RBAC | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Docker sandbox | ✅ | ✅ | ✅ | |
| Firejail sandbox | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Bubblewrap sandbox | ❌ | ✅ | ✅ | |
| Landlock LSM | ❌ | ✅ | ✅ | |
| macOS sandbox-exec | ❌ | ❌ | ✅ | 🔄 Port from RustyClaw |
| Path validation fallback | ❌ | ❌ | ✅ | 🔄 Port from RustyClaw |
| SSRF protection | ❌ | ✅ | ✅ | |
| Prompt injection guard | ❌ | ✅ | ✅ | |
| Leak detection | ❌ | ✅ | ✅ | |
| Encrypted secrets (ChaCha20) | ❌ | ✅ | ❌ | |
| Encrypted secrets (AES-256-GCM) | ❌ | ❌ | ✅ | 🔄 Port from RustyClaw |
| TOTP 2FA | ❌ | ✅ | ✅ | Already in `src/security/otp.rs` |
| WebAuthn / hardware keys | ❌ | ❌ | ✅ | 🔄 Port from RustyClaw |
| Device pairing | ✅ | ✅ | ✅ | |
| OTP validation | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Audit logger | ✅ | ✅ | ✅ | |
| E-stop (emergency shutdown) | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Cost tracking | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Workspace boundaries | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Domain matcher | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Vulnerability scanner | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| CSRF protection | ✅ | ✅ | ✅ | |
| Rate limiting | ✅ | ✅ | ✅ | |
| Nevis auth (enterprise IdP) | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Verifiable intent signing | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Redaction in logs | ✅ | ✅ | ❌ | |

**Gap summary:** ZeroClaw has the strongest security posture. TOTP 2FA, WebAuthn, and macOS sandbox from RustyClaw are worth porting.

---

## 6. Gateway / API

| Feature | OpenClaw | ZeroClaw | RustyClaw | Notes |
|---------|----------|----------|-----------|-------|
| HTTP server | ✅ | ✅ (Axum) | ✅ | |
| WebSocket | ✅ | ✅ | ✅ | |
| SSE streaming | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| REST chat API | ✅ | ✅ | ❌ | |
| Device pairing API | ✅ | ✅ | ❌ | |
| Plugin API (WASM) | ❌ | ✅ (feature) | ❌ | ZeroClaw-exclusive |
| Canvas API | ✅ | ✅ | ✅ | |
| Nodes API | ✅ | ✅ | ✅ | |
| Static file serving | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Session persistence (SQLite) | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Idempotency | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| WebChat UI | ✅ | ✅ | ❌ | |
| Config hot-reload | ✅ | ✅ | ✅ (SIGHUP) | |
| Health check | ✅ | ✅ | ✅ | |
| TLS/HTTPS | ✅ | ❌ | ✅ | 🔄 Port from RustyClaw |

**Gap summary:** Close to parity. TLS support from RustyClaw is a notable gap.

---

## 7. Observability

| Feature | OpenClaw | ZeroClaw | RustyClaw | Notes |
|---------|----------|----------|-----------|-------|
| Console/structured logging | ✅ | ✅ | ✅ | |
| Prometheus metrics | ❌ | ✅ (feature) | ✅ | |
| OpenTelemetry | ✅ (ext) | ✅ (feature) | ❌ | |
| Token usage tracking | ✅ | ✅ | ✅ | |
| Tool execution metrics | ❌ | ✅ | ✅ | |
| DORA metrics | ❌ | ❌ | ✅ | 🔄 Port from RustyClaw |
| Cost tracking | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Verbose/debug mode | ✅ | ✅ | ❌ | |

---

## 8. Hardware / Peripherals

| Feature | OpenClaw | ZeroClaw | RustyClaw | Notes |
|---------|----------|----------|-----------|-------|
| STM32 Nucleo | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Raspberry Pi GPIO | ❌ | ✅ (feature) | ❌ | ZeroClaw-exclusive |
| Arduino | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Aardvark USB (I2C/SPI) | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Serial/UART | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| GPIO node control | ❌ | ✅ | ✅ | |
| I2C/SPI node control | ❌ | ✅ | ✅ | |
| macOS native app | ✅ | ❌ | ❌ | OpenClaw-exclusive |
| iOS app | ✅ | ❌ | ❌ | OpenClaw-exclusive |
| Android app | ✅ | ❌ | ❌ | OpenClaw-exclusive |
| Camera/screen/location | ✅ | ❌ | ✅ | 🔄 Port from RustyClaw |

**Gap summary:** ZeroClaw dominates embedded hardware. OpenClaw has mobile/desktop apps that are out of scope for ZeroClaw. Camera/screen nodes from RustyClaw are portable.

---

## 9. Voice & Media

| Feature | OpenClaw | ZeroClaw | RustyClaw | Notes |
|---------|----------|----------|-----------|-------|
| TTS (text-to-speech) | ✅ | ✅ | ✅ | |
| STT (speech-to-text) | ✅ | ✅ | ✅ | |
| Wake word detection | ✅ | ✅ (feature) | ❌ | |
| Voice calls (Twilio/Telnyx) | ✅ (ext) | ❌ | ❌ | OpenClaw-exclusive |
| Push-to-talk | ✅ | ❌ | ❌ | OpenClaw-exclusive |
| Media pipeline (resize/transcode) | ✅ | ✅ | ❌ | |
| Automatic media understanding | ✅ | ✅ | ❌ | |

---

## 10. Other Systems

| Feature | OpenClaw | ZeroClaw | RustyClaw | Notes |
|---------|----------|----------|-----------|-------|
| Extension/plugin system | ✅ (37 ext) | ✅ (WASM) | ❌ | |
| Skills system | ✅ (50+) | ✅ (SkillForge) | ✅ | |
| SOP engine | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| Tunnel (ngrok/Tailscale/CF) | ✅ | ✅ | ❌ | |
| TUI (terminal UI) | ✅ | ✅ (ratatui) | ✅ (ratatui) | |
| Daemon mode | ❌ | ✅ | ✅ | |
| Doctor/diagnostics | ✅ | ✅ | ❌ | |
| Onboarding wizard | ✅ | ✅ | ❌ | |
| OpenClaw migration | ❌ | ✅ | ❌ | ZeroClaw-exclusive |
| mDNS discovery | ✅ (Bonjour) | ✅ | ✅ | |
| Heartbeat/health | ✅ | ✅ | ✅ | |
| Hooks system | ✅ | ✅ | ✅ | |
| Thinking levels | ✅ | ✅ | ❌ | |
| Context compaction | ❌ | ✅ (summarize) | ✅ (4 strategies) | ZeroClaw has basic; RustyClaw has sliding-window, importance, hybrid |
| Personality files (SOUL.md) | ❌ | ❌ | ✅ | 🔄 Port from RustyClaw |
| Service install (systemd/launchd) | ❌ | ❌ | ✅ | 🔄 Port from RustyClaw |

---

## Performance Comparison

| Metric | OpenClaw | ZeroClaw | RustyClaw |
|--------|----------|----------|-----------|
| Language | TypeScript/Node.js | Rust | Rust |
| Memory footprint | ~150 MB | <5 MB | ~15 MB |
| Startup time | ~500 ms | <50 ms | <50 ms |
| Binary size | N/A (interpreted) | Single binary | Single binary |
| Runtime deps | Node.js ≥22 + pnpm | None | None |
| Target | Server/desktop | Edge + server | Server |

---

## Summary Scorecard

| Category | OpenClaw | ZeroClaw | Winner |
|----------|----------|----------|--------|
| Providers | 13+ | 16+ | ZeroClaw |
| Channels | ~25 | 40+ | ZeroClaw |
| Tools | ~30 | 80+ | ZeroClaw |
| Memory | Basic + LanceDB | Advanced (6 backends) | ZeroClaw |
| Security | Basic | Enterprise-grade | ZeroClaw |
| Gateway | Full | Full + SSE + WASM | ZeroClaw |
| Observability | Basic + OTel | Prometheus + OTel + cost | ZeroClaw |
| Hardware | Mobile/desktop apps | Embedded (STM32/RPi/Arduino) | Tie (different focus) |
| Voice | Full (calls + wake) | Partial (TTS/STT/wake) | OpenClaw |
| Plugins | 37 extensions | WASM plugins | OpenClaw (maturity) |
| Performance | Heaviest | Lightest | ZeroClaw |

---

## Broader Ecosystem Context

> Data sourced from [rustyclaw.org PARITY_PLAN.md](../../../rustyclaw.org/PARITY_PLAN.md). The "Claw" ecosystem includes **8 implementations** with different focuses.

### Ecosystem Overview

| Implementation | Language | Target Hardware | RAM | Primary Focus |
|----------------|----------|-----------------|-----|---------------|
| **OpenClaw** | TypeScript | Mac Mini ($599+) | >1 GB | Full-featured platform |
| **ZeroClaw** | Rust | Edge + server | <5 MB | Zero-overhead, embedded-first |
| **RustyClaw** | Rust | Raspberry Pi 3B+ ($35) | ~89 MB | OpenClaw parity, security |
| **IronClaw** | Rust | Laptop/server | ~100–300 MB | Security hardening, vector search |
| **Carapace** | Rust | ARM SBCs | ~60–120 MB | Messaging-first, 10 channels |
| **Moltis** | Rust | Edge Linux | ~80–150 MB | Container deployments |
| **MicroClaw** | Rust | RPi Zero 2 | ~40–100 MB | Ultra-lightweight |
| **PicoClaw** | Go | LicheeRV-Nano ($10) | <10 MB | Ultra-minimal, RISC-V |

### Security Posture (Ranked)

| Feature | ZeroClaw | RustyClaw | IronClaw | OpenClaw | Carapace | Moltis | PicoClaw | MicroClaw |
|---------|----------|-----------|----------|----------|----------|--------|----------|-----------|
| SSRF protection | ✅ | ✅ | ✅ Enhanced | ❌ | ⚠️ Basic | ❌ | ❌ | ❌ |
| Prompt injection guard | ✅ | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| TLS/WSS gateway | ❌ | ✅ | ✅ | ✅ | ⚠️ | ❌ | ❌ | ❌ |
| TOTP 2FA | ✅ (OTP) | ✅ | ⚠️ | ✅ | ✅ | ⚠️ | ❌ | ❌ |
| WebAuthn / passkeys | ❌ | ✅ | ✅ | ❌ | ❌ | ✅ | ❌ | ❌ |
| Secrets vault | ✅ (ChaCha20) | ✅ (AES-256) | ✅ Enhanced | ✅ | ✅ | ⚠️ | ❌ | ⚠️ |
| Sandbox backends | 5 (Docker/Firejail/Bwrap/Landlock/WSB) | 1 (bwrap) | 2 (Landlock+bwrap) | Multiple | ⚠️ | ⚠️ | Workspace | ❌ |
| IAM / RBAC | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| E-stop | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |

**ZeroClaw has the broadest security surface** (IAM, 5 sandboxes, E-stop) but is missing TLS and WebAuthn — both available in RustyClaw.

### Tool Coverage (Ranked)

| Implementation | Tools | Notes |
|----------------|-------|-------|
| **ZeroClaw** | 80+ | Richest tool surface, MCP, swarm, RAG |
| **OpenClaw** | 30 | Reference implementation |
| **RustyClaw** | 30/30 | 100% OpenClaw tool parity |
| **IronClaw** | ~25 | Good coverage |
| **Carapace** | ~22 | Messaging-focused |
| **Moltis** | ~18 | Moderate |
| **MicroClaw** | ~12 | Minimal set |
| **PicoClaw** | ~8 | Ultra-minimal |

### Features Unique to Related Projects (Candidates for ZeroClaw)

| Feature | Source | Portability | RustyClaw Code | ZeroClaw Scaffold | Effort | Notes |
|---------|--------|-------------|----------------|-------------------|--------|-------|
| Event-triggered routines | IronClaw | **Easy** | ✅ `src/routines/engine.rs`, `event_matcher.rs` — full EventDispatcher with regex patterns, cooldowns | ✅ Cron scheduler + SOP engine + webhooks | S–M | Wire RustyClaw's `RoutineEngine` into existing cron/SOP |
| DM pairing per channel | IronClaw | **Easy** | ✅ `src/pairing.rs` — allowlist, TTL codes, per-messenger tracking | ✅ `src/security/pairing.rs` (device-level only) | S | Extend existing pairing with per-channel user allowlists |
| Cross-channel web dashboard | MicroClaw | **Easy** | ✅ `apps/web-ui/` — 310-line PWA, WebSocket, multi-tab | ✅ `web/src/pages/Dashboard.tsx` (basic status) | S | Adapt RustyClaw's self-contained UI into existing static serving |
| Conversation replay | Moltis | **Easy** | ✅ `src/sessions.rs` — persistent history + compaction | ✅ `src/channels/session_backend.rs` (JSONL+SQLite storage) | S | Add replay API on top of existing session storage |
| Anthropic Agent Skills compat | MicroClaw | **Easy** | ⚠️ Custom SKILL.md format (~70% compatible) | ✅ SkillForge fully integrated | S | Validation layer; mostly schema alignment |
| Ed25519 plugin signing | Carapace | **Moderate** | ⚠️ Ed25519 SSH keys in `secrets/types.rs` (not plugins) | ✅ ES256 in `verifiable_intent/crypto.rs`; WASM manifest system has no signing | M | Add signature field to plugin manifest + verify on load |
| mTLS (mutual TLS) | Carapace | **Moderate** | ⚠️ `gateway/tls.rs` (server-only, `with_no_client_auth()`) | ⚠️ Config has `mutual_tls: bool` but no impl | M | Needs Axum rustls client-cert verifier; depends on TLS port |
| PostgreSQL + pgvector | IronClaw | **Moderate** | ❌ Uses fastembed (not Postgres vectors) | ⚠️ `memory/postgres.rs` exists but avoids pgvector | M | Lower priority — Qdrant already covers vector search |
| MLS (Messaging Layer Security) | Pika | **Hard** | ❌ Not implemented | ❌ Not implemented | L | Needs `openmls` crate from scratch; niche use case |
| JSONL session persistence | Moltis | **Done** | — | ✅ `cost/tracker.rs`, `runtime_trace.rs`, `session_backend.rs` | — | Already implemented in ZeroClaw |
| 100 iteration tool-calling limit | MicroClaw | **Done** | — | ✅ Configurable | — | Already configurable in ZeroClaw |

---

## RustyClaw Port Inventory

Features already implemented in RustyClaw that can be ported to ZeroClaw with moderate effort:

| Feature | RustyClaw Source | Effort | Priority |
|---------|-----------------|--------|----------|
| X.AI Grok provider | `src/providers/` | S | Medium |
| Google Chat channel | `src/messengers/` | M | High |
| Microsoft Teams channel | `src/messengers/` | M | High |
| Line channel | `src/messengers/` | S | Low |
| XMPP channel | `src/messengers/` | S | Low |
| Session management tools | `src/tools/sessions_*.rs` | M | High |
| Secrets management tools | `src/tools/secrets_*.rs` | M | High |
| TTS tool | `src/tools/tts.rs` | S | Medium |
| PDF read tool | `src/tools/read_file.rs` | S | Medium |
| Camera/screen/location nodes | `src/tools/nodes.rs` | M | Medium |
| Context compaction strategies | `src/context_compaction/` | L | High |
| Structured memory (facts DB) | `src/structured_memory/` | L | High |
| BM25 keyword search | `src/memory/` | M | Medium |
| TOTP 2FA | `src/auth/totp.rs` | M | High |
| WebAuthn hardware keys | `src/auth/webauthn.rs` | L | Medium |
| macOS sandbox-exec | `src/sandbox/macos.rs` | S | Low |
| Path validation fallback | `src/sandbox/path_validation.rs` | S | Medium |
| TLS/HTTPS gateway | `src/gateway/` | M | High |
| DORA metrics | `src/metrics/` | S | Low |
| Service install (systemd/launchd) | `src/service/` | M | Medium |
| Personality files (SOUL.md) | `src/personality/` | S | Low |
| Cost-optimized provider selection | `src/providers/failover.rs` | M | Medium |

**Effort key:** S = small (1–2 days), M = medium (3–5 days), L = large (1–2 weeks)

---

## Development Plan

### Phase 1: Foundation Ports (Weeks 1–4)

> Goal: Close critical gaps by porting high-priority RustyClaw features that ZeroClaw lacks entirely.

**1.1 — TLS/HTTPS Gateway** (Week 1)
- Port RustyClaw's TLS support to ZeroClaw's Axum gateway
- Self-signed cert generation for dev environments
- Config schema: `[gateway.tls]` with `cert_path`, `key_path`, `auto_generate`
- Validates against: production deployments exposed to the internet

**1.2 — Session Management Tools** (Week 1–2)
- Port `sessions_list`, `sessions_spawn`, `sessions_send`, `sessions_history`
- Enables multi-agent workflows and cross-session coordination
- Integrates with existing `src/tools/` factory

**1.3 — Secrets Management Tools** (Week 2)
- Port `secrets_list`, `secrets_get`, `secrets_store` as agent-callable tools
- Layer on top of existing `src/security/secrets.rs` ChaCha20 vault
- Access policy enforcement: Always, WithApproval, WithAuth

**1.4 — Context Compaction** (Weeks 2–4)
- Port RustyClaw's 4 strategies: sliding-window, summarize, importance-based, hybrid
- Critical for long-running conversations and edge devices with limited context
- Integrate with existing memory and agent loop

**1.5 — TOTP 2FA** (Week 3)
- Port TOTP validator with recovery codes
- Integrates with pairing and gateway auth flows
- Config: `[security.totp]` section

**1.6 — Structured Memory (Facts DB)** (Weeks 3–4)
- Port confidence-scored fact extraction from conversations
- Deduplication, access tracking, LRU pruning
- Complement existing memory backends, not replace

### Phase 2: Channel Parity (Weeks 5–8)

> Goal: Add enterprise channels that OpenClaw has and ZeroClaw lacks.

**2.1 — Google Chat Channel** (Week 5)
- Port from RustyClaw's messenger implementation
- Service account authentication
- Space/thread support

**2.2 — Microsoft Teams Channel** (Weeks 5–6)
- Port from RustyClaw
- Bot Framework adapter pattern
- Adaptive card support for rich messages

**2.3 — Line Channel** (Week 7)
- Port from RustyClaw
- Messaging API with webhook verification

**2.4 — XMPP Channel** (Week 7)
- Port from RustyClaw
- Jabber-compatible messaging

**2.5 — Twitch Channel** (Week 8)
- New implementation (not in RustyClaw)
- IRC-based with Twitch extensions
- Chat bot and command handling

### Phase 3: Intelligence & Tools (Weeks 9–12)

> Goal: Add advanced capabilities that differentiate ZeroClaw.

**3.1 — BM25 Keyword Search** (Week 9)
- Port from RustyClaw to complement vector search
- Hybrid retrieval: BM25 + vector embeddings
- Useful for exact-match queries

**3.2 — TTS Tool** (Week 9)
- Port from RustyClaw as agent-callable tool
- Provider selection: OpenAI, ElevenLabs, Edge
- Voice configuration per channel

**3.3 — PDF Read Tool Enhancement** (Week 10)
- Port RustyClaw's multi-format support (docx, rtf, odt)
- Integrate with RAG pipeline

**3.4 — Camera/Screen/Location Nodes** (Weeks 10–11)
- Port from RustyClaw
- Enable mobile/desktop node capabilities
- Approval gating for sensitive operations

**3.5 — Grok Provider** (Week 11)
- Port X.AI Grok from RustyClaw
- Model support: grok-3, grok-3-mini

**3.6 — Cost-Optimized Provider Routing** (Week 12)
- Port from RustyClaw's failover module
- Strategy: select cheapest provider that meets capability requirements
- Integrate with existing RouterProvider

### Phase 4: Security Hardening (Weeks 13–16)

> Goal: Achieve best-in-class security across all deployment targets.

**4.1 — WebAuthn / Hardware Key Support** (Weeks 13–14)
- Port from RustyClaw
- YubiKey and FIDO2 support
- Gateway authentication flow integration

**4.2 — macOS Sandbox** (Week 14)
- Port sandbox-exec profiles from RustyClaw
- Seatbelt policy generation
- Add to sandbox auto-detection chain

**4.3 — Path Validation Fallback** (Week 15)
- Port software-only sandbox for platforms without kernel support
- Allowlist-based filesystem access control
- Useful for minimal/container environments

**4.4 — Service Installation** (Weeks 15–16)
- Port systemd unit generation (Linux)
- Port launchd plist generation (macOS)
- `zeroclaw service install/uninstall/status/logs` commands

### Phase 5: Observability & Polish (Weeks 17–20)

> Goal: Production readiness and operational maturity.

**5.1 — DORA Metrics** (Week 17)
- Port from RustyClaw
- Deployment frequency, lead time, failure rate, MTTR
- Feed into existing Prometheus exporter

**5.2 — Personality System** (Week 17)
- Port SOUL.md / IDENTITY.md / USER.md convention
- Agent personality customization without config changes
- Load from workspace root

**5.3 — Voice Calls** (Weeks 18–19)
- New implementation (OpenClaw has this, RustyClaw does not)
- Twilio/Telnyx/Plivo providers
- Real-time STT/TTS streaming
- Largest effort — may defer to Phase 6

**5.4 — Comprehensive Testing & Documentation** (Week 20)
- Integration tests for all ported features
- Update docs: setup guides, reference, ops
- Migration guide for RustyClaw users

---

## Milestone Targets

| Milestone | Target Date | Key Deliverables |
|-----------|-------------|------------------|
| **M1: Foundation** | Week 4 | TLS, sessions, secrets, compaction, TOTP, facts DB |
| **M2: Channels** | Week 8 | Google Chat, Teams, Line, XMPP, Twitch |
| **M3: Intelligence** | Week 12 | BM25, TTS, PDF+, nodes, Grok, cost routing |
| **M4: Security** | Week 16 | WebAuthn, macOS sandbox, path validation, service install |
| **M5: Production** | Week 20 | DORA, personality, voice calls, full test coverage |
| **M6: Ecosystem** | Week 24 | Event triggers, DM pairing, web dashboard, Skills compat, replay API, plugin signing, mTLS |

## Principles

1. **Port before build** — always check RustyClaw first; adapt existing Rust code rather than writing from scratch
2. **One concern per PR** — each port is a separate PR with its own tests
3. **Feature-gate heavy deps** — optional Cargo features for TLS, WebAuthn, voice, etc.
4. **Maintain <5 MB baseline** — ported features must not bloat the default binary
5. **Test at the boundary** — integration tests for every ported module, not just unit tests

## Phase 6 (Stretch): Ecosystem Differentiation (Weeks 21–24)

> Goal: Incorporate best ideas from the broader ecosystem (IronClaw, Carapace, Moltis, MicroClaw).
> Ordered by portability — easy ports first to maximize early value.

**6.1 — Event-Triggered Automation** (Week 21) — Easy port
- From IronClaw via RustyClaw: `src/routines/engine.rs` + `event_matcher.rs`
- RustyClaw has a complete `RoutineEngine` with `EventDispatcher`, regex patterns, cooldowns
- Wire into ZeroClaw's existing cron scheduler + SOP engine
- Enables: webhook → action, state change → action, timer → action

**6.2 — DM Pairing per Channel** (Week 21) — Easy port
- From IronClaw via RustyClaw: `src/pairing.rs` — per-messenger allowlists with TTL codes
- Extend ZeroClaw's existing `src/security/pairing.rs` (currently device-level only)
- Adds per-channel user allowlists and approval flows

**6.3 — Cross-Channel Web Dashboard** (Week 22) — Easy port
- From MicroClaw via RustyClaw: `apps/web-ui/` — self-contained 310-line PWA
- Adapt into ZeroClaw's existing `web/src/pages/Dashboard.tsx` + static file serving
- Multi-tab: Chat, Sessions, Settings with WebSocket real-time updates

**6.4 — Anthropic Agent Skills Format** (Week 22) — Easy port
- From MicroClaw: ensure compatibility with official Anthropic spec
- ZeroClaw's SkillForge already integrated; needs schema validation layer
- Mostly alignment, not reimplementation

**6.5 — Conversation Replay API** (Week 22) — Easy port
- From Moltis via RustyClaw: replay from persistent session history
- ZeroClaw's `src/channels/session_backend.rs` already stores JSONL+SQLite
- Add `GET /api/sessions/{id}/replay` endpoint + state restoration

**6.6 — Ed25519 Plugin Signature Verification** (Week 23) — Moderate
- From Carapace: cryptographically signed WASM plugins
- ZeroClaw has `ring` crypto (ES256) in `verifiable_intent/crypto.rs`
- Add Ed25519 signature field to plugin manifest + verify on load

**6.7 — mTLS Gateway** (Weeks 23–24) — Moderate
- From Carapace: mutual TLS for high-security deployments
- ZeroClaw config already declares `mutual_tls: bool` — needs implementation
- Client certificate verification via Axum `rustls` layer, on top of Phase 1 TLS
- Config: `[gateway.tls.client_auth]`

---

## Strategic Position

After completing all phases, ZeroClaw will be:

| Dimension | Position |
|-----------|----------|
| **Security** | Best-in-class (only impl with IAM + 5 sandboxes + E-stop + SSRF + prompt guard + TOTP + WebAuthn + TLS + mTLS) |
| **Tools** | Most extensive (80+ tools, MCP, swarm, RAG — 2.5× OpenClaw) |
| **Channels** | Most channels (40+ including social, enterprise, IoT) |
| **Performance** | Lightest (<5 MB RAM, <50 ms startup — 200× lighter than OpenClaw) |
| **Hardware** | Only impl with embedded peripheral support (STM32, RPi GPIO, Arduino) |
| **Memory** | Most advanced (6 backends, knowledge graph, decay, consolidation, compaction) |
| **Ecosystem compat** | OpenClaw migration path + RustyClaw feature absorption |
