//! Built-in tool exposing MCP prompts (`list` / `get`) across all servers.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::mcp_client::McpRegistry;
use zeroclaw_api::tool::{Tool, ToolResult};

/// Generic MCP prompt access tool. Routes through `McpRegistry`.
pub struct McpPromptsTool {
    registry: Arc<McpRegistry>,
}

impl McpPromptsTool {
    pub fn new(registry: Arc<McpRegistry>) -> Self {
        Self { registry }
    }

    fn ok(output: String) -> ToolResult {
        ToolResult {
            success: true,
            output,
            error: None,
        }
    }
    fn fail(msg: impl Into<String>) -> ToolResult {
        ToolResult {
            success: false,
            output: String::new(),
            error: Some(msg.into()),
        }
    }
}

zeroclaw_api::tool_attribution!(
    McpPromptsTool,
    ::zeroclaw_api::attribution::ToolKind::Plugin
);

#[async_trait]
impl Tool for McpPromptsTool {
    fn name(&self) -> &str {
        "mcp_prompts"
    }

    fn description(&self) -> &str {
        "List or get prompts exposed by connected MCP servers. \
         action=list [server,cursor] returns available prompts (names are \
         prefixed `<server>__<name>`); action=get name=<prefixed-name> \
         arguments={...} returns the resolved prompt messages."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["list", "get"] },
                "server": { "type": "string", "description": "Filter list to one server." },
                "cursor": { "type": "string", "description": "Pagination cursor for list; requires `server` (per-server opaque token)." },
                "name": { "type": "string", "description": "Prefixed prompt name for get." },
                "arguments": { "type": "object", "description": "Prompt arguments for get." }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let mut map = match args {
            serde_json::Value::Object(m) => m,
            _ => return Ok(Self::fail("arguments must be an object")),
        };
        map.remove("approved");

        let action = match map.get("action").and_then(|v| v.as_str()) {
            Some(a) => a.to_string(),
            None => return Ok(Self::fail("missing required `action` (list|get)")),
        };

        match action.as_str() {
            "list" => {
                let server_filter = map.get("server").and_then(|v| v.as_str());
                let cursor = map
                    .get("cursor")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                match (server_filter, cursor) {
                    // Single-server pagination: cursor is an opaque per-server
                    // token, so it is only meaningful with an explicit `server`.
                    (Some(server), cursor) => {
                        match self.registry.list_server_prompts(server, cursor).await {
                            Ok((defs, next_cursor)) => {
                                let body = json!({ "prompts": defs, "next_cursor": next_cursor });
                                match serde_json::to_string_pretty(&body) {
                                    Ok(s) => Ok(Self::ok(s)),
                                    Err(e) => {
                                        Ok(Self::fail(format!("failed to serialize prompts: {e}")))
                                    }
                                }
                            }
                            Err(e) => Ok(Self::fail(e.to_string())),
                        }
                    }
                    // Cross-server aggregate has no well-defined single cursor.
                    (None, Some(_)) => Ok(Self::fail(
                        "`cursor` requires a `server` (pagination is per-server); \
                         omit `cursor` for an all-server list",
                    )),
                    (None, None) => {
                        let all = self.registry.list_all_prompts().await;
                        let defs: Vec<_> = all.into_iter().map(|(_, def)| def).collect();
                        match serde_json::to_string_pretty(&defs) {
                            Ok(s) => Ok(Self::ok(s)),
                            Err(e) => Ok(Self::fail(format!("failed to serialize prompts: {e}"))),
                        }
                    }
                }
            }
            "get" => {
                let name = match map.get("name").and_then(|v| v.as_str()) {
                    Some(n) if !n.is_empty() => n.to_string(),
                    _ => return Ok(Self::fail("`get` requires a non-empty `name`")),
                };
                let arguments = map.get("arguments").cloned().unwrap_or_else(|| json!({}));
                match self.registry.get_prompt(&name, arguments).await {
                    Ok(result) => match serde_json::to_string_pretty(&result) {
                        Ok(s) => Ok(Self::ok(s)),
                        Err(e) => Ok(Self::fail(format!("failed to serialize prompt: {e}"))),
                    },
                    Err(e) => Ok(Self::fail(e.to_string())),
                }
            }
            other => Ok(Self::fail(format!(
                "unknown action `{other}` (expected list|get)"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    async fn empty_registry() -> Arc<McpRegistry> {
        Arc::new(McpRegistry::connect_all(&[]).await.unwrap())
    }

    #[tokio::test]
    async fn missing_action_is_non_fatal_error() {
        let tool = McpPromptsTool::new(empty_registry().await);
        let res = tool.execute(json!({})).await.unwrap();
        assert!(!res.success);
        assert!(res.error.unwrap().contains("action"));
    }

    #[tokio::test]
    async fn get_without_name_is_non_fatal_error() {
        let tool = McpPromptsTool::new(empty_registry().await);
        let res = tool.execute(json!({ "action": "get" })).await.unwrap();
        assert!(!res.success);
        assert!(res.error.unwrap().to_lowercase().contains("name"));
    }

    #[tokio::test]
    async fn get_strips_approved_field() {
        let tool = McpPromptsTool::new(empty_registry().await);
        let res = tool
            .execute(json!({ "action": "get", "name": "srv__p", "approved": true }))
            .await
            .unwrap();
        assert!(!res.success);
        assert!(!res.error.unwrap().to_lowercase().contains("approved"));
    }

    #[tokio::test]
    async fn list_cursor_without_server_is_rejected() {
        let tool = McpPromptsTool::new(empty_registry().await);
        let res = tool
            .execute(json!({ "action": "list", "cursor": "abc" }))
            .await
            .unwrap();
        assert!(!res.success);
        let err = res.error.unwrap();
        assert!(err.contains("cursor"), "got: {err}");
        assert!(err.contains("server"), "got: {err}");
    }

    #[tokio::test]
    async fn list_cursor_with_unknown_server_reaches_server_path() {
        let tool = McpPromptsTool::new(empty_registry().await);
        let res = tool
            .execute(json!({ "action": "list", "server": "ghost", "cursor": "abc" }))
            .await
            .unwrap();
        assert!(!res.success);
        assert!(
            res.error.unwrap().contains("unknown MCP server"),
            "cursor+server must take the per-server path"
        );
    }

    #[tokio::test]
    async fn name_is_stable() {
        let tool = McpPromptsTool::new(empty_registry().await);
        assert_eq!(tool.name(), "mcp_prompts");
    }
}
