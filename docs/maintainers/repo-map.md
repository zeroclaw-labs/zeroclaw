# ZeroClaw Repository Map

ZeroClaw is a Rust-first autonomous agent runtime. It receives messages from messaging platforms, routes them through an LLM, executes tool calls, persists memory, and returns responses. It can also control hardware peripherals and run as a long-lived daemon.

## Runtime Flow

```
User message (Telegram/Discord/Slack/...)
        │
        ▼
   ┌─────────┐     ┌────────────┐
   │ Channel  │────▶│   Agent    │  (src/agent/)
   └─────────┘     │  Loop      │
                   │            │◀──── Memory Loader (loads relevant context)
                   │            │◀──── System Prompt Builder
                   │            │◀──── Query Classifier (model routing)
                   └─────┬──────┘
                         │
                         ▼
                   ┌───────────┐
                   │  Provider  │  (LLM: Anthropic, OpenAI, Gemini, etc.)
                   └─────┬─────┘
                         │
                    tool calls?
                    ┌────┴────┐
                    ▼         ▼
               ┌────────┐  text response
               │  Tools  │     │
               └────┬───┘     │
                    │         │
                    ▼         ▼
              feed results   send back
              back to LLM    via Channel
```

---

## Top-Level Layout

```
zeroclaw/
├── src/                  # Rust source (the runtime)
├── crates/robot-kit/     # Separate crate for hardware robot kit
├── tests/                # Integration/E2E tests
├── benches/              # Benchmarks (agent loop)
├── docs/contributing/extension-examples.md  # Extension examples (custom provider/channel/tool/memory)
├── firmware/             # Embedded firmware for Arduino, ESP32, Nucleo boards
├── web/                  # Web UI (Vite + TypeScript)
├── dev/                  # Local dev tooling (Docker, CI scripts, sandbox)
├── scripts/              # CI scripts, release automation, bootstrap
├── docs/                 # Documentation system (multilingual, runtime refs)
├── .github/              # CI workflows, PR templates, automation
├── playground/           # (empty, experimental scratch space)
├── Cargo.toml            # Workspace manifest
├── Dockerfile            # Container build
├── docker-compose.yml    # Service composition
├── flake.nix             # Nix dev environment
└── install.sh            # One-command setup script
```

---

## src/ — Module-by-Module

### Entrypoints

| File | Lines | Role |
|---|---|---|
| `main.rs` | 1,977 | CLI entrypoint. Clap parser, command dispatch. All `zeroclaw <subcommand>` routing lives here. |
| `lib.rs` | 436 | Module declarations, visibility (`pub` vs `pub(crate)`), CLI command enums (`ServiceCommands`, `ChannelCommands`, `SkillCommands`, etc.) shared between lib and binary. |

### Core Runtime

| Module | Key Files | Role |
|---|---|---|
| `agent/` | `agent.rs`, `loop_.rs` (5.6k), `dispatcher.rs`, `prompt.rs`, `classifier.rs`, `memory_loader.rs` | **The brain.** `AgentBuilder` composes provider+tools+memory+observer. `loop_.rs` runs the multi-turn tool-calling loop. Dispatcher handles native vs XML tool call parsing. Classifier routes queries to different models. |
| `config/` | `schema.rs` (7.6k), `mod.rs`, `traits.rs` | **All configuration structs.** Every subsystem's config lives in `schema.rs` — providers, channels, memory, security, gateway, tools, hardware, scheduling, etc. Loaded from TOML. |
| `runtime/` | `native.rs`, `docker.rs`, `wasm.rs`, `traits.rs` | **Platform adapters.** `RuntimeAdapter` trait abstracts shell access, filesystem, storage paths, memory budgets. Native = direct OS. Docker = container isolation. WASM = experimental. |

### LLM Providers

| Module | Key Files | Role |
|---|---|---|
| `providers/` | `traits.rs`, `mod.rs` (2.9k), `reliable.rs`, `router.rs`, + 11 provider files | **LLM integrations.** `Provider` trait: `chat()`, `chat_with_system()`, `capabilities()`, `convert_tools()`. Factory in `mod.rs` creates providers by name. `ReliableProvider` wraps any provider with retry/fallback chains. `RoutedProvider` routes by classifier hints. |

Providers: `anthropic`, `openai`, `openai_codex`, `openrouter`, `gemini`, `ollama`, `compatible` (OpenAI-compat), `copilot`, `bedrock`, `telnyx`, `glm`

### Messaging Channels

| Module | Key Files | Role |
|---|---|---|
| `channels/` | `traits.rs`, `mod.rs` (6.6k), + 22 channel files | **Input/output transports.** `Channel` trait: `send()`, `listen()`, `health_check()`, `start_typing()`, draft updates. Factory in `mod.rs` wires config to channel instances, manages per-sender conversation history (max 50 messages). |

Channels: `telegram` (4.6k), `discord`, `slack`, `whatsapp`, `whatsapp_web`, `matrix`, `signal`, `email_channel`, `qq`, `dingtalk`, `lark`, `imessage`, `irc`, `nostr`, `mattermost`, `nextcloud_talk`, `wati`, `mqtt`, `linq`, `clawdtalk`, `cli`

### Tools (Agent Capabilities)

| Module | Key Files | Role |
|---|---|---|
| `tools/` | `traits.rs`, `mod.rs` (635), + 38 tool files | **What the agent can do.** `Tool` trait: `name()`, `description()`, `parameters_schema()`, `execute()`. Two registries: `default_tools()` (6 essentials) and `all_tools_with_runtime()` (full set, config-gated). |

Tool categories:
- **File/Shell**: `shell`, `file_read`, `file_write`, `file_edit`, `glob_search`, `content_search`
- **Memory**: `memory_store`, `memory_recall`, `memory_forget`
- **Web**: `browser`, `browser_open`, `web_fetch`, `web_search_tool`, `http_request`
- **Scheduling**: `cron_add`, `cron_list`, `cron_remove`, `cron_update`, `cron_run`, `cron_runs`, `schedule`
- **Delegation**: `delegate` (sub-agent spawning), `composio` (OAuth integrations)
- **Hardware**: `hardware_board_info`, `hardware_memory_map`, `hardware_memory_read`
- **SOP**: `sop_execute`, `sop_advance`, `sop_approve`, `sop_list`, `sop_status`
- **Utility**: `git_operations`, `image_info`, `pdf_read`, `screenshot`, `pushover`, `model_routing_config`, `proxy_config`, `cli_discovery`, `schema`

### Memory

| Module | Key Files | Role |
|---|---|---|
| `memory/` | `traits.rs`, `backend.rs`, `mod.rs`, + 8 backend files | **Persistent knowledge.** `Memory` trait: `store()`, `recall()`, `get()`, `list()`, `forget()`, `count()`. Categories: Core, Daily, Conversation, Custom. |

Backends: `sqlite`, `markdown`, `lucid` (hybrid SQLite + embeddings), `qdrant` (vector DB), `none`

Supporting: `embeddings.rs` (embedding generation), `vector.rs` (vector ops), `chunker.rs` (text splitting), `hygiene.rs` (cleanup), `snapshot.rs` (backup), `response_cache.rs` (caching), `cli.rs` (CLI commands)

### Security

| Module | Key Files | Role |
|---|---|---|
| `security/` | `policy.rs` (2.3k), `secrets.rs`, `pairing.rs`, `prompt_guard.rs`, `leak_detector.rs`, `audit.rs`, `otp.rs`, `estop.rs`, `domain_matcher.rs`, + 4 sandbox files | **Policy engine and enforcement.** `SecurityPolicy`: autonomy levels (ReadOnly/Supervised/Full), workspace confinement, command allowlists, forbidden paths, rate limits, cost caps. |

Sandboxing: `bubblewrap.rs`, `firejail.rs`, `landlock.rs`, `docker.rs`, `detect.rs` (auto-detect best available)

### Gateway (HTTP API)

| Module | Key Files | Role |
|---|---|---|
| `gateway/` | `mod.rs` (2.8k), `api.rs` (1.4k), `sse.rs`, `ws.rs`, `static_files.rs` | **Axum HTTP server.** Webhook receivers (WhatsApp, WATI, Linq, Nextcloud Talk), REST API, SSE streaming, WebSocket support. Rate limiting, idempotency keys, 64KB body limit, 30s timeout. |

### Hardware & Peripherals

| Module | Key Files | Role |
|---|---|---|
| `peripherals/` | `traits.rs`, `mod.rs`, `serial.rs`, `rpi.rs`, `arduino_flash.rs`, `uno_q_bridge.rs`, `uno_q_setup.rs`, `nucleo_flash.rs`, `capabilities_tool.rs` | **Hardware board abstraction.** `Peripheral` trait: `connect()`, `disconnect()`, `health_check()`, `tools()`. Each peripheral exposes its capabilities as Tools the agent can call. |
| `hardware/` | `discover.rs`, `introspect.rs`, `registry.rs`, `mod.rs` | **USB discovery and board identification.** Scans VID/PID, matches known boards, introspects connected devices. |

### Observability

| Module | Key Files | Role |
|---|---|---|
| `observability/` | `traits.rs`, `mod.rs`, `log.rs`, `prometheus.rs`, `otel.rs`, `verbose.rs`, `noop.rs`, `multi.rs`, `runtime_trace.rs` | **Metrics and tracing.** `Observer` trait: `log_event()`. Composite observer (`multi.rs`) fans out to multiple backends. |

### Skills & SkillForge

| Module | Key Files | Role |
|---|---|---|
| `skills/` | `mod.rs` (1.5k), `audit.rs` | **User/community-authored capabilities.** Loaded from `~/.zeroclaw/workspace/skills/<name>/SKILL.md`. CLI: list, install, audit, remove. Optional community sync from open-skills repo. |
| `skillforge/` | `scout.rs`, `evaluate.rs`, `integrate.rs`, `mod.rs` | **Skill discovery and evaluation.** Scouts for skills, evaluates quality/fitness, integrates into the runtime. |

### SOP (Standard Operating Procedures)

| Module | Key Files | Role |
|---|---|---|
| `sop/` | `engine.rs` (1.6k), `metrics.rs` (1.5k), `types.rs`, `dispatch.rs`, `condition.rs`, `gates.rs`, `audit.rs`, `mod.rs` | **Workflow engine.** Define multi-step procedures with conditions, gates (approval checkpoints), and metrics. Agent can execute, advance, and audit SOP runs. |

### Scheduling & Lifecycle

| Module | Key Files | Role |
|---|---|---|
| `cron/` | `scheduler.rs`, `schedule.rs`, `store.rs`, `types.rs`, `mod.rs` | **Task scheduler.** Cron expressions, one-shot timers, fixed intervals. Persistent store. |
| `heartbeat/` | `engine.rs`, `mod.rs` | **Liveness monitor.** Periodic health checks on channels/gateway. |
| `daemon/` | `mod.rs` | **Long-running daemon.** Starts gateway + channels + heartbeat + scheduler together. |
| `service/` | `mod.rs` (1.3k) | **OS service management.** Install/start/stop/restart via systemd or launchd. |
| `hooks/` | `mod.rs`, `runner.rs`, `traits.rs`, `builtin/` | **Lifecycle hooks.** Run user scripts on events (pre/post tool execution, message received, etc.). |

### Supporting Modules

| Module | Key Files | Role |
|---|---|---|
| `onboard/` | `wizard.rs` (7.2k), `mod.rs` | **First-run setup wizard.** Interactive or quick-mode onboarding: provider, API key, channels, memory backend. |
| `auth/` | `profiles.rs`, `anthropic_token.rs`, `gemini_oauth.rs`, `openai_oauth.rs`, `oauth_common.rs` | **Auth profiles and OAuth flows.** Per-provider credential management. |
| `approval/` | `mod.rs` | **Approval workflows.** Gate risky actions behind human approval. |
| `doctor/` | `mod.rs` (1.3k) | **Diagnostics.** Checks daemon health, scheduler freshness, channel connectivity. |
| `health/` | `mod.rs` | **Health check endpoints.** |
| `cost/` | `tracker.rs`, `types.rs`, `mod.rs` | **Cost tracking.** Per-session and per-day cost accounting. |
| `tunnel/` | `cloudflare.rs`, `ngrok.rs`, `tailscale.rs`, `custom.rs`, `none.rs`, `mod.rs` | **Tunnel adapters.** Expose gateway via Cloudflare, ngrok, Tailscale, or custom tunnels. |
| `rag/` | `mod.rs` | **Retrieval-augmented generation.** PDF extraction, chunking support. |
| `integrations/` | `registry.rs`, `mod.rs` | **Integration registry.** Catalog of third-party integrations. |
| `identity.rs` | (1.5k) | **Agent identity.** Name, description, persona for the agent instance. |
| `multimodal.rs` | — | **Multimodal support.** Image/vision handling config. |
| `migration.rs` | — | **Data migration.** Import from OpenClaw workspaces. |
| `util.rs` | — | **Shared utilities.** |

---

## Outside src/

| Directory | Role |
|---|---|
| `crates/robot-kit/` | Separate Rust crate for hardware robot kit functionality |
| `tests/` | Integration and E2E tests (agent loop, config persistence, channel routing, provider resolution, webhook security) |
| `benches/` | Performance benchmarks (`agent_benchmarks.rs`) |
| `docs/contributing/extension-examples.md` | Extension examples for custom providers, channels, tools, and memory backends |
| `firmware/` | Embedded firmware: `arduino/`, `esp32/`, `esp32-ui/`, `nucleo/`, `uno-q-bridge/` |
| `web/` | Web UI frontend (Vite + TypeScript) |
| `dev/` | Local development: Docker Compose, CI script (`ci.sh`), config template, sandbox configs |
| `scripts/` | CI helpers, release automation, bootstrap, contributor tier computation |
| `docs/` | Documentation system: multilingual (en/zh-CN/ja/ru/fr/vi), runtime references, operations runbooks, security proposals |
| `.github/` | CI workflows, PR templates, issue templates, automation |

---

## Dependency Direction

```
main.rs ──▶ agent/ ──▶ providers/  (LLM calls)
               │──▶ tools/      (capability execution)
               │──▶ memory/     (context persistence)
               │──▶ observability/ (event logging)
               │──▶ security/   (policy enforcement)
               │──▶ config/     (all config structs)
               │──▶ runtime/    (platform abstraction)
               │
main.rs ──▶ channels/ ──▶ agent/ (message routing)
main.rs ──▶ gateway/  ──▶ agent/ (HTTP/WS routing)
main.rs ──▶ daemon/   ──▶ gateway/ + channels/ + cron/ + heartbeat/

Concrete modules depend inward on traits/config.
Traits never import concrete implementations.
```

---

## CLI Command Tree

```
zeroclaw
├── onboard [--force] [--reinit] [--channels-only]     # First-run setup
├── agent [-m "msg"] [-p provider]        # Start agent loop
├── daemon [-p port]                      # Full runtime (gateway+channels+cron+heartbeat)
├── gateway [-p port]                     # HTTP API server only
├── channel {list|start|doctor|add|remove|bind-telegram}
├── skill {list|install|audit|remove}
├── memory {list|get|stats|clear}
├── cron {list|add|add-at|add-every|once|remove|update|pause|resume}
├── peripheral {list|add|flash|flash-nucleo|setup-uno-q}
├── hardware {discover|introspect|info}
├── service {install|start|stop|restart|status|uninstall}
├── doctor                                # Diagnostics
├── status                                # System overview
├── estop [--level] [status|resume]       # Emergency stop
├── migrate openclaw                      # Data migration
├── pair                                  # Device pairing
├── auth-profiles                         # Credential management
├── version / completions                 # Meta
└── config {show|edit|validate|reset}
```
