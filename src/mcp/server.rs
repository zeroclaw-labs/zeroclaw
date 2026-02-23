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
//! - [`run_stdio_server`]: async loop that auto-detects transport format
//!   (newline-delimited JSON or Content-Length framed) and serves accordingly.

use crate::mcp::protocol::encode_frame;
use crate::mcp::tool_bridge::McpToolBridge;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Supported MCP protocol versions (newest first).
const SUPPORTED_VERSIONS: &[&str] = &["2025-06-18", "2024-11-05"];

/// Negotiate protocol version: return the client's requested version if we
/// support it, otherwise fall back to our newest supported version.
fn negotiate_version(client_version: Option<&str>) -> &'static str {
    if let Some(cv) = client_version {
        for &v in SUPPORTED_VERSIONS {
            if v == cv {
                return v;
            }
        }
    }
    SUPPORTED_VERSIONS[0]
}

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
        "initialize" => {
            let client_version = req
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str);
            let version = negotiate_version(client_version);
            json!({
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "protocolVersion": version,
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "zeroclaw",
                        "version": "0.1.0"
                    }
                }
            })
        }

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

/// Transport mode detected from the first message on stdin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportMode {
    /// Newline-delimited JSON (used by Codex CLI / rmcp).
    Newline,
    /// Content-Length framed (LSP-style, used by standard MCP clients).
    ContentLength,
}

/// Run the MCP stdio server loop.
///
/// Auto-detects the transport format from the first line of stdin:
/// - If it starts with `{`, uses newline-delimited JSON mode.
/// - If it starts with `Content-Length:`, uses Content-Length framed mode.
///
/// The detected mode is fixed for the lifetime of the connection.
/// Returns `Ok(())` on EOF.
pub async fn run_stdio_server(bridge: Arc<McpToolBridge>) -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();

    // Detect transport mode from first line.
    let mut first_line = String::new();
    let n = reader.read_line(&mut first_line).await?;
    if n == 0 {
        return Ok(());
    }

    let mode = if first_line.trim_start().starts_with('{') {
        TransportMode::Newline
    } else {
        TransportMode::ContentLength
    };

    // Process the first line according to detected mode, then loop.
    match mode {
        TransportMode::Newline => {
            // First line is already a JSON message.
            run_newline_mode(&mut reader, &mut stdout, &bridge, Some(first_line)).await
        }
        TransportMode::ContentLength => {
            // First line is a header; continue reading headers + body.
            run_content_length_mode(&mut reader, &mut stdout, &bridge, Some(first_line)).await
        }
    }
}

/// Newline-delimited JSON transport: each line is a complete JSON-RPC message.
async fn run_newline_mode<R, W>(
    reader: &mut BufReader<R>,
    stdout: &mut W,
    bridge: &McpToolBridge,
    first_line: Option<String>,
) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    // Process the first line if provided.
    if let Some(line) = first_line {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            if let Ok(req) = serde_json::from_str::<Value>(trimmed) {
                let resp = handle_request(&req, bridge).await;
                if !resp.is_null() {
                    let mut bytes = serde_json::to_vec(&resp)?;
                    bytes.push(b'\n');
                    stdout.write_all(&bytes).await?;
                    stdout.flush().await?;
                }
            }
        }
    }

    // Read remaining lines.
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let resp = handle_request(&req, bridge).await;
        if resp.is_null() {
            continue;
        }
        let mut bytes = serde_json::to_vec(&resp)?;
        bytes.push(b'\n');
        stdout.write_all(&bytes).await?;
        stdout.flush().await?;
    }
}

/// Content-Length framed transport (LSP-style).
async fn run_content_length_mode<R, W>(
    reader: &mut BufReader<R>,
    stdout: &mut W,
    bridge: &McpToolBridge,
    first_header_line: Option<String>,
) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    // We may already have the first header line.
    let mut pending_header = first_header_line;

    loop {
        let mut content_length: Option<usize> = None;

        // Parse the pending header line if we have one.
        if let Some(ref line) = pending_header {
            let trimmed = line.trim();
            if let Some((name, value)) = trimmed.split_once(':') {
                if name.trim().eq_ignore_ascii_case("Content-Length") {
                    content_length = Some(value.trim().parse::<usize>()?);
                }
            }
        }
        pending_header = None;

        // Read remaining headers until empty line.
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                return Ok(());
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some((name, value)) = trimmed.split_once(':') {
                if name.trim().eq_ignore_ascii_case("Content-Length") {
                    content_length = Some(value.trim().parse::<usize>()?);
                }
            }
        }

        let length = match content_length {
            Some(len) => len,
            None => continue,
        };

        let mut body = vec![0u8; length];
        tokio::io::AsyncReadExt::read_exact(reader, &mut body).await?;

        let req: Value = serde_json::from_slice(&body)?;
        let resp = handle_request(&req, bridge).await;

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
        assert!(
            SUPPORTED_VERSIONS.contains(&result["protocolVersion"].as_str().unwrap()),
            "protocolVersion should be a supported version"
        );
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], "zeroclaw");
        assert_eq!(result["serverInfo"]["version"], "0.1.0");
    }

    #[tokio::test]
    async fn initialize_negotiates_client_version() {
        let bridge = test_bridge();

        // Client requests 2025-06-18 -> server returns 2025-06-18
        let req = make_request(
            "initialize",
            json!({"protocolVersion": "2025-06-18"}),
        );
        let resp = handle_request(&req, &bridge).await;
        assert_eq!(resp["result"]["protocolVersion"], "2025-06-18");

        // Client requests 2024-11-05 -> server returns 2024-11-05
        let req = make_request(
            "initialize",
            json!({"protocolVersion": "2024-11-05"}),
        );
        let resp = handle_request(&req, &bridge).await;
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");

        // Client requests unknown version -> server returns newest supported
        let req = make_request(
            "initialize",
            json!({"protocolVersion": "9999-01-01"}),
        );
        let resp = handle_request(&req, &bridge).await;
        assert_eq!(resp["result"]["protocolVersion"], SUPPORTED_VERSIONS[0]);
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

    // --- Transport mode tests ---

    #[tokio::test]
    async fn newline_mode_roundtrip() {
        let bridge = test_bridge();
        let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}});
        let ping = json!({"jsonrpc":"2.0","id":2,"method":"ping","params":{}});

        let input = format!("{}\n{}\n", init, ping);
        let mut reader = BufReader::new(input.as_bytes());
        let mut output = Vec::new();

        run_newline_mode(&mut reader, &mut output, &bridge, None).await.unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = output_str.trim().split('\n').collect();
        assert_eq!(lines.len(), 2, "should get 2 response lines");

        let resp1: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(resp1["result"]["protocolVersion"], "2025-06-18");

        let resp2: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(resp2["id"], 2);
        assert_eq!(resp2["result"], json!({}));
    }

    #[tokio::test]
    async fn content_length_mode_roundtrip() {
        let bridge = test_bridge();
        let ping = json!({"jsonrpc":"2.0","id":1,"method":"ping","params":{}});
        let body = serde_json::to_vec(&ping).unwrap();
        let input = format!("Content-Length: {}\r\n\r\n", body.len());
        let mut input_bytes = input.into_bytes();
        input_bytes.extend_from_slice(&body);

        let mut reader = BufReader::new(input_bytes.as_slice());
        let mut output = Vec::new();

        run_content_length_mode(&mut reader, &mut output, &bridge, None).await.unwrap();

        let output_str = String::from_utf8(output).unwrap();
        assert!(output_str.starts_with("Content-Length:"));
        assert!(output_str.contains("\"id\":1"));
    }

    #[tokio::test]
    async fn newline_mode_skips_notifications() {
        let bridge = test_bridge();
        let notif = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        let ping = json!({"jsonrpc":"2.0","id":1,"method":"ping","params":{}});

        let input = format!("{}\n{}\n", notif, ping);
        let mut reader = BufReader::new(input.as_bytes());
        let mut output = Vec::new();

        run_newline_mode(&mut reader, &mut output, &bridge, None).await.unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = output_str.trim().split('\n').collect();
        assert_eq!(lines.len(), 1, "notification should not produce output");
    }

    #[test]
    fn negotiate_version_returns_matching() {
        assert_eq!(negotiate_version(Some("2025-06-18")), "2025-06-18");
        assert_eq!(negotiate_version(Some("2024-11-05")), "2024-11-05");
    }

    #[test]
    fn negotiate_version_falls_back_to_newest() {
        assert_eq!(negotiate_version(Some("9999-01-01")), SUPPORTED_VERSIONS[0]);
        assert_eq!(negotiate_version(None), SUPPORTED_VERSIONS[0]);
    }
}
