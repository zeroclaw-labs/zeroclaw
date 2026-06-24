//! Deterministic built-in tools available to the replay agent.
//!
//! Replay fixtures script tool *calls*; for the agent loop to dispatch them, a
//! tool of the same name must be registered. Phase 0 ships a small, side-effect-free
//! set sufficient for the bundled sample suite. Later phases wire the real tool
//! registry (sandboxed) for live evals.

use async_trait::async_trait;
use serde_json::json;
use zeroclaw_api::attribution::{Attributable, Role, ToolKind};
use zeroclaw_api::tool::{Tool, ToolResult};

/// Echoes its `message` argument back as the tool output.
pub struct EchoTool;

impl Attributable for EchoTool {
    fn role(&self) -> Role {
        Role::Tool(ToolKind::Plugin)
    }

    fn alias(&self) -> &str {
        "echo"
    }
}

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echoes the input message back as output"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "message": { "type": "string" }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let msg = args
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("(empty)")
            .to_string();
        Ok(ToolResult {
            success: true,
            output: msg,
            error: None,
        })
    }
}

/// The default tool set the Phase 0 replay agent is built with.
pub fn default_tools() -> Vec<Box<dyn Tool>> {
    vec![Box::new(EchoTool)]
}
