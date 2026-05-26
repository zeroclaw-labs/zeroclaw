//! MCP (Model Context Protocol) client — connects to external tool servers.
//!
//! Supports multiple transports: stdio (spawn local process), HTTP, and SSE.

use std::collections::HashMap;
use std::sync::Arc;
#[cfg(not(target_has_atomic = "64"))]
use std::sync::atomic::AtomicU32;
#[cfg(target_has_atomic = "64")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result, bail};
use serde_json::json;
use tokio::sync::Mutex;
use tokio::time::{Duration, timeout};

use crate::mcp_protocol::{JsonRpcRequest, MCP_PROTOCOL_VERSION, McpToolDef, McpToolsListResult};
use crate::mcp_transport::{McpTransportConn, create_transport};
use zeroclaw_config::schema::McpServerConfig;

/// Timeout for receiving a response from an MCP server during init/list.
/// Prevents a hung server from blocking the daemon indefinitely.
const RECV_TIMEOUT_SECS: u64 = 30;

/// Default timeout for tool calls (seconds) when not configured per-server.
const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 180;

/// Maximum allowed tool call timeout (seconds) — hard safety ceiling.
const MAX_TOOL_TIMEOUT_SECS: u64 = 600;

// ── Internal server state ──────────────────────────────────────────────────

struct McpServerInner {
    config: McpServerConfig,
    transport: Box<dyn McpTransportConn>,
    #[cfg(target_has_atomic = "64")]
    next_id: AtomicU64,
    #[cfg(not(target_has_atomic = "64"))]
    next_id: AtomicU32,
    tools: Vec<McpToolDef>,
}

// ── McpServer ──────────────────────────────────────────────────────────────

/// A live connection to one MCP server (any transport).
#[derive(Clone)]
pub struct McpServer {
    inner: Arc<Mutex<McpServerInner>>,
}

impl McpServer {
    /// Connect to the server, perform the initialize handshake, and fetch the tool list.
    pub async fn connect(config: McpServerConfig) -> Result<Self> {
        // Create transport based on config
        let mut transport = create_transport(&config).with_context(|| {
            format!(
                "failed to create transport for MCP server `{}`",
                config.name
            )
        })?;

        // Initialize handshake
        let id = 1u64;
        let init_req = JsonRpcRequest::new(
            id,
            "initialize",
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "zeroclaw",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        );

        let init_resp = timeout(
            Duration::from_secs(RECV_TIMEOUT_SECS),
            transport.send_and_recv(&init_req),
        )
        .await
        .with_context(|| {
            format!(
                "MCP server `{}` timed out after {}s waiting for initialize response",
                config.name, RECV_TIMEOUT_SECS
            )
        })??;

        if init_resp.error.is_some() {
            bail!(
                "MCP server `{}` rejected initialize: {:?}",
                config.name,
                init_resp.error
            );
        }

        // Notify server that client is initialized (no response expected for notifications)
        // For notifications, we send but don't wait for response
        let notif = JsonRpcRequest::notification("notifications/initialized", json!({}));
        // Best effort - ignore errors for notifications
        let _ = transport.send_and_recv(&notif).await;

        // Fetch available tools
        let id = 2u64;
        let list_req = JsonRpcRequest::new(id, "tools/list", json!({}));

        let list_resp = timeout(
            Duration::from_secs(RECV_TIMEOUT_SECS),
            transport.send_and_recv(&list_req),
        )
        .await
        .with_context(|| {
            format!(
                "MCP server `{}` timed out after {}s waiting for tools/list response",
                config.name, RECV_TIMEOUT_SECS
            )
        })??;

        let result = list_resp.result.ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"mcp_server": &config.name})),
                "mcp_client: tools/list returned no result"
            );
            anyhow::Error::msg(format!(
                "tools/list returned no result from `{}`",
                config.name
            ))
        })?;
        let tool_list: McpToolsListResult = serde_json::from_value(result)
            .with_context(|| format!("failed to parse tools/list from `{}`", config.name))?;

        let tool_count = tool_list.tools.len();

        let inner = McpServerInner {
            config,
            transport,
            #[cfg(target_has_atomic = "64")]
            next_id: AtomicU64::new(3), // Start at 3 since we used 1 and 2
            #[cfg(not(target_has_atomic = "64"))]
            next_id: AtomicU32::new(3), // Start at 3 since we used 1 and 2
            tools: tool_list.tools,
        };

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "MCP server `{}` connected — {} tool(s) available",
                inner.config.name, tool_count
            )
        );

        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
        })
    }

    /// Tools advertised by this server.
    pub async fn tools(&self) -> Vec<McpToolDef> {
        self.inner.lock().await.tools.clone()
    }

    /// Server display name.
    pub async fn name(&self) -> String {
        self.inner.lock().await.config.name.clone()
    }

    /// Call a tool on this server. Returns the raw JSON result.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let mut inner = self.inner.lock().await;
        let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest::new(
            id,
            "tools/call",
            json!({ "name": tool_name, "arguments": arguments }),
        );

        // Use per-server tool timeout if configured, otherwise default.
        // Cap at MAX_TOOL_TIMEOUT_SECS for safety.
        let tool_timeout = inner
            .config
            .tool_timeout_secs
            .unwrap_or(DEFAULT_TOOL_TIMEOUT_SECS)
            .min(MAX_TOOL_TIMEOUT_SECS);

        let resp = timeout(
            Duration::from_secs(tool_timeout),
            inner.transport.send_and_recv(&req),
        )
        .await
        .map_err(|_| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Timeout)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "mcp_server": &inner.config.name,
                        "tool": tool_name,
                        "timeout_secs": tool_timeout,
                    })),
                "mcp_client: tool call timed out"
            );
            anyhow::Error::msg(format!(
                "MCP server `{}` timed out after {}s during tool call `{tool_name}`",
                inner.config.name, tool_timeout
            ))
        })?
        .with_context(|| {
            format!(
                "MCP server `{}` error during tool call `{tool_name}`",
                inner.config.name
            )
        })?;

        if let Some(err) = resp.error {
            bail!("MCP tool `{tool_name}` error {}: {}", err.code, err.message);
        }
        Ok(resp.result.unwrap_or(serde_json::Value::Null))
    }
}

// ── McpRegistry ───────────────────────────────────────────────────────────

#[derive(Clone)]
enum McpInvocation {
    Tool {
        server_index: usize,
        original_tool_name: String,
    },
    Method {
        server_index: usize,
        method: &'static str,
    },
}

/// Registry of all connected MCP servers, with a flat tool index.
pub struct McpRegistry {
    servers: Vec<McpServer>,
    /// prefixed_name → MCP invocation route.
    tool_index: HashMap<String, McpInvocation>,
}

impl McpRegistry {
    /// Connect to all configured servers. Non-fatal: failures are logged and skipped.
    pub async fn connect_all(configs: &[McpServerConfig]) -> Result<Self> {
        let mut servers = Vec::new();
        let mut tool_index = HashMap::new();

        for config in configs {
            match McpServer::connect(config.clone()).await {
                Ok(server) => {
                    let server_idx = servers.len();
                    // Collect tools while holding the lock once, then release
                    let tools = server.tools().await;
                    for tool in &tools {
                        // Prefix prevents name collisions across servers
                        let prefixed = format!("{}__{}", config.name, tool.name);
                        tool_index.insert(
                            prefixed,
                            McpInvocation::Tool {
                                server_index: server_idx,
                                original_tool_name: tool.name.clone(),
                            },
                        );
                    }
                    for (name, method) in mcp_surface_methods(&config.name) {
                        tool_index.insert(
                            name,
                            McpInvocation::Method {
                                server_index: server_idx,
                                method,
                            },
                        );
                    }
                    servers.push(server);
                }
                // Non-fatal — log and continue with remaining servers
                Err(e) => {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        &format!("Failed to connect to MCP server `{}`: {:#}", config.name, e)
                    );
                }
            }
        }

        Ok(Self {
            servers,
            tool_index,
        })
    }

    /// All prefixed tool names across all connected servers.
    pub fn tool_names(&self) -> Vec<String> {
        self.tool_index.keys().cloned().collect()
    }

    /// Tool definition for a given prefixed name (cloned).
    pub async fn get_tool_def(&self, prefixed_name: &str) -> Option<McpToolDef> {
        match self.tool_index.get(prefixed_name)? {
            McpInvocation::Tool {
                server_index,
                original_tool_name,
            } => {
                let inner = self.servers[*server_index].inner.lock().await;
                inner
                    .tools
                    .iter()
                    .find(|t| &t.name == original_tool_name)
                    .cloned()
            }
            McpInvocation::Method { method, .. } => mcp_surface_tool_def(prefixed_name, method),
        }
    }

    /// Execute a tool by prefixed name.
    pub async fn call_tool(
        &self,
        prefixed_name: &str,
        arguments: serde_json::Value,
    ) -> Result<String> {
        let invocation = self.tool_index.get(prefixed_name).cloned().ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"tool": prefixed_name})),
                "mcp_client: unknown MCP tool"
            );
            anyhow::Error::msg(format!("unknown MCP tool `{prefixed_name}`"))
        })?;
        let result = match invocation {
            McpInvocation::Tool {
                server_index,
                original_tool_name,
            } => {
                self.servers[server_index]
                    .call_tool(&original_tool_name, arguments)
                    .await?
            }
            McpInvocation::Method {
                server_index,
                method,
            } => {
                self.servers[server_index]
                    .call_method(method, mcp_surface_params(method, arguments)?)
                    .await?
            }
        };
        serde_json::to_string_pretty(&result)
            .with_context(|| format!("failed to serialize result of MCP tool `{prefixed_name}`"))
    }

    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    pub fn server_count(&self) -> usize {
        self.servers.len()
    }

    pub fn tool_count(&self) -> usize {
        self.tool_index.len()
    }
}

impl McpServer {
    async fn call_method(
        &self,
        method: &'static str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let mut inner = self.inner.lock().await;
        let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest::new(id, method, params);

        let resp = timeout(
            Duration::from_secs(RECV_TIMEOUT_SECS),
            inner.transport.send_and_recv(&req),
        )
        .await
        .map_err(|_| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Timeout)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "mcp_server": &inner.config.name,
                        "method": method,
                        "timeout_secs": RECV_TIMEOUT_SECS,
                    })),
                "mcp_client: method call timed out"
            );
            anyhow::Error::msg(format!(
                "MCP server `{}` timed out after {}s during `{method}`",
                inner.config.name, RECV_TIMEOUT_SECS
            ))
        })?
        .with_context(|| format!("MCP server `{}` error during `{method}`", inner.config.name))?;

        if let Some(err) = resp.error {
            bail!("MCP method `{method}` error {}: {}", err.code, err.message);
        }
        Ok(resp.result.unwrap_or(serde_json::Value::Null))
    }
}

fn mcp_surface_methods(server_name: &str) -> [(String, &'static str); 4] {
    [
        (
            format!("{server_name}__mcp_list_resources"),
            "resources/list",
        ),
        (
            format!("{server_name}__mcp_read_resource"),
            "resources/read",
        ),
        (format!("{server_name}__mcp_list_prompts"), "prompts/list"),
        (format!("{server_name}__mcp_get_prompt"), "prompts/get"),
    ]
}

fn mcp_surface_tool_def(prefixed_name: &str, method: &str) -> Option<McpToolDef> {
    let (description, input_schema) = match method {
        "resources/list" => (
            "List MCP resources exposed by this server.",
            json!({"type": "object", "properties": {}}),
        ),
        "resources/read" => (
            "Read one MCP resource by URI from this server.",
            json!({
                "type": "object",
                "properties": {
                    "uri": {
                        "type": "string",
                        "description": "Resource URI returned by mcp_list_resources."
                    }
                },
                "required": ["uri"]
            }),
        ),
        "prompts/list" => (
            "List MCP prompts exposed by this server.",
            json!({"type": "object", "properties": {}}),
        ),
        "prompts/get" => (
            "Get one MCP prompt by name, optionally passing prompt arguments.",
            json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Prompt name returned by mcp_list_prompts."
                    },
                    "arguments": {
                        "type": "object",
                        "description": "Optional prompt arguments keyed by argument name."
                    }
                },
                "required": ["name"]
            }),
        ),
        _ => return None,
    };

    Some(McpToolDef {
        name: prefixed_name.to_string(),
        description: Some(description.to_string()),
        input_schema,
    })
}

fn mcp_surface_params(method: &str, arguments: serde_json::Value) -> Result<serde_json::Value> {
    match method {
        "resources/list" | "prompts/list" => Ok(json!({})),
        "resources/read" => {
            let uri = required_string_arg(&arguments, "uri")?;
            Ok(json!({ "uri": uri }))
        }
        "prompts/get" => {
            let name = required_string_arg(&arguments, "name")?;
            let prompt_arguments = arguments
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            Ok(json!({ "name": name, "arguments": prompt_arguments }))
        }
        _ => bail!("unsupported MCP method bridge `{method}`"),
    }
}

fn required_string_arg(arguments: &serde_json::Value, name: &str) -> Result<String> {
    let value = arguments
        .get(name)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::Error::msg(format!("missing string argument `{name}`")))?;
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::McpTransport;

    #[test]
    fn tool_name_prefix_format() {
        let prefixed = format!("{}__{}", "filesystem", "read_file");
        assert_eq!(prefixed, "filesystem__read_file");
    }

    #[test]
    fn mcp_surface_methods_are_prefixed_per_server() {
        let names: Vec<_> = mcp_surface_methods("filesystem")
            .into_iter()
            .map(|(name, method)| (name, method))
            .collect();
        assert_eq!(
            names,
            vec![
                (
                    "filesystem__mcp_list_resources".to_string(),
                    "resources/list"
                ),
                (
                    "filesystem__mcp_read_resource".to_string(),
                    "resources/read"
                ),
                ("filesystem__mcp_list_prompts".to_string(), "prompts/list"),
                ("filesystem__mcp_get_prompt".to_string(), "prompts/get"),
            ]
        );
    }

    #[test]
    fn mcp_surface_tool_def_exposes_read_resource_schema() {
        let def = mcp_surface_tool_def("filesystem__mcp_read_resource", "resources/read")
            .expect("read-resource bridge should have a tool definition");
        assert_eq!(def.name, "filesystem__mcp_read_resource");
        assert!(def.description.unwrap().contains("Read one MCP resource"));
        assert_eq!(def.input_schema["required"], serde_json::json!(["uri"]));
    }

    #[test]
    fn mcp_surface_params_validate_required_resource_uri() {
        let params = mcp_surface_params("resources/read", json!({"uri": "file:///notes.md"}))
            .expect("valid resource URI should map to MCP params");
        assert_eq!(params, json!({"uri": "file:///notes.md"}));

        let err = mcp_surface_params("resources/read", json!({}))
            .expect_err("missing URI must fail before MCP dispatch");
        assert!(err.to_string().contains("missing string argument `uri`"));
    }

    #[test]
    fn mcp_surface_params_support_prompt_arguments() {
        let params = mcp_surface_params(
            "prompts/get",
            json!({"name": "summarize", "arguments": {"topic": "release"}}),
        )
        .expect("valid prompt get args should map to MCP params");
        assert_eq!(
            params,
            json!({"name": "summarize", "arguments": {"topic": "release"}})
        );
    }

    #[tokio::test]
    async fn connect_nonexistent_command_fails_cleanly() {
        // A command that doesn't exist should fail at spawn, not panic.
        let config = McpServerConfig {
            name: "nonexistent".to_string(),
            command: "/usr/bin/this_binary_does_not_exist_zeroclaw_test".to_string(),
            args: vec![],
            env: std::collections::HashMap::default(),
            tool_timeout_secs: None,
            transport: McpTransport::Stdio,
            url: None,
            headers: std::collections::HashMap::default(),
        };
        let result = McpServer::connect(config).await;
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(msg.contains("failed to create transport"), "got: {msg}");
    }

    #[tokio::test]
    async fn connect_all_nonfatal_on_single_failure() {
        // If one server config is bad, connect_all should succeed (with 0 servers).
        let configs = vec![McpServerConfig {
            name: "bad".to_string(),
            command: "/usr/bin/does_not_exist_zc_test".to_string(),
            args: vec![],
            env: std::collections::HashMap::default(),
            tool_timeout_secs: None,
            transport: McpTransport::Stdio,
            url: None,
            headers: std::collections::HashMap::default(),
        }];
        let registry = McpRegistry::connect_all(&configs)
            .await
            .expect("connect_all should not fail");
        assert!(registry.is_empty());
        assert_eq!(registry.tool_count(), 0);
    }

    #[test]
    fn http_transport_requires_url() {
        let config = McpServerConfig {
            name: "test".into(),
            transport: McpTransport::Http,
            ..Default::default()
        };
        let result = create_transport(&config);
        assert!(result.is_err());
    }

    #[test]
    fn sse_transport_requires_url() {
        let config = McpServerConfig {
            name: "test".into(),
            transport: McpTransport::Sse,
            ..Default::default()
        };
        let result = create_transport(&config);
        assert!(result.is_err());
    }

    // ── Empty registry (no servers) ────────────────────────────────────────

    #[tokio::test]
    async fn empty_registry_is_empty() {
        let registry = McpRegistry::connect_all(&[])
            .await
            .expect("connect_all on empty slice should succeed");
        assert!(registry.is_empty());
        assert_eq!(registry.server_count(), 0);
        assert_eq!(registry.tool_count(), 0);
    }

    #[tokio::test]
    async fn empty_registry_tool_names_is_empty() {
        let registry = McpRegistry::connect_all(&[])
            .await
            .expect("connect_all should succeed");
        assert!(registry.tool_names().is_empty());
    }

    #[tokio::test]
    async fn empty_registry_get_tool_def_returns_none() {
        let registry = McpRegistry::connect_all(&[])
            .await
            .expect("connect_all should succeed");
        let result = registry.get_tool_def("nonexistent__tool").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn empty_registry_call_tool_unknown_name_returns_error() {
        let registry = McpRegistry::connect_all(&[])
            .await
            .expect("connect_all should succeed");
        let err = registry
            .call_tool("nonexistent__tool", serde_json::json!({}))
            .await
            .expect_err("should fail for unknown tool");
        assert!(err.to_string().contains("unknown MCP tool"), "got: {err}");
    }

    #[tokio::test]
    async fn connect_all_empty_gives_zero_servers() {
        let registry = McpRegistry::connect_all(&[])
            .await
            .expect("connect_all should succeed");
        // Verify all three count methods agree on zero.
        assert_eq!(registry.server_count(), 0);
        assert_eq!(registry.tool_count(), 0);
        assert!(registry.is_empty());
    }
}
