---
id: ADR-003
title: WASM plugins use Extism as the initial execution bridge
date: 2026-03-15
status: superseded-by-ADR-009
relates-to:
  - ADR-009
  - crates/zeroclaw-plugins
  - crates/zeroclaw-api
---

# ADR-003: WASM Plugins Use Extism As The Initial Execution Bridge

This is a restored retroactive record. The original ADR was added under
`docs/architecture/decisions/adr-003-wasm-extism-plugin-model.md` and
was dropped during the mdBook migration. It records the historical
Extism-based plugin bridge that was accepted on 2026-03-15. The current
WIT and direct `wasmtime` model supersedes it in
[ADR-009](./ADR-009-wit-wasmtime-plugin-execution.md).

## Context

ZeroClaw compiled many tools and channels into a single binary. Every
user paid the compile time and binary size for capabilities they might
never use. Third-party developers could not extend ZeroClaw without
forking the repository and writing Rust code against internal APIs.

The Intentional Architecture RFC defined a microkernel target where
non-core tools and channels become loadable plugins. That required a
sandboxed execution model that:

1. Runs untrusted code without compromising the host process.
2. Works on Linux, macOS, Windows, ARM, and x86_64 targets.
3. Supports capability-based permissions such as HTTP access,
   environment-variable reads, and file I/O.
4. Allows plugins to be written in any language that compiles to WASM.
5. Adds minimal binary size when the feature is unused.

The original evaluation considered three WASM runtime options:

| Runtime | Pros | Cons |
| --- | --- | --- |
| Extism | High-level SDK, built-in host function system, PDKs for several guest languages, active maintenance | Adds binary size behind the feature flag |
| Raw wasmtime | Maximum control, mature runtime | Requires building the ABI, memory protocol, and host function system directly |
| Wasmer | LLVM and Cranelift backends | Smaller ecosystem and less Rust-native host-function ergonomics |

## Decision

ZeroClaw would use Extism 1.x as the initial WASM plugin runtime behind
the `plugins-wasm` feature flag.

Plugins were WASM modules exporting two JSON functions:

- `tool_metadata(String) -> String`, returning JSON with `name`,
  `description`, and `parameters_schema` fields.
- `execute(String) -> String`, receiving tool arguments as JSON and
  returning a JSON result with `success`, `output`, and optional `error`
  fields.

The runtime provided two permission-gated host functions:

- `zc_http_request(String) -> String`, gated on
  `PluginPermission::HttpClient`.
- `zc_env_read(String) -> String`, gated on
  `PluginPermission::EnvRead`.

Extism's built-in HTTP support was deliberately not used because it
would bypass ZeroClaw's permission enforcement.

Each plugin shipped a `manifest.toml` alongside its `.wasm` file. The
manifest declared name, version, capabilities such as `tool`, `channel`,
`memory`, and `observer`, and required permissions such as
`http_client`, `env_read`, `file_read`, `file_write`, `memory_read`, and
`memory_write`.

Plugin manifests supported optional Ed25519 signatures with three
enforcement modes: `disabled`, `permissive`, and `strict`.

Plugin authors depended on `extism-pdk` and compiled to
`wasm32-wasip1`. The protocol used documented JSON contracts rather than
a ZeroClaw-specific guest SDK crate.

## Consequences

Positive:

- WASM linear memory isolation prevented plugins from accessing host
  memory.
- Permission-gated host functions gave the initial bridge a clear
  capability model.
- WASM modules could run on any platform that the underlying runtime
  supported.
- Languages with `wasm32-wasip1` targets could produce plugins.
- Users who disabled `plugins-wasm` did not pay the binary-size or
  compile-time cost.

Negative:

- Extism added binary size behind the feature flag.
- Plugin authors depended on `extism-pdk`, an external SDK.
- The initial bridge made tool plugins functional before channel plugins.
- Extism calls were synchronous while ZeroClaw's `Tool` trait was async,
  so calls used blocking-task bridging.

Known gaps:

- `zc_http_request` could forward plugin-supplied URLs without the same
  private-IP, loopback, and link-local restrictions used by native HTTP
  tools.
- `env_read` granted access to any variable by name instead of a
  per-plugin allowlist.
- CPU execution did not have a fuel limit or epoch interruption.

Those gaps meant the permission model was initially a documented
contract, not yet a hardened boundary for plugins from untrusted
authors.

## References

- [ADR-009: WIT components and direct wasmtime plugin execution](./ADR-009-wit-wasmtime-plugin-execution.md)
- Historical source path:
  `docs/architecture/decisions/adr-003-wasm-extism-plugin-model.md`
- Historical implementation paths:
  `crates/zeroclaw-plugins/src/runtime.rs`,
  `crates/zeroclaw-plugins/src/wasm_tool.rs`,
  `crates/zeroclaw-plugins/src/host.rs`, and
  `crates/zeroclaw-plugins/src/signature.rs`
- Tracking issues from the original ADR:
  [#5918](https://github.com/zeroclaw-labs/zeroclaw/issues/5918) and
  [#5919](https://github.com/zeroclaw-labs/zeroclaw/issues/5919)
