# ZeroClaw Plugin System

A plugin architecture for ZeroClaw modeled after [OpenClaw's plugin system](https://github.com/openclaw/openclaw), adapted for Rust.

## Overview

The plugin system allows extending ZeroClaw with custom tools, hooks, channels, and providers without modifying the core codebase. Plugins are discovered from standard directories, loaded at startup, and registered with the host through a clean API.

## Architecture

### Key Components

1. **Manifest** (`zeroclaw.plugin.toml`): Declares plugin metadata (id, name, version, description)
2. **Plugin trait**: Defines the contract plugins must implement (`manifest()` + `register()`)
3. **PluginApi**: Passed to `register()` so plugins can contribute tools, hooks, etc.
4. **Discovery**: Scans bundled, global, and workspace extension directories
5. **Registry**: Central store managing loaded plugins, tools, hooks, and diagnostics
6. **Loader**: Orchestrates discovery → filtering → registration with error isolation

### Comparison to OpenClaw

| OpenClaw (TypeScript)              | ZeroClaw (Rust)                    |
|------------------------------------|------------------------------------|
| `openclaw.plugin.json`             | `zeroclaw.plugin.toml`             |
| `OpenClawPluginDefinition`         | `Plugin` trait                     |
| `OpenClawPluginApi`                | `PluginApi` struct                 |
| `PluginRegistry` (class)           | `PluginRegistry` struct            |
| `discover()` → `load()` → `register()` | `discover_plugins()` → `load_plugins()` |
| Try/catch isolation                | `catch_unwind()` panic isolation   |
| `[plugins]` config section         | `[plugins]` config section         |

## Writing a Plugin

### 1. Create the manifest

`extensions/hello-world/zeroclaw.plugin.toml`:

```toml
id = "hello-world"
name = "Hello World"
description = "Example plugin demonstrating the ZeroClaw plugin API."
version = "0.1.0"
```

### 2. Implement the Plugin trait

`extensions/hello-world/src/lib.rs`:

```rust
use zeroclaw::plugins::{Plugin, PluginApi, PluginManifest};
use zeroclaw::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;

pub struct HelloWorldPlugin {
    manifest: PluginManifest,
}

impl HelloWorldPlugin {
    pub fn new() -> Self {
        Self {
            manifest: PluginManifest {
                id: "hello-world".into(),
                name: Some("Hello World".into()),
                description: Some("Example plugin".into()),
                version: Some("0.1.0".into()),
                config_schema: None,
            },
        }
    }
}

impl Plugin for HelloWorldPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn register(&self, api: &mut PluginApi) -> anyhow::Result<()> {
        api.logger().info("registering hello-world plugin");
        api.register_tool(Box::new(HelloTool));
        api.register_hook(Box::new(HelloHook));
        Ok(())
    }
}

// Define your tool
struct HelloTool;

#[async_trait]
impl Tool for HelloTool {
    fn name(&self) -> &str { "hello" }
    fn description(&self) -> &str { "Greet the user" }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Name to greet" }
            },
            "required": ["name"]
        })
    }
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("world");
        Ok(ToolResult {
            success: true,
            output: format!("Hello, {name}!"),
            error: None,
        })
    }
}

// Define your hook
struct HelloHook;

#[async_trait]
impl zeroclaw::hooks::HookHandler for HelloHook {
    fn name(&self) -> &str { "hello-world:session-logger" }
    async fn on_session_start(&self, session_id: &str, channel: &str) {
        tracing::info!(plugin = "hello-world", session_id, channel, "session started");
    }
}
```

### 3. Register as a builtin plugin

For now, plugins must be compiled into the binary. In `src/gateway/mod.rs` or wherever plugins are initialized:

```rust
use zeroclaw::plugins::{load_plugins, Plugin};
use hello_world_plugin::HelloWorldPlugin;

let builtin_plugins: Vec<Box<dyn Plugin>> = vec![
    Box::new(HelloWorldPlugin::new()),
];

let registry = load_plugins(&config.plugins, workspace_dir, builtin_plugins);
```

### 4. Enable in config

`~/.zeroclaw/config.toml`:

```toml
[plugins]
enabled = true

[plugins.entries.hello-world]
enabled = true

[plugins.entries.hello-world.config]
greeting = "Howdy"  # Custom config passed to the plugin
```

## Configuration

### Master Switch

```toml
[plugins]
enabled = true  # Set to false to disable all plugin loading
```

### Allowlist / Denylist

```toml
[plugins]
allow = ["hello-world", "my-plugin"]  # Only load these (empty = all eligible)
deny = ["bad-plugin"]                 # Never load these
```

### Per-Plugin Config

```toml
[plugins.entries.my-plugin]
enabled = true

[plugins.entries.my-plugin.config]
api_key = "secret"
timeout_ms = 5000
```

Access in your plugin via `api.plugin_config()`:

```rust
fn register(&self, api: &mut PluginApi) -> anyhow::Result<()> {
    let cfg = api.plugin_config();
    let api_key = cfg.get("api_key").and_then(|v| v.as_str());
    // ...
}
```

## Discovery

Plugins are discovered from:

1. **Bundled**: Compiled-in plugins (registered directly in code)
2. **Global**: `~/.zeroclaw/extensions/`
3. **Workspace**: `<workspace>/.zeroclaw/extensions/`
4. **Custom**: Paths in `plugins.load_paths`

Each directory is scanned for subdirectories containing `zeroclaw.plugin.toml`.

## Error Isolation

Plugins are isolated from the host:

- Panics in `register()` are caught and recorded as diagnostics
- Errors returned from `register()` are logged and the plugin is marked as failed
- A bad plugin won't crash ZeroClaw

## Plugin API

### PluginApi Methods

- `register_tool(tool: Box<dyn Tool>)` — Add a tool to the registry
- `register_hook(handler: Box<dyn HookHandler>)` — Add a lifecycle hook
- `plugin_config() -> &toml::Value` — Access plugin-specific config
- `logger() -> &PluginLogger` — Get a logger scoped to this plugin

### Available Hooks

Implement `zeroclaw::hooks::HookHandler`:

- `on_session_start(session_id, channel)`
- `on_session_end(session_id, channel)`
- `on_tool_call(tool_name, args)`
- `on_tool_result(tool_name, result)`

## Future Extensions

- **Dynamic loading**: Load plugins from `.so`/`.dylib`/`.wasm` at runtime (currently requires compilation)
- **Hot reload**: Reload plugins without restarting ZeroClaw
- **Plugin marketplace**: Discover and install community plugins
- **Sandboxing**: Run untrusted plugins in isolated processes or WASM

## Testing

Run plugin system tests:

```bash
cargo test --lib plugins
```

## Example Plugins

See `extensions/hello-world/` for a complete working example.

## References

- [OpenClaw Plugin System](https://github.com/openclaw/openclaw/tree/main/src/plugins)
- [Issue #1414](https://github.com/zeroclaw-labs/zeroclaw/issues/1414)
