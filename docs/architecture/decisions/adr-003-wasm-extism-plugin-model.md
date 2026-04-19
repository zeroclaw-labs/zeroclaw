---
type: adr
status: accepted
last-reviewed: 2026-04-19
relates-to:
  - crates/zeroclaw-plugins
  - crates/zeroclaw-api
---

# ADR-003: WASM + Extism Plugin Execution Model

**Status:** Accepted

**Date:** 2026-03-15

**Note:** This retroactively records a decision made prior to the formal ADR
process. The date reflects when the decision was made, not when this record
was written.

## Context

ZeroClaw compiles 70+ tools and 30+ channels into a single monolithic binary.
Every user pays the compile time and binary size for capabilities they may never
use. Third-party developers cannot extend ZeroClaw without forking the
repository and writing Rust code against internal APIs.

The [Intentional Architecture RFC](https://github.com/zeroclaw-labs/zeroclaw/wiki/14.1-Intentional-Architecture)
defines a microkernel target where non-core tools and channels become loadable
plugins. This requires a sandboxed execution model that:

1. Runs untrusted code without compromising the host process.
2. Works on all targets (Linux, macOS, Windows, ARM, x86_64).
3. Supports capability-based permissions (HTTP access, env var reads, file I/O).
4. Allows plugins to be written in any language that compiles to WASM.
5. Adds minimal binary size when the feature is unused.

Three WASM runtime options were evaluated:

| Runtime | Pros | Cons |
|---------|------|------|
| **Extism** (wraps wasmtime) | High-level SDK, built-in host function system, PDK for Rust/Go/C/JS, active maintenance | Adds ~20-30 MB behind feature flag |
| **wasmtime** (raw) | Maximum control, mature | Requires building ABI, memory protocol, and host function system from scratch |
| **wasmer** | LLVM and Cranelift backends | Smaller ecosystem, less Rust-native host function ergonomics |

## Decision

We will use **Extism 1.x** as the WASM plugin runtime, accessed through the
`plugins-wasm` feature flag.

### Plugin protocol

Plugins are WASM modules that export two functions:

- `tool_metadata(String) -> String` — returns JSON with `name`, `description`,
  and `parameters_schema` fields.
- `execute(String) -> String` — receives tool arguments as JSON, returns a JSON
  result with `success`, `output`, and optional `error` fields.

### Host functions

The runtime provides two permission-gated host functions:

- `zc_http_request(String) -> String` — makes HTTP requests on behalf of the
  plugin. Gated on `PluginPermission::HttpClient`.
- `zc_env_read(String) -> String` — reads environment variables. Gated on
  `PluginPermission::EnvRead`.

Extism's built-in HTTP support (`extism_http_request`) is deliberately not used
because it bypasses permission enforcement.

### Plugin manifest

Each plugin ships a `manifest.toml` alongside its `.wasm` file declaring name,
version, capabilities (`tool`, `channel`, `memory`, `observer`), and required
permissions (`http_client`, `env_read`, `file_read`, `file_write`,
`memory_read`, `memory_write`).

### Signature verification

Plugin manifests support optional Ed25519 signatures with three enforcement
modes: `disabled` (default), `permissive` (warn), and `strict` (reject
unsigned). Signatures use the `ring` crate.

### Plugin authoring

Plugin authors depend on `extism-pdk` (the Extism project's guest SDK) and
compile to `wasm32-wasip1`. No ZeroClaw-specific SDK crate is required — the
protocol is documented and the JSON contracts are simple enough to implement
directly.

## Consequences

### Positive

- **Sandboxing:** WASM linear memory isolation prevents plugins from accessing
  host memory. Permission-gated host functions enforce capability boundaries.
- **Portability:** WASM modules run on any platform wasmtime supports.
- **Language freedom:** Any language with a `wasm32-wasip1` target can produce
  plugins (Rust, Go, C, AssemblyScript, Zig).
- **Feature-gated cost:** Users who disable `plugins-wasm` pay zero binary size
  or compile time overhead.
- **Ecosystem:** Extism's existing PDK ecosystem reduces plugin authoring
  friction.

### Negative

- **Binary size:** The `extism` crate (wrapping wasmtime) adds ~20-30 MB to the
  binary when the `plugins-wasm` feature is enabled.
- **External dependency:** Plugin authors must depend on `extism-pdk`, a
  third-party crate outside our control.
- **Incomplete:** The channel plugin bridge (`wasm_channel.rs`) is not yet
  connected to the Extism runtime — only tool plugins are functional.
  Channel plugins are Phase 3 (v0.9.0) per the Intentional Architecture RFC.
- **Sync bridging:** Extism plugin calls are synchronous; the `Tool` trait is
  async. Each call uses `tokio::task::spawn_blocking`, creating a fresh plugin
  instance per invocation since Extism `Plugin` is `!Send`.

### Neutral

- Plugin discovery uses the existing `~/.zeroclaw/plugins/` directory
  convention. No registry server is required yet (registry is a Phase 4
  deliverable).

### Known gaps (tracked follow-ups)

The host-function surface is intentionally minimal in this initial version.
Three gaps are acknowledged and have tracking issues filed; they do not block
the v1 bridge but must be closed before plugins can be treated as a
lower-trust surface than native tools:

- **SSRF in `zc_http_request`** — the host function forwards any plugin-supplied
  URL to `reqwest` without private-IP, loopback, or link-local restriction. A
  plugin granted `http_client` can reach cloud IMDS endpoints, local admin
  services, or the ZeroClaw gateway itself. Tracked in [#5918](https://github.com/zeroclaw-labs/zeroclaw/issues/5918).
  Native `HttpRequestTool::is_private_or_local_host()` is the reuse target.
- **Unbounded `zc_env_read`** — `env_read` permission grants access to *any*
  variable by name, including unrelated secrets (`AWS_SECRET_ACCESS_KEY`,
  `ANTHROPIC_API_KEY`, etc.). Tracked in [#5919](https://github.com/zeroclaw-labs/zeroclaw/issues/5919).
  Preferred fix: per-plugin manifest allowlist (`env_read_vars = [...]`).
- **CPU exhaustion** — no fuel limit or epoch interruption on Extism plugins.
  An adversarial plugin can loop indefinitely and hold a blocking-pool thread
  until the 120s HTTP timeout (if any request is outstanding) or forever (if
  no host call is in flight). Out of scope for D2; to be addressed when
  plugins are exposed to untrusted authors.

The first two are the operational prerequisites for accepting plugins from
third-party sources. Until they land, the permission model is a documentation
contract, not a hardened boundary — operators should only install plugins
from sources they already trust at the manifest level.

## References

- `crates/zeroclaw-plugins/src/runtime.rs` — Extism execution bridge
- `crates/zeroclaw-plugins/src/wasm_tool.rs` — Tool trait bridge
- `crates/zeroclaw-plugins/src/host.rs` — Plugin discovery and manifest loading
- `crates/zeroclaw-plugins/src/signature.rs` — Ed25519 verification
- `crates/zeroclaw-config/src/schema.rs` — `PluginsConfig`, `ImageGenConfig`
- `crates/zeroclaw-runtime/src/tools/mod.rs` — Plugin tool registration
- `plugins/image-gen-fal/` — Reference plugin implementation (lands in a follow-up PR)
- [Extism documentation](https://extism.org/docs/overview)
- [Intentional Architecture RFC](https://github.com/zeroclaw-labs/zeroclaw/wiki/14.1-Intentional-Architecture)
