# zeroclaw-api

Trait definitions and shared types for ZeroClaw — the API layer.

This crate defines the fundamental abstractions that all ZeroClaw subsystems
depend on. No implementations, no heavy dependencies. Every other crate in the
workspace depends on this. The compiler enforces that no implementation crate
can import another without going through these interfaces.

## Traits

| Trait | Module | Purpose |
|-------|--------|---------|
| `ModelProvider` | `model_provider` | LLM inference backends |
| `Channel` | `channel` | Messaging platform integrations |
| `Tool` | `tool` | Agent-callable capabilities |
| `Memory` | `memory_traits` | Conversation memory backends |
| `Observer` | `observability_traits` | Metrics and tracing |
| `RuntimeAdapter` | `runtime_traits` | Execution environment adapters |
| `Peripheral` | `peripherals_traits` | Hardware board integrations (STM32, RPi GPIO) |

## Key Modules

- **`agent`** — agent configuration and identity types
- **`attribution`** — alias-bound attribution for log events (channel, agent, session)
- **`hook`** — lifecycle hooks for channel and agent events
- **`jsonrpc`** — JSON-RPC message types for gateway communication
- **`media`** — media attachment types for channel messages
- **`schema`** — shared schema types
- **`session_keys`** — session key construction and parsing
- **`vad`** — voice activity detection types

## Layering

`zeroclaw-api` is the bottom of the dependency graph. It intentionally does NOT
depend on `zeroclaw-log` or `zeroclaw-spawn` — its small handful of internal
spawn needs go through the workspace-wide `disallowed_methods` exemption
documented in `clippy.toml`. This keeps the API surface free of runtime
dependencies.

## Stability

Experimental — targeting stable at v1.0.0 (formal milestone).
