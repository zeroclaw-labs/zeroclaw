---
type: reference
status: accepted
last-reviewed: 2026-06-21
relates-to:
  - FND-001
  - ADR-003
  - crates/zeroclaw-plugins
---

# Plugin Protocol

This document defines the protocol between ZeroClaw's plugin host and WASM
plugin modules.

## Implementation status

The current runtime bridge is transitional. `crates/zeroclaw-plugins` still supports the Extism-era string/JSON tool protocol documented below so existing prototype plugins can keep running while the WIT host lands.

The accepted target architecture is the next contract: #6943 and FND-001 move ZeroClaw plugins to WIT-defined WASI components, compiled for `wasm32-wasip2` and hosted through direct `wasmtime`. The WIT interfaces live under `wit/v0/`; see `wit/VERSIONING.md` for the current compatibility rules.

Treat new plugin-host design work as WIT / `wasm32-wasip2` work. Additions to the Extism-specific protocol should be limited to compatibility fixes needed during the migration.

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
      "version": "0.2.0",
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
  manifest.toml              # capabilities = ["skill"]
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

### Capabilities

| Value | Description |
|-------|-------------|
| `tool` | Provides tools callable by the LLM |
| `channel` | Provides a communication channel (not yet implemented) |
| `memory` | Provides a memory backend (not yet implemented) |
| `observer` | Provides an observability backend (not yet implemented) |
| `skill` | Provides one or more agentskills.io-format skills under `skills/`; no WASM payload |

### Permissions

| Value | Description |
|-------|-------------|
| `http_client` | Can make HTTP requests via `zc_http_request` |
| `config_read` | Receives its own resolved per-plugin config section in the `execute` input under `__config` |
| `file_read` | Can read files (not yet implemented) |
| `file_write` | Can write files (not yet implemented) |
| `memory_read` | Can read agent memory (not yet implemented) |
| `memory_write` | Can write agent memory (not yet implemented) |

## Current Extism bridge exports

The exports below describe the current Extism bridge. WIT components use the interfaces in `wit/v0/`.

### `tool_metadata`

**Signature:** `(String) -> String`

Called once at plugin load time to retrieve tool metadata. The input string is
ignored (pass empty string). Returns JSON:

```json
{
  "name": "my_tool",
  "description": "What this tool does",
  "parameters_schema": {
    "type": "object",
    "required": ["prompt"],
    "properties": {
      "prompt": {
        "type": "string",
        "description": "The input prompt"
      }
    }
  }
}
```

The `parameters_schema` follows JSON Schema format and is presented to the LLM
for tool calling.

### `execute`

**Signature:** `(String) -> String`

Called each time the tool is invoked. Input is JSON matching the
`parameters_schema`. Returns JSON:

```json
{
  "success": true,
  "output": "Result text shown to the LLM",
  "error": null
}
```

On failure:

```json
{
  "success": false,
  "output": "",
  "error": "Description of what went wrong"
}
```

## Host functions

Host functions are provided by the ZeroClaw runtime and callable from within
the WASM plugin. Each is gated on a manifest permission, calling without the
required permission returns an error.

### `zc_http_request`

**Permission:** `http_client`

**Input:** JSON string

```json
{
  "method": "POST",
  "url": "https://api.example.com/v1/generate",
  "headers": {
    "Authorization": "Bearer token123",
    "Content-Type": "application/json"
  },
  "body": "{\"prompt\": \"hello\"}"
}
```

Supported methods: `GET`, `POST`, `PUT`, `DELETE`, `PATCH`, `HEAD`.
Timeout: 120 seconds.

**Output:** JSON string

```json
{
  "status": 200,
  "body": "{\"result\": \"world\"}",
  "headers": {
    "content-type": "application/json"
  }
}
```

### Per-plugin config (`__config`)

**Permission:** `config_read`

A plugin does not read process environment variables. The host resolves the
plugin's own config section from `[plugins.entries.<alias>]` and injects it into
the `execute` input under the reserved `__config` key:

```json
{
  "prompt": "a sunset",
  "__config": { "api_key": "...", "base_url": "..." }
}
```

Operators set these through the schema-mirror override grammar, e.g.
`ZEROCLAW_plugins__entries__<alias>__config__api_key=$FAL_KEY`. Values are
secret and encrypt at rest under the adjacent `.secret_key`. A plugin only ever
sees its own section.

## Writing a plugin in Rust for the current bridge

This section describes the current Extism bridge.

### Dependencies

The `[workspace]` table is needed to prevent Cargo from searching for a parent
workspace.

### Declaring host functions

```rust
use extism_pdk::*;

#[host_fn]
extern "ExtismHost" {
    fn zc_http_request(input: String) -> String;
}
```

Call them with `unsafe { zc_http_request(json_string)? }`. Per-plugin config
arrives in the `execute` input under `__config`; read it from the parsed input
rather than through a host call.

### Implementing exports

```rust
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    Ok(serde_json::to_string(&serde_json::json!({
        "name": "my_tool",
        "description": "Does something useful",
        "parameters_schema": {
            "type": "object",
            "required": ["input"],
            "properties": {
                "input": { "type": "string" }
            }
        }
    }))?)
}

#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;
    let input_val = args["input"].as_str().unwrap_or("");

    // ... do work, call host functions as needed ...

    Ok(serde_json::to_string(&serde_json::json!({
        "success": true,
        "output": format!("Processed: {input_val}")
    }))?)
}
```

### Building

For the current Extism bridge, compile the plugin as a WASI Preview 1 module:

<div class="os-tabs-src">

#### sh

```sh
# Install the WASM target (once)
rustup target add wasm32-wasip1

# Build
cargo build --target wasm32-wasip1 --release
```

</div>

The output `.wasm` file is at
`target/wasm32-wasip1/release/<crate_name>.wasm`. Copy it alongside your
`manifest.toml`.

For the accepted WIT / Component Model target, compile plugin components for `wasm32-wasip2` and use `wit/v0/` as the interface source.

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

Enable the plugin system via the `plugins` and `plugins.security` sections (gateway, zerocode, or `zeroclaw config set`): see the [Config reference](../reference/config.md) for all fields, defaults, and the `signature_mode` enum.

The `plugins-wasm` feature enables the current plugin runtime integration. The direct `wasmtime` host path uses `plugins-wasm-runtime-only`, `plugins-wasm-cranelift`, and `plugins-wasm-pulley` to choose the smallest execution strategy that matches the target platform.
