//! MCP (Model Context Protocol) client integration for external tool servers.
//!
//! Allows ZeroClaw to connect to local or remote MCP servers to expand its
//! capabilities beyond the built-in toolset.

use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::process::Command;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// A tool that proxies calls to an external MCP server.
pub struct McpTool {
    name: String,
    description: String,
    parameters: Value,
    server_command: String,
    server_args: Vec<String>,
    security: Arc<SecurityPolicy>,
}

impl McpTool {
    pub fn new(
        name: String,
        description: String,
        parameters: Value,
        server_command: String,
        server_args: Vec<String>,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        Self {
            name,
            description,
            parameters,
            server_command,
            server_args,
            security,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.parameters.clone()
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<ToolResult> {
        // 1. Security check: Is this tool allowed?
        if !self.security.is_command_allowed(&self.server_command) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Security policy blocks MCP server command: {}", self.server_command)),
            });
        }

        // 2. Execute MCP server via stdio transport
        let mut child = Command::new(&self.server_command)
            .args(&self.server_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let mut stdin = child.stdin.take().expect("Failed to open stdin");
        let stdout = child.stdout.take().expect("Failed to open stdout");
        let mut reader = BufReader::new(stdout).lines();

        // 3. Send "call_tool" request (MCP protocol)
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": self.name,
                "arguments": arguments
            }
        });

        let req_text = serde_json::to_string(&request)? + "\n";
        stdin.write_all(req_text.as_bytes()).await?;
        stdin.flush().await?;

        // 4. Read response
        if let Some(line) = reader.next_line().await? {
            let response: Value = serde_json::from_str(&line)?;
            
            if let Some(error) = response.get("error") {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("MCP Error: {}", error)),
                });
            }

            if let Some(result) = response.get("result") {
                // MCP results often contain a "content" array
                let output = if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
                    let mut combined = String::new();
                    for item in content {
                        if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                            combined.push_str(text);
                        }
                    }
                    combined
                } else {
                    serde_json::to_string_pretty(result)?
                };

                return Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                });
            }
        }

        Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("MCP server returned empty or invalid response".into()),
        })
    }
}

/// Discovers tools from an MCP server using `tools/list`.
pub async fn discover_mcp_tools(
    command: &str,
    args: &[String],
    security: Arc<SecurityPolicy>,
) -> anyhow::Result<Vec<Box<dyn Tool>>> {
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let mut stdin = child.stdin.take().expect("Failed to open stdin");
    let stdout = child.stdout.take().expect("Failed to open stdout");
    let mut reader = BufReader::new(stdout).lines();

    let request = json!({
        "jsonrpc": "2.0",
        "id": "list_discovery",
        "method": "tools/list",
        "params": {}
    });

    let req_text = serde_json::to_string(&request)? + "\n";
    stdin.write_all(req_text.as_bytes()).await?;
    stdin.flush().await?;

    if let Some(line) = reader.next_line().await? {
        let response: Value = serde_json::from_str(&line)?;
        if let Some(result) = response.get("result") {
            if let Some(tools_array) = result.get("tools").and_then(|t| t.as_array()) {
                let mut tools: Vec<Box<dyn Tool>> = Vec::new();
                for tool_val in tools_array {
                    let name = tool_val.get("name").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
                    let desc = tool_val.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let params = tool_val.get("inputSchema").cloned().unwrap_or(json!({"type":"object"}));
                    
                    tools.push(Box::new(McpTool::new(
                        name,
                        desc,
                        params,
                        command.to_string(),
                        args.to_vec(),
                        security.clone(),
                    )));
                }
                return Ok(tools);
            }
        }
    }

    anyhow::bail!("Failed to list tools from MCP server")
}
