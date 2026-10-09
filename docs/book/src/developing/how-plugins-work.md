# How Plugins Work

This page explains the plugin system from an operator's point of view: how a
plugin is discovered, what it is allowed to do, and how the host keeps an
untrusted plugin contained. For the on-disk contract a plugin author
implements (manifest fields, bridge exports, host functions), see
[Plugin protocol](./plugin-protocol.md).

## The shape of the system

A plugin is a sandboxed WebAssembly module plus a manifest. The host loads it,
reads the capabilities and permissions it declares, and exposes its tools to
the agent only when the operator has turned the plugin system on. Nothing about
a plugin is implicit: a plugin gets exactly the capabilities its manifest
declares and the operator's policy allows, and nothing else. To build one
yourself, start with the [plugin guides](../plugins/index.md).

Three properties hold at every layer:

- **Disabled by default.** The plugin system does not load anything unless
  `[plugins] enabled = true`. A default build with no plugin configuration runs
  no plugin code.
- **Deny by default.** A plugin reaches a host capability (HTTP egress, config,
  memory) only by declaring the matching permission in its manifest. An
  undeclared capability is unreachable, not merely unused.
- **Verified by policy.** Whether an unsigned or untrusted plugin loads at all
  is the operator's decision, set once in config and enforced uniformly at
  discovery.

## Lifecycle of a plugin load

When the runtime builds its tool set, the plugin loader runs through these
stages in order. A plugin that fails an earlier stage never reaches a later
one.

1. **Gate.** If `[plugins] enabled` is false, the loader does nothing. This is
   the first and cheapest check.
2. **Discover.** The loader scans the resolved plugins directory
   (`[plugins] plugins_dir`, default `~/.zeroclaw/plugins/`) for subdirectories
   containing a `manifest.toml`.
3. **Validate shape.** Each manifest must declare at least one capability, and
   a non-skill plugin must name a confined relative `wasm_path`. Traversal and
   symlink paths are rejected. A malformed manifest is skipped with a warning,
   never loaded.
4. **Enforce signature policy.** Each plugin is checked against the configured
   `[plugins.security] signature_mode` and `trusted_publisher_keys`. A plugin
   that fails the policy is dropped from the loaded set, not surfaced as a tool.
5. **Admit executable bytes.** The host opens the confined component once,
   verifies any declared `wasm_sha256`, and retains those exact bytes. In
   `strict` mode the signed manifest must declare this digest. Adapters compile
   the admitted buffer rather than reopening its path. On Unix the host opens
   the package directory once and reaches the component from that handle one
   path component at a time, never following a symlink, so moving or replacing
   a directory during admission either fails it or leaves the read inside the
   package that was confined. The component must be a regular file, and a FIFO
   or device fails at once instead of blocking. The package directory and the
   manifest are still found by pathname when admission starts; in `strict`
   mode the signed `wasm_sha256` is what binds the manifest to the bytes. Other
   platforms keep pathname checks, which refuse a replacement still in place
   when they run but do not make the lookup atomic or keep a FIFO from blocking
   the open.
6. **Register tools.** Surviving tool plugins are wrapped as agent tools and
   appended after the built-ins. A plugin whose package name or tool name
   conflicts with an already registered tool is refused with a warning instead
   of being registered; see [Tool name conflicts](#tool-name-conflicts).
   Tool and skill plugins are *auto-discovered*, so this
   enumeration happens only when `[plugins] auto_discover = true` (default
   `false`, fail-closed): with `enabled = true` but `auto_discover = false`, no
   plugin tools or skills load, though channels you declare under
   `[channels.plugin.<alias>]` still activate. The skill loader applies the same
   `auto_discover` gate.

The signature stage is the one most easily misconfigured, so it is worth
understanding on its own.

## Signature policy

Every plugin manifest may carry an Ed25519 signature and the hex-encoded public
key of the publisher who signed it. The operator decides how strictly that
signature is enforced through `[plugins.security] signature_mode`:

| Mode | What loads | Use when |
|------|------------|----------|
| `disabled` | Every well-formed plugin, signed or not | Local development against plugins you built yourself |
| `permissive` | Every well-formed plugin; unsigned, untrusted, and invalid signatures load with a warning | Migrating toward signing without breaking existing installs |
| `strict` | Only plugins with a valid signature from a trusted publisher load | Any shared or production host |

In `strict` mode the manifest's `publisher_key` must appear in
`[plugins.security] trusted_publisher_keys`, and the signature must verify
against the canonical manifest bytes. Executable plugins must also declare a
signed `wasm_sha256` matching the exact admitted bytes. A plugin that fails any
of these checks is dropped at discovery and never becomes a tool. The default
is `disabled` so a fresh local checkout works without key management, but a
host that loads plugins from anywhere you do not control should run `strict`.

This policy is enforced uniformly: the same check that the host applies when you
list plugins is the check the agent runtime applies when it builds the tool set,
so a plugin you cannot see in `strict` mode is also a plugin the agent cannot
call.

## Tool name conflicts

Registration refuses a name conflict instead of letting one tool shadow
another. Tool plugins register in package-name order, and the host checks each
one twice:

1. **Package name.** A plugin whose package name (the manifest `name`) matches
   an already registered tool is refused before its component is instantiated.
   The host logs a `WARN` event with `error_key`
   `plugin_package_name_conflict` in its `attributes`.
2. **Tool name.** The guest declares its own tool name, so the host learns it
   only by instantiating the component to read its metadata. A plugin whose
   tool name matches an already registered tool is not registered. The host
   logs a `WARN` event with `error_key` `plugin_tool_name_conflict` in its
   `attributes`.

The names checked are the tools that registry build has already registered,
including plugin tools accepted earlier in the same pass, plus
`execute_pipeline` when `[pipeline] enabled = true`. This is not a fixed list
of every built-in name: a built-in that a build does not register, for example
because its config section is disabled, is not reserved in that build. Tools
that join the registry after plugin registration are outside this check. Give
plugin packages and tools names that are unique outright rather than relying
on it.

## Capabilities and permissions

A manifest declares two separate things, and the distinction matters.

- **Capabilities** are what kind of extension the plugin is: `tool`, `channel`,
  `memory`, `observer`, or `skill`. A `tool` plugin contributes tools the LLM
  can call.
- **Permissions** are what host services the plugin's code may reach at runtime:
  HTTP egress, configuration, memory. A permission the manifest does not declare
  is a host function the plugin cannot reach.

The host grants permissions narrowly: a permission the manifest does not
declare is a host function the plugin cannot reach. Config is resolved from a
host-issued instance identity, so a plugin cannot select another package or
binding and never reads the raw process environment. `http_client` gates the
outbound `wasi:http` surface; the shared SSRF-guarded egress policy remains
companion plugin-hardening work. This page covers the signature-policy
boundary.

## Configuration reference

All settings live under the `plugins.*` config paths and are set through any
config surface (zerocode, the gateway, or the CLI):

```bash
# Master switch. Nothing loads while this is false.
zeroclaw config set plugins.enabled true

# Load auto-discovered tool and skill plugins at runtime (default: false).
# Without this, `enabled = true` activates only explicitly-declared channels.
zeroclaw config set plugins.auto_discover true

# Where plugins are discovered (default: ~/.zeroclaw/plugins).
zeroclaw config set plugins.plugins_dir ~/.zeroclaw/plugins

# disabled | permissive | strict
zeroclaw config set plugins.security.signature_mode strict

# Hex-encoded Ed25519 public keys allowed to publish plugins under strict mode.
zeroclaw config set plugins.security.trusted_publisher_keys '["a1b2c3d4e5f6..."]'
```

A host meant to load third-party plugins should set `enabled = true`,
`signature_mode = "strict"`, and list only the publisher keys you trust. To load
auto-discovered tool and skill plugins as well, also set `auto_discover = true`;
it is `false` by default, so `enabled = true` alone activates only the channels
you declare under `[channels.plugin.<alias>]` and no plugin tools or skills. A
host that runs only plugins you build yourself can leave `signature_mode` at its
`disabled` default during development and tighten it before the host is shared.

## What a plugin still cannot do

Even with every permission granted, the sandbox bounds a plugin:

- It runs as a WebAssembly module with no ambient access to the host process or
  the filesystem outside its rooted workspace. Network egress is gated by the
  HTTP permission; the SSRF-guarded egress boundary itself is delivered by the
  companion plugin-hardening work.
- A trusted tool or channel plugin can read a schema-designated secret's
  plaintext through its scoped `secrets.get` import during an authorized
  service call. Tools receive access during `execute`. Channels receive
  `config.get` and `secrets.get` during `configure` and operational calls; reads
  within one call use one canonical revision, so a same-binding public/secret
  rotation is available on the next operation. Instantiation and static
  metadata discovery cannot use either import. The host prevents public config
  injection and cross-instance selection, but a plaintext-returning import
  cannot prevent a malicious guest from retaining what it reads. Compliant
  channel plugins must resolve config and credentials at each point of use.
- It cannot take the name of an already registered tool. Registration refuses
  the conflicting plugin instead of registering its tool, within the bounds
  described in [Tool name conflicts](#tool-name-conflicts).

The sandbox and namespace bounds hold regardless of what plugin code attempts.
The no-retention rule is instead part of the trusted channel-plugin contract,
which is why publisher review and signature policy still matter.
