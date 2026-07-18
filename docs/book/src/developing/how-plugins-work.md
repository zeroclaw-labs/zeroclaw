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
3. **Validate shape.** Each manifest must declare at least one capability, its
   package directory must match its canonical name, and a non-skill plugin must
   name a confined relative `wasm_path`. Traversal and symlink paths are
   rejected. A malformed manifest is skipped with a warning, never loaded.
4. **Enforce signature policy.** Each plugin is checked against the configured
   `[plugins.security] signature_mode` and `trusted_publisher_keys`. A plugin
   that fails the policy is dropped from the loaded set, not surfaced as a tool.
5. **Admit executable bytes.** The host opens the confined component once,
   verifies any declared `wasm_sha256`, and retains those exact bytes. In
   `strict` mode the signed manifest must declare this digest. Adapters compile
   the admitted buffer rather than reopening its path.
6. **Register tools.** Surviving tool plugins are wrapped as agent tools and
   appended after the built-ins. Tool dispatch resolves names first-match, so a
   plugin tool that collides with a built-in name is never selected; give plugin
   tools unique names.

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

# Where plugins are discovered (default: ~/.zeroclaw/plugins).
zeroclaw config set plugins.plugins_dir ~/.zeroclaw/plugins

# disabled | permissive | strict
zeroclaw config set plugins.security.signature_mode strict

# Hex-encoded Ed25519 public keys allowed to publish plugins under strict mode.
zeroclaw config set plugins.security.trusted_publisher_keys '["a1b2c3d4e5f6..."]'
```

A host meant to load third-party plugins should set `enabled = true`,
`signature_mode = "strict"`, and list only the publisher keys you trust. A host
that runs only plugins you build yourself can leave `signature_mode` at its
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
- It cannot displace a built-in tool: the built-ins register first and tool
  dispatch resolves names first-match, so a colliding plugin tool is simply
  never selected.

The sandbox and namespace bounds hold regardless of what plugin code attempts.
The no-retention rule is instead part of the trusted channel-plugin contract,
which is why publisher review and signature policy still matter.
