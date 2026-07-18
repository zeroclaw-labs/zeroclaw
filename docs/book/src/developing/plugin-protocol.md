---
type: reference
status: accepted
last-reviewed: 2026-06-29
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

- **`logging`, config injection, and host-fed inbound are wired; tool and channel
  adapters also implement `http_client`.** Of the permissions a manifest can
  declare, `config_read` injects the plugin's own config section. An
  `http_client` grant is necessary for outbound `wasi:http`, but the adapter must
  also opt into the host surface. Memory intentionally remains HTTP-free until
  its network boundary has component-level coverage. Filesystem and
  memory-access permissions are still accepted by the manifest schema but
  inert: their host functions are not yet registered in the linker. See
  Permissions and Host imports below.
- **No ambient host network or filesystem.** The WASI context has no preopens and
  no ambient network, so a plugin cannot open raw sockets or read host files
  through ambient WASI. A tool or channel plugin with an `http_client` grant
  gets outbound `wasi:http` because those adapters opt in; it cannot listen.
  Channel plugins that must receive inbound traffic do not open a listener
  themselves: the host runs the listener and feeds messages through the
  `inbound` import, which the plugin drains from its `poll-message` export.
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
imports the host `logging` interface.

The host lives in `crates/zeroclaw-plugins/src/component.rs`. It holds one
async-enabled `wasmtime::Engine`, generates the world bindings with
`wasmtime::component::bindgen!` from `wit/v0`, and wires a sandboxed WASI p2
surface into each world's linker. Per-call host state (`PluginState`) carries a
`WasiCtx` built with no preopens and no network, plus the `ResourceTable` WASI
requires. The only host import wired into the linker is `logging`; a plugin's
ambient authority is therefore the sandboxed WASI context and nothing else (see
Host imports).

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
The channel host adapter (`WasmChannel`, its `wasi:http` gating, `configure`
jail, and host-fed `inbound` queue) is complete and unit-covered, and
`PluginHost::channel_plugin_details()` exposes the wasm-backed channel plugins
to register. Wiring those into the live orchestrator (the discovery-to-channel
loop in the runtime, plus a per-vendor host listener that drains its transport
into each channel's `inbound` queue) is the remaining seam and lands with the
runtime channel-registration change, not this host slice. The memory bridge
(`WasmMemory`) is in the same position one step earlier: the adapter implements
the full `Memory` trait against the `memory-plugin` world, but the host does not
yet expose a memory counterpart to `channel_plugin_details()` and the runtime
does not yet construct a `WasmMemory` as a configurable backend.

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
`config_read` and `http_client` have behavioral effect. `runtime.rs` passes a
tool plugin's resolved config section into `execute` only when the manifest
grants `config_read`, and strips any caller-supplied `__config` so the section
cannot be spoofed; a channel plugin receives the same section through its
`configure` export under the same rule. `http_client` is a necessary grant, not
a complete authority decision: the capability adapter must also construct the
HTTP context and link `wasi:http`. Tool and channel adapters opt in after grant
validation. The memory adapter deliberately does not, so granting
`http_client` to a memory scope alone adds no network surface. The remaining
variants (`file_read`, `file_write`, `memory_read`, `memory_write`) are accepted
by the manifest schema but are not yet wired to a host import: declaring them
grants nothing on its own. They reserve the names for the host functions that
will gate them (see Host imports below).

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
`tool-plugin` exports `tool`, `channel-plugin` exports `channel`,
`memory-plugin` exports `memory`. The required (no-default) exports for each
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
linked alongside `add_wasi` in `component.rs`). The `channel-plugin` world also
imports `inbound`, the host-fed message queue a channel drains from
`poll-message`. Tool and channel adapters link outbound `wasi:http` only after
the admitted scope grants `http_client` (`PluginStoreSpec::with_granted_http`
and `add_wasi_http` in `component.rs`). Memory withholds both the context and
linker surface. The filesystem and memory-access permissions remain inert: the
host functions that would gate them are not yet wired into the linker. A
plugin's ambient authority is the WASI context (no preopens, no ambient network)
plus exactly the host imports its grants and adapter opt-ins jointly enable.

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

### Per-plugin config (`__config`)

**Permission:** `config_read`

A plugin does not read process environment variables. For tool plugins the host
resolves the plugin's own config section (the per-entry `config` map under the
`plugins.entries` schema) and injects it into the `execute` input under the
reserved `__config` key, but only when the manifest grants `config_read`:

```json
{
  "prompt": "a sunset",
  "__config": { "api_key": "...", "base_url": "..." }
}
```

`runtime.rs` strips any caller-supplied `__config` before injecting the resolved
section, so the section cannot be spoofed, and withholds it entirely when the
permission is absent. Operators populate this section through the configuration
surfaces above (zerocode, the CLI, the gateway) rather than hand-editing a
file, with one current exception: a freshly installed plugin has no
`plugins.entries` entry yet, and `config set` cannot materialize a missing
natural-key entry, so the first entry must be added to the file by hand
(tracked in issue #8636). The section's keys are whatever the plugin's schema
declares. The field is marked secret, so CLI-written values encrypt at rest
under the adjacent `.secret_key`; hand-written plaintext values are also
accepted at load. A plugin only ever sees its own section.

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
over the canonical manifest bytes (the TOML with the `signature` and
`publisher_key` lines stripped); the publisher's public key is hex-encoded. The
host enforces one of three modes from `plugins.security.signature_mode`:

| Mode | Unsigned plugin | Untrusted or invalid signature |
|------|-----------------|--------------------------------|
| `strict` | rejected | rejected |
| `permissive` | loaded with a warning | loaded with a warning |
| `disabled` | loaded | not checked |

Verification runs at both discovery and install. Discovery skips a plugin that
fails its policy rather than aborting the whole host; install returns the error.

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

You rarely hand-edit TOML to configure a plugin. ZeroClaw exposes the plugin
config schema through every surface, and each surface writes the same underlying
state through the schema mirror (the one current exception: seeding a fresh
plugin's `plugins.entries` entry, per the note under Per-plugin config). Pick
whichever fits the moment:

- **zerocode** the interactive config editor. Walk to the plugins section and
  set fields with validation and inline help.
- **The CLI** for plugin lifecycle. `zeroclaw plugin` provides `list`, `search`,
  `install`, `remove`, `info`, and `migrate`. `zeroclaw config set` adjusts
  individual plugin config fields.
- **The web gateway** for a dashboard view. `GET /api/plugins` reports the
  loaded plugins and whether the system is enabled.
- **The plugin schema**, if you are the plugin author. Your config surface is
  defined by the schema, not by asking operators to write TOML. The host injects
  an author-defined config section into the plugin at call time (see Per-plugin
  config), so what an operator fills in is whatever your schema declares.

The schema mirror is what makes this work: the plugin config types in
`crates/zeroclaw-config/src/schema.rs` carry `#[prefix = "plugins"]`,
`#[prefix = "plugins.entries"]`, and `#[prefix = "plugins.security"]`, and the
`Configurable` derive turns each prefixed field into a path every surface reads
and writes. Secret fields (a plugin entry's `config` map is marked `#[secret]`)
encrypt at rest under the adjacent `.secret_key`. The canonical fields,
defaults, and the `signature_mode` values live in the
[Config reference](../reference/index.md); that schema is the source of truth,
not this page.

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
