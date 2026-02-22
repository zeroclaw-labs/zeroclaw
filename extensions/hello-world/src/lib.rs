//! Hello World — example ZeroClaw plugin.
//!
//! Demonstrates the minimal plugin contract:
//! 1. Implement `Plugin` (manifest + register)
//! 2. In `register()`, use `PluginApi` to contribute tools and hooks
//!
//! To enable this plugin, add to `~/.zeroclaw/config.toml`:
//!
//! ```toml
//! [plugins]
//! enabled = true
//!
//! [plugins.entries.hello-world]
//! enabled = true
//! ```

use async_trait::async_trait;
use zeroclaw::hooks::{HookHandler, HookResult};
use zeroclaw::plugins::{Plugin, PluginApi, PluginManifest};
use zeroclaw::tools::traits::{Tool, ToolResult, ToolSpec};

// ── Manifest ─────────────────────────────────────────────────────────────────

fn manifest() -> PluginManifest {
    PluginManifest {
        id: "hello-world".into(),
        name: Some("Hello World".into()),
        description: Some("Example plugin demonstrating the ZeroClaw plugin API.".into()),
        version: Some("0.1.0".into()),
        config_schema: None,
    }
}

// ── Tool ─────────────────────────────────────────────────────────────────────

/// A simple tool that greets the user.
struct HelloTool;

#[async_trait]
impl Tool for HelloTool {
    fn name(&self) -> &str {
        "hello"
    }

    fn description(&self) -> &str {
        "Greet the user by name."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name to greet"
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("world");
        Ok(ToolResult {
            success: true,
            output: format!("Hello, {name}!"),
            error: None,
        })
    }
}

// ── Hook ─────────────────────────────────────────────────────────────────────

/// A hook that logs when a session starts.
struct HelloHook;

#[async_trait]
impl HookHandler for HelloHook {
    fn name(&self) -> &str {
        "hello-world:session-logger"
    }

    async fn on_session_start(&self, session_id: &str, channel: &str) {
        tracing::info!(
            plugin = "hello-world",
            session_id = %session_id,
            channel = %channel,
            "session started"
        );
    }
}

// ── Plugin ───────────────────────────────────────────────────────────────────

pub struct HelloWorldPlugin {
    manifest: PluginManifest,
}

impl HelloWorldPlugin {
    pub fn new() -> Self {
        Self {
            manifest: manifest(),
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
