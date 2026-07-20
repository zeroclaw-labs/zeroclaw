//! MCP (Model Context Protocol) client — connects to external tool servers.
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

use crate::mcp_prompt::{McpGetPromptResult, McpPromptsListResult};
use crate::mcp_protocol::{JsonRpcRequest, MCP_PROTOCOL_VERSION, McpToolDef, McpToolsListResult};
use crate::mcp_resource::{McpResourceContents, McpResourcesListResult};
use crate::mcp_transport::{McpTransportConn, McpTransportError, create_transport};
use zeroclaw_config::schema::McpServerConfig;

/// Timeout for receiving a response from an MCP server during init/list.
/// Prevents a hung server from blocking the daemon indefinitely.
const RECV_TIMEOUT_SECS: u64 = 30;

/// Default timeout for tool calls (seconds) when not configured per-server.
const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 180;

/// Maximum allowed tool call timeout (seconds) — hard safety ceiling.
const MAX_TOOL_TIMEOUT_SECS: u64 = 600;

/// Maximum automatic reconnect attempts on a stale session or dropped
/// transport before the tool-call error is surfaced to the caller.
const MAX_RECONNECT_ATTEMPTS: u32 = 2;

/// Fixed backoff between reconnect attempts (milliseconds).
const RECONNECT_BACKOFF_MS: u64 = 500;

/// Perform the MCP `initialize` + `notifications/initialized` handshake on a
/// transport. Shared by the initial [`McpServer::connect`] and the
/// reconnect-after-stale-session path in [`McpServer::call_tool`].
async fn handshake(
    transport: &mut dyn McpTransportConn,
    server_name: &str,
) -> Result<McpServerCapabilities> {
    let init_req = JsonRpcRequest::new(
        1,
        "initialize",
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": { "resources": {}, "prompts": {} },
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
            "MCP server `{server_name}` timed out after {RECV_TIMEOUT_SECS}s waiting for initialize response"
        )
    })??;

    if init_resp.error.is_some() {
        bail!(
            "MCP server `{server_name}` rejected initialize: {:?}",
            init_resp.error
        );
    }

    // Parse server-advertised capabilities from the initialize result.
    let capabilities = init_resp
        .result
        .as_ref()
        .map(McpServerCapabilities::from_init_result)
        .unwrap_or_default();

    // Notify the server the client is initialized (notifications expect no
    // response). Best effort — ignore errors.
    let notif = JsonRpcRequest::notification("notifications/initialized", json!({}));
    let _ = transport.send_and_recv(&notif).await;

    Ok(capabilities)
}

/// Server-advertised MCP capabilities parsed from the `initialize` result.
/// Sub-flags `subscribe` / `listChanged` are captured but currently unused
/// (reserved for a future subscriptions spec).
#[derive(Debug, Clone, Default)]
pub struct McpServerCapabilities {
    pub(crate) resources: bool,
    pub(crate) prompts: bool,
}

impl McpServerCapabilities {
    /// Parse from the raw `initialize` result value. A capability counts as
    /// supported when its object key is present under `capabilities`.
    pub fn from_init_result(result: &serde_json::Value) -> Self {
        let caps = result.get("capabilities");
        let has = |key: &str| caps.and_then(|c| c.get(key)).is_some();
        Self {
            resources: has("resources"),
            prompts: has("prompts"),
        }
    }

    pub fn supports_resources(&self) -> bool {
        self.resources
    }

    pub fn supports_prompts(&self) -> bool {
        self.prompts
    }
}

fn check_result_is_error(result: &serde_json::Value, op: &str, server_name: &str) -> Result<()> {
    if result.get("isError").and_then(serde_json::Value::as_bool) != Some(true) {
        return Ok(());
    }
    let detail = result
        .get("content")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|s: &String| !s.is_empty())
        .unwrap_or_else(|| "(no error detail returned by server)".to_string());
    let detail = zeroclaw_providers::sanitize_api_error(&detail);
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "mcp_server": server_name,
                "op": op,
                "detail": &detail,
            })),
        "mcp_client: MCP result returned isError:true"
    );
    bail!("MCP `{op}` (server `{server_name}`) returned isError: {detail}");
}

// ── Internal server state ──────────────────────────────────────────────────

struct McpServerInner {
    config: McpServerConfig,
    transport: Box<dyn McpTransportConn>,
    #[cfg(target_has_atomic = "64")]
    next_id: AtomicU64,
    #[cfg(not(target_has_atomic = "64"))]
    next_id: AtomicU32,
    tools: Vec<McpToolDef>,
    capabilities: McpServerCapabilities,
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

        // Initialize handshake (initialize + initialized notification)
        let capabilities = handshake(transport.as_mut(), &config.name).await?;

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
            capabilities,
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

    /// Server-advertised capabilities captured at handshake.
    pub async fn capabilities(&self) -> McpServerCapabilities {
        self.inner.lock().await.capabilities.clone()
    }

    /// Health-check the underlying transport without sending a real request.
    /// Returns `true` when the transport is alive, `false` otherwise.
    ///
    /// Uses `try_lock` instead of `blocking_lock` because this method may be
    /// called from async contexts (e.g. `health_check_all` during heartbeat
    /// retries inside an async test or the tokio-based heartbeat worker).
    pub fn health_check(&self) -> bool {
        self.inner
            .try_lock()
            .map(|mut inner| inner.transport.health_check())
            .unwrap_or(true) // assume healthy if lock is contended
    }

    /// Identity comparison on the underlying transport handle. Two
    /// `McpServer` values share the same connection iff `ptr_eq`
    /// returns `true` — i.e. their inner `Arc<Mutex<McpServerInner>>`
    /// points to the same allocation. Cheap Arc-level comparison, no
    /// async, no lock.
    ///
    /// Used by the daemon's reconciliation layer to verify that a
    /// "preserved" healthy server's live connection survives a
    /// recovery tick without being silently disconnected and
    /// respawned (the additive merge contract: a healthy handle
    /// covers its name and is reused verbatim via `Arc::clone`).
    pub fn ptr_eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Call a tool on this server. Returns the raw JSON result.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let mut inner = self.inner.lock().await;

        // Use per-server tool timeout if configured, otherwise default.
        // Cap at MAX_TOOL_TIMEOUT_SECS for safety.
        let tool_timeout = inner
            .config
            .tool_timeout_secs
            .unwrap_or(DEFAULT_TOOL_TIMEOUT_SECS)
            .min(MAX_TOOL_TIMEOUT_SECS);

        let mut attempt = 0u32;
        let resp = loop {
            let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
            let req = JsonRpcRequest::new(
                id,
                "tools/call",
                json!({ "name": tool_name, "arguments": arguments }),
            );

            let send_result = timeout(
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
            })?;

            match send_result {
                Ok(resp) => break resp,
                Err(err) => {
                    // Reconnect only on recoverable transport errors, within budget.
                    let recoverable_reason = err
                        .downcast_ref::<McpTransportError>()
                        .map(|te| te.to_string());
                    if let Some(reason) = recoverable_reason
                        && attempt < MAX_RECONNECT_ATTEMPTS
                    {
                        attempt += 1;
                        let server_name = inner.config.name.clone();
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Reconnect
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "mcp_server": &server_name,
                                "tool": tool_name,
                                "attempt": attempt,
                                "max_attempts": MAX_RECONNECT_ATTEMPTS,
                                "reason": &reason,
                            })),
                            "mcp_client: reconnecting after transport error and retrying tool call"
                        );
                        tokio::time::sleep(Duration::from_millis(RECONNECT_BACKOFF_MS)).await;
                        inner.transport.reset().await.with_context(|| {
                            format!(
                                "MCP server `{server_name}` failed to reset transport during reconnect"
                            )
                        })?;
                        let refreshed = handshake(inner.transport.as_mut(), &server_name)
                            .await
                            .with_context(|| {
                                format!(
                                    "MCP server `{server_name}` failed to re-handshake during reconnect"
                                )
                            })?;
                        inner.capabilities = refreshed;
                        continue;
                    }
                    return Err(err).with_context(|| {
                        format!(
                            "MCP server `{}` error during tool call `{tool_name}`",
                            inner.config.name
                        )
                    });
                }
            }
        };

        if let Some(err) = resp.error {
            bail!("MCP tool `{tool_name}` error {}: {}", err.code, err.message);
        }

        let result = resp.result.unwrap_or(serde_json::Value::Null);

        // MCP servers signal *tool-execution* failures (as opposed to JSON-RPC
        // protocol errors) with HTTP 200 + `result.isError: true` and the detail
        // in `result.content[].text`, per the MCP spec. Surface it (scrubbed and
        // length-bounded) so the failure is visible to the model and the log.
        check_result_is_error(&result, tool_name, &inner.config.name)?;

        Ok(result)
    }

    /// Generic JSON-RPC method dispatch with the same timeout, bounded
    /// reconnect, and error surfacing as `call_tool`. Returns the raw
    /// `result` value; callers apply any method-specific envelope handling.
    pub(crate) async fn dispatch_method(
        &self,
        rpc_method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let mut inner = self.inner.lock().await;

        let tool_timeout = inner
            .config
            .tool_timeout_secs
            .unwrap_or(DEFAULT_TOOL_TIMEOUT_SECS)
            .min(MAX_TOOL_TIMEOUT_SECS);

        let mut attempt = 0u32;
        let resp = loop {
            let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
            let req = JsonRpcRequest::new(id, rpc_method, params.clone());

            let send_result = timeout(
                Duration::from_secs(tool_timeout),
                inner.transport.send_and_recv(&req),
            )
            .await
            .map_err(|_| {
                anyhow::Error::msg(format!(
                    "MCP server `{}` timed out after {}s during `{rpc_method}`",
                    inner.config.name, tool_timeout
                ))
            })?;

            match send_result {
                Ok(resp) => break resp,
                Err(err) => {
                    let recoverable_reason = err
                        .downcast_ref::<McpTransportError>()
                        .map(|te| te.to_string());
                    if let Some(_reason) = recoverable_reason
                        && attempt < MAX_RECONNECT_ATTEMPTS
                    {
                        attempt += 1;
                        let server_name = inner.config.name.clone();
                        tokio::time::sleep(Duration::from_millis(RECONNECT_BACKOFF_MS)).await;
                        inner.transport.reset().await.with_context(|| {
                            format!(
                                "MCP server `{server_name}` failed to reset transport during reconnect"
                            )
                        })?;
                        let refreshed = handshake(inner.transport.as_mut(), &server_name)
                            .await
                            .with_context(|| {
                                format!(
                                    "MCP server `{server_name}` failed to re-handshake during reconnect"
                                )
                            })?;
                        inner.capabilities = refreshed;
                        continue;
                    }
                    return Err(err).with_context(|| {
                        format!(
                            "MCP server `{}` error during `{rpc_method}`",
                            inner.config.name
                        )
                    });
                }
            }
        };

        if let Some(err) = resp.error {
            bail!("MCP `{rpc_method}` error {}: {}", err.code, err.message);
        }
        let result = resp.result.unwrap_or(serde_json::Value::Null);
        check_result_is_error(&result, rpc_method, &inner.config.name)?;
        Ok(result)
    }

    /// `resources/list` — capability-gated.
    pub async fn list_resources(&self, cursor: Option<String>) -> Result<McpResourcesListResult> {
        {
            let inner = self.inner.lock().await;
            if !inner.capabilities.supports_resources() {
                bail!(
                    "MCP server `{}` does not support resources",
                    inner.config.name
                );
            }
        }
        let params = match cursor {
            Some(c) => json!({ "cursor": c }),
            None => json!({}),
        };
        let raw = self.dispatch_method("resources/list", params).await?;
        serde_json::from_value(raw).context("failed to parse resources/list result")
    }

    /// `resources/read` — capability-gated.
    pub async fn read_resource(&self, uri: &str) -> Result<McpResourceContents> {
        {
            let inner = self.inner.lock().await;
            if !inner.capabilities.supports_resources() {
                bail!(
                    "MCP server `{}` does not support resources",
                    inner.config.name
                );
            }
        }
        let raw = self
            .dispatch_method("resources/read", json!({ "uri": uri }))
            .await?;
        serde_json::from_value(raw).context("failed to parse resources/read result")
    }

    /// `prompts/list` — capability-gated.
    pub async fn list_prompts(&self, cursor: Option<String>) -> Result<McpPromptsListResult> {
        {
            let inner = self.inner.lock().await;
            if !inner.capabilities.supports_prompts() {
                bail!(
                    "MCP server `{}` does not support prompts",
                    inner.config.name
                );
            }
        }
        let params = match cursor {
            Some(c) => json!({ "cursor": c }),
            None => json!({}),
        };
        let raw = self.dispatch_method("prompts/list", params).await?;
        serde_json::from_value(raw).context("failed to parse prompts/list result")
    }

    /// `prompts/get` — capability-gated.
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<McpGetPromptResult> {
        {
            let inner = self.inner.lock().await;
            if !inner.capabilities.supports_prompts() {
                bail!(
                    "MCP server `{}` does not support prompts",
                    inner.config.name
                );
            }
        }
        let raw = self
            .dispatch_method(
                "prompts/get",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        serde_json::from_value(raw).context("failed to parse prompts/get result")
    }
}

// ── McpRegistry ───────────────────────────────────────────────────────────

/// Registry of all connected MCP servers, with a flat tool index.
pub struct McpRegistry {
    servers: Vec<McpServer>,
    /// prefixed_name → (server_index, original_tool_name)
    tool_index: HashMap<String, (usize, String)>,
    /// server name → index in `servers`.
    server_index: HashMap<String, usize>,
}

impl McpRegistry {
    /// Connect to all configured servers. Non-fatal: failures are logged and skipped.
    pub async fn connect_all(configs: &[McpServerConfig]) -> Result<Self> {
        let mut servers = Vec::new();
        let mut tool_index = HashMap::new();
        let mut server_index = HashMap::new();

        for config in configs {
            match McpServer::connect(config.clone()).await {
                Ok(server) => {
                    let server_idx = servers.len();
                    server_index.insert(config.name.clone(), server_idx);
                    // Collect tools while holding the lock once, then release
                    let tools = server.tools().await;
                    for tool in &tools {
                        // Prefix prevents name collisions across servers
                        let prefixed = format!("{}__{}", config.name, tool.name);
                        tool_index.insert(prefixed, (server_idx, tool.name.clone()));
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
            server_index,
        })
    }

    /// Build a registry with `n` placeholder servers, each backed by a no-op
    /// transport. The server names are `stub_0`, `stub_1`, ..., `stub_{n-1}`.
    ///
    /// Test-only: gated behind the `test-helpers` feature so it is NOT
    /// available in production builds. Downstream test suites (e.g.
    /// `zeroclaw-runtime::daemon`) enable the feature via their
    /// `[dev-dependencies]` declaration and use this helper to build an
    /// `Arc<McpRegistry>` whose `server_count() == n` without spawning a
    /// real stdio child, so unit tests can exercise
    /// "registry-completeness" decisions purely on `server_count()`. The
    /// transport is a local no-op so the registry is safe to drop in tests
    /// without leaking any OS resources — but it MUST NOT be used in
    /// production code: any real MCP tool call on the resulting registry
    /// will panic in the `unreachable!()` branch.
    #[cfg(feature = "test-helpers")]
    pub fn for_test_with_server_count(n: usize) -> Self {
        use crate::mcp_protocol::JsonRpcResponse;
        use async_trait::async_trait;

        /// No-op transport: never contacted in the daemon-side tests that
        /// exercise `server_count`-driven decisions. Returning `Err` would
        /// panic any caller that actually tries to use the registry; the
        /// daemon tests only read `server_count` and compare Arc pointers,
        /// so the unreachable body is acceptable.
        struct NoopTransport;

        #[async_trait]
        impl McpTransportConn for NoopTransport {
            async fn send_and_recv(
                &mut self,
                _request: &JsonRpcRequest,
            ) -> Result<JsonRpcResponse> {
                unreachable!(
                    "for_test_with_server_count registry is only used for server_count/Arc equality"
                )
            }

            async fn close(&mut self) -> Result<()> {
                Ok(())
            }
        }

        fn stub_server(name: &str) -> McpServer {
            let inner = McpServerInner {
                config: McpServerConfig {
                    name: name.to_string(),
                    ..McpServerConfig::default()
                },
                transport: Box::new(NoopTransport),
                #[cfg(target_has_atomic = "64")]
                next_id: AtomicU64::new(0),
                #[cfg(not(target_has_atomic = "64"))]
                next_id: AtomicU32::new(0),
                tools: Vec::new(),
                capabilities: McpServerCapabilities::default(),
            };
            McpServer {
                inner: Arc::new(Mutex::new(inner)),
            }
        }

        let mut servers = Vec::with_capacity(n);
        let tool_index: HashMap<String, (usize, String)> = HashMap::new();
        let mut server_index = HashMap::new();
        for i in 0..n {
            let name = format!("stub_{i}");
            let server_idx = servers.len();
            server_index.insert(name.clone(), server_idx);
            servers.push(stub_server(&name));
        }
        Self {
            servers,
            tool_index,
            server_index,
        }
    }

    /// Snapshot the live `(server_name, McpServer)` pairs registered
    /// in this registry. Returned pairs are sorted by `server_name`
    /// for deterministic ordering across ticks. Each `McpServer` is
    /// a cheap `Arc` clone of the registered handle — re-inserting it
    /// into another `McpRegistry` shares the underlying transport
    /// (no disconnect, no new stdio child).
    ///
    /// Used by the daemon heartbeat's additive reconciliation layer
    /// to preserve a healthy live connection across recovery ticks:
    /// when `current` has a healthy server whose
    /// identity still matches `fresh`, the daemon re-uses that
    /// handle instead of forcing `connect_all` to spawn a duplicate
    /// stdio child for the same endpoint.
    pub fn server_handles(&self) -> Vec<(String, McpServer)> {
        let mut out: Vec<(String, McpServer)> = self
            .server_index
            .iter()
            .map(|(name, &idx)| (name.clone(), self.servers[idx].clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Build a new registry from a list of pre-existing `McpServer`
    /// handles. The handles are cheaply Arc-cloned; transports
    /// remain alive across the move. The internal `tool_index` and
    /// `server_index` are rebuilt from each handle's advertised
    /// capabilities (synchronous `tools()` call).
    ///
    /// Companion to [`Self::server_handles`]: callers wanting to
    /// carry a healthy live connection into a fresh registry (the
    /// additive recovery path) read the handle via `server_handles`
    /// and rebuild via `from_servers`.
    pub async fn from_servers(servers: Vec<McpServer>) -> Self {
        let mut tool_index: HashMap<String, (usize, String)> = HashMap::new();
        let mut server_index: HashMap<String, usize> = HashMap::with_capacity(servers.len());
        for (idx, server) in servers.iter().enumerate() {
            let name = server.name().await;
            let tools = server.tools().await;
            for tool in &tools {
                let prefixed = format!("{}__{}", name, tool.name);
                tool_index.insert(prefixed, (idx, tool.name.clone()));
            }
            server_index.insert(name, idx);
        }
        Self {
            servers,
            tool_index,
            server_index,
        }
    }

    /// Test-only: build a registry from pre-existing `(name, handle)`
    /// pairs. Used by regression tests that need to assert the
    /// daemon's reconciliation layer preserves a healthy server's
    /// `McpServer` identity (cheap Arc pointer) across a recovery
    /// tick. The handles are not re-validated — caller is responsible
    /// for ensuring each `McpServer`'s `inner.config.name` matches the
    /// paired name.
    ///
    /// Tool index is left empty: callers in regression tests
    /// exercise identity / `server_count` / `server_names` only,
    /// never tool lookup. Use [`Self::from_servers`] in production
    /// paths where tool routing must remain valid.
    #[cfg(feature = "test-helpers")]
    pub fn for_test_with_server_handles(handles: Vec<(String, McpServer)>) -> Self {
        let mut servers: Vec<McpServer> = Vec::with_capacity(handles.len());
        let tool_index: HashMap<String, (usize, String)> = HashMap::new();
        let mut server_index: HashMap<String, usize> = HashMap::with_capacity(handles.len());
        for (idx, (name, server)) in handles.into_iter().enumerate() {
            server_index.insert(name, idx);
            servers.push(server);
        }
        Self {
            servers,
            tool_index,
            server_index,
        }
    }

    /// Test-only: build a single stub `McpServer` handle with the
    /// given `name`. The transport is a no-op (any actual call
    /// panics in `unreachable!()`); only safe to use in regression
    /// tests that exercise identity (`ptr_eq`) / `server_count` /
    /// `server_names` / `health_check_all` and never make a real
    /// tool call.
    ///
    /// Used to construct test registries where two registry builders
    /// share a server handle — e.g. a healthy A handle that must
    /// survive across a recovery tick into a freshly-merged registry.
    #[cfg(feature = "test-helpers")]
    pub fn for_test_make_stub_server(name: &str) -> McpServer {
        use crate::mcp_protocol::JsonRpcResponse;
        use async_trait::async_trait;

        struct NoopTransport;

        #[async_trait]
        impl McpTransportConn for NoopTransport {
            async fn send_and_recv(
                &mut self,
                _request: &JsonRpcRequest,
            ) -> Result<JsonRpcResponse> {
                unreachable!(
                    "for_test_make_stub_server is only used for identity / \
                     ptr_eq / server_count assertions — never for actual tool calls"
                )
            }

            async fn close(&mut self) -> Result<()> {
                Ok(())
            }
        }

        let inner = McpServerInner {
            config: McpServerConfig {
                name: name.to_string(),
                ..McpServerConfig::default()
            },
            transport: Box::new(NoopTransport),
            #[cfg(target_has_atomic = "64")]
            next_id: AtomicU64::new(0),
            #[cfg(not(target_has_atomic = "64"))]
            next_id: AtomicU32::new(0),
            tools: Vec::new(),
            capabilities: McpServerCapabilities::default(),
        };
        McpServer {
            inner: std::sync::Arc::new(Mutex::new(inner)),
        }
    }

    /// All prefixed tool names across all connected servers.
    pub fn tool_names(&self) -> Vec<String> {
        self.tool_index.keys().cloned().collect()
    }

    /// Tool definition for a given prefixed name (cloned).
    pub async fn get_tool_def(&self, prefixed_name: &str) -> Option<McpToolDef> {
        let (server_idx, original_name) = self.tool_index.get(prefixed_name)?;
        let inner = self.servers[*server_idx].inner.lock().await;
        inner
            .tools
            .iter()
            .find(|t| &t.name == original_name)
            .cloned()
    }

    /// Execute a tool by prefixed name.
    pub async fn call_tool(
        &self,
        prefixed_name: &str,
        arguments: serde_json::Value,
    ) -> Result<String> {
        let (server_idx, original_name) = self.tool_index.get(prefixed_name).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"tool": prefixed_name})),
                "mcp_client: unknown MCP tool"
            );
            anyhow::Error::msg(format!("unknown MCP tool `{prefixed_name}`"))
        })?;
        let result = self.servers[*server_idx]
            .call_tool(original_name, arguments)
            .await?;
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

    /// Names of all connected servers.
    pub fn server_names(&self) -> Vec<String> {
        self.server_index.keys().cloned().collect()
    }

    /// Check health of every connected server. Returns names of dead servers.
    pub fn health_check_all(&self) -> Vec<String> {
        let names = self.server_names();
        let mut dead = Vec::new();
        for name in names {
            if let Some(&idx) = self.server_index.get(&name)
                && !self.servers[idx].health_check()
            {
                dead.push(name);
            }
        }
        dead
    }

    /// Remove servers whose transport is dead.
    /// Returns the names of servers that were removed.
    ///
    /// This is a no-op for the `for_test_with_server_count` registries used in
    /// unit tests — those have no-op transports that always report alive, so
    /// no server will ever be removed.
    pub async fn kill_dead_connections(&mut self) -> Vec<String> {
        let dead = self.health_check_all();
        if dead.is_empty() {
            return dead;
        }

        // Rebuild the registry without dead servers, updating indices
        // so tool_index references remain valid.
        let dead_set: std::collections::HashSet<_> = dead.iter().cloned().collect();

        let mut new_servers = Vec::with_capacity(self.servers.len());
        let mut new_server_index = HashMap::with_capacity(self.server_index.len());
        let mut old_to_new_idx = HashMap::with_capacity(self.server_index.len());

        for (name, &old_idx) in &self.server_index {
            if dead_set.contains(name) {
                continue;
            }
            let new_idx = new_servers.len();
            new_servers.push(self.servers[old_idx].clone());
            old_to_new_idx.insert(old_idx, new_idx);
            new_server_index.insert(name.clone(), new_idx);
        }

        let mut new_tool_index = HashMap::with_capacity(self.tool_index.len());
        for (prefixed, (old_srv_idx, tool_name)) in &self.tool_index {
            if let Some(&new_srv_idx) = old_to_new_idx.get(old_srv_idx) {
                new_tool_index.insert(prefixed.clone(), (new_srv_idx, tool_name.clone()));
            }
        }

        self.servers = new_servers;
        self.server_index = new_server_index;
        self.tool_index = new_tool_index;

        dead
    }

    /// Split a `<server>__<rest>` prefixed name. Returns None if no prefix.
    pub fn split_prefixed(prefixed: &str) -> Option<(String, String)> {
        prefixed
            .split_once("__")
            .map(|(s, r)| (s.to_string(), r.to_string()))
    }

    fn server_by_name(&self, name: &str) -> Option<&McpServer> {
        self.server_index.get(name).map(|i| &self.servers[*i])
    }

    /// Whether the named server advertised resource capability.
    pub async fn server_supports_resources(&self, name: &str) -> bool {
        match self.server_by_name(name) {
            Some(srv) => srv.capabilities().await.supports_resources(),
            None => false,
        }
    }

    /// Whether the named server advertised prompt capability.
    pub async fn server_supports_prompts(&self, name: &str) -> bool {
        match self.server_by_name(name) {
            Some(srv) => srv.capabilities().await.supports_prompts(),
            None => false,
        }
    }

    /// Read a resource by prefixed uri (`<server>__<uri>`).
    pub async fn read_resource(
        &self,
        prefixed_uri: &str,
    ) -> Result<crate::mcp_resource::McpResourceContents> {
        let (server, uri) = Self::split_prefixed(prefixed_uri).ok_or_else(|| {
            anyhow::Error::msg(format!("missing server prefix in `{prefixed_uri}`"))
        })?;
        let srv = self
            .server_by_name(&server)
            .ok_or_else(|| anyhow::Error::msg(format!("unknown MCP server `{server}`")))?;
        srv.read_resource(&uri).await
    }

    /// Get a prompt by prefixed name (`<server>__<name>`).
    pub async fn get_prompt(
        &self,
        prefixed_name: &str,
        arguments: serde_json::Value,
    ) -> Result<crate::mcp_prompt::McpGetPromptResult> {
        let (server, name) = Self::split_prefixed(prefixed_name).ok_or_else(|| {
            anyhow::Error::msg(format!("missing server prefix in `{prefixed_name}`"))
        })?;
        let srv = self
            .server_by_name(&server)
            .ok_or_else(|| anyhow::Error::msg(format!("unknown MCP server `{server}`")))?;
        srv.get_prompt(&name, arguments).await
    }

    /// List one server's resources with optional pagination cursor. Returns the
    /// prefixed defs and the server's `next_cursor` (if any). The `cursor` is the
    /// opaque token from a prior page's `next_cursor` for this same server.
    pub async fn list_server_resources(
        &self,
        server: &str,
        cursor: Option<String>,
    ) -> Result<(Vec<crate::mcp_resource::McpResourceDef>, Option<String>)> {
        let srv = self
            .server_by_name(server)
            .ok_or_else(|| anyhow::Error::msg(format!("unknown MCP server `{server}`")))?;
        let list = srv.list_resources(cursor).await?;
        let next = list.next_cursor.clone();
        let defs = list
            .resources
            .into_iter()
            .map(|mut def| {
                def.uri = format!("{server}__{}", def.uri);
                def
            })
            .collect();
        Ok((defs, next))
    }

    /// List one server's prompts with optional pagination cursor. Returns the
    /// prefixed defs and the server's `next_cursor` (if any).
    pub async fn list_server_prompts(
        &self,
        server: &str,
        cursor: Option<String>,
    ) -> Result<(Vec<crate::mcp_prompt::McpPromptDef>, Option<String>)> {
        let srv = self
            .server_by_name(server)
            .ok_or_else(|| anyhow::Error::msg(format!("unknown MCP server `{server}`")))?;
        let list = srv.list_prompts(cursor).await?;
        let next = list.next_cursor.clone();
        let defs = list
            .prompts
            .into_iter()
            .map(|mut def| {
                def.name = format!("{server}__{}", def.name);
                def
            })
            .collect();
        Ok((defs, next))
    }

    /// List resources across all servers that support them. Each entry's uri is
    /// returned prefixed with `<server>__`. Per-server errors are skipped.
    pub async fn list_all_resources(&self) -> Vec<(String, crate::mcp_resource::McpResourceDef)> {
        let mut out = Vec::new();
        for (name, idx) in &self.server_index {
            let srv = &self.servers[*idx];
            if let Ok(list) = srv.list_resources(None).await {
                for mut def in list.resources {
                    let prefixed_uri = format!("{name}__{}", def.uri);
                    def.uri = prefixed_uri.clone();
                    out.push((prefixed_uri, def));
                }
            }
        }
        out
    }

    /// List prompts across all servers that support them, prefixed by server.
    pub async fn list_all_prompts(&self) -> Vec<(String, crate::mcp_prompt::McpPromptDef)> {
        let mut out = Vec::new();
        for (name, idx) in &self.server_index {
            let srv = &self.servers[*idx];
            if let Ok(list) = srv.list_prompts(None).await {
                for mut def in list.prompts {
                    // Rewrite the def's name to the prefixed form so the value
                    // emitted by `mcp_prompts list` can be passed straight back
                    // to `mcp_prompts get` (mirrors `list_all_resources`).
                    let prefixed = format!("{name}__{}", def.name);
                    def.name = prefixed.clone();
                    out.push((prefixed, def));
                }
            }
        }
        out
    }
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
    fn split_prefix_separates_server_and_rest() {
        assert_eq!(
            McpRegistry::split_prefixed("srvA__file:///x"),
            Some(("srvA".to_string(), "file:///x".to_string()))
        );
        assert_eq!(McpRegistry::split_prefixed("noprefix"), None);
    }

    #[tokio::test]
    async fn registry_server_supports_flags_default_false() {
        let registry = McpRegistry::connect_all(&[]).await.expect("connect_all");
        assert!(!registry.server_supports_resources("missing").await);
        assert!(!registry.server_supports_prompts("missing").await);
    }

    #[tokio::test]
    async fn registry_read_resource_unknown_server_errors() {
        let registry = McpRegistry::connect_all(&[]).await.expect("connect_all");
        let err = registry
            .read_resource("ghost__file:///x")
            .await
            .expect_err("unknown server should error");
        assert!(err.to_string().contains("unknown MCP server"), "got: {err}");
    }

    #[tokio::test]
    async fn registry_get_prompt_unknown_server_errors() {
        let registry = McpRegistry::connect_all(&[]).await.expect("connect_all");
        let err = registry
            .get_prompt("ghost__p", serde_json::json!({}))
            .await
            .expect_err("unknown server should error");
        assert!(err.to_string().contains("unknown MCP server"), "got: {err}");
    }

    #[tokio::test]
    async fn registry_list_all_empty_for_empty_registry() {
        let registry = McpRegistry::connect_all(&[]).await.expect("connect_all");
        assert!(registry.list_all_resources().await.is_empty());
        assert!(registry.list_all_prompts().await.is_empty());
    }

    #[tokio::test]
    async fn list_server_prompts_prefixes_name_and_returns_cursor() {
        use wiremock::matchers::{body_partial_json, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // initialize advertises prompts capability so the method is not gated.
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "initialize"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Mcp-Session-Id", "s")
                    .set_body_json(json!({
                        "jsonrpc":"2.0","id":1,
                        "result":{"capabilities":{"prompts":{}}}
                    })),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({"method":"notifications/initialized"}),
            ))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method":"tools/list"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc":"2.0","id":2,"result":{"tools":[]}
            })))
            .mount(&server)
            .await;
        // prompts/list returns a bare name plus a nextCursor.
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method":"prompts/list"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc":"2.0","id":3,
                "result":{"prompts":[{"name":"summarize"}],"nextCursor":"page2"}
            })))
            .mount(&server)
            .await;

        let registry = McpRegistry::connect_all(&[http_server_config(server.uri())])
            .await
            .expect("connect_all");

        // The configured server name is "remote" (see http_server_config).
        let (defs, next) = registry
            .list_server_prompts("remote", None)
            .await
            .expect("list_server_prompts should succeed");
        assert_eq!(defs.len(), 1);
        // Regression: the listed name must be the prefixed form that `get` needs.
        assert_eq!(defs[0].name, "remote__summarize");
        // Regression: the server's nextCursor must be surfaced to the caller.
        assert_eq!(next.as_deref(), Some("page2"));

        // And list_all_prompts must also carry the prefixed name in the def.
        let all = registry.list_all_prompts().await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].1.name, "remote__summarize");
    }

    #[tokio::test]
    async fn connect_nonexistent_command_fails_cleanly() {
        // A command that doesn't exist should fail at spawn, not panic.
        let config = McpServerConfig {
            pinned_resources: Vec::new(),
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
            pinned_resources: Vec::new(),
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
            pinned_resources: Vec::new(),
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

    /// Transport that ignores the request and always returns one preset result.
    struct FakeTransport {
        result: serde_json::Value,
    }

    #[async_trait::async_trait]
    impl McpTransportConn for FakeTransport {
        async fn send_and_recv(
            &mut self,
            _request: &JsonRpcRequest,
        ) -> Result<crate::mcp_protocol::JsonRpcResponse> {
            Ok(crate::mcp_protocol::JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: Some(serde_json::json!(1)),
                result: Some(self.result.clone()),
                error: None,
            })
        }

        async fn close(&mut self) -> Result<()> {
            Ok(())
        }
    }

    /// Build an `McpServer` whose transport yields `result` on every call.
    fn server_returning(result: serde_json::Value) -> McpServer {
        let inner = McpServerInner {
            config: McpServerConfig {
                name: "fake".into(),
                ..Default::default()
            },
            transport: Box::new(FakeTransport { result }),
            #[cfg(target_has_atomic = "64")]
            next_id: AtomicU64::new(3),
            #[cfg(not(target_has_atomic = "64"))]
            next_id: AtomicU32::new(3),
            tools: vec![],
            capabilities: McpServerCapabilities::default(),
        };
        McpServer {
            inner: Arc::new(Mutex::new(inner)),
        }
    }

    /// Like `server_returning`, but with explicit advertised capabilities.
    fn server_with_caps_returning(
        capabilities: McpServerCapabilities,
        result: serde_json::Value,
    ) -> McpServer {
        let inner = McpServerInner {
            config: McpServerConfig {
                name: "fake".into(),
                ..Default::default()
            },
            transport: Box::new(FakeTransport { result }),
            #[cfg(target_has_atomic = "64")]
            next_id: AtomicU64::new(3),
            #[cfg(not(target_has_atomic = "64"))]
            next_id: AtomicU32::new(3),
            tools: vec![],
            capabilities,
        };
        McpServer {
            inner: Arc::new(Mutex::new(inner)),
        }
    }

    #[tokio::test]
    async fn list_resources_gated_when_unsupported() {
        let server = server_returning(serde_json::json!({}));
        let err = server
            .list_resources(None)
            .await
            .expect_err("unsupported resources must error locally");
        assert!(
            err.to_string().contains("does not support resources"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn list_resources_parses_when_supported() {
        let server = server_with_caps_returning(
            McpServerCapabilities {
                resources: true,
                prompts: false,
            },
            serde_json::json!({"resources":[{"uri":"u","name":"n"}],"nextCursor":"c"}),
        );
        let res = server.list_resources(None).await.expect("should parse");
        assert_eq!(res.resources.len(), 1);
        assert_eq!(res.next_cursor.as_deref(), Some("c"));
    }

    #[tokio::test]
    async fn get_prompt_gated_when_unsupported() {
        let server = server_returning(serde_json::json!({}));
        let err = server
            .get_prompt("p", serde_json::json!({}))
            .await
            .expect_err("unsupported prompts must error locally");
        assert!(
            err.to_string().contains("does not support prompts"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn get_prompt_parses_when_supported() {
        let server = server_with_caps_returning(
            McpServerCapabilities {
                resources: false,
                prompts: true,
            },
            serde_json::json!({"messages":[{"role":"user","content":{"type":"text","text":"hi"}}]}),
        );
        let res = server
            .get_prompt("p", serde_json::json!({}))
            .await
            .expect("parse");
        assert_eq!(res.messages.len(), 1);
    }

    #[tokio::test]
    async fn call_tool_iserror_err_is_sanitized_and_bounded() {
        // A secret token in the server-controlled detail must be redacted
        // before it reaches the returned error (and, by the same code path,
        // the daemon log).
        let server = server_returning(serde_json::json!({
            "isError": true,
            "content": [{ "type": "text", "text": "auth failed using sk-supersecrettoken12345abcdef" }],
        }));
        let err = server
            .call_tool("do_thing", serde_json::json!({}))
            .await
            .expect_err("isError:true must map to Err");
        let msg = err.to_string();
        assert!(msg.contains("returned isError"), "got: {msg}");
        assert!(msg.contains("[REDACTED]"), "secret not scrubbed: {msg}");
        assert!(
            !msg.contains("supersecrettoken"),
            "raw secret leaked: {msg}"
        );

        // Oversized server text must be truncated; sanitize_api_error caps the
        // detail at 500 chars and appends an ellipsis.
        let huge = "A".repeat(5000);
        let server = server_returning(serde_json::json!({
            "isError": true,
            "content": [{ "type": "text", "text": huge }],
        }));
        let msg = server
            .call_tool("do_thing", serde_json::json!({}))
            .await
            .expect_err("isError:true must map to Err")
            .to_string();
        assert!(
            msg.contains("..."),
            "bounded detail should be truncated: {msg}"
        );
        assert!(
            msg.len() < 1000,
            "5000-char payload not bounded: len={}",
            msg.len()
        );
    }

    #[tokio::test]
    async fn call_tool_success_returns_ok_result() {
        // isError absent → Ok with the raw result untouched.
        let payload = serde_json::json!({
            "content": [{ "type": "text", "text": "all good" }],
        });
        let out = server_returning(payload.clone())
            .call_tool("do_thing", serde_json::json!({}))
            .await
            .expect("absent isError must be Ok");
        assert_eq!(out, payload);

        // isError explicitly false → still Ok.
        let payload = serde_json::json!({ "isError": false, "value": 42 });
        let out = server_returning(payload.clone())
            .call_tool("do_thing", serde_json::json!({}))
            .await
            .expect("isError:false must be Ok");
        assert_eq!(out, payload);
    }

    #[tokio::test]
    async fn call_tool_iserror_empty_detail_falls_back() {
        // isError true but no content array → fallback message.
        let msg = server_returning(serde_json::json!({ "isError": true }))
            .call_tool("do_thing", serde_json::json!({}))
            .await
            .expect_err("isError:true must map to Err")
            .to_string();
        assert!(
            msg.contains("(no error detail returned by server)"),
            "got: {msg}"
        );

        // isError true with content present but empty text → same fallback.
        let msg = server_returning(serde_json::json!({
            "isError": true,
            "content": [{ "type": "text", "text": "" }],
        }))
        .call_tool("do_thing", serde_json::json!({}))
        .await
        .expect_err("isError:true must map to Err")
        .to_string();
        assert!(
            msg.contains("(no error detail returned by server)"),
            "got: {msg}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_stdio_registry_reaps_child_process() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::path::Path;
        use tokio::time::{Duration, sleep};

        fn process_is_alive(pid: u32) -> bool {
            std::process::Command::new("kill")
                .arg("-0")
                .arg(pid.to_string())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        }

        async fn read_pid(path: &Path) -> u32 {
            for _ in 0..50 {
                if let Ok(raw) = tokio::fs::read_to_string(path).await
                    && let Ok(pid) = raw.trim().parse()
                {
                    return pid;
                }
                sleep(Duration::from_millis(20)).await;
            }
            panic!("stdio MCP test server did not write its pid");
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let server_path = temp.path().join("echo-mcp.sh");
        let pid_path = temp.path().join("echo-mcp.pid");
        let mut script = std::fs::File::create(&server_path).expect("script");
        script
            .write_all(
                br#"#!/bin/sh
echo "$$" > "$1"
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"echo-mcp","version":"0.1.0"}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}'
      exec tail -f /dev/null
      ;;
  esac
done
"#,
            )
            .expect("write script");
        drop(script);
        let mut perms = std::fs::metadata(&server_path)
            .expect("metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&server_path, perms).expect("chmod");

        let config = McpServerConfig {
            pinned_resources: Vec::new(),
            name: "echo".to_string(),
            command: server_path.display().to_string(),
            args: vec![pid_path.display().to_string()],
            env: std::collections::HashMap::default(),
            tool_timeout_secs: None,
            transport: McpTransport::Stdio,
            url: None,
            headers: std::collections::HashMap::default(),
        };

        let registry = McpRegistry::connect_all(&[config])
            .await
            .expect("connect_all should not fail");
        assert_eq!(registry.server_count(), 1);
        assert_eq!(registry.tool_count(), 0);
        let child_pid = read_pid(&pid_path).await;
        assert!(
            process_is_alive(child_pid),
            "stdio MCP child should be alive while the registry is alive"
        );

        drop(registry);

        for _ in 0..50 {
            if !process_is_alive(child_pid) {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
        panic!("stdio MCP child process {child_pid} survived after registry drop");
    }

    // ── Server capabilities parsing ──────────────────────────────────────────

    #[test]
    fn capabilities_parse_from_init_result() {
        let init = serde_json::json!({
            "capabilities": {
                "resources": { "subscribe": true, "listChanged": false },
                "prompts": { "listChanged": true }
            }
        });
        let caps = McpServerCapabilities::from_init_result(&init);
        assert!(caps.supports_resources());
        assert!(caps.supports_prompts());
    }

    #[test]
    fn capabilities_absent_means_unsupported() {
        let init = serde_json::json!({ "capabilities": {} });
        let caps = McpServerCapabilities::from_init_result(&init);
        assert!(!caps.supports_resources());
        assert!(!caps.supports_prompts());
    }

    #[test]
    fn capabilities_missing_object_is_unsupported() {
        let init = serde_json::json!({});
        let caps = McpServerCapabilities::from_init_result(&init);
        assert!(!caps.supports_resources());
        assert!(!caps.supports_prompts());
    }

    // ── Reconnect on stale session (streamable HTTP) ───────────────────────

    fn http_server_config(uri: String) -> McpServerConfig {
        McpServerConfig {
            name: "remote".into(),
            transport: McpTransport::Http,
            url: Some(uri),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn call_tool_reconnects_on_stale_session() {
        use wiremock::matchers::{body_partial_json, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // initialize → 200 + session header. Hit twice: initial connect plus the
        // reconnect that follows the stale-session error.
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "initialize"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Mcp-Session-Id", "sess-1")
                    .set_body_json(json!({"jsonrpc": "2.0", "id": 1, "result": {}})),
            )
            .expect(2)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({"method": "notifications/initialized"}),
            ))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "tools/list"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {"tools": [{"name": "echo", "description": "d", "inputSchema": {"type": "object"}}]}
            })))
            .expect(1)
            .mount(&server)
            .await;

        // First tools/call → 404 (stale session). Highest priority, single use,
        // so after it is exhausted the success mock below takes over.
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "tools/call"})))
            .respond_with(ResponseTemplate::new(404))
            .up_to_n_times(1)
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;

        // Retried tools/call after reconnect → success.
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "tools/call"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0", "id": 3, "result": {"ok": true}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let srv = McpServer::connect(http_server_config(server.uri()))
            .await
            .expect("connect");
        let result = srv
            .call_tool("echo", json!({}))
            .await
            .expect("call_tool should succeed after reconnect");
        assert_eq!(result, json!({"ok": true}));
        server.verify().await;
    }

    #[tokio::test]
    async fn call_tool_does_not_retry_on_tool_error() {
        use wiremock::matchers::{body_partial_json, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // initialize is expected exactly once — a genuine tool error must NOT
        // trigger a reconnect (which would re-run initialize).
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "initialize"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Mcp-Session-Id", "sess-1")
                    .set_body_json(json!({"jsonrpc": "2.0", "id": 1, "result": {}})),
            )
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({"method": "notifications/initialized"}),
            ))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "tools/list"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {"tools": [{"name": "echo", "description": "d", "inputSchema": {"type": "object"}}]}
            })))
            .mount(&server)
            .await;

        // tools/call → JSON-RPC error body over HTTP 200 (a real tool failure).
        // Expected exactly once: no retry.
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "tools/call"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0", "id": 3, "error": {"code": -32000, "message": "boom"}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let srv = McpServer::connect(http_server_config(server.uri()))
            .await
            .expect("connect");
        let err = srv
            .call_tool("echo", json!({}))
            .await
            .expect_err("tool error should surface");
        assert!(err.to_string().contains("boom"), "got: {err}");
        server.verify().await;
    }

    #[tokio::test]
    async fn call_tool_does_not_retry_sessionless_404() {
        use wiremock::matchers::{body_partial_json, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // initialize returns 200 with NO Mcp-Session-Id header — a stateless server,
        // so the transport never holds a session id. Expected exactly once: a 404
        // with no session in play must NOT trigger a reconnect (re-running initialize).
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "initialize"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"jsonrpc": "2.0", "id": 1, "result": {}})),
            )
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({"method": "notifications/initialized"}),
            ))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "tools/list"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {"tools": [{"name": "echo", "description": "d", "inputSchema": {"type": "object"}}]}
            })))
            .mount(&server)
            .await;

        // tools/call → 404 with no session. This is a missing endpoint, not a stale
        // session: it surfaces as a plain error and is hit exactly once (no retry).
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "tools/call"})))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        let srv = McpServer::connect(http_server_config(server.uri()))
            .await
            .expect("connect");
        let err = srv
            .call_tool("echo", json!({}))
            .await
            .expect_err("sessionless 404 should surface as an error");
        // The 404 lives in the error source chain (call_tool wraps it with context).
        assert!(
            format!("{err:?}").contains("MCP server returned HTTP 404"),
            "got: {err:?}"
        );
        // server.verify() pins the no-retry: initialize and tools/call each hit once.
        server.verify().await;
    }

    // ── dispatch_method: generic JSON-RPC dispatch ────────────────────────

    #[tokio::test]
    async fn dispatch_method_returns_raw_result() {
        let server = server_returning(serde_json::json!({ "ok": 1 }));
        let out = server
            .dispatch_method("resources/list", serde_json::json!({}))
            .await
            .expect("dispatch should succeed");
        assert_eq!(out, serde_json::json!({ "ok": 1 }));
    }

    #[tokio::test]
    async fn dispatch_method_surfaces_is_error_envelope_scrubbed() {
        // An `isError: true` envelope on a resources/prompts result must map to
        // Err (not be returned as success), with the server-controlled detail
        // secret-scrubbed and length-bounded — same contract as `call_tool`.
        let server = server_returning(serde_json::json!({
            "isError": true,
            "content": [{ "type": "text", "text": "boom using sk-supersecrettoken12345abcdef" }],
        }));
        let err = server
            .dispatch_method("resources/read", serde_json::json!({}))
            .await
            .expect_err("isError:true must map to Err");
        let msg = err.to_string();
        assert!(msg.contains("returned isError"), "got: {msg}");
        assert!(msg.contains("[REDACTED]"), "secret not scrubbed: {msg}");
        assert!(
            !msg.contains("supersecrettoken"),
            "raw secret leaked: {msg}"
        );
    }

    #[tokio::test]
    async fn dispatch_method_surfaces_jsonrpc_error() {
        use wiremock::matchers::{body_partial_json, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "initialize"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Mcp-Session-Id", "s")
                    .set_body_json(
                        json!({"jsonrpc":"2.0","id":1,"result":{"capabilities":{"resources":{}}}}),
                    ),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({"method": "notifications/initialized"}),
            ))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "tools/list"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc":"2.0","id":2,"result":{"tools":[]}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "resources/list"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc":"2.0","id":3,"error":{"code":-32601,"message":"nope"}
            })))
            .mount(&server)
            .await;

        let srv = McpServer::connect(http_server_config(server.uri()))
            .await
            .expect("connect");
        let err = srv
            .dispatch_method("resources/list", json!({}))
            .await
            .expect_err("jsonrpc error should surface");
        assert!(err.to_string().contains("nope"), "got: {err}");
    }
}
