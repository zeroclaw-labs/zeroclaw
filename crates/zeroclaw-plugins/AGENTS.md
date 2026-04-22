# zeroclaw-plugins

## What this crate is

WASM plugin host for ZeroClaw. Handles plugin discovery, manifest parsing,
Ed25519 signature verification, and Extism-based WASM execution. Bridges
plugin-exported functions into ZeroClaw's `Tool` and `Channel` traits so
plugins appear as native capabilities to the agent runtime.

## What this crate is allowed to depend on

- `zeroclaw-api` (traits only — `Tool`, `Channel`, `ToolResult`)
- `extism` (WASM runtime)
- `reqwest` (blocking, for host function HTTP support)
- `ring` (Ed25519 signatures)
- `serde`, `serde_json`, `toml` (serialization)
- `tokio` (async bridging via `spawn_blocking`)
- `tracing` (logging)
- `anyhow`, `thiserror` (error handling)

Do not add dependencies on specific tools, channels, providers, or config
schemas. This crate knows how to run plugins, not what they do.

## Extension points

- **New host functions:** Add to `runtime.rs` alongside `zc_http_request` and
  `zc_env_read`. Register in `create_plugin()`. Gate on a `PluginPermission`
  variant (add to `lib.rs` if needed).
- **New capability bridges:** Add alongside `wasm_tool.rs` and
  `wasm_channel.rs` (e.g., `wasm_memory.rs` for memory backend plugins).
- **New permissions:** Add variants to `PluginPermission` in `lib.rs`.

## What does NOT belong here

- Concrete tool or channel implementations (those go in `zeroclaw-tools` or
  `zeroclaw-channels`, or in a WASM plugin)
- Plugin business logic (that belongs in the plugin's own crate)
- Config schema definitions (those go in `zeroclaw-config`)
- Plugin registry client or distribution (future separate crate)

## Related ADRs

- ADR-003: WASM + Extism plugin execution model
