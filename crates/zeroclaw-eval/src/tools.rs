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

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::attribution::{Attributable, Role, ToolKind};

    #[test]
    fn echo_tool_name_and_alias() {
        let t = EchoTool;
        assert_eq!(t.name(), "echo");
        assert_eq!(t.alias(), "echo");
    }

    #[test]
    fn echo_tool_role_is_plugin() {
        let t = EchoTool;
        assert_eq!(t.role(), Role::Tool(ToolKind::Plugin));
    }

    #[test]
    fn echo_tool_description_is_non_empty() {
        assert!(!EchoTool.description().is_empty());
    }

    #[test]
    fn echo_tool_parameters_schema_has_message_property() {
        let schema = EchoTool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["message"].is_object());
    }

    #[tokio::test]
    async fn echo_tool_execute_returns_message() {
        let args = serde_json::json!({ "message": "hello world" });
        let result = EchoTool
            .execute(args)
            .await
            .expect("execute should succeed");
        assert!(result.success);
        assert_eq!(result.output, "hello world");
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn echo_tool_execute_missing_message_uses_default() {
        let args = serde_json::json!({});
        let result = EchoTool
            .execute(args)
            .await
            .expect("execute should succeed");
        assert!(result.success);
        assert_eq!(result.output, "(empty)");
    }

    #[test]
    fn default_tools_contains_echo() {
        let tools = default_tools();
        assert!(!tools.is_empty());
        assert!(tools.iter().any(|t| t.name() == "echo"));
    }
}
