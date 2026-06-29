//! Built-in tool exposing MCP resources (`list` / `read`) across all servers.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::mcp_client::McpRegistry;
use zeroclaw_api::tool::{Tool, ToolResult};

/// Generic MCP resource access tool. Routes through `McpRegistry`.
pub struct McpResourcesTool {
    registry: Arc<McpRegistry>,
}

impl McpResourcesTool {
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
    McpResourcesTool,
    ::zeroclaw_api::attribution::ToolKind::Plugin
);

#[async_trait]
impl Tool for McpResourcesTool {
    fn name(&self) -> &str {
        "mcp_resources"
    }

    fn description(&self) -> &str {
        "List or read resources exposed by connected MCP servers. \
         action=list [server,cursor] returns available resources (uris are \
         prefixed `<server>__<uri>`); action=read uri=<prefixed-uri> returns \
         the resource contents."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["list", "read"] },
                "server": { "type": "string", "description": "Filter list to one server." },
                "cursor": { "type": "string", "description": "Pagination cursor for list; requires `server` (per-server opaque token)." },
                "uri": { "type": "string", "description": "Prefixed resource uri for read." }
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
            None => return Ok(Self::fail("missing required `action` (list|read)")),
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
                        match self.registry.list_server_resources(server, cursor).await {
                            Ok((defs, next_cursor)) => {
                                let body = json!({ "resources": defs, "next_cursor": next_cursor });
                                match serde_json::to_string_pretty(&body) {
                                    Ok(s) => Ok(Self::ok(s)),
                                    Err(e) => Ok(Self::fail(format!(
                                        "failed to serialize resources: {e}"
                                    ))),
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
                        let all = self.registry.list_all_resources().await;
                        let defs: Vec<_> = all.into_iter().map(|(_, def)| def).collect();
                        match serde_json::to_string_pretty(&defs) {
                            Ok(s) => Ok(Self::ok(s)),
                            Err(e) => Ok(Self::fail(format!("failed to serialize resources: {e}"))),
                        }
                    }
                }
            }
            "read" => {
                let uri = match map.get("uri").and_then(|v| v.as_str()) {
                    Some(u) if !u.is_empty() => u.to_string(),
                    _ => return Ok(Self::fail("`read` requires a non-empty `uri`")),
                };
                match self.registry.read_resource(&uri).await {
                    Ok(contents) => match serde_json::to_string_pretty(&contents) {
                        Ok(s) => Ok(Self::ok(s)),
                        Err(e) => Ok(Self::fail(format!("failed to serialize contents: {e}"))),
                    },
                    Err(e) => Ok(Self::fail(e.to_string())),
                }
            }
            other => Ok(Self::fail(format!(
                "unknown action `{other}` (expected list|read)"
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
        let tool = McpResourcesTool::new(empty_registry().await);
        let res = tool.execute(json!({})).await.unwrap();
        assert!(!res.success);
        assert!(res.error.unwrap().contains("action"));
    }

    #[tokio::test]
    async fn read_without_uri_is_non_fatal_error() {
        let tool = McpResourcesTool::new(empty_registry().await);
        let res = tool.execute(json!({ "action": "read" })).await.unwrap();
        assert!(!res.success);
        assert!(res.error.unwrap().to_lowercase().contains("uri"));
    }

    #[tokio::test]
    async fn read_strips_approved_field() {
        let tool = McpResourcesTool::new(empty_registry().await);
        // unknown server (empty registry) → non-fatal error, but NOT about `approved`.
        let res = tool
            .execute(json!({ "action": "read", "uri": "srv__u", "approved": true }))
            .await
            .unwrap();
        assert!(!res.success);
        assert!(!res.error.unwrap().to_lowercase().contains("approved"));
    }

    #[tokio::test]
    async fn list_cursor_without_server_is_rejected() {
        // A cursor is a per-server opaque token; supplying it without a `server`
        // for the all-server aggregate must fail with a clear, non-fatal error
        // (regression: cursor was previously advertised but silently ignored).
        let tool = McpResourcesTool::new(empty_registry().await);
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
        // With a `server`, the cursor is threaded to the per-server path. An
        // empty registry has no such server, so this surfaces the server-path
        // error ("unknown MCP server") — proving the cursor branch is taken
        // rather than the all-server branch (which would ignore the cursor).
        let tool = McpResourcesTool::new(empty_registry().await);
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
    async fn name_and_schema_are_stable() {
        let tool = McpResourcesTool::new(empty_registry().await);
        assert_eq!(tool.name(), "mcp_resources");
        assert!(tool.parameters_schema().get("properties").is_some());
    }
}
