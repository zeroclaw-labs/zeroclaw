# Crates

The workspace is split into layers. Edge crates talk to the outside world; core crates orchestrate; support crates provide utilities. Each crate has its own rustdoc, see [API (rustdoc)](../api.md).

## Layer: Core

### `zeroclaw-runtime`

The agent loop, security-policy enforcement, SOP engine, cron scheduler, SubAgent lifecycle, and RPC layer for zerocode. Depends on every other core and edge crate.

Notable submodules:

- `agent/`: the main request/response loop, streaming, tool-call orchestration
- `security/`: policy types, sandbox detection, OTP, emergency stop
- `sop/`: Standard Operating Procedure engine (see [SOP → Overview](../sop/index.md))
- `subagent/`: SubAgent spawning and lifecycle (see [Delegation & SubAgents](../agents/delegation.md))
- `cron/`, `daemon/`, `heartbeat/`: scheduling and long-running process management
- `skillforge/`, `skills/`: skill compilation and execution
- `service/`: systemd / launchctl / Windows Service integration
- `rpc/`: the RPC layer for zerocode

### `zeroclaw-config`

TOML schema and its validation. Handles:

- Autonomy level enum (`ReadOnly` / `Supervised` / `Full`)
- Encrypted secrets store (local key file)
- Workspace resolution (env vars, Homebrew paths, XDG, container detection)
- Schema versioning and migration

All user-facing config keys are documented in [Reference → Config](../reference/config.md), which is generated from this crate.

### `zeroclaw-api`

The kernel ABI. Defines the core public traits, including:

- `ModelProvider`: LLM client interface with streaming capability flags
- `Channel`: inbound/outbound messaging surface
- `Tool`: agent-callable capabilities
- `Memory`: conversation storage and retrieval
- `Observer`: typed metrics/observability sink

The runtime depends only on these traits, not on concrete implementations. This is what makes provider/channel/tool additions a matter of implementing a trait rather than patching the core.

## Layer: Edge

### `zeroclaw-providers`

All LLM client implementations plus the routing and retry wrappers. See [Model Providers → Overview](../providers/overview.md) for the list.

Structure:

- `traits.rs`: re-exports from `zeroclaw-api` plus provider-internal helpers
- `anthropic.rs`, `openai.rs`, `ollama.rs`, …: one file per native provider
- `compatible.rs`: a single OpenAI-compatible implementation reused by 20+ providers (Groq, Mistral, xAI, Venice, etc.)
- `router.rs`: hint-based per-call model route selection
- `reliable.rs`: same-provider retry / backoff / API-key rotation wrapper
- `streaming.rs`: SSE parsing, token estimation, tool-call deltas

### `zeroclaw-channels`

30+ messaging integrations. See [Channels → Overview](../channels/overview.md) for the catalogue.

All channels implement the `Channel` trait from `zeroclaw-api`. Each is feature-gated, a minimal build includes only the channels you compile in.

The `orchestrator/` submodule handles message streaming, draft updates, multi-message splits, and the ACP server.

### `zeroclaw-gateway`

HTTP/WebSocket gateway. Exposes the runtime over:

- REST API (sessions, memory, status, cron management)
- WebSocket for streaming responses
- Web dashboard (static assets + auth)
- Webhook endpoints (inbound from channels that push)

Pairing is required by default; `[gateway.allow_public_bind = true]` enables binding to `0.0.0.0`.

### `zeroclaw-tools`

Callable tools the agent invokes. Not to be confused with CLI `zeroclaw` subcommands.

Includes: `browser`, `http_request`, `web_search`, `shell`, `file_read`, `file_write`, hardware probes (`hardware_board_info`, `hardware_memory_read`), and more. See [Tools → Overview](../tools/overview.md).

Each tool is registered via factory and described to the model via Fluent-localised strings.

## Layer: Support

### `zeroclaw-memory`

Conversation memory and retrieval. SQLite is the default backend; PostgreSQL is available behind `--features memory-postgres` for multi-instance deployments that need a shared, concurrent-write store. Optional:

- Embedding backends (OpenAI, Ollama, local)
- Vector retrieval over stored conversations (pgvector when on PostgreSQL)
- Memory consolidation (summaries, fact extraction)

### `zeroclaw-tool-call-parser`

Model-side tool-call syntax parsing. Handles variations between providers:

- OpenAI-style `tool_calls` JSON
- Anthropic-style `<tool_use>` blocks
- Qwen/Ollama's function-call formats
- Native tool-call streaming deltas

### `zeroclaw-plugins`

Sandboxed WASM plugin host: loads component-model plugins (tool, channel, memory, skill bundles) in-process under WASI with per-call fuel and memory limits. See [Developing → Plugin protocol](../developing/plugin-protocol.md).

### `zeroclaw-hardware`

Hardware abstraction: GPIO, I2C, SPI, USB. Platform-gated. See [Hardware → Overview](../hardware/index.md).

### `zeroclaw-log`

The single emission surface for every log event in the workspace. Owns
the on-disk JSONL schema (`LogEvent`), the alias-bound attribution
registry (`ATTRIBUTION_FIELDS` + `COMPOSITE_PREFIXES`), the
`tracing-subscriber` Layer that captures every `tracing::*` call, the
`record!` and `scope!` macros, the rolling-trim writer, the
paginated cursor reader behind `/api/logs`, and the bridge to the
typed `Observer` for Prometheus / OTel consumers. See
[`architecture/logging.md`](./logging.md).

### `zeroclaw-spawn`

The sanctioned wrapper around `tokio::spawn`. Provides the `spawn!`
macro, which instruments every background task with the caller's
current attribution span so a `record!` emitted inside the spawned
future inherits the parent's `agent_alias` / `channel` / `session_key`.
Call sites use `spawn!` instead of `tokio::spawn` directly.

### `zeroclaw-infra`

Process-level support: debouncers, watchdogs, the SQLite session
backend. Not a tracing/metrics layer, that's `zeroclaw-log`. See
[Runtime state and persistence](./runtime-state-and-persistence.md) for the
state ownership and durability boundaries across config, sessions, memory,
logs, costs, cron, and gateway metadata.

### `zeroclaw-macros`

Derive macros for config schema, tool registration, and channel registration. Saves boilerplate across the workspace.

### `zerocode`

Terminal UI, built as a separate app under `apps/zerocode/`. It is its own workspace member with no `zeroclaw-*` crate dependency (see [Docs & Translations → zerocode strings](../maintainers/docs-and-translations.md) for its independent i18n catalogue).

### `aardvark-sys`, `robot-kit`

Specialised hardware support used by the `hardware` submodule. Out-of-scope unless you're bringing up specific peripherals.

## Feature flags

The microkernel roadmap (RFC #5574) defines a feature-flag taxonomy. The practical upshot for a user:

- `default`: a sensible core build
- `ci-all`: everything on, for CI
- `channel-<name>`: opt-in per channel (e.g. `channel-matrix`, `channel-discord`)
- `hardware`: enable hardware subsystem
- `gateway`, `acp-bridge`, `whatsapp-web`: opt-in capability groups

Providers are not feature-gated; they all compile in. Channel selection is the main per-build knob. Read the top-level `Cargo.toml` `[features]` table for the full list.
