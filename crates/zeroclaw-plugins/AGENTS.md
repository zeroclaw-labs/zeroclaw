# zeroclaw-plugins

## What this crate is

WASM plugin host for ZeroClaw. Handles plugin discovery, manifest parsing,
Ed25519 signature verification, and **wasmtime component-model (WIT) execution**
of plugins defined by `wit/v0/`. Bridges WIT tool components into ZeroClaw's
`Tool` trait so plugins appear as native capabilities to the agent runtime.
Host capabilities are deny-by-default; the concrete host services (HTTP egress,
secrets, workspace) are supplied by the agent runtime, not this crate.

## What this crate is allowed to depend on

- `zeroclaw-api` (traits only — `Tool`, `Channel`, `ToolResult`)
- `wasmtime` + `wasmtime-wasi` (component-model runtime; gated on `plugins-wasmtime`)
- `reqwest` (blocking — used by the recording HTTP fixture; production egress
  lives in `zeroclaw-runtime`)
- `ring` (Ed25519 signatures)
- `serde`, `serde_json`, `toml` (serialization)
- `tokio` (async bridging via `spawn_blocking`)
- `anyhow`, `thiserror` (error handling)

Do not add dependencies on specific tools, channels, providers, or config
schemas. This crate knows how to run plugins, not what they do — concrete host
capability implementations belong in `zeroclaw-runtime`.

## Extension points

- **New host capability:** add a function to the `host` interface in
  `wit/v0/host.wit` (run the **wit-breaking-change-check** skill), implement it
  on `StoreData` in `store.rs`, add a capability trait + deny default in
  `wit_host.rs`, and gate it on a `PluginPermission` variant (`lib.rs`). The
  concrete production impl goes in `zeroclaw-runtime`'s `tools/plugin_host.rs`.
- **New permissions:** Add variants to `PluginPermission` in `lib.rs`.

## What does NOT belong here

- Concrete tool or channel implementations (those go in `zeroclaw-tools` or
  `zeroclaw-channels`, or in a WASM plugin)
- Plugin business logic (that belongs in the plugin's own crate)
- Config schema definitions (those go in `zeroclaw-config`)
- Plugin registry client or distribution (future separate crate)

## Related

- WIT contract: `wit/v0/` (package `zeroclaw:plugin@0.1.0`)
- Protocol reference: `docs/book/src/developing/plugin-protocol.md`
- Example plugins + registry: [zeroclaw-labs/zeroclaw-plugins](https://github.com/zeroclaw-labs/zeroclaw-plugins) (examples are NOT vendored in this repo)
