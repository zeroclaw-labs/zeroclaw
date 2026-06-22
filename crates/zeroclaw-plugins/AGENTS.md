# zeroclaw-plugins

## What this crate is

WASM plugin host for ZeroClaw. Handles plugin discovery, manifest parsing,
Ed25519 signature verification, and the current Extism-based WASM execution
bridge while the WIT / direct `wasmtime` host lands. Bridges plugin-exported
functions into ZeroClaw's `Tool` and `Channel` traits so plugins appear as
native capabilities to the agent runtime.

## What this crate is allowed to depend on

- `zeroclaw-api` (traits only — `Tool`, `Channel`, `ToolResult`)
- `extism` (current WASM runtime bridge)
- `wasmtime` (optional Component Model host transition)
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

- ADR-003: WASM plugin model and the Extism-to-WIT transition

## Security model

The host functions `zc_http_request` and `zc_env_read` are
**deny-by-default** for both network egress and environment-variable reads.
The plugin manifest must declare explicit allowlists
(`http_allowed_hosts`, `allow_private_hosts`, `env_read_vars`) — see
issues #5918 and #5919 and ADR-003 §"Known gaps". Empty allowlists reject
every call, regardless of the permission bit.

The host functions also perform a DNS-rebinding check: before opening any
socket, the hostname is resolved and any non-globally-routable resolved
address is rejected. This guards against a hostname that *looks* public
but resolves to a private IP.

### `url_guard` helper duplication

`src/url_guard.rs` duplicates four IP-classification helpers and one URL
parser from `crates/zeroclaw-tools/src/helpers/domain_guard.rs` and
`crates/zeroclaw-tools/src/http_request.rs`. The duplication is
intentional: the `zeroclaw-plugins` crate cannot depend on `zeroclaw-tools`
(see "What this crate is allowed to depend on" above — `domain_guard`
counts as a "specific tool" surface). The long-term move is to relocate
`domain_guard` to a shared crate (`zeroclaw-api` or a new
`zeroclaw-infra`) and have both consumers depend on it; that's a
follow-up refactor, not coupled to a security fix.

**If you change `url_guard.rs`, change the canonical copies in the same
commit.** The `parity_with_tools_helper` test in `url_guard.rs` re-runs a
representative subset of cases to detect accidental drift.
