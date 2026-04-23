---
type: reference
status: accepted
last-reviewed: 2026-04-19
relates-to:
  - ADR-003
  - crates/zeroclaw-plugins
---

# Plugin Protocol

This document defines the protocol between ZeroClaw's plugin host and WASM
plugin modules.

## Plugin structure

A plugin is a directory containing:

```
my-plugin/
  manifest.toml    # Plugin metadata and permissions
  plugin.wasm      # Compiled WASM module
```

Plugins are discovered from `~/.zeroclaw/plugins/` (configurable via
`plugins.plugins_dir` in config).

## Manifest format

```toml
name = "my-plugin"                    # Unique identifier (required)
version = "0.1.0"                     # Semver version (required)
description = "What this plugin does" # Human-readable (optional)
author = "Your Name"                  # Author (optional)
wasm_path = "plugin.wasm"             # Path to .wasm relative to manifest (required)
capabilities = ["tool"]               # What the plugin provides (required)
permissions = ["http_client"]          # What the plugin needs (optional)
signature = "base64url..."            # Ed25519 signature (optional)
publisher_key = "hex..."              # Publisher public key (optional)
```

### Capabilities

| Value | Description |
|-------|-------------|
| `tool` | Provides tools callable by the LLM |
| `channel` | Provides a communication channel (not yet implemented) |
| `memory` | Provides a memory backend (not yet implemented) |
| `observer` | Provides an observability backend (not yet implemented) |

### Permissions

| Value | Description |
|-------|-------------|
| `http_client` | Can make HTTP requests via `zc_http_request` |
| `env_read` | Can read environment variables via `zc_env_read` |
| `file_read` | Can read files (not yet implemented) |
| `file_write` | Can write files (not yet implemented) |
| `memory_read` | Can read agent memory (not yet implemented) |
| `memory_write` | Can write agent memory (not yet implemented) |

## Required WASM exports

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
the WASM plugin. Each is gated on a manifest permission — calling without the
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

### `zc_env_read`

**Permission:** `env_read`

**Input:** Environment variable name (plain string, not JSON).

**Output:** Environment variable value (plain string). Returns an error if the
variable is not set.

## Writing a plugin in Rust

### Dependencies

```toml
[package]
name = "my-plugin"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
extism-pdk = "1.4"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

[workspace]
```

The `[workspace]` table is needed to prevent Cargo from searching for a parent
workspace.

### Declaring host functions

```rust
use extism_pdk::*;

#[host_fn]
extern "ExtismHost" {
    fn zc_http_request(input: String) -> String;
    fn zc_env_read(input: String) -> String;
}
```

Call them with `unsafe { zc_http_request(json_string)? }`.

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

```bash
# Install the WASM target (once)
rustup target add wasm32-wasip1

# Build
cargo build --target wasm32-wasip1 --release
```

The output `.wasm` file is at
`target/wasm32-wasip1/release/<crate_name>.wasm`. Copy it alongside your
`manifest.toml`.

### Installing

```bash
# Copy to plugin directory
zeroclaw plugin install /path/to/my-plugin/

# Or manually
cp -r my-plugin/ ~/.zeroclaw/plugins/my-plugin/
```

## Reference implementation

See `plugins/image-gen-wasm/` in the repository for a complete example that
generates images via the fal.ai API using `zc_http_request` and `zc_env_read`.

## Configuration

Enable the plugin system in your ZeroClaw config:

```toml
[plugins]
enabled = true
plugins_dir = "~/.zeroclaw/plugins"
auto_discover = true

[plugins.security]
signature_mode = "disabled"  # or "permissive", "strict"
trusted_publisher_keys = []
```

The `plugins-wasm` feature flag must be enabled at compile time (included in the
default `ci-all` feature set).
