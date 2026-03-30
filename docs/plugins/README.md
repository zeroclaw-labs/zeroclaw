# Plugins

ZeroClaw supports **WASM plugins** that extend the agent with custom tools, capabilities, and integrations. Plugins run inside a sandboxed WebAssembly runtime with a capability-based security model -- they can only access the host features they explicitly declare and the operator explicitly approves.

## Why plugins?

| Concern | How plugins help |
|---|---|
| **Custom tools** | Ship domain-specific tools (e.g. CRM lookup, internal API wrappers) without forking the core |
| **Isolation** | Each plugin runs in its own WASM sandbox with no shared memory, no filesystem escape, and enforced timeouts |
| **Auditability** | Every capability a plugin uses is declared in its manifest and can be reviewed before installation |
| **Hot-reload** | Add, remove, or update plugins without restarting the agent |

## How it works

```
  your-plugin/
    manifest.toml      <-- declares name, tools, capabilities, permissions
    plugin.wasm         <-- compiled WASM binary
    plugin.wasm.sha256  <-- optional integrity sidecar
```

1. The operator places (or installs) plugin directories under the configured `plugins_dir`.
2. On startup (or on `zeroclaw plugin reload`), ZeroClaw discovers manifests, validates security policy, verifies WASM integrity, and loads approved plugins.
3. Plugin tools become available to the agent alongside built-in tools.
4. At runtime, host functions enforce capability boundaries -- a plugin that didn't declare `memory.write` simply cannot call `zeroclaw_memory_store`.

## Documentation map

| Page | Audience | What's covered |
|---|---|---|
| [Quickstart](quickstart.md) | Plugin authors | Create your first plugin in 10 minutes using the SDK |
| [Manifest Reference](manifest-reference.md) | Plugin authors | Every manifest field, capability, and permission explained |
| [SDK Reference](sdk-reference.md) | Plugin authors | Full API docs for `zeroclaw-plugin-sdk` |
| [Security Model](security.md) | Operators & authors | Security levels, signature verification, sandboxing, rate limits |
| [CLI Reference](cli-reference.md) | Operators | `zeroclaw plugin` subcommands for install, audit, doctor, reload |
| [REST API Reference](api-reference.md) | Developers | Gateway endpoints for managing plugins programmatically |

## Quick links

- SDK crate: `crates/zeroclaw-plugin-sdk/`
- Example plugin: `tests/plugins/sdk-example-plugin/`
- Plugin test suite: `tests/integration/plugin_*.rs`
- Config reference: [`[plugins]` section](../reference/api/config-reference.md)
