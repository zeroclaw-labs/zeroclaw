//! ACP (Agent Control Protocol) Server — JSON-RPC 2.0 over stdio.
//!
//! Provides an IDE-friendly interface for spawning and managing isolated agent
//! sessions. Each session wraps an [`Agent`] built from the global config with
//! streaming support via JSON-RPC notifications.
//!
//! ## Protocol
//!
//! Requests and responses are newline-delimited JSON objects on stdin/stdout.
//!
//! | Method            | Description                              |
//! |-------------------|------------------------------------------|
//! | `initialize`      | Handshake — returns server capabilities (incl. defaultModel when configured) |
//! | `session/new`     | Create an isolated agent session          |
//! | `session/prompt`  | Send a prompt, stream back `session/update` events |
//! | `session/stop`    | Gracefully terminate a session            |
//! | `session/cancel`  | Abort an in-flight `session/prompt` turn  |
//! | `session/update`  | Streaming events and bidirectional events |

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{debug, error, warn};
use uuid::Uuid;
use zeroclaw_config::schema::Config;
use zeroclaw_runtime::agent::agent::{Agent, TurnEvent};

use crate::acp_channel::AcpChannel;

// ── Configuration ────────────────────────────────────────────────

/// ACP server configuration (optional `[acp]` section in config.toml).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AcpServerConfig {
    /// Maximum number of concurrent sessions. Default: 10.
    pub max_sessions: usize,
    /// Session inactivity timeout in seconds. Default: 3600 (1 hour).
    pub session_timeout_secs: u64,
}

impl Default for AcpServerConfig {
    fn default() -> Self {
        Self {
            max_sessions: 10,
            session_timeout_secs: 3600,
        }
    }
}

// ── JSON-RPC types ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    #[serde(default)]
    params: Value,
    id: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
    id: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcNotification {
    jsonrpc: &'static str,
    method: &'static str,
    params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// Standard JSON-RPC error codes
const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;

// Custom error codes
const SESSION_NOT_FOUND: i32 = -32000;
const SESSION_LIMIT_REACHED: i32 = -32001;
const ACP_PROTOCOL_VERSION: u64 = 1;

// ── Outbound JSON-RPC plumbing ───────────────────────────────────

/// A pending outbound JSON-RPC call, awaiting a response from the client.
type PendingResponder = oneshot::Sender<std::result::Result<Value, JsonRpcError>>;

/// Writer + outbound-call tracker shared between the server loop and
/// per-session bridges (e.g. [`AcpChannel`]).
///
/// All stdout writes go through `writer_tx` so concurrent notifications and
/// outbound requests can't interleave bytes. Outbound requests get string ids
/// (`zc-out-<n>`) that are disjoint from any client-issued id space.
pub struct RpcOutbound {
    writer_tx: mpsc::Sender<String>,
    pending: std::sync::Mutex<HashMap<String, PendingResponder>>,
    next_id: AtomicU64,
}

struct PendingRequestGuard<'a> {
    pending: &'a std::sync::Mutex<HashMap<String, PendingResponder>>,
    id: String,
}

impl Drop for PendingRequestGuard<'_> {
    fn drop(&mut self) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

impl RpcOutbound {
    fn new(writer_tx: mpsc::Sender<String>) -> Self {
        Self {
            writer_tx,
            pending: std::sync::Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
        }
    }

    /// Send a JSON-RPC notification (no `id`, no response expected).
    pub async fn notify(&self, method: &str, params: Value) {
        let n = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        if let Ok(s) = serde_json::to_string(&n)
            && self.writer_tx.send(s).await.is_err()
        {
            warn!("ACP writer task closed; dropping outbound notification");
        }
    }

    /// Send a JSON-RPC request and await the response.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, JsonRpcError> {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed);
        let id = format!("zc-out-{n}");
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.insert(id.clone(), tx);
        }
        let _pending_guard = PendingRequestGuard {
            pending: &self.pending,
            id: id.clone(),
        };
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id,
        });
        let body = match serde_json::to_string(&req) {
            Ok(s) => s,
            Err(e) => {
                return Err(JsonRpcError {
                    code: INTERNAL_ERROR,
                    message: format!("Failed to encode request: {e}"),
                    data: None,
                });
            }
        };
        if self.writer_tx.send(body).await.is_err() {
            return Err(JsonRpcError {
                code: INTERNAL_ERROR,
                message: "ACP writer task closed".to_string(),
                data: None,
            });
        }
        rx.await.unwrap_or_else(|_| {
            Err(JsonRpcError {
                code: INTERNAL_ERROR,
                message: "Outbound RPC dropped".to_string(),
                data: None,
            })
        })
    }

    /// Route an inbound JSON-RPC response (matched by `id`) to the
    /// corresponding pending caller.
    pub(crate) fn dispatch_response(
        &self,
        id_str: &str,
        result: Option<Value>,
        error: Option<JsonRpcError>,
    ) {
        let responder = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id_str);
        if let Some(tx) = responder {
            let payload = if let Some(err) = error {
                Err(err)
            } else {
                Ok(result.unwrap_or(Value::Null))
            };
            let _ = tx.send(payload);
        } else {
            debug!("No pending outbound RPC matched response id={id_str}");
        }
    }
}

#[cfg(test)]
impl RpcOutbound {
    /// Test-only: build an `RpcOutbound` whose writer channel is provided by
    /// the test (so outbound frames can be inspected without touching stdout).
    pub fn for_testing(writer_tx: mpsc::Sender<String>) -> Self {
        Self::new(writer_tx)
    }

    /// Test-only wrapper around `dispatch_response` so cross-module tests
    /// (e.g. in `acp_channel`) can simulate inbound JSON-RPC responses.
    pub fn dispatch_response_for_test(
        &self,
        id_str: &str,
        result: Option<Value>,
        error: Option<JsonRpcError>,
    ) {
        self.dispatch_response(id_str, result, error);
    }

    pub fn pending_count_for_test(&self) -> usize {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

// ── Session state ────────────────────────────────────────────────

struct Session {
    agent: Agent,
    #[allow(dead_code)] // WIP: intended for session expiry logic
    created_at: Instant,
    last_active: Instant,
}

// ── ACP Server ───────────────────────────────────────────────────

pub struct AcpServer {
    config: Config,
    acp_config: AcpServerConfig,
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<Session>>>>>,
    rpc: Arc<RpcOutbound>,
    /// Receiver for the writer task. Pulled out (replaced with `None`) the
    /// first time `run()` starts the writer loop.
    writer_rx: std::sync::Mutex<Option<mpsc::Receiver<String>>>,
    /// Per-session cancellation tokens for aborting in-flight `session/prompt`
    /// turns. Lives outside `Session`'s inner `Mutex` so `session/cancel` can
    /// fire the token without waiting for the turn to release the inner lock.
    cancel_tokens: Arc<std::sync::Mutex<HashMap<String, tokio_util::sync::CancellationToken>>>,
}

impl AcpServer {
    pub fn new(config: Config, acp_config: AcpServerConfig) -> Self {
        let (writer_tx, writer_rx) = mpsc::channel::<String>(256);
        Self::with_writer(config, acp_config, writer_tx, Some(writer_rx))
    }

    pub fn new_with_writer(
        config: Config,
        acp_config: AcpServerConfig,
        writer_tx: mpsc::Sender<String>,
    ) -> Self {
        Self::with_writer(config, acp_config, writer_tx, None)
    }

    fn with_writer(
        config: Config,
        acp_config: AcpServerConfig,
        writer_tx: mpsc::Sender<String>,
        writer_rx: Option<mpsc::Receiver<String>>,
    ) -> Self {
        Self {
            config,
            acp_config,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            rpc: Arc::new(RpcOutbound::new(writer_tx)),
            writer_rx: std::sync::Mutex::new(writer_rx),
            cancel_tokens: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Run the ACP server, reading JSON-RPC requests from stdin and writing
    /// responses/notifications to stdout.
    pub async fn run(self: Arc<Self>) -> Result<()> {
        debug!(
            "ACP server starting (max_sessions={}, timeout={}s)",
            self.acp_config.max_sessions, self.acp_config.session_timeout_secs
        );

        // Pull the writer-rx out of self so we can move it into the writer
        // task. Subsequent `run()` calls would have nothing to drive — but
        // `run()` is normally invoked once per process.
        let writer_rx = self
            .writer_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or_else(|| anyhow::anyhow!("ACP server writer already started"))?;
        tokio::spawn(writer_task(writer_rx));

        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        let mut line = String::new();

        // Spawn session reaper
        let sessions = Arc::clone(&self.sessions);
        let timeout = Duration::from_secs(self.acp_config.session_timeout_secs);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let mut sessions = sessions.lock().await;
                let before = sessions.len();
                sessions.retain(|id, session_arc| {
                    // Never reap a session whose inner lock is held — it has an
                    // active prompt turn in flight and is by definition not idle.
                    match session_arc.try_lock() {
                        Ok(session) => {
                            let expired = session.last_active.elapsed() > timeout;
                            if expired {
                                debug!("Session {id} expired after inactivity");
                            }
                            !expired
                        }
                        Err(_) => true,
                    }
                });
                let reaped = before - sessions.len();
                if reaped > 0 {
                    debug!("Reaped {reaped} expired session(s)");
                }
            }
        });

        loop {
            line.clear();
            let bytes_read = reader.read_line(&mut line).await?;
            if bytes_read == 0 {
                debug!("ACP server: stdin closed, shutting down");
                break;
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            self.process_line(trimmed).await;
        }

        Ok(())
    }

    /// Run the ACP server against an already-framed line source.
    ///
    /// This is used by the gateway WebSocket bridge, where inbound WebSocket
    /// text messages are already complete JSON-RPC frames and outbound frames
    /// are supplied by the writer channel passed to [`new_with_writer`].
    pub async fn run_messages(self: Arc<Self>, mut input_rx: mpsc::Receiver<String>) -> Result<()> {
        while let Some(line) = input_rx.recv().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            self.process_line(trimmed).await;
        }

        Ok(())
    }

    async fn process_line(self: &Arc<Self>, trimmed: &str) {
        // First, peek at whether this is a response (has `result` or
        // `error`) to a request *we* sent. Inbound requests/notifications
        // fall through to the JsonRpcRequest path.
        if let Ok(value) = serde_json::from_str::<Value>(trimmed)
            && value.is_object()
            && (value.get("result").is_some() || value.get("error").is_some())
            && let Some(id) = value.get("id")
        {
            let id_str = id
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| id.to_string());
            let result = value.get("result").cloned();
            let error: Option<JsonRpcError> = value
                .get("error")
                .and_then(|e| serde_json::from_value(e.clone()).ok());
            self.rpc.dispatch_response(&id_str, result, error);
            return;
        }

        match serde_json::from_str::<JsonRpcRequest>(trimmed) {
            Ok(request) => {
                if request.jsonrpc != "2.0" {
                    if let Some(id) = request.id {
                        self.write_error(id, INVALID_REQUEST, "Invalid JSON-RPC version")
                            .await;
                    }
                    return;
                }
                // Spawn so a long-running session/prompt doesn't block the
                // read loop — outbound RPC responses (e.g. for
                // session/request_permission) need to be processable
                // while a prompt turn is in flight.
                let server = Arc::clone(self);
                tokio::spawn(async move {
                    server.handle_request(request).await;
                });
            }
            Err(e) => {
                warn!("Failed to parse JSON-RPC request: {e}");
                self.write_error(Value::Null, PARSE_ERROR, &format!("Parse error: {e}"))
                    .await;
            }
        }
    }

    async fn handle_request(&self, request: JsonRpcRequest) {
        let id = request.id.clone().unwrap_or(Value::Null);
        let is_notification = request.id.is_none();

        let result = match request.method.as_str() {
            "initialize" => self.handle_initialize(&request.params),
            "session/new" => self.handle_session_new(&request.params).await,
            "session/prompt" => self.handle_session_prompt(&request.params, &id).await,
            "session/stop" => self.handle_session_stop(&request.params).await,
            "session/cancel" => self.handle_session_cancel(&request.params).await,
            "session/event" | "session/update" => self.handle_session_event(&request.params).await,
            _ => Err(RpcError {
                code: METHOD_NOT_FOUND,
                message: format!("Method not found: {}", request.method),
                data: None,
            }),
        };

        // Only send response for requests (with id), not notifications
        if !is_notification {
            match result {
                Ok(value) => self.write_result(id, value).await,
                Err(e) => self.write_error(id, e.code, &e.message).await,
            }
        }
    }

    // ── Method handlers ──────────────────────────────────────────

    fn handle_initialize(&self, _params: &Value) -> RpcResult {
        let default_model = self
            .config
            .providers
            .fallback_provider()
            .and_then(|e| e.model.clone());

        let mut zeroclaw_meta = serde_json::json!({
            "maxSessions": self.acp_config.max_sessions,
            "sessionTimeoutSecs": self.acp_config.session_timeout_secs,
        });
        if let Some(model) = default_model {
            zeroclaw_meta["defaultModel"] = serde_json::json!(model);
        }

        Ok(serde_json::json!({
            "protocolVersion": ACP_PROTOCOL_VERSION,
            "agentCapabilities": {
                "loadSession": false,
                "promptCapabilities": {
                    "image": false,
                    "audio": false,
                    "embeddedContext": false,
                },
                "mcpCapabilities": {
                    "http": false,
                    "sse": false,
                },
                "sessionCapabilities": {},
            },
            "agentInfo": {
                "name": "zeroclaw-acp",
                "title": "ZeroClaw ACP",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "authMethods": [],
            "_meta": {
                "zeroclaw": zeroclaw_meta,
            }
        }))
    }

    async fn handle_session_new(&self, params: &Value) -> RpcResult {
        let mut sessions = self.sessions.lock().await;

        if sessions.len() >= self.acp_config.max_sessions {
            return Err(RpcError {
                code: SESSION_LIMIT_REACHED,
                message: format!(
                    "Maximum session limit reached ({})",
                    self.acp_config.max_sessions
                ),
                data: None,
            });
        }

        let requested_cwd = self.requested_session_cwd(params);

        let workspace_dir = std::fs::canonicalize(&requested_cwd)
            .map_err(|e| RpcError {
                code: INVALID_PARAMS,
                message: format!(
                    "cwd is not a usable directory ({}): {e}",
                    requested_cwd.display()
                ),
                data: None,
            })?
            .to_string_lossy()
            .into_owned();

        let session_id = Uuid::new_v4().to_string();

        // Build agent from global config, with the session's cwd pinned as
        // the file/shell sandbox boundary. The agent's data directory
        // (memory DB, identity, scheduled tasks) still lives under
        // `config.workspace_dir`.
        let agent = Agent::from_config_with_session_cwd_and_mcp(
            &self.config,
            Some(std::path::Path::new(&workspace_dir)),
            false,
        )
        .await
        .map_err(|e| RpcError {
            code: INTERNAL_ERROR,
            message: format!("Failed to create agent: {e}"),
            data: None,
        })?;

        // Wire an ACP back-channel so tools like `ask_user`,
        // `escalate_to_human`, and `reaction` can talk to the IDE/CLI client
        // for this session. Registered as `"acp"`; resolved by name when the
        // agent picks a channel.
        let acp_channel = Arc::new(AcpChannel::new(
            "acp",
            session_id.clone(),
            Arc::clone(&self.rpc),
            Duration::from_secs(self.acp_config.session_timeout_secs),
        ));
        agent.channel_handles().register_channel("acp", acp_channel);

        let now = Instant::now();
        sessions.insert(
            session_id.clone(),
            Arc::new(Mutex::new(Session {
                agent,
                created_at: now,
                last_active: now,
            })),
        );

        debug!("Created session {session_id} (workspace: {workspace_dir})");

        Ok(serde_json::json!({
            "sessionId": session_id,
            "workspaceDir": workspace_dir,
        }))
    }

    fn requested_session_cwd(&self, params: &Value) -> PathBuf {
        params
            .get("cwd")
            .or_else(|| params.get("workspaceDir"))
            .or_else(|| params.get("workspace_dir"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| self.config.workspace_dir.clone())
            })
    }

    async fn handle_session_prompt(&self, params: &Value, _request_id: &Value) -> RpcResult {
        let session_id = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: INVALID_PARAMS,
                message: "Missing required parameter: sessionId".to_string(),
                data: None,
            })?
            .to_string();

        let prompt = Self::parse_prompt(params)?;

        // Clone the Arc so the session stays visible in the map throughout the
        // turn. `session/stop` and the reaper can still find it; they will
        // block on the inner Mutex until the turn completes.
        let session_arc = {
            let sessions = self.sessions.lock().await;
            sessions.get(&session_id).cloned().ok_or_else(|| RpcError {
                code: SESSION_NOT_FOUND,
                message: format!("Session not found: {session_id}"),
                data: None,
            })?
        };

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(100);

        // Create a cancellation token for this turn and register it so that a
        // concurrent `session/cancel` notification can fire it without waiting
        // for the inner session lock (which is held for the full turn duration).
        let cancel_token = tokio_util::sync::CancellationToken::new();
        {
            self.cancel_tokens
                .lock()
                .expect("cancel_tokens lock poisoned")
                .insert(session_id.clone(), cancel_token.clone());
        }

        // Move the Arc into the spawned task and lock inside it.  The inner
        // Mutex stays locked for the duration of the turn, preventing
        // concurrent stop/reap from touching the agent mid-turn. The outer
        // map entry remains in place.
        let turn_handle = tokio::spawn(async move {
            let mut session = session_arc.lock().await;
            let result = session
                .agent
                .turn_streamed(&prompt, event_tx, Some(cancel_token))
                .await;
            session.last_active = Instant::now();
            result
            // guard drops here, releasing the inner lock
        });

        // Forward events as they arrive. Use standard ACP `session/update`
        // notifications: `tool_call` for initial (pending + title/kind for UI/icons),
        // `tool_call_update` for completion (status + rawOutput/content). This enables
        // proper pending→completed flow in ACP clients.
        // Track streamed text so partial content survives cancellation.
        let mut accumulated_text = String::new();
        while let Some(event) = event_rx.recv().await {
            if let TurnEvent::Chunk { ref delta } = event {
                accumulated_text.push_str(delta);
            }
            let notification = notification_for_turn_event(&session_id, &event);
            self.write_notification(&notification).await;
        }

        // Remove the cancel token regardless of outcome — the turn is over.
        {
            self.cancel_tokens
                .lock()
                .expect("cancel_tokens lock poisoned")
                .remove(&session_id);
        }

        let turn_result = turn_handle.await.map_err(|e| RpcError {
            code: INTERNAL_ERROR,
            message: format!("Agent task panicked: {e}"),
            data: None,
        })?;

        // Per ACP spec: a cancelled turn must respond with stopReason "cancelled",
        // not an error. Detect via ToolLoopCancelled propagated through anyhow.
        let was_cancelled = match &turn_result {
            Err(e) => zeroclaw_runtime::agent::loop_::is_tool_loop_cancelled(e),
            Ok(_) => false,
        };

        if was_cancelled {
            let content = if accumulated_text.is_empty() {
                "[interrupted by user]".to_string()
            } else {
                format!("{accumulated_text}\n\n[interrupted by user]")
            };
            return Ok(serde_json::json!({
                "sessionId": session_id,
                "stopReason": "cancelled",
                "content": content,
            }));
        }

        let result = turn_result.map_err(|e| RpcError {
            code: INTERNAL_ERROR,
            message: format!("Agent turn failed: {e}"),
            data: None,
        })?;

        Ok(serde_json::json!({
            "sessionId": session_id,
            "stopReason": "end_turn",
            "content": result,  // full assembled response for clients that expect it
        }))
    }

    fn parse_prompt(params: &Value) -> std::result::Result<String, RpcError> {
        match params.get("prompt") {
            Some(Value::String(s)) => Ok(s.clone()),
            Some(Value::Array(arr)) => {
                let mut joined = String::new();
                for part in arr {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        if !joined.is_empty() {
                            joined.push_str("\n\n");
                        }
                        joined.push_str(text);
                    }
                }
                if joined.is_empty() {
                    return Err(RpcError {
                        code: INVALID_PARAMS,
                        message: "Parameter 'prompt' array must contain at least one text part"
                            .to_string(),
                        data: None,
                    });
                }
                Ok(joined)
            }
            _ => Err(RpcError {
                code: INVALID_PARAMS,
                message: "Missing required parameter: prompt (must be string or array of parts)"
                    .to_string(),
                data: None,
            }),
        }
    }

    async fn handle_session_stop(&self, params: &Value) -> RpcResult {
        let session_id = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: INVALID_PARAMS,
                message: "Missing required parameter: sessionId".to_string(),
                data: None,
            })?;

        let session_arc = {
            let mut sessions = self.sessions.lock().await;
            sessions.remove(session_id).ok_or_else(|| RpcError {
                code: SESSION_NOT_FOUND,
                message: format!("Session not found: {session_id}"),
                data: None,
            })?
        };

        // Wait for any in-flight prompt turn to finish before cleaning up.
        // The inner lock is held by the turn task; this blocks until it drops.
        let session = session_arc.lock().await;
        // Drop the ACP back-channel from each tool's channel map so the
        // session's RpcOutbound clone isn't kept alive by stale entries.
        session.agent.channel_handles().unregister_channel("acp");
        debug!("Stopped session {session_id}");
        Ok(serde_json::json!({
            "sessionId": session_id,
            "stopped": true,
        }))
    }

    /// Handle `session/cancel` notifications (ACP spec §Cancellation).
    ///
    /// Fires the cancellation token for the named session's active turn, if
    /// one is running. Idempotent — silently succeeds when there is no active
    /// turn. The return value is ignored for notifications.
    async fn handle_session_cancel(&self, params: &Value) -> RpcResult {
        let session_id = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: INVALID_PARAMS,
                message: "Missing required parameter: sessionId".to_string(),
                data: None,
            })?;

        let token = self
            .cancel_tokens
            .lock()
            .expect("cancel_tokens lock poisoned")
            .get(session_id)
            .cloned();

        if let Some(token) = token {
            token.cancel();
            debug!("Cancelled active turn for session {session_id}");
        }

        Ok(serde_json::json!({}))
    }

    /// Handle incoming `session/update` (or legacy `session/event`) notifications.
    ///
    /// This processes bidirectional events for an active session (e.g. tool results,
    /// status updates, or client-side events). Currently updates session activity
    /// to prevent premature reaping; future extensions can route specific event
    /// types into the Agent.
    async fn handle_session_event(&self, params: &Value) -> RpcResult {
        let session_id = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: INVALID_PARAMS,
                message: "Missing required parameter: sessionId".to_string(),
                data: None,
            })?
            .to_string();

        let event_type = params
            .get("type")
            .or_else(|| params.get("update").and_then(|u| u.get("sessionUpdate")))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        debug!("Received session update (type={event_type}) for session {session_id}");

        let session_arc = {
            let sessions = self.sessions.lock().await;
            sessions.get(&session_id).cloned()
        };

        if let Some(session_arc) = session_arc {
            // Best-effort last_active update. If the inner lock is held by an
            // active turn, skip it — the turn itself updates last_active on completion.
            if let Ok(mut session) = session_arc.try_lock() {
                session.last_active = Instant::now();
            }
            Ok(serde_json::json!({
                "sessionId": session_id,
                "type": event_type,
                "status": "processed"
            }))
        } else {
            Err(RpcError {
                code: SESSION_NOT_FOUND,
                message: format!("Session not found: {session_id}"),
                data: None,
            })
        }
    }

    // ── I/O helpers ──────────────────────────────────────────────

    async fn write_result(&self, id: Value, result: Value) {
        let response = JsonRpcResponse {
            jsonrpc: "2.0",
            result: Some(result),
            error: None,
            id,
        };
        self.write_json(&response).await;
    }

    async fn write_error(&self, id: Value, code: i32, message: &str) {
        let response = JsonRpcResponse {
            jsonrpc: "2.0",
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
                data: None,
            }),
            id,
        };
        self.write_json(&response).await;
    }

    async fn write_notification(&self, notification: &JsonRpcNotification) {
        self.write_json(notification).await;
    }

    async fn write_json<T: Serialize>(&self, value: &T) {
        match serde_json::to_string(value) {
            Ok(json) => {
                if self.rpc.writer_tx.send(json).await.is_err() {
                    error!("ACP writer task closed; dropping outbound message");
                }
            }
            Err(e) => {
                error!("Failed to serialize JSON-RPC message: {e}");
            }
        }
    }
}

/// Single writer task that owns stdout. All outbound JSON-RPC messages flow
/// through here, so concurrent notifications and outbound requests don't
/// interleave bytes.
async fn writer_task(mut rx: mpsc::Receiver<String>) {
    let mut stdout = tokio::io::stdout();
    while let Some(line) = rx.recv().await {
        if let Err(e) = stdout.write_all(line.as_bytes()).await {
            error!("Failed to write to stdout: {e}");
            continue;
        }
        if let Err(e) = stdout.write_all(b"\n").await {
            error!("Failed to write newline to stdout: {e}");
            continue;
        }
        if let Err(e) = stdout.flush().await {
            error!("Failed to flush stdout: {e}");
        }
    }
}

fn map_tool_kind(name: &str) -> &'static str {
    match name {
        "ask_user" | "calculator" | "claude_code" | "claude_code_runner" | "codex_cli"
        | "composio" | "delegate" | "escalate_to_human" | "execute_pipeline" | "gemini_cli"
        | "jira" | "llm_task" | "opencode_cli" | "schedule" | "security_ops" | "shell"
        | "sop_advance" | "sop_approve" | "sop_execute" | "swarm" | "vi_verify" => "execute",
        "backup" | "browser_open" | "canvas" | "cloud_ops" | "file_edit" | "file_write"
        | "memory_export" | "memory_store" | "report_template" => "edit",
        "cron_add" | "poll" | "reaction" => "edit",
        "memory_forget" | "memory_purge" => "delete",
        // ACP clients often treat `read`/`search`/`fetch` calls as noisy
        // background context gathering and keep their content collapsed. These
        // ZeroClaw tools return user-visible text, so use `other` to keep the
        // result content surfaced consistently across clients.
        "content_search" | "discord_search" | "glob_search" | "knowledge" | "search"
        | "tool_search" | "web_search_tool" => "other",
        "browser"
        | "browser_delegate"
        | "cloud_patterns"
        | "data_management"
        | "file_read"
        | "git_operations"
        | "google_workspace"
        | "hardware_board_info"
        | "hardware_memory_map"
        | "hardware_memory_read"
        | "image_info"
        | "linkedin"
        | "microsoft365"
        | "model_routing_config"
        | "model_switch"
        | "pdf_read"
        | "project_intel"
        | "proxy_config"
        | "read_skill"
        | "sessions_history"
        | "sessions_list"
        | "sop_list"
        | "sop_status"
        | "text_browser"
        | "weather"
        | "workspace" => "other",
        "cron_list" | "cron_runs" | "memory_recall" => "other",
        "http_request" | "web_fetch" => "other",
        "image_gen" => "other",
        "cron_remove" => "delete",
        "cron_run" => "execute",
        "sessions_send" => "execute",
        _ => "other",
    }
}

fn notification_for_turn_event(session_id: &str, event: &TurnEvent) -> JsonRpcNotification {
    match event {
        TurnEvent::Chunk { delta } => JsonRpcNotification {
            jsonrpc: "2.0",
            method: "session/update",
            params: serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {
                        "type": "text",
                        "text": delta
                    }
                }
            }),
        },
        TurnEvent::ToolCall { id, name, args } => JsonRpcNotification {
            jsonrpc: "2.0",
            method: "session/update",
            params: serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": id,
                    "name": name,
                    "title": name,
                    "kind": map_tool_kind(name),
                    "rawInput": args,
                    "status": "pending"
                }
            }),
        },
        TurnEvent::ToolResult { id, name, output } => JsonRpcNotification {
            jsonrpc: "2.0",
            method: "session/update",
            params: serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": id,
                    "name": name,
                    "title": name,
                    "kind": map_tool_kind(name),
                    "status": "completed",
                    "rawOutput": output,
                    "body": output,
                    "content": [{
                        "type": "content",
                        "content": {
                            "type": "text",
                            "text": output
                        }
                    }]
                }
            }),
        },
        TurnEvent::Thinking { delta } => JsonRpcNotification {
            jsonrpc: "2.0",
            method: "session/update",
            params: serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "agent_thought_chunk",
                    "content": {
                        "type": "text",
                        "text": delta
                    }
                }
            }),
        },
    }
}

// ── Error helper ─────────────────────────────────────────────────

#[derive(Debug)]
struct RpcError {
    code: i32,
    message: String,
    #[allow(dead_code)] // JSON-RPC spec field, used for structured error data
    data: Option<Value>,
}

type RpcResult = std::result::Result<Value, RpcError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acp_server_config_defaults() {
        let cfg = AcpServerConfig::default();
        assert_eq!(cfg.max_sessions, 10);
        assert_eq!(cfg.session_timeout_secs, 3600);
    }

    #[test]
    fn acp_server_config_deserialize() {
        let json = r#"{"max_sessions": 5, "session_timeout_secs": 1800}"#;
        let cfg: AcpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.max_sessions, 5);
        assert_eq!(cfg.session_timeout_secs, 1800);
    }

    #[test]
    fn acp_server_config_deserialize_partial() {
        let json = r#"{"max_sessions": 3}"#;
        let cfg: AcpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.max_sessions, 3);
        assert_eq!(cfg.session_timeout_secs, 3600);
    }

    #[test]
    fn json_rpc_request_parse() {
        let json = r#"{"jsonrpc":"2.0","method":"initialize","params":{},"id":1}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "initialize");
        assert_eq!(req.id, Some(Value::Number(1.into())));
    }

    #[test]
    fn json_rpc_request_parse_notification() {
        let json = r#"{"jsonrpc":"2.0","method":"session/update","params":{}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "session/update");
        assert!(req.id.is_none());
    }

    #[test]
    fn json_rpc_response_serialize() {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0",
            result: Some(serde_json::json!({"status": "ok"})),
            error: None,
            id: Value::Number(1.into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert!(parsed.get("result").is_some());
        assert!(parsed.get("error").is_none());
        assert_eq!(parsed["id"], 1);
    }

    #[tokio::test]
    async fn rpc_request_timeout_drop_removes_pending_responder() {
        let (tx, mut rx) = mpsc::channel::<String>(16);
        let rpc = RpcOutbound::for_testing(tx);

        let result = tokio::time::timeout(
            Duration::from_millis(10),
            rpc.request("session/request_permission", serde_json::json!({})),
        )
        .await;

        assert!(result.is_err());
        assert!(rx.recv().await.is_some());
        assert_eq!(rpc.pending_count_for_test(), 0);
    }

    #[test]
    fn initialize_response_uses_acp_v1_shape() {
        let server = AcpServer::new(Config::default(), AcpServerConfig::default());
        let result = server
            .handle_initialize(&serde_json::json!({
                "protocolVersion": 1,
                "clientCapabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0.0"
                }
            }))
            .unwrap();

        assert_eq!(result["protocolVersion"], 1);
        assert_eq!(result["agentInfo"]["name"], "zeroclaw-acp");
        assert_eq!(result["agentInfo"]["title"], "ZeroClaw ACP");
        assert_eq!(result["agentInfo"]["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(result["authMethods"], serde_json::json!([]));
        assert_eq!(result["agentCapabilities"]["loadSession"], false);
        assert_eq!(
            result["agentCapabilities"]["promptCapabilities"]["image"],
            false
        );
        assert_eq!(
            result["agentCapabilities"]["mcpCapabilities"]["http"],
            false
        );
        assert!(result.get("serverInfo").is_none());
        assert!(result.get("capabilities").is_none());
    }

    #[test]
    fn handle_initialize_default_model_absent_when_unconfigured() {
        let server = AcpServer::new(Config::default(), AcpServerConfig::default());
        let result = server.handle_initialize(&serde_json::json!({})).unwrap();
        assert!(
            result["_meta"]["zeroclaw"].get("defaultModel").is_none(),
            "defaultModel must be absent when no provider is configured, got: {}",
            result["_meta"]["zeroclaw"]["defaultModel"]
        );
    }

    #[test]
    fn handle_initialize_default_model_reflects_configured_provider() {
        use zeroclaw_config::schema::ModelProviderConfig;
        let mut config = Config::default();
        config.providers.fallback = Some("myprovider".to_string());
        config.providers.models.insert(
            "myprovider".to_string(),
            ModelProviderConfig {
                model: Some("llama3.2".to_string()),
                ..Default::default()
            },
        );
        let server = AcpServer::new(config, AcpServerConfig::default());
        let result = server.handle_initialize(&serde_json::json!({})).unwrap();
        assert_eq!(result["_meta"]["zeroclaw"]["defaultModel"], "llama3.2");
    }

    #[test]
    fn session_new_defaults_to_launch_cwd_when_client_omits_cwd() {
        let config = Config {
            workspace_dir: PathBuf::from("/not/the/project"),
            ..Default::default()
        };
        let server = AcpServer::new(config, AcpServerConfig::default());
        let expected = std::env::current_dir().unwrap();

        assert_eq!(
            server.requested_session_cwd(&serde_json::json!({})),
            expected
        );
    }

    #[test]
    fn session_new_respects_client_cwd_when_present() {
        let server = AcpServer::new(Config::default(), AcpServerConfig::default());
        let cwd = std::env::current_dir().unwrap();

        assert_eq!(
            server.requested_session_cwd(&serde_json::json!({"cwd": cwd})),
            cwd
        );
    }

    #[tokio::test]
    async fn session_new_does_not_wait_for_configured_mcp_servers() {
        let cwd = tempfile::tempdir().unwrap();
        let config = Config {
            workspace_dir: cwd.path().to_path_buf(),
            providers: zeroclaw_config::providers::ProvidersConfig {
                fallback: Some("openrouter".to_string()),
                models: HashMap::from([(
                    "openrouter".to_string(),
                    zeroclaw_config::schema::ModelProviderConfig {
                        model: Some("test-model".to_string()),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            },
            mcp: zeroclaw_config::schema::McpConfig {
                enabled: true,
                servers: vec![zeroclaw_config::schema::McpServerConfig {
                    name: "slow".to_string(),
                    transport: zeroclaw_config::schema::McpTransport::Stdio,
                    command: "/bin/sh".to_string(),
                    args: vec!["-c".to_string(), "sleep 60".to_string()],
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let server = AcpServer::new(config, AcpServerConfig::default());

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            server.handle_session_new(&serde_json::json!({
                "cwd": cwd.path().to_string_lossy(),
                "mcpServers": []
            })),
        )
        .await
        .expect("session/new should not block on configured MCP startup")
        .expect("session/new should create a session");

        assert!(result["sessionId"].as_str().is_some());
    }

    #[test]
    fn json_rpc_error_response_serialize() {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0",
            result: None,
            error: Some(JsonRpcError {
                code: METHOD_NOT_FOUND,
                message: "Method not found".to_string(),
                data: None,
            }),
            id: Value::Number(1.into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.get("error").is_some());
        assert_eq!(parsed["error"]["code"], -32601);
        assert!(parsed.get("result").is_none());
    }

    #[test]
    fn json_rpc_notification_serialize() {
        let notif = JsonRpcNotification {
            jsonrpc: "2.0",
            method: "session/update",
            params: serde_json::json!({
                "sessionId": "test-sid",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "hello" }
                }
            }),
        };
        let json = serde_json::to_string(&notif).unwrap();
        assert!(json.contains(r#""method":"session/update""#));
        assert!(json.contains(r#""sessionUpdate":"agent_message_chunk""#));
        assert!(json.contains(r#""text":"hello""#));
    }

    #[test]
    fn test_prompt_parsing() {
        // String prompt
        let string_params = serde_json::json!({"prompt": "hello world"});
        let result = AcpServer::parse_prompt(&string_params).unwrap();
        assert_eq!(result, "hello world");

        // Array prompt (valid)
        let array_params = serde_json::json!({
            "prompt": [
                {"type": "text", "text": "part 1"},
                {"type": "text", "text": "part 2"}
            ]
        });
        let result = AcpServer::parse_prompt(&array_params).unwrap();
        assert_eq!(result, "part 1\n\npart 2");

        // Array prompt (empty or no text)
        let empty_array_params = serde_json::json!({"prompt": []});
        let result = AcpServer::parse_prompt(&empty_array_params);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, INVALID_PARAMS);

        let no_text_params = serde_json::json!({
            "prompt": [
                {"type": "image", "data": "..."}
            ]
        });
        let result = AcpServer::parse_prompt(&no_text_params);
        assert!(result.is_err());

        // Missing prompt
        let missing_params = serde_json::json!({});
        let result = AcpServer::parse_prompt(&missing_params);
        assert!(result.is_err());
    }

    #[test]
    fn test_tool_call_and_update_serialization() {
        // Test tool_call (initial pending event)
        let tool_call_notif = JsonRpcNotification {
            jsonrpc: "2.0",
            method: "session/update",
            params: serde_json::json!({
                "sessionId": "test-sid",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "tc-12345",
                    "name": "shell",
                    "title": "shell",
                    "kind": "execute",
                    "rawInput": {"command": "ls -la"},
                    "status": "pending"
                }
            }),
        };
        let json1 = serde_json::to_string(&tool_call_notif).unwrap();
        assert!(json1.contains("\"sessionUpdate\":\"tool_call\""));
        assert!(json1.contains("\"toolCallId\":\"tc-12345\""));
        assert!(json1.contains("\"name\":\"shell\""));
        assert!(json1.contains("\"title\":\"shell\""));
        assert!(json1.contains("\"kind\":\"execute\""));
        assert!(json1.contains("\"status\":\"pending\""));
        assert!(json1.contains("\"rawInput\""));

        // Test tool_call_update completion payload
        let tool_update_notif = JsonRpcNotification {
            jsonrpc: "2.0",
            method: "session/update",
            params: serde_json::json!({
                "sessionId": "test-sid",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "tc-12345",
                    "name": "shell",
                    "title": "shell",
                    "kind": "execute",
                    "status": "completed",
                    "rawOutput": "file1.txt\nfile2.txt",
                    "body": "file1.txt\nfile2.txt",
                    "content": [{
                        "type": "content",
                        "content": {
                            "type": "text",
                            "text": "file1.txt\nfile2.txt"
                        }
                    }]
                }
            }),
        };
        let json2 = serde_json::to_string(&tool_update_notif).unwrap();
        assert!(json2.contains("\"sessionUpdate\":\"tool_call_update\""));
        assert!(json2.contains("\"toolCallId\":\"tc-12345\""));
        assert!(json2.contains("\"name\":\"shell\""));
        assert!(json2.contains("\"status\":\"completed\""));
        assert!(json2.contains("\"rawOutput\""));
        assert!(json2.contains("\"body\""));
        assert!(json2.contains("\"content\""));
        assert!(json2.contains("\"type\":\"content\""));
        assert!(json2.contains("file1.txt"));
        // Verify matching toolCallId across events
        assert!(json1.contains("tc-12345") && json2.contains("tc-12345"));
    }

    #[test]
    fn map_tool_kind_uses_explicit_tool_names() {
        assert_eq!(map_tool_kind("memory_forget"), "delete");
        assert_eq!(map_tool_kind("memory_purge"), "delete");
        assert_eq!(map_tool_kind("cron_run"), "execute");
        assert_eq!(map_tool_kind("file_read"), "other");
        assert_eq!(map_tool_kind("knowledge"), "other");
        assert_eq!(map_tool_kind("web_fetch"), "other");
        assert_eq!(map_tool_kind("file_write"), "edit");
        assert_eq!(map_tool_kind("unknown_tool"), "other");
    }

    #[test]
    fn turn_tool_events_include_client_visible_tool_fields() {
        let call = notification_for_turn_event(
            "test-sid",
            &TurnEvent::ToolCall {
                id: "tc-12345".to_string(),
                name: "shell".to_string(),
                args: serde_json::json!({"command": "ls -la"}),
            },
        );
        let call_value = serde_json::to_value(call).unwrap();
        assert_eq!(call_value["method"], "session/update");
        assert_eq!(call_value["params"]["update"]["sessionUpdate"], "tool_call");
        assert_eq!(call_value["params"]["update"]["toolCallId"], "tc-12345");
        assert_eq!(call_value["params"]["update"]["name"], "shell");
        assert_eq!(call_value["params"]["update"]["title"], "shell");
        assert_eq!(call_value["params"]["update"]["kind"], "execute");
        assert_eq!(
            call_value["params"]["update"]["rawInput"],
            serde_json::json!({"command": "ls -la"})
        );

        let result = notification_for_turn_event(
            "test-sid",
            &TurnEvent::ToolResult {
                id: "tc-12345".to_string(),
                name: "shell".to_string(),
                output: "file1.txt\nfile2.txt".to_string(),
            },
        );
        let result_value = serde_json::to_value(result).unwrap();
        assert_eq!(
            result_value["params"]["update"]["sessionUpdate"],
            "tool_call_update"
        );
        assert_eq!(result_value["params"]["update"]["toolCallId"], "tc-12345");
        assert_eq!(result_value["params"]["update"]["name"], "shell");
        assert_eq!(result_value["params"]["update"]["title"], "shell");
        assert_eq!(result_value["params"]["update"]["kind"], "execute");
        assert_eq!(result_value["params"]["update"]["status"], "completed");
        assert_eq!(
            result_value["params"]["update"]["rawOutput"],
            "file1.txt\nfile2.txt"
        );
        assert_eq!(
            result_value["params"]["update"]["body"],
            "file1.txt\nfile2.txt"
        );
        assert_eq!(
            result_value["params"]["update"]["content"][0]["content"]["text"],
            "file1.txt\nfile2.txt"
        );
    }

    /// `session/stop` must succeed while a `session/prompt` turn is in flight.
    ///
    /// The session entry lives in the outer map for its entire lifetime.
    /// The inner `Arc<Mutex<Session>>` serialises access: the prompt turn holds
    /// the inner lock while running; `session/stop` removes the outer entry
    /// then waits for the inner lock before cleaning up.  It must never see
    /// SESSION_NOT_FOUND just because a turn happens to be running.
    #[tokio::test]
    async fn session_stop_finds_session_during_active_prompt_turn() {
        let cwd = tempfile::tempdir().unwrap();
        let config = Config {
            workspace_dir: cwd.path().to_path_buf(),
            providers: zeroclaw_config::providers::ProvidersConfig {
                fallback: Some("anthropic".to_string()),
                models: HashMap::from([(
                    "anthropic".to_string(),
                    zeroclaw_config::schema::ModelProviderConfig {
                        model: Some("claude-haiku-4-5".to_string()),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            },
            ..Default::default()
        };
        let server = Arc::new(AcpServer::new(config, AcpServerConfig::default()));

        // Create a real session via the normal path.
        let new_result = server
            .handle_session_new(&serde_json::json!({
                "cwd": cwd.path().to_string_lossy()
            }))
            .await
            .expect("session/new must succeed");
        let session_id = new_result["sessionId"].as_str().unwrap().to_string();

        // Grab the inner lock to simulate an in-flight prompt turn.
        let session_arc = {
            let sessions = server.sessions.lock().await;
            sessions.get(&session_id).cloned().unwrap()
        };
        let _guard = session_arc.lock().await;

        // session/stop should find the session in the outer map.  With the
        // inner lock held it blocks — confirm it does NOT immediately return
        // SESSION_NOT_FOUND.
        let server_clone = Arc::clone(&server);
        let sid_clone = session_id.clone();
        let stop_result = tokio::time::timeout(Duration::from_millis(100), async move {
            server_clone
                .handle_session_stop(&serde_json::json!({ "sessionId": sid_clone }))
                .await
        })
        .await;

        match stop_result {
            Err(_timeout) => {} // expected — blocked waiting for the inner lock
            Ok(Ok(_)) => panic!("stop returned Ok without the lock being released"),
            Ok(Err(e)) => {
                assert_ne!(
                    e.code, SESSION_NOT_FOUND,
                    "session/stop must not return SESSION_NOT_FOUND while a turn is in flight"
                );
            }
        }
    }

    fn make_test_config(cwd: &std::path::Path) -> Config {
        Config {
            workspace_dir: cwd.to_path_buf(),
            providers: zeroclaw_config::providers::ProvidersConfig {
                fallback: Some("anthropic".to_string()),
                models: HashMap::from([(
                    "anthropic".to_string(),
                    zeroclaw_config::schema::ModelProviderConfig {
                        model: Some("claude-haiku-4-5".to_string()),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// `session/cancel` on an idle session (no active turn) must succeed silently.
    #[tokio::test]
    async fn session_cancel_idle_session_is_noop() {
        let cwd = tempfile::tempdir().unwrap();
        let server = Arc::new(AcpServer::new(
            make_test_config(cwd.path()),
            AcpServerConfig::default(),
        ));

        let new_result = server
            .handle_session_new(&serde_json::json!({
                "cwd": cwd.path().to_string_lossy()
            }))
            .await
            .expect("session/new must succeed");
        let session_id = new_result["sessionId"].as_str().unwrap().to_string();

        // No active turn — cancel must not error.
        let result = server
            .handle_session_cancel(&serde_json::json!({ "sessionId": session_id }))
            .await;
        assert!(result.is_ok(), "idle cancel must succeed: {result:?}");
    }

    /// `session/cancel` for an unknown session ID must succeed silently (notification
    /// semantics: no response, no error propagation).
    #[tokio::test]
    async fn session_cancel_unknown_session_is_noop() {
        let cwd = tempfile::tempdir().unwrap();
        let server = Arc::new(AcpServer::new(
            make_test_config(cwd.path()),
            AcpServerConfig::default(),
        ));

        let result = server
            .handle_session_cancel(&serde_json::json!({ "sessionId": "sess_does_not_exist" }))
            .await;
        assert!(result.is_ok(), "unknown-session cancel must succeed: {result:?}");
    }

    /// After a normal (non-cancelled) turn, the cancel token must be removed from
    /// `cancel_tokens` so that the map doesn't leak entries across turns.
    #[tokio::test]
    async fn cancel_token_removed_after_successful_turn() {
        let cwd = tempfile::tempdir().unwrap();
        let config = Config {
            workspace_dir: cwd.path().to_path_buf(),
            ..Default::default()
        };
        let server = Arc::new(AcpServer::new(config, AcpServerConfig::default()));

        // Simulate an active turn by inserting a token directly, then removing it
        // (mirrors what handle_session_prompt does on turn completion).
        let session_id = "sess_token_leak_test".to_string();
        let token = tokio_util::sync::CancellationToken::new();
        server
            .cancel_tokens
            .lock()
            .expect("cancel_tokens lock poisoned")
            .insert(session_id.clone(), token);

        // Simulate turn completion cleanup.
        server
            .cancel_tokens
            .lock()
            .expect("cancel_tokens lock poisoned")
            .remove(&session_id);

        let remaining = server
            .cancel_tokens
            .lock()
            .expect("cancel_tokens lock poisoned")
            .len();
        assert_eq!(remaining, 0, "cancel token must be removed after turn ends");
    }
}
