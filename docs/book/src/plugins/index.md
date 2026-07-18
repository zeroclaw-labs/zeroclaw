# Plugins

ZeroClaw's plugin system lets you add capabilities to the agent without
touching the core binary. This page explains the technology decision: what a
plugin is made of, why it is WebAssembly, and how the host keeps an untrusted
component contained. The guides below it walk through building each kind of
plugin, getting more technical as you go down.

- [Writing a tool plugin](./writing-a-tool-plugin.md): a callable tool the
  model can invoke. Start here; it is the complete worked path from empty
  crate to installed tool.
- [Writing a channel plugin](./writing-a-channel-plugin.md): a messaging
  platform integration with the full capability-flag surface.
- [Writing a memory plugin](./writing-a-memory-plugin.md): a storage backend
  implementing agent-attributed recall.
- [Distributing plugins](./distributing-plugins.md): signing, registries, and
  install security.

Markdown-only [skill bundles](../tools/skill-bundles.md) are not plugins,
but they travel through the same manifest, signing, and install machinery;
that page lives with the Skills documentation.

For the operator's view of discovery, signature policy, and configuration, see
[How plugins work](../developing/how-plugins-work.md). For the normative
contract reference, see [Plugin protocol](../developing/plugin-protocol.md).

## Why WebAssembly

A plugin runs arbitrary third-party code inside a process that holds your API
keys, your conversation history, and shell access. The isolation boundary has
to be real, not advisory. ZeroClaw uses the WASI Component Model on `wasmtime`
because it gives four properties no dynamic-library or subprocess scheme
matches at once:

1. **Capability-based sandboxing.** A WebAssembly component has no ambient
   authority. It cannot open files, sockets, or environment variables unless
   the host explicitly wires that capability into its linker. ZeroClaw's host
   builds every plugin store with a WASI context that has no filesystem
   preopens and no network (`PluginState` in
   `crates/zeroclaw-plugins/src/component.rs`). What a plugin can reach is
   exactly the set of host imports its world declares plus whatever its
   manifest permissions add, and nothing else.
2. **Metered execution.** The engine is built with fuel metering enabled, and
   every call gets a fresh fuel budget. A plugin that loops forever traps; it
   cannot hang the agent. Memory, table, and instance ceilings are enforced by
   a store limiter. All four bounds come from operator config
   (`plugins.limits.*`) and are validated non-zero, and a store cannot be
   constructed without them, so no load path can produce an unsandboxed
   plugin.
3. **A typed, language-agnostic ABI.** The contract between host and plugin is
   a set of WIT interface files (`wit/v0/` in the ZeroClaw repository), not a
   Rust API. The host generates its bindings from those files with wasmtime's
   `bindgen!`; a plugin generates the mirror-image guest bindings with
   `wit-bindgen` in Rust or the equivalent tooling in any language that
   compiles to a `wasm32-wasip2` component. Records, variants, results, and
   option types cross the boundary with their types intact.
4. **Behavior identical to built-ins.** Each plugin kind is adapted onto the
   same Rust trait the first-party implementations use: a tool plugin becomes
   a `Tool` (`wasm_tool.rs`), a channel plugin a `Channel`
   (`wasm_channel.rs`), a memory plugin a `Memory` (`wasm_memory.rs`). The
   agent loop, attribution, receipts, and security policy see no difference.

## The pieces

A plugin on disk is a directory holding a manifest and a compiled component:

```text
~/.zeroclaw/plugins/
└── my-plugin/
    ├── manifest.toml     # identity, capabilities, permissions, signature
    └── my-plugin.wasm    # wasm32-wasip2 component
```

The manifest declares two orthogonal things:

- **Capabilities**: what the plugin *is*. One or more of `tool`, `channel`,
  `memory`, `observer`, `skill` (the `PluginCapability` enum in
  `crates/zeroclaw-plugins/src/lib.rs`). Each WASM capability selects the WIT
  world the component must export. The `skill` capability is the odd one out:
  it marks a markdown [skill bundle](../tools/skill-bundles.md) riding the
  install machinery, not code, and needs no component.
- **Permissions**: what host services the plugin's code may *reach*. The
  `PluginPermission` enum in the same file. Today `config_read` (tool and
  channel adapters receive their own schema-materialized, validated public
  config and can
  resolve schema-designated secrets in authorized service calls) and
  `http_client`, `state_read`, and `state_write` have behavioral effect. The
  HTTP permission is the necessary grant for adapters that implement outbound
  `wasi:http`: tool and channel enable that surface, while memory intentionally
  does not yet. The state permissions gate encrypted durable state owned by the
  exact package, capability, and binding.
  `config_read` must be paired with the manifest's `config_schema`; either one
  without the other is rejected. The filesystem and memory-access permissions
  are accepted by the schema but not yet backed by host functions, so declaring
  them grants nothing.

## The worlds

`wit/v0/` defines one world per WASM capability. Every world imports the host
`logging` interface, whose `log-record` events land in the structured log
carrying the [span attribution](../ops/observability.md#zeroclaw-attribution)
of the host call site, and exports `plugin-info` (self-reported name and
version) plus its primary interface:

| World | Exports | Store lifecycle |
|-------|---------|-----------------|
| `tool-plugin` | `tool`: name, description, parameters-schema, execute | Fresh store per `execute`; imports scoped `secrets` and durable `state` |
| `channel-plugin` | `channel`: configure, send, poll-message, plus {{#include ../_snippets/plugin-channel-flag-count.md}} capability-gated methods | Warm store behind an async mutex, refueled per call; imports scoped `config`, `secrets`, durable `state`, and host-fed `inbound` |
| `memory-plugin` | `memory`: store, recall, get, forget, plus {{#include ../_snippets/plugin-memory-flag-count.md}} capability-gated methods | Warm store behind an async mutex, refueled per call |

The channel and memory worlds use **capability flags**: a bitmask the host
reads once at load time (`get-channel-capabilities` /
`get-memory-capabilities`). For every unset flag the host uses the Rust trait
default and never calls the plugin's export. This is how the WIT contract
stays additive: a new optional method is a new flag plus a new function, never
a break.

## Execution model

The host (`crates/zeroclaw-plugins/src/component.rs`) owns one async
`wasmtime::Engine` for the process. Loading is backend-dependent: a build with
the Cranelift JIT compiles `.wasm` on load; a runtime-only build deserializes
a precompiled `.cwasm`. Each plugin instantiation gets:

- a `Store` carrying the sandboxed WASI context, the resource table, the
  optional HTTP context, and the fuel budget;
- a `Linker` with exactly the imports its world, grants, and adapter support call
  `logging` always, `secrets` for tools and channels, `config` and `inbound` for
  channels, and `wasi:http` for tool and channel adapters only when the manifest
  grants `http_client`. Memory creates neither an HTTP context nor an HTTP
  linker. Each adapter cross-checks its context and linker at instantiation
  (`ensure_http_coherent`).

Tool calls are stateless by construction: `WasmTool::execute` builds a fresh
store, runs the call, and drops it. Channels and memory backends hold one warm
store for the plugin's lifetime; the host refuels it before every call so a
long-lived plugin gets a full budget per call rather than draining over time.
During an authorized channel call, `config.get` and `secrets.get` materialize at
most one revision of that admitted instance's canonical config. The host drops
the view when the call ends. A compliant channel plugin **must** resolve both at
each point of use and must not retain the returned config or plaintext secret in
warm guest state. The host cannot enforce non-retention after returning data to
trusted guest code.

The boundary is 32-bit: `wasm32-wasip2` is the only WASI Preview 2 target the
Rust toolchain ships, and the component ABI lowers offsets as 32-bit
regardless of host word size. Large values (a channel attachment's bytes)
cross by value. See the
[protocol page](../developing/plugin-protocol.md#32-bit-address-space-wasip2-is-wasm32)
for why this is an upstream constraint.

## Current wiring status

Be aware of what is registered end to end versus what is host-complete but
not yet reachable from a running daemon:

| Capability | Host adapter | Runtime wiring |
|------------|--------------|----------------|
| `tool` | `WasmTool` | Registered end to end; discovered tool plugins appear in the agent's tool set |
| `skill` | markdown loader | Registered end to end; skills load namespaced as `plugin:<plugin>/<skill>` |
| `channel` | `WasmChannel`, complete and unit-covered | Orchestrator registration and the per-vendor host listener are the remaining seam |
| `memory` | `WasmMemory`, implements the full `Memory` trait | The runtime does not yet construct it as a configurable backend |
| `observer` | none | `PluginCapability::Observer` is reserved; no WIT world or adapter exists yet |

## Configuration

Static plugin-host settings use the same schema mirror as everything else.
Per-instance values currently use generic TOML or `zeroclaw config set`; the
plugin manifest schema is not yet rendered as a zerocode or gateway form. Take
care when hand-editing: a syntax slip in a section (for example
`[plugins.entries]` where `[[plugins.entries]]` is meant) currently makes the
whole `[plugins]` section fail deserialization and silently fall back to
defaults, which reads back as `plugins.enabled = false` with no warning
(tracked in issue #8636). The common operations:

```bash
# turn the system on
zeroclaw config set plugins.enabled true

# where plugins are discovered (default: ~/.zeroclaw/plugins)
zeroclaw config set plugins.plugins_dir /srv/zeroclaw/plugins

# signature policy: disabled | permissive | strict
zeroclaw config set plugins.security.signature_mode strict

# per-call sandbox limits
zeroclaw config set plugins.limits.call_fuel 1000000000
zeroclaw config set plugins.limits.max_memory_mb 256
```

Per-instance settings live under `plugins.entries`, keyed by a versioned
`zpi1_…` string derived from the host-owned package, capability, and binding
identity. Installation prints and seeds the keys for the package's default
tool binding; `zeroclaw plugin info <package>` prints that key again. Alias-owned
channel instances declare package selection separately from values:

```toml
[channels.plugin.operations]
package = "my-platform"
enabled = true

[agents.operator]
channels = ["plugin.operations"]
```

The host derives the channel instance key from `my-platform`, the `channel`
capability, and `operations`; the declaration never copies credentials or
other plugin values. Full-identity keys let different packages and capability
worlds safely reuse aliases such as `main` without sharing credentials. The
canonical operator values are a secret-marked string map and remain encrypted
at rest (`enc2:…`). A plugin that requests `config_read`
declares the map's single type contract in `config_schema`: a closed Draft
2020-12 object whose
top-level properties explicitly use `string`, `boolean`, `integer`, `number`,
`array`, or `object`. A tool or channel consumer may set `x-secret = true` on a
top-level string property; the host validates it with the full object, removes
it from public config, and makes it available only through the admitted
instance's `secrets.get` import. Tools can read secrets during `execute`;
channels obtain public config through `config.get` and secrets through
`secrets.get` during `configure` and operational calls. Without the effective
`config_read` grant, either import returns `access-denied`; instantiation,
static metadata discovery, resolution failure, and host-call budget exhaustion
return `unavailable`. Store strings directly, JSON scalar text for booleans and
numbers, and JSON text for arrays and objects. The host
materializes and validates the resulting typed object before using tool or
channel guest code; unknown, malformed, or out-of-range values fail instead of
reaching the plugin. Memory plugins do not yet have a config import and must
not request `config_read` until that ABI is added.

Tool and channel components can request `state_read` and/or `state_write` for
the `state` import. The host, not the guest, supplies the admitted package,
capability, and binding namespace. Portable logical keys address authenticated
byte values within that exact instance. Reads return a revision; create,
replace, and delete operations use exact compare-and-swap revisions. Durable
rows live as `enc2:` ciphertext behind keyed blind indexes in
`data/plugin-state.db`, using the install `.secret_key`. Fixed quotas and any
key, storage, or integrity failure fail closed.

Pre-1.0 plugin authors must migrate explicitly: a manifest that requests
`config_read` without `config_schema` is no longer discovered. Add a closed
schema matching the current values, update tool/channel guests to deserialize
typed JSON rather than a string map, rebuild, and re-sign because the schema is
signature-covered. Host integrations inject `PluginHostServices`, which wraps
a `PluginConfigResolver`, instead of an owned config map. Each authorized tool
or channel frame materializes at most one scope-bound `ResolvedPluginConfig`,
uses that view for every config read in the frame, and drops it when the frame
ends. A channel calls `config.get` and `secrets.get` at point of use, so a public
config plus credential rotation within the same logical binding is visible as
one revision on the next operation. Static identity and capability exports are
read once at load; changing the bot/account identity or other static metadata
requires channel lifecycle reconstruction.

This is a strict pre-1.0 key format: legacy entries named only after a package
or binding are not consulted. For an existing tool package, run `zeroclaw
plugin info <package>` to obtain its full-instance key, rename the old entry to
that key, and save the config. Fresh tool installs seed it automatically.

Effective grants are checked separately from manifest requests. If
`config_read` is denied, the host validates an empty object. Required properties
make startup fail closed. When the empty object is valid, a tool omits the empty
`__config` key and channel config/secret imports return `access-denied`. The
canonical host field list and defaults are in the
[Config reference](../reference/config.md); `zeroclaw config list` shows the live
stored values.

## Where the trust boundary actually is

The sandbox bounds what a loaded plugin can do; the signature policy bounds
what loads at all. Both are operator decisions, and they compose:

- `plugins.enabled` false (the default): no plugin code runs, ever.
- Signature `strict`: only components whose manifest carries a valid Ed25519
  signature from a key in your trusted set load.
- Loaded plugin: bounded by fuel, memory ceilings, no-preopen WASI, and the
  permission-gated import set.

What the sandbox does *not* bound is the semantic behavior of a tool the model
chooses to call: a tool with the `http_client` grant and the tool adapter's HTTP
surface can send whatever the model passes it to wherever its code decides.
Signature policy exists because "which code do I load" is the decision that
matters most; make it deliberately.
