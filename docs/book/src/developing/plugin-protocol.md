---
type: reference
status: accepted
last-reviewed: 2026-07-17
relates-to:
  - FND-001
  - ADR-003
  - crates/zeroclaw-plugins
---

# Plugin Protocol

This document defines the protocol between ZeroClaw's plugin host and WASM
plugin components.

## What a plugin is

A plugin is a self-contained WebAssembly component that ZeroClaw loads at
runtime to add a capability the core binary does not ship. It lives in its own
directory under `~/.zeroclaw/plugins/`, alongside a manifest that names it and
declares what it provides. ZeroClaw discovers it on startup, verifies it, and
wires its exported functions into the running agent so they behave like
built-in capabilities: a tool plugin shows up to the model as just another
callable tool (`WasmTool` implements the same `Tool` trait a native tool does),
a channel plugin behaves as a messaging channel, a memory plugin as a storage
backend.

A plugin can provide one or more of the capabilities defined in
`PluginCapability` (`crates/zeroclaw-plugins/src/lib.rs`): a callable tool, a
messaging channel, a memory backend, an observability backend, or a bundle of
markdown skills. The skill case is special: it ships no WASM at all, just a
`skills/` directory of markdown, which is why it is the one capability that
omits the compiled component.

### Why build one

- **Extend without forking.** Add a tool or channel without modifying the
  ZeroClaw source tree or waiting on a release; the plugin is yours and loads
  from your install directory.
- **Native behavior.** A loaded plugin is not a second-class add-on. The bridge
  implements the same runtime traits the built-ins use, so a plugin tool is
  offered to the model, attributed, and invoked exactly like a first-party one.
- **Language choice.** The contract is WIT and the WASI Component Model, not a
  Rust API. Any language that compiles to a `wasm32-wasip2` component can
  implement a world. The worked guide below is Rust because that is the path
  with the most support today, but the boundary itself is language-agnostic.
- **Sandboxed by default.** The host loads each plugin into a WASI context with
  no filesystem preopens and no ambient network. A plugin cannot quietly reach
  the host; it gets exactly the host functions wired into its world and nothing
  more. Outbound HTTP is the one network surface that can be opened, and only
  when the manifest grants `http_client` and that capability adapter explicitly
  enables its tested HTTP boundary. Tool and channel adapters do; memory does
  not yet.
- **Verifiable provenance.** Manifests can be Ed25519-signed, and an operator
  can require signatures from trusted publishers before any plugin loads.

### What a plugin cannot do (today)

These are real limits of the current host, not style preferences. Know them
before you design around a capability that is not there.

- **`logging`, typed config, instance-scoped secrets and durable state,
  `http_client`, and host-fed inbound are wired.** Of the permissions a
  manifest can declare, `config_read` exposes the plugin's own schema-validated
  public config. A tool or channel schema can designate secrets withheld from
  public config and resolved in authorized service calls. `state_read` and
  `state_write` gate encrypted, compare-and-swap state owned by the exact
  admitted instance. An `http_client` grant is necessary for outbound
  `wasi:http`, but the capability adapter must also opt into that host surface.
  Tool and channel adapters do; memory intentionally remains HTTP-free until
  its network boundary has component-level coverage. Filesystem and
  memory-access permissions are still accepted by the manifest schema but
  inert: their host functions are not yet registered in the linker. See
  Permissions and Host imports below.
- **No ambient host network or filesystem.** The WASI context has no preopens and
  no ambient network, so a plugin cannot open raw sockets or read host files
  through ambient WASI. A tool or channel plugin with an `http_client` grant
  gets outbound `wasi:http` because those adapters opt in; it cannot listen.
  Channel plugins that must receive inbound traffic do not open a listener
  themselves. A host producer can feed messages through the `inbound` import,
  which the plugin drains from `poll-message`. A webhook-capable channel can
  instead advertise a single host-owned HTTP route and authenticate and decode
  each raw request through `parse-webhook`.
- **A 32-bit boundary.** The target is `wasm32-wasip2`. Guest memory is a 32-bit
  address space and the component ABI lowers offsets as 32-bit regardless of
  host word size. Large values (for example a channel attachment's raw bytes)
  cross the boundary by value. See the 32-bit address space section for why this
  is an upstream-toolchain constraint, not a flag this repo can flip.
- **One tool per tool plugin.** The `tool-plugin` world exports a single `tool`
  interface with one name and schema. A plugin that needs to expose several
  tools ships several components, or a different world.
- **Experimental, unfrozen contract.** `wit/v0` carries no `.frozen` marker yet,
  so the interfaces can still change before the first stable release. Pin to a
  version and expect to recompile across a WIT bump.

## Architecture

ZeroClaw plugins are WebAssembly components defined by WIT interfaces under
`wit/v0/` and hosted through direct `wasmtime` (`crates/zeroclaw-plugins`). A
plugin is compiled to a WASI Preview 2 component (`wasm32-wasip2`) that exports
one of the plugin worlds (`tool-plugin`, `channel-plugin`, `memory-plugin`) and
imports the host interfaces declared by that world in `wit/v0/`.

The host lives in `crates/zeroclaw-plugins/src/component.rs`. It holds one
async-enabled `wasmtime::Engine`, generates the world bindings with
`wasmtime::component::bindgen!` from `wit/v0`, and wires a sandboxed WASI p2
surface into each world's linker. Per-store host state (`PluginState`) carries a
`WasiCtx` built with no preopens and no network, plus the `ResourceTable` WASI
requires, its host-issued scope, and typed live service handles. Every world
imports `logging`; tool imports `secrets`, while channel imports `config`,
`secrets`, and `inbound`. A granted `http_client` permission additionally
attaches and links `wasi:http`.
The world declarations and the admitted scope remain the canonical contracts
for that surface (see Host imports).

The three world bridges map each WIT world onto the runtime's native traits:

| World | Bridge module | Runtime surface |
|-------|---------------|-----------------|
| `tool-plugin` | `runtime.rs`, `wasm_tool.rs` | `zeroclaw_api::tool::Tool` |
| `channel-plugin` | `wasm_channel.rs` | channel trait |
| `memory-plugin` | `wasm_memory.rs` | memory backend trait |

Tool plugins use a fresh store per call (stateless). Channel and memory plugins
hold a warm store guarded by an async mutex for the lifetime of the plugin.

Tool plugins are discovered and registered end to end: the runtime walks
`channel_plugin_details()`'s tool counterpart and builds a `WasmTool` for each.
The channel host adapter (`WasmChannel`, its `wasi:http` gating, point-of-use
config services, and host-fed `inbound` queue) is complete and unit-covered, and
`PluginHost::channel_plugin_details()` exposes the wasm-backed channel plugins
to register. The runtime admits enabled `[channels.plugin.<alias>]` declarations
owned by enabled agents, constructs them from the exact component bytes admitted
by the host, and supervises them in the ordinary channel listener lifecycle.
Explicit channel declarations do not require `plugins.auto_discover`; that flag
controls package-scoped tool and skill discovery. One deterministic
`plugins.max_active_instances` decision covers all three logical capability
types, with explicit channels admitted before auto-discovered candidates.

The generic host-owned producer that feeds a channel's `inbound` queue remains
available for vendor tunnels and polling clients. For HTTP push transports, a
channel can advertise `webhook-ingress`; the daemon then owns the route, queue,
deadline, rate limiting, and idempotency while a disposable component instance
authenticates and decodes the raw request. The memory bridge (`WasmMemory`) is
one step earlier:
the adapter implements the full `Memory` trait against the `memory-plugin`
world, but the host does not yet expose a memory counterpart to
`channel_plugin_details()` and the runtime does not yet construct a
`WasmMemory` as a configurable backend.

## Plugin structure

A plugin is a directory containing:

```
my-plugin/
  manifest.toml    # Plugin metadata and permissions
  plugin.wasm      # Compiled WASM module (optional for skill-only plugins)
```

Plugins are discovered from `~/.zeroclaw/plugins/` (configurable via
`plugins.plugins_dir` in config).

## Registry search and install

The local plugin install path remains the source of truth for installed
plugins. A registry is only a JSON index used at command time to discover and
download a plugin archive:

```bash
zeroclaw plugin search calendar
zeroclaw plugin install team-calendar
zeroclaw plugin install team-calendar@0.2.0
zeroclaw plugin search calendar --registry https://example.invalid/registry.json
zeroclaw plugin install team-calendar --registry https://example.invalid/registry.json
```

`zeroclaw plugin search` fetches registry metadata and matches the query against
plugin names and descriptions. It does not install, enable, or execute plugin
code.

`zeroclaw plugin install <name>` resolves the name from the registry, downloads
the selected zip archive, verifies the optional SHA-256 digest, safely extracts
the archive, and then hands the extracted plugin directory to the existing
`PluginHost::install` path. Local path installs are unchanged:

When no version is pinned, ZeroClaw chooses the last matching entry in the
registry index, so registry publishers should order repeated names
intentionally.

```bash
zeroclaw plugin install ./my-plugin
zeroclaw plugin install ./my-plugin/manifest.toml
```

The default registry URL is:

```text
https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw-plugins/main/registry.json
```

For private or staged registries, use `--registry <url>` per command or set
`ZEROCLAW_PLUGIN_REGISTRY_URL`.

Registry entries use this shape:

```json
{
  "plugins": [
    {
      "name": "team-calendar",
      "version": "0.8.3",
      "description": "Schedule meetings on a team calendar",
      "author": "Example Team",
      "capabilities": ["tool"],
      "url": "https://example.invalid/team-calendar-0.2.0.zip",
      "sha256": "sha256:<hex digest of the zip>"
    }
  ]
}
```

The archive must contain either a root-level `manifest.toml` or one nested
plugin directory containing `manifest.toml`. Archives with traversal paths,
absolute paths, Windows drive-prefixed paths, or more than one manifest are
rejected before install. Downloads are capped while streaming, so a server
without `Content-Length` cannot force ZeroClaw to buffer an oversized archive.
Extraction is also capped, so a compressed archive cannot expand without bound
in the temporary install area.

Search is unauthenticated discovery. Install is the security boundary: registry
installs use the configured plugin signature policy and trusted publisher keys,
the same as local plugin installs through `PluginHost::install`.

### Skill-only plugin layout (markdown bundle)

A plugin whose only capability is `skill` ships skills under a `skills/`
directory in [agentskills.io](https://agentskills.io) format and omits
`wasm_path`:

```
my-toolkit/
  manifest.toml              # declares the skill capability, no wasm_path
  README.md                  # optional bundle-level overview
  skills/
    design-review/
      SKILL.md
      scripts/
      references/
    code-review/
      SKILL.md
    data-analysis/
      SKILL.md
      references/
```

Each `SKILL.md` must include YAML frontmatter with `name` and `description`
fields; the runtime rejects bundles whose skills omit either at discovery time
rather than at first invocation. Skills register under plugin-namespaced IDs
of the form `plugin:<plugin-name>/<skill-name>` (e.g.
`plugin:my-toolkit/design-review`) to avoid collisions with user-authored
skills and between bundles.

## Manifest format

{{#include ../_snippets/plugin-manifest-fields.md}}

### Capabilities

`capabilities` is a non-empty list of `PluginCapability` values, defined in
`crates/zeroclaw-plugins/src/lib.rs` (serialized `snake_case`). Each value
selects the WIT world the plugin exports (`tool`, `channel`, `memory`), names an
observability backend (`observer`), or marks a markdown-only skill bundle
(`skill`). Read the enum for the canonical set; it is the source of truth and
this page does not restate it.

A manifest must declare at least one capability. `wasm_path` is required for
every capability except a plugin whose only capability is `skill`, which carries
no WASM payload and is rejected at discovery if it omits a valid `skills/`
bundle (`validate_manifest_shape` in `host.rs`).

### Permissions

`permissions` is a list of `PluginPermission` values, also defined in
`crates/zeroclaw-plugins/src/lib.rs`. Read the enum for the canonical set.

Be aware of the gap between declared and enforced: in the component host today
`config_read`, `http_client`, `state_read`, and `state_write` have behavioral
effect. Requesting
`config_read` requires a `config_schema`, and declaring that schema without the
permission is also rejected. Before a tool or channel component is used, the
host resolves its effective grant, materializes the plugin's operator values to
typed JSON, and validates the complete object. `runtime.rs` strips any
caller-supplied `__config` before injecting validated non-secret values into a
tool call; direct top-level string properties marked `x-secret: true` are
omitted from public config and read through the host-scoped `secrets` import.
Tools receive that service during `execute`. Channels receive public config
through `config.get` and secrets through `secrets.get` during `configure` and
operational calls, while instantiation and static metadata discovery remain
unavailable. `http_client` is a necessary grant, not a complete authority
decision: the capability adapter must also construct the HTTP context and link
`wasi:http`. Tool and channel adapters opt in after grant validation. The
memory adapter deliberately does not, so granting `http_client` to a memory
scope alone adds no network surface. The state permissions independently gate
the `state.get` and `state.put`/`delete`
imports for tool and channel service frames. Package, capability, and binding
come only from the admitted host scope; a guest supplies no namespace. The
remaining variants
(`file_read`, `file_write`, `memory_read`, `memory_write`) are accepted by the
manifest schema but are not yet wired to a host import: declaring them grants
nothing on its own. They reserve the names for the host functions that will
gate them (see Host imports below).

## WIT interfaces

The plugin contract is the set of WIT files in `wit/v0/`, package
`zeroclaw:plugin@0.1.0`. Every item is gated behind
`@unstable(feature = plugins-wit-v0)` until the package stabilizes; see
`wit/VERSIONING.md` for the compatibility rules. The interfaces below are
summarized for orientation; the `.wit` files are authoritative for the exact
signatures.

### Worlds

`wit/v0/` defines three worlds, bound by `bindgen!` in `component.rs`. Each
imports `logging` (host) and exports `plugin-info` plus its primary interface:
`tool-plugin` exports `tool`, `channel-plugin` exports `channel`, and
`memory-plugin` exports `memory`. Tool also imports `secrets` and `state`;
channel imports `config`, `secrets`, `state`, and `inbound`. The required
(no-default) exports for each
world are listed in the world's doc comment in its `.wit` file.

### `tool` interface

`wit/v0/tool.wit` defines the single-tool surface. The host calls `name`,
`description`, and `parameters-schema` once at load time, then dispatches
`execute` per invocation:

```wit
record tool-result {
    success: bool,
    output: string,
    error: option<string>,
}

name: func() -> string;
description: func() -> string;
parameters-schema: func() -> json-string;
execute: func(args: json-string) -> result<tool-result, string>;
```

`parameters-schema` returns a JSON Schema string presented to the LLM for tool
calling. `execute` receives JSON-encoded arguments matching that schema and
returns a `tool-result` or an error string. `json-string` is a `string` type
alias from `wit/v0/types.wit`; callers produce valid JSON, receivers parse it.

### `channel` and `memory` interfaces

`wit/v0/channel.wit` and `wit/v0/memory.wit` define capability-gated surfaces.
The host calls `get-channel-capabilities` / `get-memory-capabilities` once at
load time, and for each unset flag it uses the Rust trait default instead of
calling the plugin. A plugin must still export every function (a stub returning
the documented default value is sufficient); the host simply never calls the
ones whose flag is absent. The default each unset flag resolves to is documented
inline in the WIT next to the `*-capabilities` flags, which is the source of
truth for both the flag set and its defaults.

### Capability flags

Optional methods are advertised through `flags channel-capabilities` and
`flags memory-capabilities`. Because flags are a bitmask, new optional methods
can be added to a `vN/` package without a breaking change, paired with a new
`@since` function. Removing or renaming a flag, function, field, or variant case
is breaking and requires a new `vN+1/` directory.

## Host imports

Host functions are imported by the plugin and provided by the runtime. Every
world's linker wires `logging` (via the host impl in `component_logging.rs`,
linked alongside `add_wasi` in `component.rs`). Tool and channel link the
instance-scoped `secrets` service. Channel also imports `config` for its typed
public object and `inbound` for the host-fed message queue it drains from
`poll-message`. Tool and channel adapters link outbound `wasi:http` only after
the admitted scope grants `http_client` (`PluginStoreSpec::with_granted_http`
and `add_wasi_http` in `component.rs`). Memory withholds both the context and
linker surface. The filesystem and memory-access permissions remain inert: the
host functions that would gate them are not yet wired into the linker. A
plugin's ambient authority is the WASI context (no preopens, no ambient network)
plus exactly the host imports its grants and adapter opt-ins jointly enable.

ZeroClaw-owned imports share a fixed safety budget per host-dispatched service
frame. The canonical ceiling is `MAX_HOST_CALLS_PER_FRAME` in
`crates/zeroclaw-plugins/src/component.rs`. On exhaustion, logging becomes a
no-op, inbound polling reports empty, and public-config or secret reads return
`unavailable`. A new frame resets the budget. This ceiling is fixed host policy,
not duplicated operator configuration.

### `inbound`

`wit/v0/inbound.wit` is imported by the `channel-plugin` world. A channel plugin
runs with no listener of its own, so the host runs the listener (a webhook
server, a vendor tunnel, a polling client) and enqueues each received message.
The plugin drains the queue from its `poll-message` export by calling
`inbound-poll`, with `inbound-pending` available to drain in batches:

```wit
inbound-poll: func() -> option<host-inbound-message>;
inbound-pending: func() -> u32;
```

The host side owns an `InboundQueue` per channel; `WasmChannel::inbound` hands a
clone to the listener task so enqueued traffic is visible to the plugin's drain.

### Channel webhook ingress

A channel that includes `webhook-ingress` in `channel-capabilities` may expose
one single-segment path with `webhook-path`. The daemon registers that logical
instance at `GET /plugin/<path>` and `POST /plugin/<path>`. Empty paths, `.` and
`..`, embedded `/`, and ambiguous claims are rejected by the host; guest
metadata cannot choose a different channel alias or route owner.

The gateway passes lower-cased raw headers and the bounded raw body to
`parse-webhook`. Before dispatch, the host removes caller-supplied copies of the
reserved `x-webhook-method` and `x-webhook-query` headers and regenerates them
from the actual request. The guest must authenticate the platform request before
returning messages, normally by reading the current public config and scoped
secret with `config.get` and `secrets.get` in that call. It returns either a
list of inbound messages or a typed `unauthorized` / `bad-request` rejection.
For a provider verification handshake, one message with channel
`__webhook_reply__` returns its content as the HTTP 200 body without enqueueing
an agent message. The content is limited to 4 KiB (4096 UTF-8 bytes); an
oversized response produces a fixed 502 response and no delivery.
The host replaces guest-supplied channel identity with the admitted `plugin`
endpoint, checks each sender against the current `peer_groups` policy for that
alias, and only then applies gateway idempotency to non-empty message IDs.

Webhook parsing uses a fresh store built from the exact admitted component
bytes. Cancellation discards that disposable store, so a stalled request does
not retain the warm channel mutex or block later polling and outbound calls.
The supervised `listen` invocation owns both polling and webhook queue drain;
there is no second detached startup path for webhook-capable channels.

### `logging`

`wit/v0/logging.wit` is imported by all three worlds. Plugins call `log-record`
to emit structured events back to the host:

```wit
log-record: func(level: log-level, event: plugin-event);
```

The call is fire-and-forget: it returns nothing and the host
(`component_logging.rs`) absorbs all errors, so a failed log write can never
crash plugin execution. `plugin-action` and `plugin-outcome` mirror the closed
`Action` / `EventOutcome` taxonomies in `zeroclaw-log`; there is no escape-hatch
variant on purpose. Do not call `wasi:logging` directly, plugin events would be
formatted inconsistently and would not reach all of the destinations
`zeroclaw_log` writes to.

### `config`

`wit/v0/config.wit` is imported by the channel world. It returns the current
schema-validated, non-secret object as JSON:

```wit
get: func() -> result<json-string, config-error>;
```

The object preserves the types declared by `config_schema`; properties marked
`x-secret: true` are omitted. The service is available during `configure` and
operational channel exports. It returns `access-denied` when the admitted
instance lacks the effective `config_read` grant. Calls during component
initialization or static metadata discovery, resolver or validation failure,
and host-call budget exhaustion return `unavailable` without exposing internal
detail.

`config.get` is point-of-use access, not a load-time snapshot. A compliant
channel plugin **must** call it in each operation that uses config and must not
retain its returned object in warm guest state. This is a plugin conformance
rule: after returning JSON to trusted guest code, the host cannot prevent a
malicious component from copying it.

### `secrets`

`wit/v0/secrets.wit` is imported by the tool and channel worlds. The guest
supplies only a top-level property name:

```wit
get: func(name: string) -> result<string, secret-error>;
```

The host derives package, capability, binding, and effective grants from the
admitted `PluginInstanceScope`; none are guest inputs. Only direct top-level
string properties marked `x-secret: true` in the manifest schema are readable.
Tools can read them while the host dispatches `execute`. Channels can read them
during `configure` and operational calls such as send, poll, health, and
capability-gated actions. Component initialization and static metadata exports
return `unavailable` without resolving config. Within one channel service frame,
every `config.get` and `secrets.get` uses one resolved canonical config revision;
that frame is dropped on every exit path. A compliant plugin therefore observes
a same-binding public/secret rotation together on its next operation.
`access-denied`, `not-found`, and `unavailable` deliberately reveal no resolver
or schema detail. Successful reads return plaintext to the trusted guest. The
service prevents public injection and cross-instance selection; it is not an
egress proxy that keeps the value hidden from plugin code. A compliant channel
plugin **must** resolve secrets at each point of use and must not retain a second
copy in warm state. The host cannot enforce non-retention after returning the
plaintext.

Secret property names use the same portable plugin-local grammar as state keys:
1–128 ASCII bytes containing only letters, digits, `_`, `-`, or `.`. URI, path,
namespace, control, and non-ASCII syntax is rejected during manifest admission.

### `state`

`wit/v0/state.wit` is imported by the tool and channel worlds. It provides
encrypted durable byte values owned by the exact admitted package, capability,
and binding:

```wit
get: func(key: string) -> result<option<state-entry>, state-error>;
put: func(key: string, value: list<u8>, expected-revision: option<u64>)
    -> result<u64, state-error>;
delete: func(key: string, expected-revision: u64) -> result<_, state-error>;
```

`state_read` permits `get`; `state_write` permits `put` and `delete`. `none` on
`put` is an absent-key compare-and-swap, while `some(revision)` must match the
current revision exactly. Writes return the new revision. Keys use the portable
plugin-local grammar above and cannot select another instance. State is
available only during tool execution and channel service frames; static calls,
host-call budget exhaustion, storage/key failures, and integrity failures return
closed errors. Fixed per-instance entry, value, and total-size quotas return
`quota-exceeded` rather than partially committing a write.

The runtime stores one authenticated envelope per value in
`data/plugin-state.db`, encrypted with the install's existing `.secret_key`.
The database contains only keyed blind indexes and `enc2:` ciphertext. Missing
or replaced install keys fail closed and are never silently regenerated while
durable rows exist. Back up the database and `.secret_key` together.

### Per-plugin config (`__config` and `config.get`)

**Permission:** `config_read`

A plugin does not read process environment variables. Its manifest must pair
`config_read` with a Draft 2020-12 `config_schema`; either one without the other
is an invalid manifest. The schema root must be an object with a `properties`
map and `additionalProperties = false`. Each top-level property must declare
one of `string`, `boolean`, `integer`, `number`, `array`, or `object`, directly
or through a package-local JSON Pointer. Tool and channel consumers may set
`x-secret: true` on a direct top-level string property; nested, false, or
non-boolean markers and secret non-string properties are rejected.
Unknown keys, malformed encodings, and constraint violations reject the
instance before values are delivered to guest code.

The operator's canonical `plugins.entries.<instance-key>.config` values remain
a secret-marked string map in memory and are encrypted when persisted. The host
derives the versioned `zpi1_…` entry key from the full package, capability, and
binding identity; this lets different packages and capability worlds safely
reuse aliases such as `main`. Channel bindings select the package under
`[channels.plugin.<alias>]` and agents own them through the ordinary
`plugin.<alias>` channel reference; neither surface stores operator values.
The admitted package manifest selects the schema.
A `string` value is stored directly; `boolean`,
`integer`, and `number` values use JSON scalar text such as `"true"`, `"4"`,
or `"0.5"`; `array` and `object` values use JSON text such as
`'["urgent","ops"]'` or `'{"region":"us-east"}'`. The host materializes and
validates the complete data into typed JSON for each use, then partitions every
property exactly once. For a tool, non-secret values are injected under the
reserved `__config` key:

```json
{
  "prompt": "a sunset",
  "__config": {
    "retry_limit": 4,
    "enabled": true,
    "labels": ["urgent", "ops"]
  }
}
```

The omitted `api_key` is read explicitly with `secrets.get("api_key")` if its
schema marks it secret. `runtime.rs` strips any caller-supplied `__config`
before injecting the public section, so the section cannot be spoofed. Tool
public injection and secret reads within one `execute` frame share one resolved
live-config revision; the frame is dropped on success, error, trap, panic, or
cancellation. A channel's `configure` export has no config parameter. It calls
`config.get` for the public object and `secrets.get` for secret properties, as
does each later operational export that uses configuration. Both imports within
one call share one resolved revision. The host drops that materialized view on
success, error, trap, panic, or cancellation. Public config and credential
changes within the same logical binding are therefore available together on the
next operation when the compliant guest resolves both at point of use.

When the manifest requests `config_read` but the host does not effectively
grant it, resolution substitutes an empty object and validates that object
before guest code runs. A schema with required fields fails closed during
construction. If the empty object is valid, tools omit the empty `__config`,
while channel `config.get` and `secrets.get` return `access-denied`. A plugin
only ever sees its own section.

Channel static metadata exports cannot call either config service and are read
once at load. Changing a bot/account identity or any config-derived capability,
self-handle, mention, or multi-message delay therefore requires channel
lifecycle reconstruction; ordinary public config and credential rotation for
the same logical binding does not.
Tool and channel are the current config consumers. The memory world has no
config import yet, so memory plugins must not request `config_read` until that
ABI and runtime wiring land.

## WASI Component Host

The host (`crates/zeroclaw-plugins/src/component.rs`) compiles and instantiates
components against a single async `wasmtime::Engine`. How a `.wasm` file is
loaded depends on the build's execution backend:

- **`plugins-wasm-cranelift`**: a JIT backend is present, so `load_component`
  compiles a `.wasm` component on load via `Component::from_file`.
- **No JIT backend** (`plugins-wasm-pulley` or runtime-only): there is no
  compiler in the binary, so `load_component` deserializes the file directly via
  `Component::deserialize_file`, treating it as a precompiled `.cwasm` produced
  by a matching wasmtime. A mismatched artifact is rejected by deserialize's
  version check.

Both backend features pull in `plugins-wasmtime`; the load path keys off whether
the cranelift compiler is in the build, not off pulley.

### Per-call execution limits

Every plugin call runs under per-call resource limits the host applies to the
store. The engine enables fuel metering, and each call is given a fresh fuel
budget so a runaway or malicious component traps instead of hanging the host. A
`StoreLimits` ceiling bounds linear memory, table elements, and instance count.
The tool world gets a fresh store per execute; the warm channel and memory
stores are refueled before each call so a long-lived plugin gets a fresh budget
rather than draining over its lifetime.

The four bounds are operator-tunable and every value is validated as non-zero:
`plugins.limits.call_fuel` (default 1,000,000,000 instruction units),
`plugins.limits.max_memory_mb` (default 256), `plugins.limits.max_table_elements`
(default 100,000), and `plugins.limits.max_instances` (default 64). A store can
only be built with explicit limits, so no load path can construct an
unsandboxed plugin. The canonical fields and defaults live in the
[Config reference](../reference/index.md).

### 32-bit address space (wasip2 is wasm32)

The plugin target is `wasm32-wasip2`, and the host engine is built with fuel
metering enabled (`Config::consume_fuel(true)`) without `wasm_memory64`. The
plugin boundary is a fixed 32-bit format, and that has consequences worth
stating plainly:

- **The guest address space is 32-bit.** A plugin runs in a wasm32 linear
  memory. Large values cross the boundary by value: a channel plugin's
  `media-attachment` carries its full bytes as a `list<u8>`, and `wit/v0/channel.wit`
  already notes this can be several megabytes and leaves a resource-handle model
  to a future revision. Within that 32-bit space the host applies an explicit
  per-store memory ceiling from `plugins.limits.max_memory_mb` (default 256),
  so a guest is bounded by the smaller of the wasm32 address space and that
  ZeroClaw-configured cap.
- **The component ABI lowers offsets as 32-bit regardless of host word size.**
  Even on a 64-bit host, list and string offsets in the canonical ABI are
  `i32`. `memory64` widens a guest's linear-memory addressing, not the
  component-model canonical ABI, so enabling it would not make WIT-level fields
  64-bit.
- **There is no 64-bit wasip2 target to bind against.** `wasm32-wasip2` is the
  only WASI Preview 2 target in rustc and LLVM today; a plugin cannot be
  compiled to a 64-bit p2 component, so there is nothing for the host to load
  even if the engine enabled `memory64`.

This is an upstream-toolchain constraint, not a host limitation that a flag in
this repo can lift. When a 64-bit p2 target and a wider component ABI land
upstream, the `bindgen!` seam regenerates against them and field widths are
revisited in the WIT under the `wit/VERSIONING.md` window. Until then, treat the
plugin boundary as 32-bit by construction.

## Signatures

Plugin manifests may carry an Ed25519 signature
(`crates/zeroclaw-plugins/src/signature.rs`). The signature is base64url-encoded
over the canonical manifest bytes (the parsed TOML with only the exact root
`signature` and `publisher_key` entries removed); the publisher's public key is
hex-encoded. Nested schema properties with those names remain signed. The host
enforces one of three modes from `plugins.security.signature_mode`:

| Mode | Unsigned plugin | Untrusted or invalid signature |
|------|-----------------|--------------------------------|
| `strict` | rejected | rejected |
| `permissive` | loaded with a warning | loaded with a warning |
| `disabled` | loaded | not checked |

Verification runs at both discovery and install. Discovery skips a plugin that
fails its policy rather than aborting the whole host; install returns the error.
For executable plugins, `strict` mode additionally requires the signed manifest
to declare `wasm_sha256`. The host confines `wasm_path`, rejects symlinks, reads
one stable file generation, verifies that digest, and gives adapters only those
admitted bytes. A declared digest is enforced in every signature mode.

## Writing a plugin in Rust

A plugin is a `cdylib` crate that targets the component model. Generate the
guest bindings from the same `wit/v0` package the host uses, implement the
exported world, and compile to `wasm32-wasip2`. For the full worked
walkthroughs from empty crate to installed plugin, see the
[plugin guides](../plugins/index.md); the notes below cover the
build and install mechanics.

### Building

<div class="os-tabs-src">

#### sh

```sh
# Install the WASI Preview 2 target (once)
rustup target add wasm32-wasip2

# Build the component
cargo build --target wasm32-wasip2 --release
```

</div>

The output component is at `target/wasm32-wasip2/release/<crate_name>.wasm`.
Copy it alongside your `manifest.toml`. For a runtime-only host build with no JIT
backend, precompile the component to a `.cwasm` with a matching wasmtime and ship
that instead, since such a host deserializes rather than compiles on load.

The reference fixture is not committed to the tree (it is a build artifact, not
source). When `crates/zeroclaw-plugins/tests/fixtures/reference-plugin.wasm` is
provisioned by a clean `cargo build --target wasm32-wasip2` of the published
reference plugin, `reference_plugin.rs` and `reference_plugin_e2e.rs` load it
through the same `PluginHost` and config-resolution paths the daemon runs. When
the artifact is absent, those tests skip.

### Installing

<div class="os-tabs-src">

#### sh

```sh
# Copy to plugin directory
zeroclaw plugin install /path/to/my-plugin/

# Or manually
cp -r my-plugin/ ~/.zeroclaw/plugins/my-plugin/
```

</div>

## Configuration

Operator values currently enter through generic string-map storage: edit
`[[plugins.entries]]` in TOML, or use `zeroclaw config set` after a tool install
has seeded its package-name binding entry. `zeroclaw plugin info <package>`
prints the same key for migration and later edits. Schema-driven forms and
inline field help are not implemented yet. The current surfaces are:

- **The CLI** handles plugin lifecycle with `list`, `search`, `install`,
  `remove`, `info`, and `migrate`. `zeroclaw config set` writes individual raw
  plugin values; it does not interpret the plugin's schema.
- **zerocode** can edit ZeroClaw's static plugin-host settings, but does not yet
  generate per-plugin fields from `config_schema`.
- **The web gateway** is read-only for plugins: `GET /api/plugins` reports the
  loaded plugins and whether the system is enabled.
- **The host** validates `config_schema` when admitting the package and
  validates/materializes operator values again before guest use.
- **The manifest schema**, for plugin authors, is the sole type and validation
  contract at the guest boundary. Define every supported key and constraint
  there; do not duplicate that contract in a host runtime config struct. Guest
  code should deserialize the host-validated JSON into its native typed struct.

The static config schema supplies the generic storage and secret-marking path,
not a dynamic per-plugin editor. The plugin config types in
`crates/zeroclaw-config/src/schema.rs` carry `#[prefix = "plugins"]`,
`#[prefix = "plugins.entries"]`, and `#[prefix = "plugins.security"]`, and the
`Configurable` derive turns each prefixed field into a generic config path.
Secret fields (a plugin entry's `config` map is marked `#[secret]`) encrypt at
rest under the adjacent `.secret_key`. The canonical fields,
defaults, and the `signature_mode` values for host configuration live in the
[Config reference](../reference/index.md); that schema is the source of truth,
while each plugin manifest is the source of truth for its private config shape.

### Build features

The plugin host is a compile-time opt-in. The binary-level features in the
workspace `Cargo.toml` select whether plugins are built in at all and which
execution backend ships:

- `plugins-wasm` is the umbrella that pulls the plugin host and its runtime
  integration into the binary. It is **required**: the backend features below
  select an execution engine but do not imply the umbrella, so building with
  only a backend feature succeeds and silently yields a binary with no
  `plugin` subcommand. Always pass `plugins-wasm` plus your chosen backend,
  e.g. `--features plugins-wasm,plugins-wasm-cranelift`.
- `plugins-wasm-runtime-only` is the smallest and fastest to start: no JIT, so
  components are deserialized from a precompiled `.cwasm`.
- `plugins-wasm-cranelift` adds the Cranelift JIT, so a `.wasm` component is
  compiled on load.
- `plugins-wasm-pulley` is the most portable, supporting compilation on targets
  Cranelift does not cover.

These delegate to the `zeroclaw-plugins` crate features
(`plugins-wasmtime`, `plugins-wasm-cranelift`, `plugins-wasm-pulley`) that wire
up `wasmtime`. The load path keys off whether the Cranelift compiler is in the
build, as described under WASI Component Host. Read the feature comments in the
workspace `Cargo.toml` for the authoritative descriptions.
