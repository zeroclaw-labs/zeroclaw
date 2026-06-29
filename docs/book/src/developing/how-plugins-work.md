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
declares and the operator's policy allows, and nothing else.

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
3. **Validate shape.** Each manifest must declare at least one capability, and a
   non-skill plugin must name a `wasm_path` that exists. A malformed manifest is
   skipped with a warning, never loaded.
4. **Enforce signature policy.** Each plugin is checked against the configured
   `[plugins.security] signature_mode` and `trusted_publisher_keys`. A plugin
   that fails the policy is dropped from the loaded set, not surfaced as a tool.
5. **Register tools.** Surviving tool plugins are wrapped as agent tools. A
   plugin tool can never shadow a built-in tool; collisions are namespaced.

The signature stage is the one most easily misconfigured, so it is worth
understanding on its own.

## Signature policy

Every plugin manifest may carry an Ed25519 signature and the hex-encoded public
key of the publisher who signed it. The operator decides how strictly that
signature is enforced through `[plugins.security] signature_mode`:

| Mode | What loads | Use when |
|------|------------|----------|
| `disabled` | Every well-formed plugin, signed or not | Local development against plugins you built yourself |
| `permissive` | Unsigned plugins load; a present-but-invalid signature is rejected | Migrating toward signing without breaking existing installs |
| `strict` | Only plugins with a valid signature from a trusted publisher load | Any shared or production host |

In `strict` mode the manifest's `publisher_key` must appear in
`[plugins.security] trusted_publisher_keys`, and the signature must verify
against the canonical manifest bytes. A plugin that is unsigned, signed by an
untrusted key, or whose signature does not verify is dropped at discovery and
never becomes a tool. The default is `disabled` so a fresh local checkout works
without key management, but a host that loads plugins from anywhere you do not
control should run `strict`.

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

The host grants permissions narrowly: a permission the manifest does not declare
is a host function the plugin cannot reach. The HTTP-egress and per-plugin
configuration boundaries (SSRF-guarded egress that cannot reach loopback,
private, link-local, or cloud-metadata addresses or redirect into one; config
injected per-alias so a plugin reads only its own resolved values and never the
raw process environment or another plugin's secrets) are delivered by the
companion plugin-hardening work and are documented alongside those changes. This
page covers the signature-policy boundary.

## Configuration reference

```toml
[plugins]
# Master switch. Nothing loads while this is false.
enabled = true
# Where plugins are discovered (default: ~/.zeroclaw/plugins/).
plugins_dir = "~/.zeroclaw/plugins"
# Cap on how many plugins may load.
max_plugins = 32

[plugins.security]
# disabled | permissive | strict
signature_mode = "strict"
# Hex-encoded Ed25519 public keys allowed to publish plugins under strict mode.
trusted_publisher_keys = [
  "a1b2c3d4e5f6...",
]
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
- Secret-value isolation at the egress boundary (the host injecting credentials
  so the plugin learns only that a secret exists, never its value) is part of
  that same companion work; this PR does not add it.
- It cannot shadow or impersonate a built-in tool; the built-ins claim their
  names first and a colliding plugin tool is namespaced.

These bounds hold regardless of what the plugin's own code attempts, which is
what makes it safe to load a plugin you did not write, provided your signature
policy says you trust whoever published it.
