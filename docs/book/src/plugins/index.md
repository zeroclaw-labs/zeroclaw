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
   every call gets a fresh fuel budget plus a wall-clock deadline that includes
   awaited host work. A plugin that loops forever or waits forever fails; it
   cannot hang the agent. Memory, table, and instance ceilings are enforced by
   a store limiter. All five bounds come from operator config
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

### Checking that a plugin still loads here

`wit/v0` is experimental, so a plugin built against a vendored copy that has
drifted from this host compiles cleanly and then fails to instantiate.
`zeroclaw plugin install` refuses such a plugin, but that gate cannot help one
installed before the gate existed, installed with `--no-verify`, or left in
place across a host upgrade. `zeroclaw plugin info <name>` always runs the same
instantiation check the daemon runs at startup and prints the verdict, the full
wasmtime cause chain, and the rebuild hint; it exits non-zero when the plugin
does not load, so a script can branch on it. `zeroclaw plugin list --verify`
runs the check for every installed package and annotates each row with the
verdict, or with the first line of the cause when it fails. Plain
`zeroclaw plugin list` is unchanged and still costs only a directory read,
because verifying compiles and instantiates every component. A skill bundle
ships no component, so it is reported as not applicable rather than as a
failure.

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
store, runs the call, and drops it. Channels and memory backends are stateful
by nature, so they hold one warm store for the plugin's lifetime; the host
refuels it before every call so a long-lived plugin gets a full budget per
call rather than draining over time. A deadline interruption discards the warm
store instead of resuming partially unwound guest state. Channels recreate the
instance on the next call; memory stays unavailable until its owner rebuilds it.
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
| `channel` | `WasmChannel`, complete and unit-covered | Alias-owned construction and runtime config resolution landed ([#10146](https://github.com/zeroclaw-labs/zeroclaw/pull/10146)); the per-vendor host listener that drains each transport into the channel's `inbound` queue is a follow-up |
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

# load auto-discovered tool and skill plugins at runtime (default: false)
zeroclaw config set plugins.auto_discover true

# where plugins are discovered (default: ~/.zeroclaw/plugins)
zeroclaw config set plugins.plugins_dir /srv/zeroclaw/plugins

# signature policy: disabled | permissive | strict
zeroclaw config set plugins.security.signature_mode strict

# per-call sandbox limits
zeroclaw config set plugins.limits.call_fuel 1000000000
zeroclaw config set plugins.limits.call_timeout_ms 30000
zeroclaw config set plugins.limits.max_memory_mb 256
```

`plugins.limits.max_connections_per_instance` (default 16) caps how many
outbound connections one logical plugin instance may hold open at once. The
ceiling is per instance, not per call: a response holds its connection until
the guest has drained the body or dropped the response, so a tool that keeps
sixteen responses alive inside one invocation cannot open a seventeenth
connection. Sequential requests that drain or drop each response as they go
never approach the limit. When the ceiling binds, the guest sees
`wasi:http`'s own `connection-limit-reached` error, not an egress denial: the
destination was granted, and the host is reporting a resource it counted.

`plugins.enabled = true` turns the plugin host on, but auto-discovered tool and
skill capabilities load only when `plugins.auto_discover = true` as well. That
flag is `false` by default (fail-closed), so `enabled = true` on its own gives
you the channels you declare under `[channels.plugin.<alias>]` and no plugin
tools or skills: a tool or skill package can list and `info` cleanly yet
contribute nothing at runtime. Explicit channel bindings are operator-named
rather than auto-discovered, so they do not need `auto_discover`; the flag gates
only auto-discovered tools and skills.

Per-instance settings live under `plugins.entries`, keyed by a versioned
`zpi1_…` string derived from the host-owned package, capability, and binding
identity. Installation prints and seeds the key for the package's default
tool binding. A channel instance's binding is its configured alias rather than
an invented package-name binding, so its key exists once an alias is bound:
`zeroclaw plugin bind <package> --channel-alias <alias>`, or
`zeroclaw plugin install <source> --channel-alias <alias>`, creates the
`[channels.plugin.<alias>]` binding and seeds that instance's row (see
[Binding a channel instance](#binding-a-channel-instance)). A daemon resolves
the typed config of an explicitly declared `[channels.plugin.<alias>]` instance
through that same key, the runtime path that landed in
[#10146](https://github.com/zeroclaw-labs/zeroclaw/pull/10146).
`zeroclaw plugin info <package>` prints every key the package owns: its tool
binding's and one per bound alias.
Full-identity keys let different packages and capability
worlds safely reuse aliases such as `main` without sharing credentials. The
canonical operator values are a secret-marked string map and
remain encrypted at rest (`enc2:…`). A plugin that requests `config_read`
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

State belongs to the instance identity (package name, capability, binding), the
same identity its config entry uses, not to a publisher. `zeroclaw plugin remove`
keeps both, so an upgrade (remove, then install) keeps its state. It also means a
different package installed later under the same name inherits that instance's
state and configured secrets. Removal keeps every `[channels.plugin.<alias>]`
binding that names the package too, so the newcomer is bound to those aliases and
their rows as well. `plugin remove` prints each kept row that still grants egress
and each binding it left behind, naming what to delete. Before installing an
unrelated plugin under a removed plugin's name, delete its `[[plugins.entries]]`
rows and those bindings, and treat its state as readable by the newcomer.

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
[Migrating to typed config](./migrating-to-typed-config.md) is the step-by-step
recipe, including the release decision to ship this enforcement without a
compatibility shim.

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

## Declaring and granting egress

A plugin's network reach is two separate facts, and only one of them is yours:

- The **declaration** is the manifest's `[egress]` table (`hosts = [...]`): the
  destinations the author says the plugin needs. It is part of the canonical
  manifest bytes, so a signed manifest covers it. It grants nothing. An
  unsigned component that writes its own `[egress]` table still reaches
  nothing.
- The **grant** is `plugins.entries.<instance-key>.egress_hosts` on the
  instance's own `zpi1_…` row, with the narrower `egress_allow_private`
  carveout beside it. This is the list the host enforces, read from live config
  on each request, so your edit applies without restarting the plugin.

On this release the grant alone governs. The host checks a destination against
`egress_hosts` and does not additionally require it to appear in the manifest,
so a declaration neither grants a host nor bounds a grant you authored. The
declaration's job here is to seed and to diff, described below. Narrowing
effective reach to the intersection of declaration and grant is later rollout
work tracked in
[#8850](https://github.com/zeroclaw-labs/zeroclaw/issues/8850).

An empty `egress_hosts` is the default and means no network reach at all: a
transport permission such as `http_client` grants the surface, this field
grants the destinations. Entries are exact hosts (`api.example.com`,
`10.0.0.5`) or explicit suffix patterns (`*.example.com`, which matches
subdomains but not the apex, so list the apex separately when you need it).
Ports are not part of an entry, granting a host grants every port on it, and
there is no `*` meaning "anywhere".

`egress_hosts` is deliberately a plaintext sibling of the encrypted `config`
map rather than a key inside it. The allowlist is the thing an operator audits,
so it stays readable in the same file being audited while the secrets beside it
remain `enc2:…`.

### What install and list do

`zeroclaw plugin install` is the one moment the two sides are reconciled for
the package's default tool binding without you typing anything (a channel
instance's row is created when you bind its alias, with a grant decision you
state on the command line, as described in
[Binding a channel instance](#binding-a-channel-instance)):

- For a row it **creates**, it seeds the declaration into `egress_hosts`,
  prints each destination it granted, and prints the `zeroclaw config set`
  command that edits the grant later.
- For a row that **already exists** (an upgrade, a reinstall, or a row you
  authored), it changes nothing. It prints the difference instead: destinations
  the manifest declares that the row does not grant, destinations the row
  grants that the manifest no longer declares, and the exact command that
  applies the addition. A package update therefore cannot widen its own network
  reach; you apply the difference deliberately.
- A reinstall that finds an unsupported **pre-1.0 package-name row** refuses
  before creating a canonical row and prints the same ordered update steps as
  `plugin list`. Like `plugin list`, it reports a deployment-wide refusal (a
  malformed `security.nat64_prefixes`, a zero
  `plugins.limits.max_connections_per_instance`) once, on its own, and prints
  no row steps that could not take effect until that is fixed. The failed
  install rolls back before it announces anything, and the old `config`,
  `egress_hosts`, and `egress_allow_private` values remain untouched until you
  update the beta configuration and retry.

The printed command carries the union of the existing grant and the
declaration, because `zeroclaw config set` replaces a list rather than
appending to it. Running it as printed adds the declared destinations without
dropping a host you authored yourself.

Every printed command also begins with `zeroclaw --config-dir '<dir>'`, naming
the configuration directory it was computed against. `--config-dir` (and the
`ZEROCLAW_CONFIG_DIR` it sets) only affects the process you pass it to, so a
command copied out of `zeroclaw --config-dir /srv/a plugin list` would
otherwise act on whichever configuration your shell resolves by default. The
`zpi1_…` row key names the package, capability and binding but not the
profile, so that command would replace a different profile's allowlist with a
list computed from this one. The directory, the host list, and every other
argument a command carries (a config path naming a schema property, a package,
an alias) are each quoted as one literal argument, so nothing a manifest
declares can be expanded or substituted by your shell; paste the command as
printed. The exceptions are the placeholders in a printed `plugin bind`
command: `<alias>`, which you replace with the alias you choose, and
`<declared|none>`, which you replace with your egress decision. Replace both
before you run it. On Linux and macOS, a placeholder left at the end of the
line is a shell syntax error, so that command runs nothing. An `<alias>` left
in front of a filled-in `--egress`, though, is read by the shell as two
redirections, from a file named `alias` and into a file named `--egress`: the
command then fails on the missing file, or runs with your decision taken as
the alias and no `--egress` flag, and the ceremony refuses it for the missing
decision.

The quoting follows the shell of the platform the command was printed on. On
Linux and macOS it is the POSIX single-quoted form (`sh`, `bash`, `zsh`,
`fish`), where an embedded quote is written `'\''`. On Windows, ZeroClaw
cannot tell whether you are in `cmd.exe` or PowerShell, so it prints the one
form both pass literally: each value in double quotes, which is correct to
paste into either shell as long as the value contains nothing either shell
expands inside double quotes. An ordinary host list and an ordinary profile
path, spaces included, always qualify. When a value does not (it contains `"`,
`%`, `!`, `$` or a backtick, or ends in a backslash; `$(id).example.com` is the
shape a hostile manifest would declare), the whole line is printed instead as
the marker `# PowerShell only, cmd.exe cannot pass this value literally:`
followed by a space and the PowerShell form, where an embedded quote is
doubled (`''`). Pasted whole, that
line runs nothing in either shell (`cmd.exe` cannot run `#`, and PowerShell
reads it as a comment); copy the command after the marker into PowerShell
alone. `cmd.exe` has no quoting that keeps such a value literal (`%name%`
expands inside its double quotes, and a single quote is an ordinary character
there), so it is never trusted with one.

`zeroclaw plugin list` repeats the same comparison as a standing diagnostic,
one instance at a time: the default tool binding of every installed tool plugin
holding `http_client`, and every bound channel alias whose package holds
`http_client`, `websocket_client`, or `socket_client`, printed as
`package (plugin.<alias>)`. For each, one line names the destinations it
declares that its row does not grant, since requests there are denied, plus the
command that closes the gap; for an instance with no row yet, that command
creates the row with the grant. A channel instance counts every transport the
egress authority governs because a channel that speaks only over a socket or a
WebSocket, such as IRC or MQTT, would otherwise never be reported; a tool row
keeps the `http_client` rule it shipped with. The reverse is never flagged. A
grant with no matching declaration is a first-class path, not a finding: the
plugin whose destination is deployment configuration (a self-hosted Gitea, a
LAN Nextcloud) cannot have that host declared by its author, so you author it.

When the instance's row still carries a pre-1.0 key (a package name rather than
the `zpi1_…` key), `plugin list` always prints the rename described above,
because the runtime resolves a grant by the `zpi1_…` key alone: whatever that
row grants is not in effect, and requests are denied, until it is renamed. If
the row's grant already covers everything the manifest declares, the rename is
the whole instruction: no `zeroclaw config set` is offered, since it would
only replace a list you already have right. If the declaration still names
destinations the row does not grant, the rename is step one and the grant
command is step two, and that command carries the row's existing grant forward
so it revokes nothing you authored. Dotted `plugins.entries.<key>.…` paths
only resolve rows already present in live config, so running the grant command
before the rename fails with `Unknown property`; applying the printed steps in
the printed order works. `plugin list` diagnoses this and never edits your
config itself. Only a default tool binding's row can be in this state: channel
instances postdate the pre-1.0 key format, so no channel row is reported for a
rename.

`plugin list` judges a row with the same policy constructor the runtime uses
at request time, so its verdict is the runtime's. If the runtime would refuse
the row (a single-label wildcard such as `*.com`, an entry with boundary
whitespace, or an `egress_allow_private` carve-out no granted host covers), the
config loader only warns about it, but the runtime refuses the whole allowlist
and denies every request. `plugin list` prints the runtime's reason and a grant
command built only from the entries the runtime accepts; on a pre-1.0 row that
command follows the rename, and the rename is never offered alone for a row the
runtime would refuse. The printed command replaces `egress_hosts` only. If a
private carve-out would still be refused afterwards, the report says so and
names the row's `egress_allow_private` path, since that has to be fixed by
hand. A refusal that is not about the row at all, a malformed
`security.nat64_prefixes` or a zero `plugins.limits.max_connections_per_instance`,
refuses every plugin's policy alike; `plugin list` reports it once, naming
those two paths, prints no per-plugin lines until it is fixed, and never
offers a per-plugin grant repair for it. `plugin install` and `plugin bind`
report an existing row with the same verdict, in the same words, so they and
list never disagree about whether a grant is usable.

A channel package with no bound alias has no instance yet, so `plugin list`
reports no channel row for it: there is no row to compare against, and install
does not invent one.

### Binding a channel instance

A channel package has no instance until a `[channels.plugin.<alias>]` table
names it, and the alias is yours to choose, so installing the package cannot
create a channel instance's row by itself. Binding an alias is the ceremony
that does:

```bash
zeroclaw plugin bind acme.chat --channel-alias operations --egress declared
```

`plugin bind` works on an installed package; it is also how you add a second
alias and how you repair a partial binding. `zeroclaw plugin install <source>
--channel-alias <alias>` runs the same ceremony as part of the install, and
takes the same `--egress` flag, which install accepts only together with
`--channel-alias`. The flag governs only the channel instance's row: a tool
row of the same package is seeded by install's own rule, as it always was.
There, every refusal below happens before the package is published; the tool
row, the binding, and the channel row are written in one save; and a failure
while writing them rolls the package back. An install of a channel package
without `--channel-alias` publishes it as before. If bindings already name
the package, as a `plugin remove` leaves them, it then prints each bound
alias's readiness report, described below; otherwise it prints the
`plugin bind` command to run, with an `<alias>` placeholder and, when the
manifest declares destinations the channel can reach, an
`--egress <declared|none>` placeholder for your decision. The ceremony never
prompts: every decision is a flag.

The command refuses, and writes nothing, when:

- the configuration file is on an older schema version; run
  `zeroclaw config migrate` first, as `zeroclaw config set` requires, since a
  save would stamp the current version onto sections still in the older
  shape;
- the package does not provide a channel;
- the alias is not 1 to 63 lowercase letters, digits, and single underscores,
  starting and ending with a letter or digit;
- the alias is already bound to a different package, which the refusal names;
  another binding's `package` is never rewritten;
- the configuration file, or its `[plugins]`, `[channels]`,
  `[channels.plugin]`, or `[channels.plugin.<alias>]` section, is malformed
  and was not loaded, because the command never writes over a section the
  loader dropped.

Otherwise it creates the binding, written with `package` and the default
`enabled = true`, or keeps one that already names the package exactly as it
is, `enabled = false` included. It then creates the instance's
`[[plugins.entries]]` row, when the row does not exist yet and the instance has
host-owned state to hold: a `config_schema`, a declared destination, or a
transport the egress authority governs (`http_client`, `websocket_client`, or
`socket_client`).

Creating that row from a declaration the instance can use takes a decision.
When the manifest declares destinations and the package holds a governed
transport:

- `--egress declared` seeds the declaration into `egress_hosts` and prints
  each destination with the command that edits the grant later, the same
  printout install gives a tool row.
- `--egress none` creates the row with an empty grant, so the instance has no
  network reach, and prints the declared destinations with the command that
  grants them later.
- Without either flag, the command refuses, lists the declared destinations,
  and names both flags. Nothing is written.

Without such a declaration no decision is needed: the row is created with an
empty grant, and if the package holds a governed transport but declares no
destinations, the command prints the one that grants the hosts your deployment
uses.

The flag only ever applies to a row the command creates. A row that already
exists, from an earlier run, written by you, or left behind by a removed
package, is never extended: the command prints the same difference install
prints for an existing row and, if you passed `--egress`, notes that it was not
applied. A script that always passes `--egress declared` therefore cannot widen
a grant. Each alias's row is seeded from the manifest alone, never copied from
another alias's row or from the package's tool row, and `egress_allow_private`
is never written. A second run with the same arguments leaves the configuration file
byte-identical, and a run that finds a binding without its row, or a row
without its binding, creates the missing half without touching the other.

After binding, the command reports whether the instance can start, reading
each answer from the source the runtime uses:

1. The permissions the instance holds, which are the ones its manifest
   requests. They are shown here, not decided: the grant you decide is the
   egress allowlist.
2. Each key the schema lists as `required`, set or missing. A missing key
   comes with the `config set plugins.entries.<key>.config.<name>` command
   that sets it, and a secret key's command prompts for the value without
   echoing it. For a property name the command line cannot address, the report
   names the row to edit in the config file instead. An instance whose row does
   not exist yet, such as a binding written by hand, gets the `plugin bind`
   command that creates the row in place of per-key commands, since
   `config set` resolves only rows that exist. When the manifest declares
   destinations the row can use, the report names them and the command ends in
   `--egress <declared|none>`, for your decision. Values are never printed.
3. The runtime config resolver's verdict on the row, which names the schema
   path or property that fails, never a value.
4. The egress gap, in the words `plugin list` uses. `plugin bind` and
   `plugin install --channel-alias` leave it out, because they have just
   reported the row's grant with its command.
5. The activation plan's own verdict, which names the first precondition the
   runtime finds unmet. It checks, in order, `plugins.enabled`, the binding's
   `enabled`, an enabled agent that lists `plugin.<alias>` in its `channels`,
   the package installed and providing a channel, and a free slot under
   `plugins.max_active_instances`. A disabled plugin system or binding
   comes with the command that enables it. A missing owner is described
   rather than given a command, because `config set` on an agent's `channels`
   list replaces the whole list. When every precondition holds, the plan
   admits the instance, and it starts at the next daemon start or reload if
   its component loads. If the runtime cannot build its activation plan at
   all, the report says so with the runtime's error.
6. A reminder that a running daemon starts the instance only after a restart
   or reload.

The ceremony never turns on `plugins.enabled` and never edits an agent's
`channels`: both stay your decisions. A binding that succeeds exits zero even
when the instance cannot start yet; only a refusal or a failed write exits
non-zero. `zeroclaw plugin info <package>` prints the same report after each
bound alias's key, and for a channel package with no bound alias it prints the
same `plugin bind` hint install does.

A binding whose alias no plugin instance can be named by, such as one holding
a control character, which configuration validation only warns about, does
not stop the package's other instances from being reported. `plugin list`,
`plugin info`, `plugin install`, and `plugin remove` print one line saying the
binding is skipped, with the alias escaped, and name the
`[channels.plugin.<alias>]` table to rename or remove.

## Where the trust boundary actually is

The sandbox bounds what a loaded plugin can do; the signature policy bounds
what loads at all. Both are operator decisions, and they compose:

- `plugins.enabled` false (the default): no plugin code runs, ever.
- `plugins.auto_discover` false (the default): auto-discovered tool and skill
  capabilities do not load. `plugins.enabled = true` alone activates only the
  channels you declare under `[channels.plugin.<alias>]`; tools and skills load
  only when `auto_discover = true` as well.
- Signature `strict`: only components whose manifest carries a valid Ed25519
  signature from a key in your trusted set load.
- Loaded plugin: bounded by fuel, memory ceilings, no-preopen WASI, and the
  permission-gated import set.

What the sandbox does *not* bound is the semantic behavior of a tool the model
chooses to call: a tool with the `http_client` grant and the tool adapter's HTTP
surface can send whatever the model passes it to wherever its code decides.
Signature policy exists because "which code do I load" is the decision that
matters most; make it deliberately.

### Certificate trust for plugin HTTPS

A plugin request over HTTPS verifies against the bundled webpki root program
plus the roots this machine already trusts. An endpoint whose certificate chains
to a locally installed CA, such as an enterprise MDM root or a private PKI,
therefore works for plugins exactly as it already works for provider requests.
Verification itself is unchanged: chain building and hostname matching stay in
force, and the egress policy still decides which destinations a guest may reach.

Sockets and WebSocket connections start from the same roots. A plugin that must
reach a service behind a private CA, or present a client certificate, names a
TLS profile the operator configured on its instance:

```toml
[[plugins.entries]]
name = "zpi1_…"                       # the instance key
egress_hosts = ["imap.corp.example.com"]

[[plugins.entries.tls_profiles]]
name = "corp"
hosts = ["imap.corp.example.com"]     # must be inside egress_hosts
system_roots = false                  # trust only the CA below
custom_ca_secret = "corp_ca"          # x-secret properties of the plugin's
client_certificate_secret = "cert"    # config schema holding PEM material
client_private_key_secret = "key"
```

A profile chooses certificates only. Its `hosts` must each be granted by
`egress_hosts`, which config validation checks, and a request that names it
still passes the ordinary grant first. The certificate material stays in the
instance's encrypted config; the profile fields are just the property names.
The host reads that material when it builds a connection, and the plugin cannot:
`secrets.get` refuses any property a profile names, so a client private key
never enters the guest.

Those roots are read once per process. Rewriting the certificate file at the same
path, or changing the operating system store, does not reach a running daemon;
restart it before expecting plugin HTTPS to see the change. The same applies to
an unlucky first read: a machine whose store was briefly unreadable serves
bundled-only trust until the process restarts, and the
`plugin_egress_trust_anchors` log line is what says which of the two happened.
