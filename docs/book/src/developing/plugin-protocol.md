---
type: reference
status: accepted
last-reviewed: 2026-06-22
relates-to:
  - FND-001
  - crates/zeroclaw-plugins
---

# Plugin Protocol

This document defines the protocol between ZeroClaw's plugin host and WASM tool
plugins.

## Runtime: wasmtime component model (WIT)

ZeroClaw plugins are **WebAssembly components** defined by the WIT interfaces
under [`wit/v0/`](https://github.com/zeroclaw-labs/zeroclaw/tree/master/wit/v0)
(package `zeroclaw:plugin@0.1.0`), compiled for `wasm32-wasip2`, and executed
directly on [`wasmtime`](https://wasmtime.dev). The earlier Extism string/JSON
bridge has been removed.

The runtime is **deny-by-default and resource-bounded**: each invocation runs in
a fresh component instance with fuel metering, an epoch-interruption wall-clock
deadline, and a memory limit; host capabilities a plugin was not granted resolve
to fail-closed stubs. See `wit/VERSIONING.md` for compatibility rules.

## Plugin structure

A plugin is a directory containing:

```
my-plugin/
  manifest.toml    # Plugin metadata, capabilities, permissions
  plugin.wasm      # Compiled WIT component (optional for skill-only plugins)
```

Plugins are discovered from the configured `plugins.plugins_dir` (default
`~/.zeroclaw/plugins/`).

### Skill-only plugin layout (markdown bundle)

A plugin whose only capability is `skill` ships skills under a `skills/`
directory in [agentskills.io](https://agentskills.io) format and omits
`wasm_path` (no WASM is involved):

```
my-toolkit/
  manifest.toml              # capabilities = ["skill"]
  skills/
    design-review/
      SKILL.md
```

Each `SKILL.md` must include YAML frontmatter with `name` and `description`;
invalid bundles are rejected at discovery. Skills register under
`plugin:<plugin-name>/<skill-name>` to avoid collisions.

## Manifest format

```toml
name = "wikipedia-summary"
version = "0.1.0"
description = "Look up a short summary of a topic from Wikipedia"
author = "ZeroClaw Labs"
wasm_path = "wikipedia_summary.wasm"
capabilities = ["tool"]
permissions = ["http_client"]
```

### Capabilities

| Value | Description |
|-------|-------------|
| `tool` | Provides a tool callable by the LLM |
| `skill` | Provides agentskills.io-format skills under `skills/`; no WASM payload |
| `channel`, `memory`, `observer` | Reserved; not yet implemented |

### Permissions

Each permission maps to a WIT `host` import. A permission the manifest does not
declare resolves to a deny-by-default host service, so the capability is
unreachable from the guest.

| Value | Host import | Description |
|-------|-------------|-------------|
| `http_client` | `http-request` | Make HTTP requests (host applies the SSRF allowlist and injects credentials) |
| `workspace_read` | `workspace-read` | Read workspace files (rooted, no `..`) |
| `secret_exists` | `secret-exists` | Check whether a named secret exists (never its value) |
| `tool_invoke` | `tool-invoke` | Invoke other agent tools by alias *(host wiring is a follow-up)* |
| `memory_read`, `memory_write` | — | Reserved |
| `env_read`, `file_read`, `file_write` | — | Deprecated; accepted for back-compat, grant nothing new |

The clock (`now-millis`) and structured logging (`log-record`) are always
available — they expose no secret surface.

### Credential injection

Secrets are **never exposed to the guest**. A plugin checks existence via
`secret_exists`; the *host* injects the actual value into matching outbound HTTP
requests at the egress boundary, declared by signed `[[credentials]]` entries:

```toml
permissions = ["http_client", "secret_exists"]

[[credentials]]
secret = "MY_API_KEY"            # resolved from [http_request].secrets or env
header = "Authorization"
value_template = "Bearer {secret}"
url_prefix = "https://api.example.com/"   # optional; default = any allowed URL
```

## The WIT contract

The component implements the `tool-plugin` world (`wit/v0/tool.wit`):

```wit
world tool-plugin {
    import logging;     // structured log-record
    import host;        // now-millis, workspace-read, http-request,
                        // tool-invoke, secret-exists
    export plugin-info; // plugin-name, plugin-version
    export tool;        // name, description, parameters-schema, execute
}
```

The `tool` interface a plugin exports:

```wit
record tool-result { success: bool, output: string, error: option<string> }

name: func() -> string;                 // tool name for LLM function calling
description: func() -> string;          // human-readable, for the LLM
parameters-schema: func() -> string;    // JSON Schema (object) for the args
execute: func(args: string) -> result<tool-result, string>;
```

The `host` interface a plugin may import (per its permissions):

```wit
now-millis: func() -> u64;
workspace-read: func(path: string) -> option<string>;
http-request: func(method, url, headers-json, body: option<list<u8>>,
                   timeout-ms: option<u32>) -> result<http-response, string>;
tool-invoke: func(alias: string, params-json: string) -> result<string, string>;
secret-exists: func(name: string) -> bool;
```

## Writing a plugin in Rust (`wit-bindgen`)

See the [zeroclaw-plugins registry](https://github.com/zeroclaw-labs/zeroclaw-plugins)
for complete, working examples (e.g. `wikipedia-summary` and `mastodon-post`).
Example plugins live in that repository, not the main tree, so the core stays
lean and plugins can be fetched and installed on demand.

```toml
# Cargo.toml
[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "0.46"
serde_json = "1"

[workspace]   # standalone crate, not part of the host workspace
```

```rust
wit_bindgen::generate!({
    world: "tool-plugin",
    path: "../../wit/v0",
    features: ["plugins-wit-v0"],
});

use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfoGuest;
use exports::zeroclaw::plugin::tool::{Guest as ToolGuest, ToolResult};
use zeroclaw::plugin::host;

struct MyPlugin;

impl PluginInfoGuest for MyPlugin {
    fn plugin_name() -> String { "my-plugin".into() }
    fn plugin_version() -> String { "0.1.0".into() }
}

impl ToolGuest for MyPlugin {
    fn name() -> String { "my_tool".into() }
    fn description() -> String { "Does something useful".into() }
    fn parameters_schema() -> String {
        r#"{"type":"object","properties":{"q":{"type":"string"}},"required":["q"]}"#.into()
    }
    fn execute(args: String) -> Result<ToolResult, String> {
        let v: serde_json::Value = serde_json::from_str(&args).map_err(|e| e.to_string())?;
        let q = v.get("q").and_then(|x| x.as_str()).ok_or("missing 'q'")?;
        // Call host capabilities, e.g. host::http_request(...).
        Ok(ToolResult { success: true, output: format!("got {q}"), error: None })
    }
}

export!(MyPlugin);
```

### Building

```sh
rustup target add wasm32-wasip2          # once
cargo build --target wasm32-wasip2 --release
```

`wasm32-wasip2` produces a component directly. Copy
`target/wasm32-wasip2/release/<crate>.wasm` next to your `manifest.toml`.

### Installing

```sh
zeroclaw plugin install /path/to/my-plugin/
# or: cp -r my-plugin/ ~/.zeroclaw/plugins/my-plugin/
```

## Configuration and build features

Enable the system via the `[plugins]` / `[plugins.security]` config sections
(see the [Config reference](../reference/config.md), including the
`signature_mode` enum). Plugin HTTP egress reuses the `[http_request]`
allowlist and named secrets.

The host needs a **compiler backend** to load components: build with
`plugins-wasm` **plus** `plugins-wasm-cranelift` (x86-64/aarch64) or
`plugins-wasm-pulley` (portable). `plugins-wasm` alone is runtime-only and will
refuse to compile components with a clear error.

## Known limitations

- `tool-invoke` host wiring (alias indirection from inside a plugin) is not yet
  connected; the capability is reserved.
- The manifest signature covers the manifest, not a digest of the `.wasm`; a
  signed manifest paired with a swapped component is a tracked follow-up.
