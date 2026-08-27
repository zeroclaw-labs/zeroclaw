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
use tokio::sync::{Mutex, RwLock};
use tokio::time::{Duration, Instant, timeout, timeout_at};

use crate::mcp_prompt::{McpGetPromptResult, McpPromptsListResult};
use crate::mcp_protocol::{JsonRpcRequest, MCP_PROTOCOL_VERSION, McpToolDef, McpToolsListResult};
use crate::mcp_resource::{McpResourceContents, McpResourcesListResult};
use crate::mcp_transport::{
    McpRecoveryGate, McpRequestLifecycle, McpTransportError, SharedMcpTransportConn,
    create_shared_transport,
};
use zeroclaw_config::schema::{McpServerConfig, McpTransport};

/// Timeout for receiving a response from an MCP server during init/list.
/// Prevents a hung server from blocking the daemon indefinitely.
const RECV_TIMEOUT_SECS: u64 = 30;

/// Default timeout for tool calls (seconds) when not configured per-server.
const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 180;

/// Maximum allowed tool call timeout (seconds) — hard safety ceiling.
const MAX_TOOL_TIMEOUT_SECS: u64 = 600;

/// Maximum automatic reconnect attempts when a request is known not to have
/// been written. Outcome-unknown requests are never replayed.
const MAX_RECONNECT_ATTEMPTS: u32 = 2;

/// Perform the MCP `initialize` + `notifications/initialized` handshake on a
/// transport. Shared by the initial [`McpServer::connect`] and the
/// reconnect-after-stale-session path in [`McpServer::call_tool`].
async fn handshake(
    transport: &dyn SharedMcpTransportConn,
    server_name: &str,
    epoch: u64,
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

    let init_lifecycle = McpRequestLifecycle::uncoordinated(epoch);
    let init_resp = timeout(
        Duration::from_secs(RECV_TIMEOUT_SECS),
        transport.send_and_recv(&init_req, &init_lifecycle),
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
    let notif_lifecycle = McpRequestLifecycle::uncoordinated(epoch);
    let _ = transport.send_and_recv(&notif, &notif_lifecycle).await;

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
    #[cfg(target_has_atomic = "64")]
    next_id: AtomicU64,
    #[cfg(not(target_has_atomic = "64"))]
    next_id: AtomicU32,
    tools: Vec<McpToolDef>,
    capabilities: McpServerCapabilities,
}

// ── Recovery barrier ────────────────────────────────────────────────────────

/// Synchronously-published gate that blocks new writes while a post-write
/// outcome-unknown request is being recovered.
///
/// When a request's outcome becomes unknown after its bytes may have reached
/// the server, `arm` is called *synchronously* — before any lock the failing
/// request held is released — so that a second write already queued on the
/// serial/epoch gate observes the recovery-needed state and waits, instead of
/// racing ahead onto the ambiguous session. `finish` clears the gate once
/// reset + re-handshake succeed; `poison` leaves it permanently closed after a
/// failed recovery so later writes fail closed rather than proceeding without a
/// successful MCP handshake.
struct RecoveryBarrier {
    /// The epoch that must be recovered before writes may resume. `None` means
    /// no recovery is pending.
    needed_epoch: std::sync::Mutex<Option<u64>>,
    /// Set once recovery has permanently failed; the connection is unusable.
    poisoned: std::sync::atomic::AtomicBool,
    /// Pulsed whenever the recovery-needed state changes (cleared or poisoned)
    /// so writers waiting in `wait_ready` wake up.
    notify: tokio::sync::Notify,
}

impl RecoveryBarrier {
    fn new() -> Self {
        Self {
            needed_epoch: std::sync::Mutex::new(None),
            poisoned: std::sync::atomic::AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }

    fn needed_epoch(&self) -> std::sync::MutexGuard<'_, Option<u64>> {
        match self.needed_epoch.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Publish that the observed epoch now needs recovery before any further
    /// write. Idempotent for a given epoch; a newer epoch supersedes an older
    /// pending one. This is intentionally synchronous (no `.await`) so it takes
    /// effect the instant an outcome becomes unknown.
    fn arm(&self, epoch: u64) {
        let mut needed = self.needed_epoch();
        match *needed {
            Some(existing) if existing >= epoch => {}
            _ => *needed = Some(epoch),
        }
    }

    /// Clear the recovery-needed state after a successful reset + re-handshake
    /// for `recovered_epoch`, then wake any waiting writers.
    fn finish(&self, recovered_epoch: u64) {
        {
            let mut needed = self.needed_epoch();
            if matches!(*needed, Some(pending) if pending <= recovered_epoch) {
                *needed = None;
            }
        }
        self.notify.notify_waiters();
    }

    /// Mark recovery as permanently failed. Subsequent writers fail closed.
    fn poison(&self) {
        self.poisoned
            .store(true, std::sync::atomic::Ordering::Release);
        self.notify.notify_waiters();
    }

    fn is_poisoned(&self) -> bool {
        self.poisoned.load(std::sync::atomic::Ordering::Acquire)
    }

    fn recovery_pending(&self) -> bool {
        self.needed_epoch().is_some()
    }
}

impl McpRecoveryGate for RecoveryBarrier {
    fn arm(&self, epoch: u64) {
        RecoveryBarrier::arm(self, epoch);
    }

    fn write_blocked(&self) -> bool {
        self.is_poisoned() || self.recovery_pending()
    }
}

/// RAII guard that arms the recovery barrier synchronously if a write's outcome
/// became unknown, at the moment its `send_request` future completes or is
/// cancelled. Held under the serial gate and dropped before it, so a queued
/// writer observes the armed barrier before it can acquire the gate.
struct WriteBarrierArm<'a> {
    recovery: &'a RecoveryBarrier,
    lifecycle: &'a McpRequestLifecycle,
}

impl Drop for WriteBarrierArm<'_> {
    fn drop(&mut self) {
        if let Some(epoch) = self.lifecycle.outcome_unknown_epoch() {
            self.recovery.arm(epoch);
        }
    }
}

// ── McpServer ──────────────────────────────────────────────────────────────

/// A live connection to one MCP server (any transport).
#[derive(Clone)]
pub struct McpServer {
    inner: Arc<Mutex<McpServerInner>>,
    transport: Arc<dyn SharedMcpTransportConn>,
    epoch_gate: Arc<RwLock<u64>>,
    /// Preserves the existing single-request behavior for HTTP/SSE while
    /// allowing stdio requests to multiplex by response id.
    serial_gate: Option<Arc<Mutex<()>>>,
    /// Synchronously-published gate that holds back new writes until an
    /// outcome-unknown request has been recovered (or fails them closed after a
    /// failed recovery).
    recovery: Arc<RecoveryBarrier>,
}

struct OutcomeUnknownGuard {
    server: McpServer,
    lifecycle: Arc<McpRequestLifecycle>,
    operation: String,
    armed: bool,
}

impl OutcomeUnknownGuard {
    fn new(server: McpServer, lifecycle: Arc<McpRequestLifecycle>, operation: String) -> Self {
        Self {
            server,
            lifecycle,
            operation,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for OutcomeUnknownGuard {
    fn drop(&mut self) {
        if self.armed
            && let Some(epoch) = self.lifecycle.outcome_unknown_epoch()
        {
            self.server.spawn_recovery(epoch, self.operation.clone());
        }
    }
}

impl McpServer {
    /// Connect to the server, perform the initialize handshake, and fetch the tool list.
    pub async fn connect(config: McpServerConfig) -> Result<Self> {
        // Create transport based on config
        let transport: Arc<dyn SharedMcpTransportConn> =
            Arc::from(create_shared_transport(&config).with_context(|| {
                format!(
                    "failed to create transport for MCP server `{}`",
                    config.name
                )
            })?);
        let epoch_gate = Arc::new(RwLock::new(0));
        let serial_gate =
            (config.transport != McpTransport::Stdio).then(|| Arc::new(Mutex::new(())));

        // Initialize handshake (initialize + initialized notification)
        let capabilities = handshake(transport.as_ref(), &config.name, 0).await?;

        // Fetch available tools
        let id = 2u64;
        let list_req = JsonRpcRequest::new(id, "tools/list", json!({}));

        let list_lifecycle = McpRequestLifecycle::uncoordinated(0);
        let list_resp = timeout(
            Duration::from_secs(RECV_TIMEOUT_SECS),
            transport.send_and_recv(&list_req, &list_lifecycle),
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
            transport,
            epoch_gate,
            serial_gate,
            recovery: Arc::new(RecoveryBarrier::new()),
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
    /// This reads transport-owned atomic connection state and does not acquire
    /// the async server metadata lock.
    pub fn health_check(&self) -> bool {
        self.transport.health_check()
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

    async fn send_request(
        &self,
        request: &JsonRpcRequest,
        lifecycle: &McpRequestLifecycle,
    ) -> Result<crate::mcp_protocol::JsonRpcResponse> {
        loop {
            // Fail closed / wait for recovery before touching the write path. A
            // request whose outcome became unknown publishes the recovery-needed
            // state synchronously, so any writer that reaches here after that point
            // must not POST/write on the ambiguous session until reset +
            // re-handshake succeed.
            self.wait_recovery_ready().await?;
            let serial_guard = match &self.serial_gate {
                Some(gate) => Some(gate.lock().await),
                None => None,
            };
            // Re-check under the serial gate. A concurrent HTTP/SSE writer may have
            // been queued on this gate when the outcome-unknown state was armed;
            // acquiring the gate serializes us behind it, so this second check
            // guarantees we never write on a session that still needs recovery.
            if self.recovery.is_poisoned() || self.recovery.recovery_pending() {
                drop(serial_guard);
                continue;
            }
            // Arm the recovery barrier synchronously the instant this write's
            // outcome becomes unknown — *before* the serial gate is released.
            // Declared after `serial_guard` so it drops first: the next queued
            // writer therefore observes the armed barrier under the gate and waits,
            // instead of racing onto the ambiguous session.
            let barrier_arm = WriteBarrierArm {
                recovery: &self.recovery,
                lifecycle,
            };
            let result = self.transport.send_and_recv(request, lifecycle).await;
            drop(barrier_arm);
            drop(serial_guard);

            if matches!(
                result
                    .as_ref()
                    .err()
                    .and_then(|error| error.downcast_ref::<McpTransportError>()),
                Some(McpTransportError::RecoveryPending)
            ) {
                continue;
            }
            return result;
        }
    }

    /// Block until no recovery is pending, or fail closed if recovery has
    /// permanently failed. Returns immediately when the connection is healthy.
    async fn wait_recovery_ready(&self) -> Result<()> {
        loop {
            if self.recovery.is_poisoned() {
                let server_name = self.inner.lock().await.config.name.clone();
                bail!(
                    "MCP server `{server_name}` is unavailable: a prior request's outcome became \
                     unknown and recovery failed; not writing on an unrecovered session"
                );
            }
            if !self.recovery.recovery_pending() {
                return Ok(());
            }
            // Register for a wakeup *before* re-checking so we cannot miss a
            // concurrent `finish`/`poison` pulse.
            let notified = self.recovery.notify.notified();
            if self.recovery.is_poisoned() {
                continue;
            }
            if !self.recovery.recovery_pending() {
                return Ok(());
            }
            notified.await;
        }
    }

    fn start_recovery(
        &self,
        observed_epoch: u64,
        operation: String,
    ) -> tokio::task::JoinHandle<Result<()>> {
        let server = self.clone();
        zeroclaw_spawn::spawn!(async move {
            let result = server.reestablish(observed_epoch).await;
            if let Err(error) = &result {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reconnect)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "operation": operation,
                            "error": format!("{error:#}"),
                        })),
                    "mcp_client: asynchronous recovery failed after outcome-unknown request"
                );
            }
            result
        })
    }

    fn spawn_recovery(&self, observed_epoch: u64, operation: String) {
        // Publish the recovery-needed state synchronously so any writer that
        // queues after this point waits for reset + re-handshake instead of
        // racing onto the ambiguous session. This runs before the detached
        // recovery task is scheduled and before the failing request releases
        // its locks.
        self.recovery.arm(observed_epoch);
        // Dropping a Tokio JoinHandle detaches the task. Recovery therefore
        // continues even if the request future that initiated it is cancelled.
        drop(self.start_recovery(observed_epoch, operation));
    }

    async fn reestablish(&self, observed_epoch: u64) -> Result<()> {
        // Keep HTTP/SSE reset ordering consistent with ordinary calls:
        // serial gate first, then the epoch write gate.
        let serial_guard = match &self.serial_gate {
            Some(gate) => Some(gate.lock().await),
            None => None,
        };
        let mut epoch = self.epoch_gate.write().await;
        if *epoch != observed_epoch {
            // Another recovery already advanced past this epoch; the connection
            // is live again, so release any writers still waiting on the
            // barrier for this (or an older) epoch.
            self.recovery.finish(observed_epoch);
            return Ok(());
        }
        let server_name = self.inner.lock().await.config.name.clone();

        if let Err(reset_error) = self.transport.reset().await {
            // A failed reset leaves the session unrecoverable: fail closed so
            // later writes do not proceed without a successful handshake.
            self.recovery.poison();
            let close_result = self.transport.close().await;
            return match close_result {
                Ok(()) => Err(reset_error).with_context(|| {
                    format!("MCP server `{server_name}` failed to reset transport during recovery")
                }),
                Err(close_error) => Err(anyhow::Error::msg(format!(
                    "MCP server `{server_name}` failed to reset transport during recovery: \
                     {reset_error:#}; cleanup also failed: {close_error:#}"
                ))),
            };
        }

        let refreshed = match handshake(self.transport.as_ref(), &server_name, *epoch).await {
            Ok(capabilities) => capabilities,
            Err(handshake_error) => {
                // A failed re-handshake leaves the connection without a live
                // MCP session; poison the barrier so later tool calls fail
                // closed instead of writing on an unhandshaken transport.
                self.recovery.poison();
                let close_result = self.transport.close().await;
                return match close_result {
                    Ok(()) => Err(handshake_error).with_context(|| {
                        format!("MCP server `{server_name}` failed to re-handshake during recovery")
                    }),
                    Err(close_error) => Err(anyhow::Error::msg(format!(
                        "MCP server `{server_name}` failed to re-handshake during recovery: \
                         {handshake_error:#}; cleanup also failed: {close_error:#}"
                    ))),
                };
            }
        };

        self.inner.lock().await.capabilities = refreshed;
        *epoch = epoch.wrapping_add(1);
        // Reset + re-handshake succeeded: clear the recovery-needed state and
        // release any writers waiting on the barrier.
        self.recovery.finish(observed_epoch);
        drop(serial_guard);
        Ok(())
    }

    async fn dispatch_rpc(
        &self,
        rpc_method: &str,
        params: serde_json::Value,
        timeout_secs: u64,
        operation: &str,
    ) -> Result<crate::mcp_protocol::JsonRpcResponse> {
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        let mut pre_write_retries = 0;

        loop {
            let (id, server_name) = {
                let inner = self.inner.lock().await;
                (
                    inner.next_id.fetch_add(1, Ordering::Relaxed),
                    inner.config.name.clone(),
                )
            };
            let request = JsonRpcRequest::new(id, rpc_method, params.clone());
            let recovery_gate: Arc<dyn McpRecoveryGate> = self.recovery.clone();
            let lifecycle = Arc::new(McpRequestLifecycle::coordinated(
                Arc::clone(&self.epoch_gate),
                Some(recovery_gate),
            ));
            let mut cancellation_guard = OutcomeUnknownGuard::new(
                self.clone(),
                Arc::clone(&lifecycle),
                operation.to_string(),
            );

            let send_result = timeout_at(deadline, self.send_request(&request, &lifecycle)).await;
            match send_result {
                Err(_) => {
                    let unknown_epoch = lifecycle.outcome_unknown_epoch();
                    cancellation_guard.disarm();
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Timeout)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "mcp_server": &server_name,
                                "rpc_method": rpc_method,
                                "timeout_secs": timeout_secs,
                                "outcome_unknown": unknown_epoch.is_some(),
                            })),
                        "mcp_client: MCP request timed out"
                    );
                    if let Some(epoch) = unknown_epoch {
                        self.spawn_recovery(epoch, operation.to_string());
                        bail!(
                            "MCP server `{server_name}` timed out after {timeout_secs}s during \
                             {operation}; outcome unknown and request was not replayed"
                        );
                    }
                    bail!(
                        "MCP server `{server_name}` timed out after {timeout_secs}s before writing \
                         {operation}"
                    );
                }
                Ok(Ok(response)) => {
                    cancellation_guard.disarm();
                    return Ok(response);
                }
                Ok(Err(error)) => {
                    if let Some(epoch) = lifecycle.outcome_unknown_epoch() {
                        cancellation_guard.disarm();
                        self.spawn_recovery(epoch, operation.to_string());
                        return Err(error).with_context(|| {
                            format!(
                                "MCP server `{server_name}` failed during {operation}; outcome \
                                 unknown and request was not replayed"
                            )
                        });
                    }

                    cancellation_guard.disarm();
                    let recoverable = error.downcast_ref::<McpTransportError>().is_some();
                    if recoverable && pre_write_retries < MAX_RECONNECT_ATTEMPTS {
                        pre_write_retries += 1;
                        let observed_epoch = lifecycle.pre_write_epoch().unwrap_or(0);
                        let recovery = self.start_recovery(observed_epoch, operation.to_string());
                        match timeout_at(deadline, recovery).await {
                            Ok(Ok(result)) => result?,
                            Ok(Err(join_error)) => {
                                return Err(anyhow::Error::new(join_error)).with_context(|| {
                                    format!(
                                        "MCP server `{server_name}` recovery task failed before \
                                         writing {operation}"
                                    )
                                });
                            }
                            Err(_) => {
                                bail!(
                                    "MCP server `{server_name}` exhausted the {timeout_secs}s \
                                     budget recovering before writing {operation}"
                                );
                            }
                        }
                        continue;
                    }
                    return Err(error).with_context(|| {
                        format!("MCP server `{server_name}` error during {operation}")
                    });
                }
            }
        }
    }

    /// Call a tool on this server. Returns the raw JSON result.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let tool_timeout = {
            let inner = self.inner.lock().await;
            inner
                .config
                .tool_timeout_secs
                .unwrap_or(DEFAULT_TOOL_TIMEOUT_SECS)
                .min(MAX_TOOL_TIMEOUT_SECS)
        };
        let operation = format!("tool call `{tool_name}`");
        let resp = self
            .dispatch_rpc(
                "tools/call",
                json!({ "name": tool_name, "arguments": arguments }),
                tool_timeout,
                &operation,
            )
            .await?;

        if let Some(err) = resp.error {
            bail!("MCP tool `{tool_name}` error {}: {}", err.code, err.message);
        }

        let result = resp.result.unwrap_or(serde_json::Value::Null);

        // MCP servers signal *tool-execution* failures (as opposed to JSON-RPC
        // protocol errors) with HTTP 200 + `result.isError: true` and the detail
        // in `result.content[].text`, per the MCP spec. Surface it (scrubbed and
        // length-bounded) so the failure is visible to the model and the log.
        let server_name = self.inner.lock().await.config.name.clone();
        check_result_is_error(&result, tool_name, &server_name)?;

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
        let tool_timeout = {
            let inner = self.inner.lock().await;
            inner
                .config
                .tool_timeout_secs
                .unwrap_or(DEFAULT_TOOL_TIMEOUT_SECS)
                .min(MAX_TOOL_TIMEOUT_SECS)
        };
        let operation = format!("`{rpc_method}`");
        let resp = self
            .dispatch_rpc(rpc_method, params, tool_timeout, &operation)
            .await?;

        if let Some(err) = resp.error {
            bail!("MCP `{rpc_method}` error {}: {}", err.code, err.message);
        }
        let result = resp.result.unwrap_or(serde_json::Value::Null);
        let server_name = self.inner.lock().await.config.name.clone();
        check_result_is_error(&result, rpc_method, &server_name)?;
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
        impl SharedMcpTransportConn for NoopTransport {
            async fn send_and_recv(
                &self,
                _request: &JsonRpcRequest,
                _lifecycle: &McpRequestLifecycle,
            ) -> Result<JsonRpcResponse> {
                Err(anyhow::Error::msg(
                    "for_test_with_server_count transport does not support requests",
                ))
            }

            async fn close(&self) -> Result<()> {
                Ok(())
            }
        }

        fn stub_server(name: &str) -> McpServer {
            let transport: Arc<dyn SharedMcpTransportConn> = Arc::new(NoopTransport);
            let inner = McpServerInner {
                config: McpServerConfig {
                    name: name.to_string(),
                    ..McpServerConfig::default()
                },
                #[cfg(target_has_atomic = "64")]
                next_id: AtomicU64::new(0),
                #[cfg(not(target_has_atomic = "64"))]
                next_id: AtomicU32::new(0),
                tools: Vec::new(),
                capabilities: McpServerCapabilities::default(),
            };
            McpServer {
                inner: Arc::new(Mutex::new(inner)),
                transport,
                epoch_gate: Arc::new(RwLock::new(0)),
                serial_gate: None,
                recovery: Arc::new(RecoveryBarrier::new()),
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
        impl SharedMcpTransportConn for NoopTransport {
            async fn send_and_recv(
                &self,
                _request: &JsonRpcRequest,
                _lifecycle: &McpRequestLifecycle,
            ) -> Result<JsonRpcResponse> {
                Err(anyhow::Error::msg(
                    "for_test_make_stub_server transport does not support requests",
                ))
            }

            async fn close(&self) -> Result<()> {
                Ok(())
            }
        }

        let transport: Arc<dyn SharedMcpTransportConn> = Arc::new(NoopTransport);
        let inner = McpServerInner {
            config: McpServerConfig {
                name: name.to_string(),
                ..McpServerConfig::default()
            },
            #[cfg(target_has_atomic = "64")]
            next_id: AtomicU64::new(0),
            #[cfg(not(target_has_atomic = "64"))]
            next_id: AtomicU32::new(0),
            tools: Vec::new(),
            capabilities: McpServerCapabilities::default(),
        };
        McpServer {
            inner: std::sync::Arc::new(Mutex::new(inner)),
            transport,
            epoch_gate: Arc::new(RwLock::new(0)),
            serial_gate: None,
            recovery: Arc::new(RecoveryBarrier::new()),
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
    use crate::mcp_transport::create_transport;
    #[cfg(unix)]
    use crate::mcp_transport::{StdioTransport, StdioWriteTestHook};
    use zeroclaw_config::schema::McpTransport;

    #[cfg(unix)]
    fn write_executable_script(path: &std::path::Path, body: &[u8]) {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let mut script = std::fs::File::create(path).expect("create script");
        script.write_all(body).expect("write script");
        drop(script);
        let mut permissions = std::fs::metadata(path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("chmod script");
    }

    #[cfg(unix)]
    fn make_fifo(path: &std::path::Path) {
        let status = std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "mkfifo failed for {}", path.display());
    }

    #[cfg(unix)]
    async fn read_fifo(path: &std::path::Path) -> String {
        tokio::time::timeout(Duration::from_secs(5), tokio::fs::read_to_string(path))
            .await
            .expect("fifo writer timed out")
            .expect("read fifo")
    }

    #[cfg(unix)]
    fn process_is_alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[cfg(unix)]
    fn stdio_test_config(
        name: &str,
        script: &std::path::Path,
        args: Vec<String>,
        timeout_secs: u64,
    ) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            command: script.display().to_string(),
            args,
            tool_timeout_secs: Some(timeout_secs),
            transport: McpTransport::Stdio,
            ..Default::default()
        }
    }

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
            .respond_with(|request: &wiremock::Request| {
                let id = serde_json::from_slice::<serde_json::Value>(&request.body)
                    .expect("JSON-RPC request")
                    .get("id")
                    .cloned()
                    .expect("request id");
                ResponseTemplate::new(200).set_body_json(json!({
                    "jsonrpc":"2.0","id":id,
                    "result":{"prompts":[{"name":"summarize"}],"nextCursor":"page2"}
                }))
            })
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
            tls_ca_cert_path: None,
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
            tls_ca_cert_path: None,
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
    impl SharedMcpTransportConn for FakeTransport {
        async fn send_and_recv(
            &self,
            request: &JsonRpcRequest,
            _lifecycle: &McpRequestLifecycle,
        ) -> Result<crate::mcp_protocol::JsonRpcResponse> {
            Ok(crate::mcp_protocol::JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id.clone(),
                result: Some(self.result.clone()),
                error: None,
            })
        }

        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    fn server_with_transport(
        name: &str,
        transport: Arc<dyn SharedMcpTransportConn>,
        timeout_secs: u64,
    ) -> McpServer {
        let inner = McpServerInner {
            config: McpServerConfig {
                name: name.into(),
                tool_timeout_secs: Some(timeout_secs),
                ..Default::default()
            },
            #[cfg(target_has_atomic = "64")]
            next_id: AtomicU64::new(3),
            #[cfg(not(target_has_atomic = "64"))]
            next_id: AtomicU32::new(3),
            tools: vec![],
            capabilities: McpServerCapabilities::default(),
        };
        McpServer {
            inner: Arc::new(Mutex::new(inner)),
            transport,
            epoch_gate: Arc::new(RwLock::new(0)),
            serial_gate: None,
            recovery: Arc::new(RecoveryBarrier::new()),
        }
    }

    struct PreWriteBlockingTransport {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        resets: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl SharedMcpTransportConn for PreWriteBlockingTransport {
        async fn send_and_recv(
            &self,
            _request: &JsonRpcRequest,
            _lifecycle: &McpRequestLifecycle,
        ) -> Result<crate::mcp_protocol::JsonRpcResponse> {
            self.entered.notify_one();
            self.release.notified().await;
            Err(McpTransportError::TransportClosed.into())
        }

        async fn reset(&self) -> Result<()> {
            self.resets.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn cancellation_before_write_does_not_reset_or_replay() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let resets = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let transport: Arc<dyn SharedMcpTransportConn> = Arc::new(PreWriteBlockingTransport {
            entered: Arc::clone(&entered),
            release,
            resets: Arc::clone(&resets),
        });
        let server = server_with_transport("pre-write", transport, 5);
        let call_server = server.clone();
        let call =
            zeroclaw_spawn::spawn!(async move { call_server.call_tool("test", json!({})).await });
        entered.notified().await;
        call.abort();
        assert!(
            call.await
                .expect_err("call must be cancelled")
                .is_cancelled()
        );
        tokio::task::yield_now().await;
        assert_eq!(resets.load(Ordering::SeqCst), 0);
    }

    struct CancellationSafeRecoveryTransport {
        tool_calls: Arc<std::sync::atomic::AtomicUsize>,
        resets: Arc<std::sync::atomic::AtomicUsize>,
        handshake_entered: Arc<tokio::sync::Notify>,
        release_handshake: Arc<tokio::sync::Notify>,
        handshake_completed: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl SharedMcpTransportConn for CancellationSafeRecoveryTransport {
        async fn send_and_recv(
            &self,
            request: &JsonRpcRequest,
            _lifecycle: &McpRequestLifecycle,
        ) -> Result<crate::mcp_protocol::JsonRpcResponse> {
            match request.method.as_str() {
                "tools/call" if self.tool_calls.fetch_add(1, Ordering::SeqCst) == 0 => {
                    Err(McpTransportError::TransportClosed.into())
                }
                "initialize" => {
                    self.handshake_entered.notify_one();
                    self.release_handshake.notified().await;
                    Ok(crate::mcp_protocol::JsonRpcResponse {
                        jsonrpc: "2.0".into(),
                        id: request.id.clone(),
                        result: Some(json!({
                            "protocolVersion": MCP_PROTOCOL_VERSION,
                            "capabilities": {"tools": {}},
                            "serverInfo": {"name": "recovery", "version": "1"}
                        })),
                        error: None,
                    })
                }
                "notifications/initialized" => {
                    self.handshake_completed.notify_one();
                    Ok(crate::mcp_protocol::JsonRpcResponse {
                        jsonrpc: "2.0".into(),
                        id: None,
                        result: None,
                        error: None,
                    })
                }
                "tools/call" => Ok(crate::mcp_protocol::JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: request.id.clone(),
                    result: Some(json!({"ok": true})),
                    error: None,
                }),
                other => panic!("unexpected method {other}"),
            }
        }

        async fn reset(&self) -> Result<()> {
            self.resets.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn cancellation_during_recovery_does_not_abandon_rehandshake() {
        let tool_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let resets = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handshake_entered = Arc::new(tokio::sync::Notify::new());
        let release_handshake = Arc::new(tokio::sync::Notify::new());
        let handshake_completed = Arc::new(tokio::sync::Notify::new());
        let transport: Arc<dyn SharedMcpTransportConn> =
            Arc::new(CancellationSafeRecoveryTransport {
                tool_calls: Arc::clone(&tool_calls),
                resets: Arc::clone(&resets),
                handshake_entered: Arc::clone(&handshake_entered),
                release_handshake: Arc::clone(&release_handshake),
                handshake_completed: Arc::clone(&handshake_completed),
            });
        let server = server_with_transport("cancel-recovery", transport, 5);

        let call_server = server.clone();
        let call =
            zeroclaw_spawn::spawn!(
                async move { call_server.call_tool("side_effect", json!({})).await }
            );
        handshake_entered.notified().await;
        call.abort();
        assert!(
            call.await
                .expect_err("call must be cancelled")
                .is_cancelled()
        );

        release_handshake.notify_one();
        timeout(Duration::from_secs(2), handshake_completed.notified())
            .await
            .expect("detached recovery must finish its handshake");

        let result = timeout(Duration::from_secs(2), server.call_tool("probe", json!({})))
            .await
            .expect("next call must not hang behind abandoned recovery")
            .expect("next call must use recovered connection");
        assert_eq!(result, json!({"ok": true}));
        assert_eq!(resets.load(Ordering::SeqCst), 1);
        assert_eq!(tool_calls.load(Ordering::SeqCst), 2);
    }

    struct FailedResetTransport;

    #[async_trait::async_trait]
    impl SharedMcpTransportConn for FailedResetTransport {
        async fn send_and_recv(
            &self,
            _request: &JsonRpcRequest,
            _lifecycle: &McpRequestLifecycle,
        ) -> Result<crate::mcp_protocol::JsonRpcResponse> {
            unreachable!("failed-reset test never sends a request")
        }

        async fn reset(&self) -> Result<()> {
            bail!("reset/reap failed")
        }

        async fn close(&self) -> Result<()> {
            bail!("cleanup failed")
        }
    }

    #[tokio::test]
    async fn recovery_surfaces_reset_and_cleanup_failures() {
        let transport: Arc<dyn SharedMcpTransportConn> = Arc::new(FailedResetTransport);
        let server = server_with_transport("broken", transport, 5);
        let error = server
            .reestablish(0)
            .await
            .expect_err("failed reset and cleanup must surface");
        let detail = format!("{error:#}");
        assert!(detail.contains("reset/reap failed"), "got: {detail}");
        assert!(detail.contains("cleanup failed"), "got: {detail}");
    }

    struct TimeoutThenRecoverTransport {
        tool_calls: Arc<std::sync::atomic::AtomicUsize>,
        recovered: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl SharedMcpTransportConn for TimeoutThenRecoverTransport {
        async fn send_and_recv(
            &self,
            request: &JsonRpcRequest,
            lifecycle: &McpRequestLifecycle,
        ) -> Result<crate::mcp_protocol::JsonRpcResponse> {
            if request.method == "tools/call" {
                self.tool_calls.fetch_add(1, Ordering::SeqCst);
                lifecycle.mark_outcome_unknown(0);
                std::future::pending().await
            }
            Ok(crate::mcp_protocol::JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id.clone(),
                result: Some(json!({})),
                error: None,
            })
        }

        async fn reset(&self) -> Result<()> {
            self.recovered.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn configured_timeout_recovers_without_replaying_outcome_unknown_tool() {
        let tool_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recovered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let transport: Arc<dyn SharedMcpTransportConn> = Arc::new(TimeoutThenRecoverTransport {
            tool_calls: Arc::clone(&tool_calls),
            recovered: Arc::clone(&recovered),
        });
        let server = server_with_transport("timeout", transport, 1);

        let error = timeout(
            Duration::from_secs(2),
            server.call_tool("side_effect", json!({})),
        )
        .await
        .expect("configured timeout must return promptly")
        .expect_err("tool must time out");
        assert!(
            error.to_string().contains("outcome unknown")
                && error.to_string().contains("not replayed"),
            "got: {error:#}"
        );
        timeout(Duration::from_secs(2), async {
            while !recovered.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("recovery did not run");
        assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
    }

    /// Like `server_with_transport` but with an HTTP/SSE-style serial gate so a
    /// second concurrent write queues behind the first.
    fn server_with_serialized_transport(
        name: &str,
        transport: Arc<dyn SharedMcpTransportConn>,
        timeout_secs: u64,
    ) -> McpServer {
        let mut server = server_with_transport(name, transport, timeout_secs);
        server.serial_gate = Some(Arc::new(Mutex::new(())));
        server
    }

    /// Transport whose first `tools/call` marks the outcome unknown after
    /// "writing" and then hangs (the caller cancels it). A later `reset` +
    /// re-handshake succeeds. Records the exact ordering of writes vs. reset so
    /// tests can prove a queued second call never writes on the ambiguous
    /// session before recovery completes.
    struct QueuedAfterUnknownTransport {
        tool_writes: Arc<std::sync::atomic::AtomicUsize>,
        reset_done: Arc<std::sync::atomic::AtomicBool>,
        wrote_before_reset: Arc<std::sync::atomic::AtomicBool>,
        first_entered: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl SharedMcpTransportConn for QueuedAfterUnknownTransport {
        async fn send_and_recv(
            &self,
            request: &JsonRpcRequest,
            lifecycle: &McpRequestLifecycle,
        ) -> Result<crate::mcp_protocol::JsonRpcResponse> {
            match request.method.as_str() {
                "tools/call" => {
                    let n = self.tool_writes.fetch_add(1, Ordering::SeqCst);
                    if !self.reset_done.load(Ordering::SeqCst) && n > 0 {
                        // A write reached the transport while recovery had not
                        // yet reset the session — the exact bug we guard.
                        self.wrote_before_reset.store(true, Ordering::SeqCst);
                    }
                    if n == 0 {
                        // First call: outcome becomes unknown after the write,
                        // then the future hangs until the caller cancels it.
                        lifecycle.mark_outcome_unknown(0);
                        self.first_entered.notify_one();
                        std::future::pending::<()>().await;
                        unreachable!("cancelled before resuming");
                    }
                    Ok(crate::mcp_protocol::JsonRpcResponse {
                        jsonrpc: "2.0".into(),
                        id: request.id.clone(),
                        result: Some(json!({"ok": true})),
                        error: None,
                    })
                }
                "initialize" => Ok(crate::mcp_protocol::JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: request.id.clone(),
                    result: Some(json!({
                        "protocolVersion": MCP_PROTOCOL_VERSION,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "queued", "version": "1"}
                    })),
                    error: None,
                }),
                "notifications/initialized" => Ok(crate::mcp_protocol::JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: None,
                    result: None,
                    error: None,
                }),
                other => panic!("unexpected method {other}"),
            }
        }

        async fn reset(&self) -> Result<()> {
            self.reset_done.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    /// A second serialized call queued while the first call's outcome is
    /// unknown must wait for reset + re-handshake and must never write on the
    /// ambiguous session before recovery completes.
    #[tokio::test]
    async fn queued_write_waits_for_recovery_after_outcome_unknown() {
        let tool_writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reset_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wrote_before_reset = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let first_entered = Arc::new(tokio::sync::Notify::new());
        let transport: Arc<dyn SharedMcpTransportConn> = Arc::new(QueuedAfterUnknownTransport {
            tool_writes: Arc::clone(&tool_writes),
            reset_done: Arc::clone(&reset_done),
            wrote_before_reset: Arc::clone(&wrote_before_reset),
            first_entered: Arc::clone(&first_entered),
        });
        let server = server_with_serialized_transport("queued", transport, 5);

        // First call writes, marks outcome-unknown, then hangs; cancel it.
        let first_server = server.clone();
        let first = zeroclaw_spawn::spawn!(async move {
            first_server.call_tool("side_effect", json!({})).await
        });
        first_entered.notified().await;
        first.abort();
        let _ = first.await;

        // The queued second call must not resolve until recovery reset the
        // session, and must never write before that reset.
        let second = timeout(Duration::from_secs(3), server.call_tool("probe", json!({})))
            .await
            .expect("second call must not hang behind recovery")
            .expect("second call must succeed on the recovered session");
        assert_eq!(second, json!({"ok": true}));
        assert!(
            !wrote_before_reset.load(Ordering::SeqCst),
            "second call wrote on the ambiguous session before reset/re-handshake"
        );
        assert!(
            reset_done.load(Ordering::SeqCst),
            "recovery must have reset the session before the second write"
        );
        // Two tool writes total: the cancelled first and the recovered second.
        assert_eq!(tool_writes.load(Ordering::SeqCst), 2);
    }

    /// Transport whose first `tools/call` becomes outcome-unknown after writing,
    /// but whose recovery re-handshake permanently fails.
    struct FailedRehandshakeTransport {
        tool_writes: Arc<std::sync::atomic::AtomicUsize>,
        first_entered: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl SharedMcpTransportConn for FailedRehandshakeTransport {
        async fn send_and_recv(
            &self,
            request: &JsonRpcRequest,
            lifecycle: &McpRequestLifecycle,
        ) -> Result<crate::mcp_protocol::JsonRpcResponse> {
            match request.method.as_str() {
                "tools/call" => {
                    self.tool_writes.fetch_add(1, Ordering::SeqCst);
                    lifecycle.mark_outcome_unknown(0);
                    self.first_entered.notify_one();
                    std::future::pending::<()>().await;
                    unreachable!("cancelled before resuming");
                }
                // Re-handshake fails: `initialize` errors during recovery.
                "initialize" => bail!("re-handshake refused"),
                other => panic!("unexpected method {other}"),
            }
        }

        async fn reset(&self) -> Result<()> {
            Ok(())
        }

        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    /// After a post-write outcome-unknown request whose recovery re-handshake
    /// fails, later calls must fail closed instead of writing on an
    /// unrecovered/unhandshaken session.
    #[tokio::test]
    async fn failed_rehandshake_fails_closed_for_later_calls() {
        let tool_writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let first_entered = Arc::new(tokio::sync::Notify::new());
        let transport: Arc<dyn SharedMcpTransportConn> = Arc::new(FailedRehandshakeTransport {
            tool_writes: Arc::clone(&tool_writes),
            first_entered: Arc::clone(&first_entered),
        });
        let server = server_with_serialized_transport("failing", transport, 5);

        let first_server = server.clone();
        let first = zeroclaw_spawn::spawn!(async move {
            first_server.call_tool("side_effect", json!({})).await
        });
        first_entered.notified().await;
        first.abort();
        let _ = first.await;

        // Recovery (detached) must poison the barrier once its re-handshake
        // fails; the next call then fails closed without writing.
        let error = timeout(Duration::from_secs(3), server.call_tool("probe", json!({})))
            .await
            .expect("later call must not hang behind a failed recovery")
            .expect_err("later call must fail closed after failed re-handshake");
        let detail = format!("{error:#}");
        assert!(
            detail.contains("unavailable") || detail.contains("recovery failed"),
            "expected fail-closed error, got: {detail}"
        );
        // Only the first (cancelled) call ever wrote a tool request.
        assert_eq!(tool_writes.load(Ordering::SeqCst), 1);
    }

    /// Build an `McpServer` whose transport yields `result` on every call.
    fn server_returning(result: serde_json::Value) -> McpServer {
        let transport: Arc<dyn SharedMcpTransportConn> = Arc::new(FakeTransport { result });
        let inner = McpServerInner {
            config: McpServerConfig {
                name: "fake".into(),
                ..Default::default()
            },
            #[cfg(target_has_atomic = "64")]
            next_id: AtomicU64::new(3),
            #[cfg(not(target_has_atomic = "64"))]
            next_id: AtomicU32::new(3),
            tools: vec![],
            capabilities: McpServerCapabilities::default(),
        };
        McpServer {
            inner: Arc::new(Mutex::new(inner)),
            transport,
            epoch_gate: Arc::new(RwLock::new(0)),
            serial_gate: None,
            recovery: Arc::new(RecoveryBarrier::new()),
        }
    }

    /// Like `server_returning`, but with explicit advertised capabilities.
    fn server_with_caps_returning(
        capabilities: McpServerCapabilities,
        result: serde_json::Value,
    ) -> McpServer {
        let transport: Arc<dyn SharedMcpTransportConn> = Arc::new(FakeTransport { result });
        let inner = McpServerInner {
            config: McpServerConfig {
                name: "fake".into(),
                ..Default::default()
            },
            #[cfg(target_has_atomic = "64")]
            next_id: AtomicU64::new(3),
            #[cfg(not(target_has_atomic = "64"))]
            next_id: AtomicU32::new(3),
            tools: vec![],
            capabilities,
        };
        McpServer {
            inner: Arc::new(Mutex::new(inner)),
            transport,
            epoch_gate: Arc::new(RwLock::new(0)),
            serial_gate: None,
            recovery: Arc::new(RecoveryBarrier::new()),
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
            tls_ca_cert_path: None,
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

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_concurrent_calls_route_mismatched_and_out_of_order_replies() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script_path = temp.path().join("multiplex-mcp.sh");
        let first_received = temp.path().join("first-received.fifo");
        make_fifo(&first_received);
        write_executable_script(
            &script_path,
            br#"#!/bin/sh
first_id=
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"multiplex\",\"version\":\"1\"}}}"
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"A\",\"inputSchema\":{\"type\":\"object\"}},{\"name\":\"B\",\"inputSchema\":{\"type\":\"object\"}}]}}"
      ;;
    *'"method":"tools/call"'*'"name":"A"'*)
      first_id=$id
      printf '%s\n' ready > "$1"
      ;;
    *'"method":"tools/call"'*'"name":"B"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":"3","result":{"which":"wrong-shape"}}'
      printf '%s\n' '{"jsonrpc":"2.0","id":999999,"result":{"which":"wrong-id"}}'
      printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"which\":\"B\"}}"
      printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":$first_id,\"result\":{\"which\":\"A\"}}"
      ;;
  esac
done
"#,
        );

        let server = McpServer::connect(stdio_test_config(
            "multiplex",
            &script_path,
            vec![first_received.display().to_string()],
            5,
        ))
        .await
        .expect("connect");
        let first_server = server.clone();
        let first =
            zeroclaw_spawn::spawn!(async move { first_server.call_tool("A", json!({})).await });
        assert_eq!(read_fifo(&first_received).await.trim(), "ready");
        let second_server = server.clone();
        let second =
            zeroclaw_spawn::spawn!(async move { second_server.call_tool("B", json!({})).await });

        let (first_result, second_result) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(first, second)
        })
        .await
        .expect("multiplexed calls timed out");
        assert_eq!(
            first_result.expect("first task").expect("first response"),
            json!({"which":"A"})
        );
        assert_eq!(
            second_result
                .expect("second task")
                .expect("second response"),
            json!({"which":"B"})
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_post_write_cancellation_reaps_rehandshakes_and_never_replays() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script_path = temp.path().join("cancel-mcp.sh");
        let effect_ready = temp.path().join("effect-ready.fifo");
        let recovered = temp.path().join("recovered.fifo");
        let generations = temp.path().join("generations.log");
        let effects = temp.path().join("effects.log");
        make_fifo(&effect_ready);
        make_fifo(&recovered);
        write_executable_script(
            &script_path,
            br#"#!/bin/sh
printf '%s\n' "$$" >> "$3"
generation=$(wc -l < "$3" | tr -d ' ')
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"cancel\",\"version\":\"1\"}}}"
      ;;
    *'"method":"notifications/initialized"'*)
      if [ "$generation" -gt 1 ]; then printf '%s\n' recovered > "$2"; fi
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"side_effect\",\"inputSchema\":{\"type\":\"object\"}},{\"name\":\"probe\",\"inputSchema\":{\"type\":\"object\"}}]}}"
      ;;
    *'"method":"tools/call"'*'"name":"side_effect"'*)
      printf '%s\n' effect >> "$4"
      if [ "$generation" -eq 1 ]; then
        printf '%s\n' ready > "$1"
        exec tail -f /dev/null
      fi
      printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"replayed\":true}}"
      ;;
    *'"method":"tools/call"'*'"name":"probe"'*)
      printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"ok\":true}}"
      ;;
  esac
done
"#,
        );

        let server = McpServer::connect(stdio_test_config(
            "cancel",
            &script_path,
            vec![
                effect_ready.display().to_string(),
                recovered.display().to_string(),
                generations.display().to_string(),
                effects.display().to_string(),
            ],
            5,
        ))
        .await
        .expect("connect");
        let call_server = server.clone();
        let call =
            zeroclaw_spawn::spawn!(
                async move { call_server.call_tool("side_effect", json!({})).await }
            );
        assert_eq!(read_fifo(&effect_ready).await.trim(), "ready");
        call.abort();
        assert!(
            call.await
                .expect_err("call must be cancelled")
                .is_cancelled()
        );
        assert_eq!(read_fifo(&recovered).await.trim(), "recovered");

        let effects_text = tokio::fs::read_to_string(&effects)
            .await
            .expect("read effects");
        assert_eq!(effects_text.lines().count(), 1, "tool call was replayed");
        let pids = tokio::fs::read_to_string(&generations)
            .await
            .expect("read generation pids");
        assert_eq!(pids.lines().count(), 2, "expected exactly one respawn");
        let first_pid = pids
            .lines()
            .next()
            .expect("first generation pid")
            .parse::<u32>()
            .expect("numeric first generation pid");
        assert!(
            !process_is_alive(first_pid),
            "old child must be reaped before recovery completes"
        );
        let result = server
            .call_tool("probe", json!({}))
            .await
            .expect("fresh child should accept subsequent call");
        assert_eq!(result, json!({"ok":true}));
    }

    /// A stdio writer already queued on the transport state must re-check a
    /// recovery published by the cancelled writer before emitting any bytes.
    /// This exercises the real stdio boundary without an HTTP/SSE serial gate.
    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_queued_writer_waits_for_recovery_at_writer_boundary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script_path = temp.path().join("queued-writer-mcp.sh");
        let requests = temp.path().join("requests.log");
        let generations = temp.path().join("generations.log");
        write_executable_script(
            &script_path,
            br#"#!/bin/sh
printf '%s\n' "$$" >> "$2"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$1"
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"queued-writer\",\"version\":\"1\"}}}"
      ;;
    *'"method":"tools/call"'*)
      printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"ok\":true}}"
      ;;
  esac
done
"#,
        );

        let config = stdio_test_config(
            "queued-writer",
            &script_path,
            vec![
                requests.display().to_string(),
                generations.display().to_string(),
            ],
            5,
        );
        let transport = Arc::new(StdioTransport::new(&config).expect("build transport"));
        let hook = Arc::new(StdioWriteTestHook::new());
        hook.pause_next_payload();
        transport.set_write_test_hook(Arc::clone(&hook));
        let shared_transport: Arc<dyn SharedMcpTransportConn> = transport.clone();
        let server = server_with_transport("queued-writer", shared_transport, 5);
        timeout(Duration::from_secs(3), async {
            loop {
                if tokio::fs::read_to_string(&generations)
                    .await
                    .is_ok_and(|log| log.lines().count() == 1)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("initial stdio child did not start");

        // Pause the first request after its JSON payload has crossed the OS
        // stdin boundary but before newline/flush completes. `state` remains
        // locked and the request outcome is unknown.
        let first_server = server.clone();
        let first = zeroclaw_spawn::spawn!(async move {
            first_server.call_tool("side_effect", json!({})).await
        });
        timeout(Duration::from_secs(3), hook.wait_for_payload_pause())
            .await
            .expect("first stdio write did not reach the post-payload pause");

        // Enter a second call before cancellation and prove it reached the
        // transport, where it is queued on the same stdio state lock.
        let second_server = server.clone();
        let second =
            zeroclaw_spawn::spawn!(
                async move { second_server.call_tool("probe", json!({})).await }
            );
        timeout(Duration::from_secs(3), hook.wait_for_attempts(2))
            .await
            .expect("second stdio writer did not queue on transport state");

        first.abort();
        assert!(
            first
                .await
                .expect_err("first call must be cancelled")
                .is_cancelled()
        );

        let second_result = timeout(Duration::from_secs(5), second)
            .await
            .expect("second call hung behind recovery")
            .expect("second task failed")
            .expect("second call failed after recovery");
        assert_eq!(second_result, json!({"ok": true}));

        let request_log = tokio::fs::read_to_string(&requests)
            .await
            .expect("read request log");
        let tool_writes = request_log
            .lines()
            .filter(|line| line.contains(r#""method":"tools/call""#))
            .count();
        assert_eq!(
            tool_writes, 1,
            "queued writer reached the ambiguous child before recovery"
        );
        let request_lines = request_log.lines().collect::<Vec<_>>();
        let initialized_index = request_lines
            .iter()
            .position(|line| line.contains(r#""method":"notifications/initialized""#))
            .expect("recovery handshake notification missing");
        let tool_index = request_lines
            .iter()
            .position(|line| line.contains(r#""method":"tools/call""#))
            .expect("recovered tool write missing");
        assert!(
            initialized_index < tool_index,
            "queued tool write occurred before recovery handshake completed"
        );

        let generation_log = tokio::fs::read_to_string(&generations)
            .await
            .expect("read generation log");
        assert_eq!(
            generation_log.lines().count(),
            2,
            "expected exactly one recovery respawn"
        );

        drop(server);
        SharedMcpTransportConn::close(transport.as_ref())
            .await
            .expect("close transport");
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
    async fn call_tool_recovers_stale_session_without_replaying_tool() {
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

        // tools/call → 404 (stale session). Even though the response indicates
        // a stale session, the request crossed the write boundary, so the
        // client recovers the connection but does not replay the tool.
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "tools/call"})))
            .respond_with(ResponseTemplate::new(404))
            .up_to_n_times(1)
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;

        let srv = McpServer::connect(http_server_config(server.uri()))
            .await
            .expect("connect");
        let error = srv
            .call_tool("echo", json!({}))
            .await
            .expect_err("outcome-unknown tool call must not be replayed");
        assert!(
            error.to_string().contains("request was not replayed"),
            "got: {error:#}"
        );
        timeout(Duration::from_secs(5), async {
            loop {
                if *srv.epoch_gate.read().await == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("stale-session recovery did not complete");
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
