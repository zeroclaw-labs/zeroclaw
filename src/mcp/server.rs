//! MCP JSON-RPC server dispatcher.
//!
//! This module implements the core request dispatcher for the Model Context
//! Protocol (MCP) over JSON-RPC 2.0. It routes incoming method calls to the
//! appropriate handler and formats responses according to the MCP specification.
//!
//! Two public entry points are provided:
//!
//! - [`handle_request`]: stateless dispatcher that maps a single JSON-RPC
//!   request to a response value.
//! - [`run_stdio_server`]: async loop that reads Content-Length framed
//!   JSON-RPC messages from stdin and writes responses to stdout.

use crate::mcp::protocol::encode_frame;
use crate::mcp::tool_bridge::McpToolBridge;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Dispatch a single MCP JSON-RPC request and return the response value.
///
/// Notifications (methods starting with `notifications/`) may return
/// `Value::Null` to indicate that no response frame should be sent.
pub async fn handle_request(req: &Value, bridge: &McpToolBridge) -> Value {
    let req_id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();

    match method {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "zeroclaw",
                    "version": "0.1.0"
                }
            }
        }),

        "notifications/initialized" => Value::Null,

        "tools/list" => {
            let descriptors = bridge.list_tools();
            let tools: Vec<Value> = descriptors
                .into_iter()
                .map(|d| {
                    json!({
                        "name": d.name,
                        "description": d.description,
                        "inputSchema": d.input_schema,
                    })
                })
                .collect();
            json!({
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "tools": tools
                }
            })
        }

        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or_default();
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

            match bridge.call_tool(name, arguments).await {
                Ok(result) => json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "content": [{
                            "type": "text",
                            "text": result.output
                        }],
                        "isError": false
                    }
                }),
                Err(e) => json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "content": [{
                            "type": "text",
                            "text": e.to_string()
                        }],
                        "isError": true
                    }
                }),
            }
        }

        "resources/list" => json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "resources": []
            }
        }),

        "resources/templates/list" => json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "resourceTemplates": []
            }
        }),

        "ping" => json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {}
        }),

        _ => json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "error": {
                "code": -32601,
                "message": "Method not found"
            }
        }),
    }
}

/// Run the MCP stdio server loop.
///
/// Reads Content-Length framed JSON-RPC messages from stdin, dispatches each
/// through [`handle_request`], and writes the framed response to stdout.
/// Returns `Ok(())` on EOF.
pub async fn run_stdio_server(bridge: Arc<McpToolBridge>) -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();

    loop {
        // Read headers until we find an empty line (the \r\n\r\n separator).
        let mut header_buf = String::new();
        let mut content_length: Option<usize> = None;

        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                // EOF on stdin — clean shutdown.
                return Ok(());
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                // End of headers.
                break;
            }

            header_buf.push_str(&line);

            if let Some((name, value)) = trimmed.split_once(':') {
                if name.trim().eq_ignore_ascii_case("Content-Length") {
                    content_length = Some(value.trim().parse::<usize>()?);
                }
            }
        }

        let length = match content_length {
            Some(len) => len,
            None => {
                // No Content-Length found and we hit an empty line. If
                // header_buf is also empty we just read a stray newline;
                // continue to the next frame.
                if header_buf.is_empty() {
                    continue;
                }
                anyhow::bail!("missing Content-Length header in frame");
            }
        };

        // Read exactly `length` bytes for the JSON body.
        let mut body = vec![0u8; length];
        tokio::io::AsyncReadExt::read_exact(&mut reader, &mut body).await?;

        let req: Value = serde_json::from_slice(&body)?;
        let resp = handle_request(&req, &bridge).await;

        // Notifications produce a Null response — do not send a frame.
        if resp.is_null() {
            continue;
        }

        let frame = encode_frame(&resp)?;
        stdout.write_all(&frame).await?;
        stdout.flush().await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Tool, ToolResult};
    use async_trait::async_trait;

    /// Minimal deterministic tool for server dispatcher testing.
    struct DummyTool;

    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            "dummy_tool"
        }

        fn description(&self) -> &str {
            "A dummy tool"
        }

        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                }
            })
        }

        async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: args
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                error: None,
            })
        }
    }

    fn test_bridge() -> McpToolBridge {
        McpToolBridge::new(vec![Box::new(DummyTool) as Box<dyn Tool>])
    }

    fn make_request(method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    }

    #[tokio::test]
    async fn initialize_returns_server_capabilities() {
        let bridge = test_bridge();
        let req = make_request("initialize", json!({}));
        let resp = handle_request(&req, &bridge).await;

        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);

        let result = &resp["result"];
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], "zeroclaw");
        assert_eq!(result["serverInfo"]["version"], "0.1.0");
    }

    #[tokio::test]
    async fn tools_list_returns_registered_tools() {
        let bridge = test_bridge();
        let req = make_request("tools/list", json!({}));
        let resp = handle_request(&req, &bridge).await;

        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);

        let tools = resp["result"]["tools"]
            .as_array()
            .expect("tools should be an array");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "dummy_tool");
        assert_eq!(tools[0]["description"], "A dummy tool");
        assert_eq!(tools[0]["inputSchema"]["type"], "object");
        assert_eq!(
            tools[0]["inputSchema"]["properties"]["value"]["type"],
            "string"
        );
    }

    #[tokio::test]
    async fn tools_call_executes_and_returns_content() {
        let bridge = test_bridge();
        let req = make_request(
            "tools/call",
            json!({"name": "dummy_tool", "arguments": {"value": "ok"}}),
        );
        let resp = handle_request(&req, &bridge).await;

        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);

        let result = &resp["result"];
        assert_eq!(result["isError"], false);
        let content = result["content"]
            .as_array()
            .expect("content should be an array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "ok");
    }

    #[tokio::test]
    async fn tools_call_unknown_tool_returns_error() {
        let bridge = test_bridge();
        let req = make_request(
            "tools/call",
            json!({"name": "nonexistent", "arguments": {}}),
        );
        let resp = handle_request(&req, &bridge).await;

        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);

        let result = &resp["result"];
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"]
            .as_str()
            .expect("error text should be a string");
        assert!(
            text.contains("unknown tool"),
            "error text should mention unknown tool, got: {}",
            text
        );
    }

    #[tokio::test]
    async fn ping_returns_empty_result() {
        let bridge = test_bridge();
        let req = make_request("ping", json!({}));
        let resp = handle_request(&req, &bridge).await;

        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"], json!({}));
    }

    #[tokio::test]
    async fn unknown_method_returns_error_code() {
        let bridge = test_bridge();
        let req = make_request("bogus/method", json!({}));
        let resp = handle_request(&req, &bridge).await;

        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["error"]["code"], -32601);
        assert_eq!(resp["error"]["message"], "Method not found");
    }

    #[tokio::test]
    async fn resources_list_returns_empty() {
        let bridge = test_bridge();
        let req = make_request("resources/list", json!({}));
        let resp = handle_request(&req, &bridge).await;

        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        let resources = resp["result"]["resources"]
            .as_array()
            .expect("resources should be an array");
        assert!(resources.is_empty());
    }

    #[tokio::test]
    async fn duplicate_tool_name_handled() {
        // Two tools with the same name — last-wins semantics from McpToolBridge.
        struct ToolV1;
        struct ToolV2;

        #[async_trait]
        impl Tool for ToolV1 {
            fn name(&self) -> &str {
                "dup"
            }
            fn description(&self) -> &str {
                "v1"
            }
            fn parameters_schema(&self) -> Value {
                json!({"type": "object", "properties": {}})
            }
            async fn execute(&self, _args: Value) -> anyhow::Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: "v1".into(),
                    error: None,
                })
            }
        }

        #[async_trait]
        impl Tool for ToolV2 {
            fn name(&self) -> &str {
                "dup"
            }
            fn description(&self) -> &str {
                "v2"
            }
            fn parameters_schema(&self) -> Value {
                json!({"type": "object", "properties": {}})
            }
            async fn execute(&self, _args: Value) -> anyhow::Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: "v2".into(),
                    error: None,
                })
            }
        }

        let bridge = McpToolBridge::new(vec![
            Box::new(ToolV1) as Box<dyn Tool>,
            Box::new(ToolV2) as Box<dyn Tool>,
        ]);

        // tools/list should show exactly one entry (last wins).
        let list_req = make_request("tools/list", json!({}));
        let list_resp = handle_request(&list_req, &bridge).await;
        let tools = list_resp["result"]["tools"]
            .as_array()
            .expect("tools should be an array");
        assert_eq!(
            tools.len(),
            1,
            "duplicate names should collapse to one entry"
        );

        // tools/call should execute the last-registered implementation.
        let call_req = make_request("tools/call", json!({"name": "dup", "arguments": {}}));
        let call_resp = handle_request(&call_req, &bridge).await;
        assert_eq!(call_resp["result"]["content"][0]["text"], "v2");
    }

    #[tokio::test]
    async fn resources_templates_list_returns_empty() {
        let bridge = test_bridge();
        let req = make_request("resources/templates/list", json!({}));
        let resp = handle_request(&req, &bridge).await;

        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        let templates = resp["result"]["resourceTemplates"]
            .as_array()
            .expect("resourceTemplates should be an array");
        assert!(templates.is_empty());
    }

    #[tokio::test]
    async fn notification_initialized_returns_null() {
        let bridge = test_bridge();
        let req = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let resp = handle_request(&req, &bridge).await;
        assert!(resp.is_null(), "notifications should produce null response");
    }

    #[tokio::test]
    async fn request_id_is_passed_through() {
        let bridge = test_bridge();
        let req = json!({"jsonrpc": "2.0", "id": 42, "method": "ping", "params": {}});
        let resp = handle_request(&req, &bridge).await;
        assert_eq!(resp["id"], 42);
    }

    #[tokio::test]
    async fn string_request_id_is_passed_through() {
        let bridge = test_bridge();
        let req = json!({"jsonrpc": "2.0", "id": "abc-123", "method": "ping", "params": {}});
        let resp = handle_request(&req, &bridge).await;
        assert_eq!(resp["id"], "abc-123");
    }
}
