//! JSON-RPC 2.0 client over a local IPC stream (Unix socket / Windows
//! named pipe, NDJSON) or WebSocket (WSS).
//!
//! Uses local JSON-RPC transport types so `zerocode` stays an RPC-only surface.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc};

use crate::jsonrpc::{self, JsonRpcError, RpcOutbound, field};
use crate::wire::{ConfigFieldEntry, DoctorRunResult, FsListDirResponse, SectionShape};

const CONFIG_RENAME_TIMEOUT: Duration = Duration::from_secs(120);
const CRON_TRIGGER_TIMEOUT: Duration = Duration::from_secs(600);
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(10);

// ── Platform local-stream shim ──────────────────────────────────

#[cfg(unix)]
type LocalStream = tokio::net::UnixStream;
#[cfg(windows)]
type LocalStream = tokio::net::windows::named_pipe::NamedPipeClient;

/// Open a connection to the daemon's local IPC endpoint.
#[cfg(unix)]
async fn open_local_stream(path: &Path) -> Result<LocalStream> {
    tokio::net::UnixStream::connect(path)
        .await
        .map_err(anyhow::Error::from)
}

#[cfg(windows)]
async fn open_local_stream(path: &Path) -> Result<LocalStream> {
    use tokio::net::windows::named_pipe::ClientOptions;
    use tokio::time::{Duration, sleep};
    // The daemon may not yet have a pending pipe instance; retry briefly.
    let name = path.to_string_lossy().into_owned();
    for _ in 0..50 {
        match ClientOptions::new().open(&name) {
            Ok(c) => return Ok(c),
            Err(e) if e.raw_os_error() == Some(231) => {
                // ERROR_PIPE_BUSY — server hasn't recreated a pending instance yet.
                sleep(Duration::from_millis(20)).await;
            }
            Err(e) => return Err(anyhow::Error::from(e)),
        }
    }
    anyhow::bail!("named pipe {name} never became available")
}

// ── Wire method names used by the TUI ────────────────────────────

pub mod method {
    pub const INITIALIZE: &str = "initialize";
    pub const CONFIG_LIST: &str = "config/list";
    pub const CONFIG_SET: &str = "config/set";
    pub const CONFIG_DELETE: &str = "config/delete";
    pub const CONFIG_RELOAD: &str = "config/reload";
    pub const CONFIG_MAP_KEYS: &str = "config/map-keys";
    pub const CONFIG_RESOLVE_ALIAS_SOURCE: &str = "config/resolve-alias-source";
    pub const CONFIG_MAP_KEY_CREATE: &str = "config/map-key-create";
    pub const CONFIG_MAP_KEY_DELETE: &str = "config/map-key-delete";
    pub const CONFIG_RENAME_MAP_KEY: &str = "config/map-key-rename";
    pub const CONFIG_TEMPLATES: &str = "config/templates";
    pub const CONFIG_SECTIONS: &str = "config/sections";
    pub const CONFIG_CATALOG_MODELS: &str = "config/catalog-models";
    // Locales
    pub const LOCALES_LIST: &str = "locales/list";
    pub const LOCALES_FETCH: &str = "locales/fetch";
    // Personality
    pub const PERSONALITY_LIST: &str = "personality/list";
    pub const PERSONALITY_GET: &str = "personality/get";
    pub const PERSONALITY_PUT: &str = "personality/put";
    pub const PERSONALITY_TEMPLATES: &str = "personality/templates";
    // Skills
    pub const SKILLS_LIST: &str = "skills/list";
    pub const SKILLS_READ: &str = "skills/read";
    pub const SKILLS_WRITE: &str = "skills/write";
    pub const SKILLS_DELETE: &str = "skills/delete";
    // Session
    pub const SESSION_NEW: &str = "session/new";
    pub const SESSION_PROMPT: &str = "session/prompt";
    pub const SESSION_CONFIGURE: &str = "session/configure";
    pub const SESSION_CANCEL: &str = "session/cancel";
    pub const SESSION_GIT_BRANCH: &str = "session/git_branch";
    pub const SESSION_APPROVE: &str = "session/approve";
    pub const SESSION_CLOSE: &str = "session/close";
    pub const SESSION_KILL: &str = "session/kill";
    // Dashboard
    pub const STATUS: &str = "status";
    pub const HEALTH: &str = "health";
    pub const DOCTOR_RUN: &str = "doctor/run";
    pub const COST_QUERY: &str = "cost/query";
    pub const COST_ORG: &str = "cost/org";
    pub const SESSION_LIST: &str = "session/list";
    pub const SESSION_LIST_ACP: &str = "session/list-acp";
    pub const AGENTS_STATUS: &str = "agents/status";
    pub const CRON_LIST: &str = "cron/list";
    pub const CRON_RUNS: &str = "cron/runs";
    pub const CRON_TRIGGER: &str = "cron/trigger";
    pub const MEMORY_LIST: &str = "memory/list";
    pub const MEMORY_SEARCH: &str = "memory/search";
    pub const SESSION_MESSAGES: &str = "session/messages";
    // TUI identity
    pub const TUI_LIST: &str = "tui/list";
    pub const FS_LIST_DIR: &str = "fs/list_dir";
    // Quickstart
    pub const QUICKSTART_STATE: &str = "quickstart/state";
    pub const QUICKSTART_FIELDS: &str = "quickstart/fields";
    pub const QUICKSTART_VALIDATE: &str = "quickstart/validate";
    pub const QUICKSTART_APPLY: &str = "quickstart/apply";
    pub const QUICKSTART_DISMISS: &str = "quickstart/dismiss";
    pub const SOPS_LIST: &str = "sops/list";
    pub const SOPS_GET: &str = "sops/get";
    pub const SOPS_GRAPH: &str = "sops/graph";
    pub const SOPS_RUN: &str = "sops/run";
    pub const SOPS_RUN_OVERLAY: &str = "sops/run-overlay";
    pub const SOPS_SAVE: &str = "sops/save";
    pub const SOPS_CREATE: &str = "sops/create";
    pub const SOPS_DELETE: &str = "sops/delete";
    pub const SOPS_DECIDE: &str = "sops/decide";
    pub const SOPS_WIRE_DRAFT: &str = "sops/wire-draft";
    pub const SOPS_GRAPH_DRAFT: &str = "sops/graph-draft";
    pub const SOPS_TRIGGER_SOURCES: &str = "sops/trigger-sources";
}

// ── Socket path resolution ───────────────────────────────────────

/// Resolve the daemon's local IPC endpoint path.
/// CLI flag > `$ZEROCLAW_SOCKET` > `<config_dir>/data/daemon.sock` on Unix
/// or a `\\.\pipe\zeroclaw-<hash>` derived name on Windows.
pub fn resolve_socket_path(config_dir: &Path) -> Result<PathBuf> {
    if let Ok(p) = std::env::var("ZEROCLAW_SOCKET") {
        let p = p.trim();
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    #[cfg(unix)]
    {
        Ok(config_dir.join("data").join("daemon.sock"))
    }
    #[cfg(windows)]
    {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let data_dir = config_dir.join("data");
        let mut hasher = DefaultHasher::new();
        data_dir.hash(&mut hasher);
        Ok(PathBuf::from(format!(
            r"\\.\pipe\zeroclaw-{:x}",
            hasher.finish()
        )))
    }
}

/// Resolve config dir: CLI flag > `$ZEROCLAW_CONFIG_DIR` > home directory.
pub fn resolve_config_dir(cli_override: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = cli_override {
        return Ok(dir.to_path_buf());
    }
    if let Ok(d) = std::env::var("ZEROCLAW_CONFIG_DIR") {
        let d = d.trim();
        if !d.is_empty() {
            return Ok(PathBuf::from(d));
        }
    }
    #[cfg(unix)]
    {
        let home = std::env::var("HOME").context("HOME not set")?;
        Ok(PathBuf::from(home).join(".zeroclaw"))
    }
    #[cfg(windows)]
    {
        let profile = std::env::var("USERPROFILE").context("USERPROFILE not set")?;
        Ok(PathBuf::from(profile).join(".zeroclaw"))
    }
}

// ── Notifications ────────────────────────────────────────────────

/// A server-initiated notification (no `id` field).
#[derive(Debug, Clone)]
pub struct RpcNotification {
    pub method: String,
    pub params: Value,
}

/// A server-initiated JSON-RPC request (has both `id` and `method`)
/// that expects a response back on the same id.
///
/// The daemon issues these for ACP `elicitation/create` calls when
/// the TUI advertised `clientCapabilities.elicitation.form` during
/// `initialize`. The recipient of an `RpcInboundRequest` is the
/// `Chat` widget for the targeted session — it surfaces a modal,
/// waits for the user's choice, and writes a JSON-RPC response back
/// via `RpcClient::respond_to_inbound_request`.
#[derive(Debug, Clone)]
pub struct RpcInboundRequest {
    /// The JSON-RPC `id`. Echoed back verbatim in the response.
    pub id: Value,
    pub method: String,
    pub params: Value,
}

/// Buffer capacity for the server-initiated inbound-request broadcast.
///
/// These frames are response-bearing (today: `elicitation/create`): a dropped
/// one parks the daemon's tool call until the session timeout. The buffer is
/// sized generously so a busy TUI draw loop does not lag the receiver and lose
/// an elicitation. The Chat pane additionally surfaces a `Lagged` overflow so
/// the rare drop is diagnosable rather than a silent hang.
pub const INBOUND_REQUEST_CHANNEL_CAPACITY: usize = 1024;

// ── Typed session updates ────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum SessionUpdate {
    AgentMessageChunk {
        session_id: String,
        text: String,
    },
    AgentThoughtChunk {
        session_id: String,
        text: String,
    },
    ToolCall {
        session_id: String,
        tool_call_id: String,
        name: String,
        raw_input: serde_json::Value,
    },
    ToolResult {
        session_id: String,
        tool_call_id: String,
        raw_output: String,
    },
    ApprovalRequest {
        session_id: String,
        request_id: String,
        tool_name: String,
        arguments_summary: String,
        timeout_secs: u64,
    },
    /// Emitted once per LLM call with current context size and configured limit.
    ContextUsage {
        session_id: String,
        input_tokens: Option<u64>,
        max_context_tokens: Option<u64>,
    },
    /// Older complete turns were removed from structured session history.
    HistoryTrimmed {
        session_id: String,
        dropped_messages: u64,
        kept_turns: u64,
        reason: String,
    },
    /// Terminal event for a turn. Replaces the JSON-RPC response of
    /// `session/prompt`. `outcome` distinguishes a clean finish from a cancel
    /// or a failure; the daemon-composed `content` carries the attributed
    /// reason for non-completed outcomes.
    TurnComplete {
        session_id: String,
        outcome: TurnEndOutcome,
        content: String,
    },
    /// The agent published or updated its execution plan (TodoWrite).
    /// Whole-list replacement; `entries` is the complete authoritative
    /// list. An empty vec clears the tracker.
    Plan {
        session_id: String,
        entries: Vec<crate::wire::PlanEntry>,
    },
}

/// Wire mirror of the daemon's `TurnCompletionOutcome`. Decoded straight from
/// the `outcome` field; an unrecognised or absent value maps to `Completed` so
/// a turn never appears stuck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEndOutcome {
    Completed,
    Cancelled,
    Failed,
}

impl TurnEndOutcome {
    fn from_wire(value: Option<&serde_json::Value>) -> Self {
        value
            .and_then(|v| serde_json::from_value::<Self>(v.clone()).ok())
            .unwrap_or(Self::Completed)
    }
}

pub fn parse_session_update(params: &serde_json::Value) -> Option<SessionUpdate> {
    let kind = params.get("type")?.as_str()?;
    let sid = params.get("session_id")?.as_str()?.to_string();
    match kind {
        "agent_message_chunk" => Some(SessionUpdate::AgentMessageChunk {
            session_id: sid,
            text: params.get("text")?.as_str()?.to_string(),
        }),
        "agent_thought_chunk" => Some(SessionUpdate::AgentThoughtChunk {
            session_id: sid,
            text: params.get("text")?.as_str()?.to_string(),
        }),
        "tool_call" => Some(SessionUpdate::ToolCall {
            session_id: sid,
            tool_call_id: params.get("tool_call_id")?.as_str()?.to_string(),
            name: params.get("name")?.as_str()?.to_string(),
            raw_input: params.get("raw_input")?.clone(),
        }),
        "tool_result" => Some(SessionUpdate::ToolResult {
            session_id: sid,
            tool_call_id: params.get("tool_call_id")?.as_str()?.to_string(),
            raw_output: params.get("raw_output")?.as_str()?.to_string(),
        }),
        "approval_request" => Some(SessionUpdate::ApprovalRequest {
            session_id: sid,
            request_id: params.get("request_id")?.as_str()?.to_string(),
            tool_name: params.get("tool_name")?.as_str()?.to_string(),
            arguments_summary: params.get("arguments_summary")?.as_str()?.to_string(),
            timeout_secs: params.get("timeout_secs")?.as_u64().unwrap_or(30),
        }),
        "context_usage" => Some(SessionUpdate::ContextUsage {
            session_id: sid,
            input_tokens: params.get("input_tokens").and_then(|v| v.as_u64()),
            max_context_tokens: params.get("max_context_tokens").and_then(|v| v.as_u64()),
        }),
        "history_trimmed" => Some(SessionUpdate::HistoryTrimmed {
            session_id: sid,
            dropped_messages: params.get("dropped_messages")?.as_u64()?,
            kept_turns: params.get("kept_turns")?.as_u64()?,
            reason: params.get("reason")?.as_str()?.to_string(),
        }),
        "turn_complete" => Some(SessionUpdate::TurnComplete {
            session_id: sid,
            outcome: TurnEndOutcome::from_wire(params.get("outcome")),
            content: params
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        }),
        "plan" => {
            let entries = params.get("entries")?.clone();
            let entries: Vec<crate::wire::PlanEntry> = serde_json::from_value(entries).ok()?;
            Some(SessionUpdate::Plan {
                session_id: sid,
                entries,
            })
        }
        _ => None,
    }
}

pub fn spawn_notification_router(
    mut bcast_rx: broadcast::Receiver<RpcNotification>,
    update_tx: mpsc::Sender<SessionUpdate>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match bcast_rx.recv().await {
                Ok(notif) => {
                    if notif.method != "session/update" {
                        continue;
                    }
                    if let Some(update) = parse_session_update(&notif.params)
                        && update_tx.send(update).await.is_err()
                    {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

// ── Transport ────────────────────────────────────────────────────

/// Transport protocol of the established RPC connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// Local IPC stream — Unix socket on Unix, named pipe on Windows.
    Local,
    Wss,
}

// ── Connection state ──────────────────────────────────────────────

/// Observable connection state, written by the socket read task.
/// This is the single source of truth for daemon connectivity.
#[derive(Clone, Debug)]
pub enum ConnectionState {
    Connected,
    Disconnected { reason: String },
}

/// The TUI and daemon are built from the same package version and do not
/// promise cross-version wire compatibility.
#[derive(Debug)]
pub struct DaemonVersionMismatch {
    client_version: &'static str,
    server_version: String,
}

impl DaemonVersionMismatch {
    fn new(server_version: impl Into<String>) -> Self {
        Self {
            client_version: env!("CARGO_PKG_VERSION"),
            server_version: server_version.into(),
        }
    }

    pub fn client_version(&self) -> &'static str {
        self.client_version
    }

    pub fn server_version(&self) -> &str {
        &self.server_version
    }
}

impl fmt::Display for DaemonVersionMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Version mismatch: zerocode is {} but the daemon is {}. \
             Rebuild and restart the daemon from the same checkout as zerocode.",
            self.client_version, self.server_version
        )
    }
}

impl std::error::Error for DaemonVersionMismatch {}

/// The transport connected, but the daemon did not finish the ACP handshake.
#[derive(Debug)]
pub struct DaemonInitializeTimeout {
    timeout: Duration,
}

impl DaemonInitializeTimeout {
    pub(crate) fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    pub fn timeout_seconds(&self) -> u64 {
        self.timeout.as_secs()
    }
}

impl fmt::Display for DaemonInitializeTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "daemon did not complete initialization within {}s",
            self.timeout_seconds()
        )
    }
}

impl std::error::Error for DaemonInitializeTimeout {}

#[derive(Debug)]
struct InitializeResponse {
    server_version: String,
    tui_id: Option<String>,
    tui_sig: Option<String>,
}

fn parse_initialize_response(resp: &Value) -> Result<InitializeResponse> {
    let server_version = resp
        .get("server_version")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    if server_version != env!("CARGO_PKG_VERSION") {
        return Err(DaemonVersionMismatch::new(server_version).into());
    }

    Ok(InitializeResponse {
        server_version,
        tui_id: resp.get("tui_id").and_then(Value::as_str).map(String::from),
        tui_sig: resp
            .get("tui_sig")
            .and_then(Value::as_str)
            .map(String::from),
    })
}

async fn request_initialize(
    rpc: &RpcOutbound,
    init_params: Value,
    timeout: Duration,
) -> Result<Value> {
    match tokio::time::timeout(timeout, rpc.request(method::INITIALIZE, init_params)).await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(e)) => Err(anyhow::Error::msg(format!(
            "initialize: {} ({})",
            e.message, e.code
        ))),
        Err(_) => Err(DaemonInitializeTimeout::new(timeout).into()),
    }
}

// ── Client ───────────────────────────────────────────────────────

/// Classify an incoming JSON-RPC frame and route it to the right
/// sink.
///
/// Frames are one of three shapes (per JSON-RPC 2.0):
/// 1. **Response** — has `id` plus `result` or `error`, but no
///    `method`. Routed to `RpcOutbound::dispatch_response` to wake
///    the pending outbound call on the same id.
/// 2. **Server-initiated request** — has both `id` and `method`.
///    Routed to `inbound_tx` for an in-TUI handler to answer (today:
///    `elicitation/create`). The id is preserved verbatim so the
///    response correlates correctly.
/// 3. **Notification** — has `method` but no `id`. Routed to
///    `notif_tx` for the existing notification router.
fn route_inbound_frame(
    rpc: &Arc<RpcOutbound>,
    notif_tx: &broadcast::Sender<RpcNotification>,
    inbound_tx: &broadcast::Sender<RpcInboundRequest>,
    frame: Value,
) {
    let id = frame.get(field::ID).cloned();
    let method = frame
        .get(field::METHOD)
        .and_then(Value::as_str)
        .map(str::to_string);

    match (id, method) {
        // Server-initiated request: both id and method present.
        (Some(id), Some(method)) if !id.is_null() => {
            let params = frame.get("params").cloned().unwrap_or(Value::Null);
            let _ = inbound_tx.send(RpcInboundRequest { id, method, params });
        }
        // Response: id present (typically a string), result or error,
        // no method.
        (Some(id), None) => {
            // The outbound id format is always a string; defensively
            // only dispatch when we can stringify it.
            if let Some(id_str) = id.as_str() {
                let result = frame.get(field::RESULT).cloned();
                let error: Option<JsonRpcError> = frame
                    .get(field::ERROR)
                    .and_then(|e| serde_json::from_value(e.clone()).ok());
                rpc.dispatch_response(id_str, result, error);
            }
        }
        // Notification: method present, no id (or null id).
        (None, Some(method)) => {
            let params = frame.get("params").cloned().unwrap_or(Value::Null);
            let _ = notif_tx.send(RpcNotification { method, params });
        }
        _ => {}
    }
}

#[derive(Debug)]
pub struct RpcClient {
    pub(crate) rpc: Arc<RpcOutbound>,
    _read_task: tokio::task::JoinHandle<()>,
    _router_task: tokio::task::JoinHandle<()>,
    pub server_version: String,
    notifications_bcast: broadcast::Sender<RpcNotification>,
    /// Broadcast channel for server-initiated requests that expect a
    /// response (today: `elicitation/create`). The Chat widget for the
    /// targeted session subscribes and answers via
    /// [`RpcClient::respond_to_inbound_request`].
    inbound_requests_bcast: broadcast::Sender<RpcInboundRequest>,
    connection_state: Arc<Mutex<ConnectionState>>,
    /// TUI session UID assigned by the daemon during initialize.
    pub tui_id: Option<String>,
    /// HMAC signature for reconnection. Pass back in next initialize.
    pub tui_sig: Option<String>,
    /// Transport protocol of this connection.
    transport: Transport,
}

impl RpcClient {
    /// Connect to the daemon's local IPC endpoint and complete the
    /// `initialize` handshake.
    ///
    /// Pass previous `tui_id` and `tui_sig` on reconnect to reclaim
    /// the same identity. Pass `None` for both on first connect.
    pub async fn connect(
        socket: &Path,
        prev_tui_id: Option<&str>,
        prev_tui_sig: Option<&str>,
    ) -> Result<Self> {
        let stream = open_local_stream(socket)
            .await
            .with_context(|| format!("connecting to {}", socket.display()))?;
        let (read_half, write_half) = tokio::io::split(stream);

        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(64);
        tokio::spawn(async move {
            let mut w = write_half;
            while let Some(mut line) = writer_rx.recv().await {
                if !line.ends_with('\n') {
                    line.push('\n');
                }
                if w.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });

        let rpc = Arc::new(RpcOutbound::new(writer_tx));
        let (notif_tx, _) = broadcast::channel::<RpcNotification>(256);
        let notif_tx_for_reader = notif_tx.clone();
        let (inbound_tx, _) =
            broadcast::channel::<RpcInboundRequest>(INBOUND_REQUEST_CHANNEL_CAPACITY);
        let inbound_tx_for_reader = inbound_tx.clone();

        let conn_state = Arc::new(Mutex::new(ConnectionState::Connected));
        let conn_state_for_reader = conn_state.clone();

        let rpc_for_reader = rpc.clone();
        let read_task = tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            let mut buf = String::new();
            loop {
                buf.clear();
                match reader.read_line(&mut buf).await {
                    Ok(0) => {
                        *conn_state_for_reader.lock().unwrap() = ConnectionState::Disconnected {
                            reason: "EOF (daemon closed connection)".to_string(),
                        };
                        break;
                    }
                    Err(e) => {
                        *conn_state_for_reader.lock().unwrap() = ConnectionState::Disconnected {
                            reason: e.to_string(),
                        };
                        break;
                    }
                    Ok(_) => {}
                }
                let frame: Value = match serde_json::from_str(buf.trim()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                route_inbound_frame(
                    &rpc_for_reader,
                    &notif_tx_for_reader,
                    &inbound_tx_for_reader,
                    frame,
                );
            }
        });

        let mut init_params = serde_json::json!({
            "protocol_version": jsonrpc::ACP_PROTOCOL_VERSION,
            // Advertise the ACP `elicitation` capability (form mode) so the
            // daemon's per-session `RpcApprovalChannel` routes `request_choice`
            // / `request_multi_choice` over `elicitation/create` instead of
            // silently returning `Ok(None)`. The Code tab handles inbound
            // `elicitation/create` requests via `route_inbound_frame` →
            // the chat widget's pending-elicitation modal.
            "clientCapabilities": {
                "elicitation": { "form": {} }
            }
        });
        if let Some(id) = prev_tui_id {
            init_params["tui_id"] = serde_json::Value::String(id.to_string());
        }
        if let Some(sig) = prev_tui_sig {
            init_params["tui_sig"] = serde_json::Value::String(sig.to_string());
        }
        // Forward the TUI's full shell environment to the daemon so that
        // subprocesses spawned by agents inherit the user's real env
        // (PATH, SSH_AUTH_SOCK, credential helpers, etc.).  This is safe
        // on a local Unix-socket connection because the daemon is on the
        // same machine and the socket paths / env values are meaningful.
        let env_map: std::collections::HashMap<String, String> = std::env::vars().collect();
        init_params["env"] = serde_json::to_value(env_map).unwrap_or_default();
        let resp = match request_initialize(&rpc, init_params, INITIALIZE_TIMEOUT).await {
            Ok(resp) => resp,
            Err(e) => {
                read_task.abort();
                return Err(e);
            }
        };

        let init = match parse_initialize_response(&resp) {
            Ok(init) => init,
            Err(e) => {
                read_task.abort();
                return Err(e);
            }
        };

        let bcast_rx = notif_tx.subscribe();
        let (update_tx, _update_rx) = mpsc::channel::<SessionUpdate>(64);
        let router_task = spawn_notification_router(bcast_rx, update_tx);

        Ok(Self {
            rpc,
            _read_task: read_task,
            _router_task: router_task,
            server_version: init.server_version,
            notifications_bcast: notif_tx,
            inbound_requests_bcast: inbound_tx,
            connection_state: conn_state,
            tui_id: init.tui_id,
            tui_sig: init.tui_sig,
            transport: Transport::Local,
        })
    }

    /// Connect to the daemon via WebSocket Secure (WSS).
    ///
    /// Same handshake and reconnect semantics as [`Self::connect`] — pass
    /// previous `tui_id`/`tui_sig` to reclaim identity on reconnect.
    ///
    /// When `tls_skip_verify` is true, certificate verification is
    /// disabled — required for self-signed certs on remote hosts.
    pub async fn connect_wss(
        url: &str,
        prev_tui_id: Option<&str>,
        prev_tui_sig: Option<&str>,
        tls_skip_verify: bool,
    ) -> Result<Self> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let connector = if tls_skip_verify {
            Some(tokio_tungstenite::Connector::Rustls(
                Self::insecure_tls_config(),
            ))
        } else {
            None
        };

        let (ws_stream, _response) =
            tokio_tungstenite::connect_async_tls_with_config(url, None, false, connector)
                .await
                .with_context(|| format!("WSS connect to {url}"))?;

        let (mut sink, mut stream) = ws_stream.split();

        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(64);
        tokio::spawn(async move {
            while let Some(line) = writer_rx.recv().await {
                if sink.send(Message::Text(line.into())).await.is_err() {
                    break;
                }
            }
        });

        let rpc = Arc::new(jsonrpc::RpcOutbound::new(writer_tx));
        let (notif_tx, _) = broadcast::channel::<RpcNotification>(256);
        let notif_tx_for_reader = notif_tx.clone();
        let (inbound_tx, _) =
            broadcast::channel::<RpcInboundRequest>(INBOUND_REQUEST_CHANNEL_CAPACITY);
        let inbound_tx_for_reader = inbound_tx.clone();

        let conn_state = Arc::new(Mutex::new(ConnectionState::Connected));
        let conn_state_for_reader = conn_state.clone();

        let rpc_for_reader = rpc.clone();
        let read_task = tokio::spawn(async move {
            loop {
                match stream.next().await {
                    Some(Ok(Message::Text(text))) => {
                        let frame: Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        route_inbound_frame(
                            &rpc_for_reader,
                            &notif_tx_for_reader,
                            &inbound_tx_for_reader,
                            frame,
                        );
                    }
                    Some(Ok(Message::Close(frame))) => {
                        let reason = frame
                            .map(|f| f.reason.to_string())
                            .unwrap_or_else(|| "server closed connection".to_string());
                        *conn_state_for_reader.lock().unwrap() =
                            ConnectionState::Disconnected { reason };
                        break;
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
                    Some(Ok(Message::Binary(_))) => continue,
                    Some(Err(e)) => {
                        *conn_state_for_reader.lock().unwrap() = ConnectionState::Disconnected {
                            reason: e.to_string(),
                        };
                        break;
                    }
                    None => {
                        *conn_state_for_reader.lock().unwrap() = ConnectionState::Disconnected {
                            reason: "EOF (WSS connection closed)".to_string(),
                        };
                        break;
                    }
                }
            }
        });

        // Initialize handshake — identical to Unix socket path.
        let mut init_params = serde_json::json!({
            "protocol_version": jsonrpc::ACP_PROTOCOL_VERSION,
            // Advertise ACP elicitation form-mode support. See
            // `connect` above for the rationale.
            "clientCapabilities": {
                "elicitation": { "form": {} }
            }
        });
        if let Some(id) = prev_tui_id {
            init_params["tui_id"] = serde_json::Value::String(id.to_string());
        }
        if let Some(sig) = prev_tui_sig {
            init_params["tui_sig"] = serde_json::Value::String(sig.to_string());
        }
        // NOTE: We intentionally do NOT forward the TUI's environment here.
        // In a WSS connection the daemon is on a remote machine, so env values
        // like SSH_AUTH_SOCK, VIRTUAL_ENV, or any path-based socket/credential
        // would refer to paths that don't exist on the remote host.  Forwarding
        // them would be misleading at best and silently broken at worst.
        // Env pass-through is only meaningful on a local Unix-socket connection
        // (see `connect` above), where the TUI and daemon share the same filesystem.
        let resp = match request_initialize(&rpc, init_params, INITIALIZE_TIMEOUT).await {
            Ok(resp) => resp,
            Err(e) => {
                read_task.abort();
                return Err(e);
            }
        };

        let init = match parse_initialize_response(&resp) {
            Ok(init) => init,
            Err(e) => {
                read_task.abort();
                return Err(e);
            }
        };

        let bcast_rx = notif_tx.subscribe();
        let (update_tx, _update_rx) = mpsc::channel::<SessionUpdate>(64);
        let router_task = spawn_notification_router(bcast_rx, update_tx);

        Ok(Self {
            rpc,
            _read_task: read_task,
            _router_task: router_task,
            server_version: init.server_version,
            notifications_bcast: notif_tx,
            inbound_requests_bcast: inbound_tx,
            connection_state: conn_state,
            tui_id: init.tui_id,
            tui_sig: init.tui_sig,
            transport: Transport::Wss,
        })
    }

    /// Build a rustls `ClientConfig` that accepts any server certificate.
    fn insecure_tls_config() -> std::sync::Arc<rustls::ClientConfig> {
        use std::sync::Arc;

        /// Verifier that accepts every certificate without checking.
        #[derive(Debug)]
        struct NoVerify;

        impl rustls::client::danger::ServerCertVerifier for NoVerify {
            fn verify_server_cert(
                &self,
                _end_entity: &rustls::pki_types::CertificateDer<'_>,
                _intermediates: &[rustls::pki_types::CertificateDer<'_>],
                _server_name: &rustls::pki_types::ServerName<'_>,
                _ocsp_response: &[u8],
                _now: rustls::pki_types::UnixTime,
            ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
                Ok(rustls::client::danger::ServerCertVerified::assertion())
            }

            fn verify_tls12_signature(
                &self,
                _message: &[u8],
                _cert: &rustls::pki_types::CertificateDer<'_>,
                _dss: &rustls::DigitallySignedStruct,
            ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
            {
                Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
            }

            fn verify_tls13_signature(
                &self,
                _message: &[u8],
                _cert: &rustls::pki_types::CertificateDer<'_>,
                _dss: &rustls::DigitallySignedStruct,
            ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
            {
                Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
            }

            fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
                rustls::crypto::ring::default_provider()
                    .signature_verification_algorithms
                    .supported_schemes()
            }
        }

        let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("ring provider supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify))
        .with_no_client_auth();

        Arc::new(config)
    }

    pub async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        self.call_with_timeout(method, params, std::time::Duration::from_secs(5))
            .await
    }

    pub async fn call_with_timeout<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
        timeout: std::time::Duration,
    ) -> Result<T> {
        // Timeout prevents indefinite hangs when the daemon dies between
        // the connection-state check and the actual RPC send/recv.
        let result = tokio::time::timeout(timeout, self.rpc.request(method, params))
            .await
            .map_err(|_| {
                anyhow::Error::msg(format!(
                    "RPC {method}: timed out after {}s",
                    timeout.as_secs()
                ))
            })?
            .map_err(|e| anyhow::Error::msg(format!("RPC {method}: {} ({})", e.message, e.code)))?;
        serde_json::from_value(result).with_context(|| format!("deserializing {method} result"))
    }

    // ── Connection state ─────────────────────────────────────────

    /// Current connection state. Cheap mutex read, safe to call on every frame.
    pub fn connection_state(&self) -> ConnectionState {
        self.connection_state.lock().unwrap().clone()
    }

    // ── Notifications ─────────────────────────────────────────────

    /// Get a receiver for server-initiated notifications.
    pub fn subscribe_notifications(&self) -> broadcast::Receiver<RpcNotification> {
        self.notifications_bcast.subscribe()
    }

    /// Get a receiver for server-initiated JSON-RPC requests that
    /// expect a response (today: `elicitation/create`). The Chat
    /// widget subscribes per Code tab, filters by `params.sessionId`,
    /// surfaces a modal, and answers via [`Self::respond_to_inbound_request`].
    pub fn subscribe_inbound_requests(&self) -> broadcast::Receiver<RpcInboundRequest> {
        self.inbound_requests_bcast.subscribe()
    }

    /// Send a JSON-RPC response back to the daemon for a previously
    /// received server-initiated request. The `id` must be the same
    /// `Value` carried by the originating `RpcInboundRequest`.
    pub async fn respond_to_inbound_request(
        &self,
        id: Value,
        result: std::result::Result<Value, JsonRpcError>,
    ) -> Result<()> {
        let sent = self.rpc.respond(id, result).await;
        if !sent {
            anyhow::bail!("writer task closed before response could be sent");
        }
        Ok(())
    }

    /// Ask the daemon to start streaming log events as notifications.
    pub async fn logs_subscribe(&self) -> Result<()> {
        let _: Value = self.call("logs/subscribe", serde_json::json!({})).await?;
        Ok(())
    }

    /// Query persisted log events from the daemon.
    pub async fn logs_query(&self, params: LogsQueryParams) -> Result<LogsQueryResult> {
        self.call("logs/query", serde_json::to_value(params)?).await
    }

    /// `logs/get { id }` — fetch one event's full payload. The Logs
    /// pane keeps only preview data in memory and lazy-fetches the
    /// full event when the detail pane opens; on close the detail is
    /// dropped back to `None`.
    pub async fn logs_get(&self, id: &str) -> Result<LogsGetResult> {
        self.call("logs/get", serde_json::json!({ "id": id })).await
    }

    // ── Typed config helpers ─────────────────────────────────────

    pub async fn config_list(&self, prefix: Option<&str>) -> Result<Vec<ConfigFieldEntry>> {
        let result: ConfigListResult = self
            .call(method::CONFIG_LIST, serde_json::json!({ "prefix": prefix }))
            .await?;
        Ok(result.entries)
    }

    pub async fn config_set(&self, prop: &str, value: Value) -> Result<()> {
        let _: ConfigSetResult = self
            .call(
                method::CONFIG_SET,
                serde_json::json!({ "prop": prop, "value": value }),
            )
            .await?;
        Ok(())
    }

    pub async fn config_delete(&self, prop: &str) -> Result<()> {
        let _: ConfigDeleteResult = self
            .call(method::CONFIG_DELETE, serde_json::json!({ "prop": prop }))
            .await?;
        Ok(())
    }

    /// Signal the daemon to reload in place. Mirrors `POST /admin/reload`.
    pub async fn config_reload(&self) -> Result<ConfigReloadResult> {
        self.call(method::CONFIG_RELOAD, serde_json::json!({}))
            .await
    }

    /// List the build's available locales (embedded `locales.toml` registry).
    pub async fn locales_list(&self) -> Result<Vec<LocaleOption>> {
        let r: LocalesListResult = self
            .call(method::LOCALES_LIST, serde_json::json!({}))
            .await?;
        Ok(r.locales)
    }

    /// Fetch translated FTL catalogue bytes for `locale` from upstream. The
    /// daemon validates the locale/catalog and returns file contents; the
    /// caller writes them locally.
    pub async fn locales_fetch(
        &self,
        locale: &str,
        catalog: &[String],
    ) -> Result<LocalesFetchResult> {
        self.call(
            method::LOCALES_FETCH,
            serde_json::json!({ "locale": locale, "catalog": catalog }),
        )
        .await
    }

    pub async fn config_sections(&self) -> Result<Vec<ConfigSectionEntry>> {
        let result: ConfigSectionsResult = self
            .call(method::CONFIG_SECTIONS, serde_json::json!({}))
            .await?;
        Ok(result.sections)
    }

    pub async fn config_map_keys(&self, path: &str) -> Result<Vec<String>> {
        let result: ConfigMapKeysResult = self
            .call(method::CONFIG_MAP_KEYS, serde_json::json!({ "path": path }))
            .await?;
        Ok(result.keys)
    }

    pub async fn config_resolve_alias_source(
        &self,
        source: crate::wire::AliasSource,
    ) -> Result<Vec<String>> {
        let result: ConfigResolveAliasSourceResult = self
            .call(
                method::CONFIG_RESOLVE_ALIAS_SOURCE,
                serde_json::json!({ "source": source }),
            )
            .await?;
        Ok(result.values)
    }

    pub async fn config_map_key_create(&self, path: &str, key: &str) -> Result<()> {
        let _: Value = self
            .call(
                method::CONFIG_MAP_KEY_CREATE,
                serde_json::json!({ "path": path, "key": key }),
            )
            .await?;
        Ok(())
    }

    pub async fn config_map_key_delete(&self, path: &str, key: &str) -> Result<()> {
        let _: Value = self
            .call(
                method::CONFIG_MAP_KEY_DELETE,
                serde_json::json!({ "path": path, "key": key }),
            )
            .await?;
        Ok(())
    }

    pub async fn config_map_key_rename(
        &self,
        path: &str,
        from: &str,
        to: &str,
    ) -> Result<ConfigRenameMapKeyResult> {
        self.call_with_timeout(
            method::CONFIG_RENAME_MAP_KEY,
            serde_json::json!({ "path": path, "from": from, "to": to }),
            CONFIG_RENAME_TIMEOUT,
        )
        .await
    }

    pub async fn config_templates(&self) -> Result<Vec<ConfigTemplateEntry>> {
        let result: ConfigTemplatesResult = self
            .call(method::CONFIG_TEMPLATES, serde_json::json!({}))
            .await?;
        Ok(result.templates)
    }

    pub async fn catalog_models(&self, provider: &str) -> Result<CatalogModelsResult> {
        self.call_with_timeout(
            method::CONFIG_CATALOG_MODELS,
            serde_json::json!({ "model_provider": provider }),
            std::time::Duration::from_secs(20),
        )
        .await
    }

    // ── Personality helpers ──────────────────────────────────────

    pub async fn personality_list(&self, agent: Option<&str>) -> Result<PersonalityListResult> {
        self.call(
            method::PERSONALITY_LIST,
            serde_json::json!({ "agent": agent }),
        )
        .await
    }

    pub async fn personality_get(
        &self,
        agent: &str,
        filename: &str,
    ) -> Result<PersonalityGetResult> {
        self.call(
            method::PERSONALITY_GET,
            serde_json::json!({ "agent": agent, "filename": filename }),
        )
        .await
    }

    pub async fn personality_put(
        &self,
        agent: &str,
        filename: &str,
        content: &str,
    ) -> Result<PersonalityPutResult> {
        self.call(
            method::PERSONALITY_PUT,
            serde_json::json!({ "agent": agent, "filename": filename, "content": content }),
        )
        .await
    }

    pub async fn personality_templates(
        &self,
        agent: Option<&str>,
    ) -> Result<PersonalityTemplatesResult> {
        self.call(
            method::PERSONALITY_TEMPLATES,
            serde_json::json!({ "agent": agent }),
        )
        .await
    }

    // ── Skills helpers ───────────────────────────────────────────

    pub async fn skills_list(&self, bundle: Option<&str>) -> Result<SkillsListResult> {
        self.call(method::SKILLS_LIST, serde_json::json!({ "bundle": bundle }))
            .await
    }

    pub async fn skills_read(&self, bundle: &str, name: &str) -> Result<SkillsReadResult> {
        self.call(
            method::SKILLS_READ,
            serde_json::json!({ "bundle": bundle, "name": name }),
        )
        .await
    }

    pub async fn skills_write(
        &self,
        bundle: &str,
        name: &str,
        frontmatter: &SkillFrontmatter,
        body: &str,
    ) -> Result<SkillsWriteResult> {
        self.call(
            method::SKILLS_WRITE,
            serde_json::json!({
                "bundle": bundle,
                "name": name,
                "frontmatter": frontmatter,
                "body": body,
            }),
        )
        .await
    }

    pub async fn skills_delete(&self, bundle: &str, name: &str) -> Result<SkillsDeleteResult> {
        self.call(
            method::SKILLS_DELETE,
            serde_json::json!({ "bundle": bundle, "name": name }),
        )
        .await
    }

    // ── Quickstart methods ───────────────────────────────────────
    //
    // Thin RPC mirror of the gateway's `/api/quickstart/*` HTTP routes.
    // Same shapes both ways; the daemon-side handlers live in
    // `zeroclaw_runtime::rpc::dispatch` and call into
    // `zeroclaw_runtime::quickstart::{validate_only,apply}_with_surface`.

    pub async fn quickstart_state(&self) -> Result<QuickstartStateResult> {
        self.call(method::QUICKSTART_STATE, serde_json::json!({}))
            .await
    }

    pub async fn quickstart_fields(
        &self,
        section: QuickstartFieldSection,
        type_key: &str,
    ) -> Result<QuickstartFieldsResult> {
        self.call(
            method::QUICKSTART_FIELDS,
            serde_json::json!({ "section": section, "type_key": type_key }),
        )
        .await
    }

    pub async fn quickstart_validate(
        &self,
        submission: &crate::wire::BuilderSubmission,
    ) -> Result<QuickstartValidateResult> {
        self.call(
            method::QUICKSTART_VALIDATE,
            serde_json::json!({ "submission": submission }),
        )
        .await
    }

    pub async fn quickstart_apply(
        &self,
        submission: &crate::wire::BuilderSubmission,
    ) -> Result<QuickstartApplyResult> {
        self.call(
            method::QUICKSTART_APPLY,
            serde_json::json!({ "submission": submission }),
        )
        .await
    }

    pub async fn quickstart_dismiss(
        &self,
        run_id: &str,
        surface: QuickstartSurface,
        last_step: Option<QuickstartStep>,
    ) -> Result<QuickstartDismissResult> {
        self.call(
            method::QUICKSTART_DISMISS,
            serde_json::json!({
                "run_id": run_id,
                "surface": surface,
                "last_step": last_step,
            }),
        )
        .await
    }

    pub async fn sops_list(&self) -> Result<Value> {
        self.call(method::SOPS_LIST, serde_json::json!({})).await
    }

    pub async fn sops_get(&self, name: &str) -> Result<Value> {
        self.call(method::SOPS_GET, serde_json::json!({ "name": name }))
            .await
    }

    pub async fn sops_graph(&self, name: &str) -> Result<Value> {
        self.call(method::SOPS_GRAPH, serde_json::json!({ "name": name }))
            .await
    }

    pub async fn sops_graph_view(&self, name: &str) -> Result<SopGraphView> {
        let value = self.sops_graph(name).await?;
        serde_json::from_value(value).map_err(Into::into)
    }

    pub async fn sops_run_overlay(&self, name: &str, run_id: &str) -> Result<Value> {
        self.call(
            method::SOPS_RUN_OVERLAY,
            serde_json::json!({ "name": name, "run_id": run_id }),
        )
        .await
    }

    /// Fire a Manual run for `name` with an optional JSON-string payload and
    /// return its run id. Mirrors the web `runSop` path; the daemon builds the
    /// Manual `SopEvent` and requires a matching manual trigger.
    pub async fn sops_run(&self, name: &str, payload: Option<&str>) -> Result<String> {
        let value: Value = self
            .call(
                method::SOPS_RUN,
                serde_json::json!({ "name": name, "payload": payload }),
            )
            .await?;
        value
            .get("run_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow::Error::msg("sops/run: response missing run_id"))
    }

    pub async fn sops_save(&self, sop: Value) -> Result<Value> {
        self.call(method::SOPS_SAVE, serde_json::json!({ "sop": sop }))
            .await
    }

    pub async fn sops_create(&self, sop: Value) -> Result<Value> {
        self.call(method::SOPS_CREATE, serde_json::json!({ "sop": sop }))
            .await
    }

    pub async fn sops_delete(&self, name: &str) -> Result<Value> {
        self.call(method::SOPS_DELETE, serde_json::json!({ "name": name }))
            .await
    }

    /// Resolve a paused checkpoint on a live run. `decision` is the raw
    /// `ApprovalDecision` wire value (`"approve"` or `{"deny": {"reason": ..}}`);
    /// the daemon deserializes it into the canonical enum. Returns the refreshed
    /// run overlay so the surface re-renders the post-decision state.
    pub async fn sops_decide(&self, name: &str, run_id: &str, decision: Value) -> Result<Value> {
        self.call(
            method::SOPS_DECIDE,
            serde_json::json!({ "name": name, "run_id": run_id, "decision": decision }),
        )
        .await
    }

    pub async fn sops_wire_draft(&self, sop: Value, edit: Value) -> Result<Value> {
        self.call(
            method::SOPS_WIRE_DRAFT,
            serde_json::json!({ "sop": sop, "edit": edit }),
        )
        .await
    }

    pub async fn sops_graph_draft(&self, sop: Value) -> Result<SopGraphView> {
        let value = self
            .call(method::SOPS_GRAPH_DRAFT, serde_json::json!({ "sop": sop }))
            .await?;
        serde_json::from_value(value).map_err(Into::into)
    }

    pub async fn sops_trigger_sources(&self) -> Result<TriggerSourceRegistryView> {
        let value = self
            .call(method::SOPS_TRIGGER_SOURCES, serde_json::json!({}))
            .await?;
        serde_json::from_value(value).map_err(Into::into)
    }

    // ── Session methods ──────────────────────────────────────────

    pub async fn session_new(
        &self,
        agent_alias: &str,
        cwd: Option<&str>,
    ) -> Result<SessionNewResult> {
        self.session_new_with_id(agent_alias, cwd, None).await
    }

    /// Like [`Self::session_new_with_id`] but sets `exclude_memory: true` so the
    /// daemon strips memory tools and uses a NoneMemory backend. Used by the
    /// ACP pane, which should never have access to persistent memory.
    pub async fn session_new_acp(
        &self,
        agent_alias: &str,
        cwd: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<SessionNewResult> {
        let tui_id = self.tui_id.as_deref();
        self.call(
            method::SESSION_NEW,
            serde_json::json!({
                "agent_alias": agent_alias,
                "cwd": cwd,
                "session_id": session_id,
                "tui_id": tui_id,
                "exclude_memory": true,
                "chat_mode": "acp",
            }),
        )
        .await
    }

    /// Create or rehydrate a session. When `session_id` is `Some`, the daemon
    /// creates the session with that ID, restoring persisted history if it
    /// exists — effectively "attaching" to a prior session.
    pub async fn session_new_with_id(
        &self,
        agent_alias: &str,
        cwd: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<SessionNewResult> {
        let tui_id = self.tui_id.as_deref();
        self.call(
            method::SESSION_NEW,
            serde_json::json!({ "agent_alias": agent_alias, "cwd": cwd, "session_id": session_id, "tui_id": tui_id }),
        )
        .await
    }

    pub async fn session_cancel(&self, session_id: &str) -> Result<SessionCancelResult> {
        self.call(
            method::SESSION_CANCEL,
            serde_json::json!({ "session_id": session_id }),
        )
        .await
    }

    /// Apply session-scoped overrides (model, model_provider, temperature) to a
    /// live session. The daemon applies them immediately and returns the merged
    /// set. A `model_provider` override triggers a live provider-box rebuild
    /// daemon-side.
    pub async fn session_configure(
        &self,
        session_id: &str,
        overrides: SessionOverrides,
    ) -> Result<SessionConfigureResult> {
        self.call(
            method::SESSION_CONFIGURE,
            serde_json::json!({ "session_id": session_id, "overrides": overrides }),
        )
        .await
    }

    pub async fn session_git_branch(&self, session_id: &str) -> Result<SessionGitBranchResult> {
        self.call(
            method::SESSION_GIT_BRANCH,
            serde_json::json!({ "session_id": session_id }),
        )
        .await
    }

    pub async fn session_approve(
        &self,
        session_id: &str,
        request_id: &str,
        decision: ApprovalDecision,
    ) -> Result<SessionApproveResult> {
        let mut params = serde_json::json!({
            "session_id": session_id,
            "request_id": request_id,
            "decision": decision.kind(),
        });
        if let ApprovalDecision::RejectWithEdit { ref replacement } = decision {
            params["replacement"] = serde_json::Value::String(replacement.clone());
        }
        self.call(method::SESSION_APPROVE, params).await
    }

    pub async fn session_close(&self, session_id: &str) -> Result<Value> {
        self.call(
            method::SESSION_CLOSE,
            serde_json::json!({ "session_id": session_id }),
        )
        .await
    }

    pub async fn session_kill(&self, session_id: &str) -> Result<()> {
        let _: serde_json::Value = self
            .call(
                method::SESSION_KILL,
                serde_json::json!({ "session_id": session_id }),
            )
            .await?;
        Ok(())
    }

    // ── Dashboard helpers ────────────────────────────────────────

    pub async fn status(&self) -> Result<StatusResult> {
        self.call(method::STATUS, serde_json::json!({})).await
    }

    pub async fn health(&self) -> Result<Value> {
        self.call(method::HEALTH, serde_json::json!({})).await
    }

    pub async fn doctor_run(&self) -> Result<DoctorRunResult> {
        self.call(method::DOCTOR_RUN, serde_json::json!({})).await
    }

    pub async fn cost_query(&self, agent: Option<&str>) -> Result<CostSummaryResult> {
        self.call(method::COST_QUERY, serde_json::json!({ "agent": agent }))
            .await
    }

    /// Optional organization-level billed-cost snapshot from the daemon's
    /// `<data_dir>/org_cost.json`. Returns `None` when the file is absent (a
    /// vanilla build never writes it), so the dashboard simply omits the
    /// organization row. An integrator can populate it via an external sync.
    pub async fn cost_org(&self) -> Result<Option<OrgCost>> {
        let v: serde_json::Value = self.call(method::COST_ORG, serde_json::json!({})).await?;
        if v.is_null() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_value(v)?))
    }

    /// Cost summary scoped to a `[from, to)` window (RFC3339). The daemon rolls
    /// up only records in the window, so `session_cost_usd` / `total_tokens` /
    /// `by_model` reflect that period — used by the Cost tab's day/month/
    /// quarter/YTD breakdown.
    pub async fn cost_query_window(
        &self,
        from: &str,
        to: &str,
        agent: Option<&str>,
    ) -> Result<CostSummaryResult> {
        self.call(
            method::COST_QUERY,
            serde_json::json!({ "from": from, "to": to, "agent": agent }),
        )
        .await
    }

    pub async fn session_list(&self, query: Option<&str>) -> Result<SessionListResult> {
        self.call(method::SESSION_LIST, serde_json::json!({ "query": query }))
            .await
    }

    /// List ACP sessions from the dedicated ACP session store. The Code (ACP)
    /// pane's picker uses this so its list only contains ACP-origin sessions
    /// — chat sessions live in a separate backend and must not show up here.
    pub async fn acp_session_list(&self) -> Result<SessionListResult> {
        self.call(method::SESSION_LIST_ACP, serde_json::json!({}))
            .await
    }

    pub async fn agents_status(&self) -> Result<AgentsStatusResult> {
        self.call(method::AGENTS_STATUS, serde_json::json!({}))
            .await
    }

    pub async fn cron_list(&self) -> Result<CronListResult> {
        self.call(method::CRON_LIST, serde_json::json!({})).await
    }

    pub async fn cron_runs(&self, id: &str, limit: Option<u32>) -> Result<CronRunsResult> {
        self.call(
            method::CRON_RUNS,
            serde_json::json!({ "id": id, "limit": limit }),
        )
        .await
    }

    pub async fn cron_trigger(&self, id: &str) -> Result<CronTriggerResult> {
        self.call_with_timeout(
            method::CRON_TRIGGER,
            serde_json::json!({ "id": id }),
            CRON_TRIGGER_TIMEOUT,
        )
        .await
    }

    pub async fn memory_list(&self, category: Option<&str>) -> Result<MemoryListResult> {
        self.call(
            method::MEMORY_LIST,
            serde_json::json!({ "category": category }),
        )
        .await
    }

    pub async fn memory_search(&self, query: &str, limit: usize) -> Result<MemorySearchResult> {
        self.call(
            method::MEMORY_SEARCH,
            serde_json::json!({ "query": query, "limit": limit }),
        )
        .await
    }

    /// `memory/get { key }` — fetch one memory entry's full content.
    /// The Memory pane keeps only preview rows in memory and
    /// lazy-fetches the full entry when the detail pane opens.
    pub async fn memory_get(&self, key: &str) -> Result<MemoryGetResult> {
        self.call("memory/get", serde_json::json!({ "key": key }))
            .await
    }

    pub async fn session_messages(&self, session_id: &str) -> Result<SessionMessagesResult> {
        self.call(
            method::SESSION_MESSAGES,
            serde_json::json!({ "session_id": session_id }),
        )
        .await
    }

    /// Paginated variant of `session_messages`. `limit` caps the page
    /// size, `before_index` paginates older slices. Returns
    /// `(messages, total, start)` so the Sessions pane can size
    /// scroll affordances and render "X of Y" without holding the
    /// full history in memory.
    pub async fn session_messages_page(
        &self,
        session_id: &str,
        limit: Option<usize>,
        before_index: Option<usize>,
    ) -> Result<SessionMessagesResult> {
        let mut params = serde_json::json!({ "session_id": session_id });
        if let Some(l) = limit {
            params["limit"] = serde_json::json!(l);
        }
        if let Some(b) = before_index {
            params["before_index"] = serde_json::json!(b);
        }
        self.call(method::SESSION_MESSAGES, params).await
    }

    // ── TUI identity helpers ─────────────────────────────────────

    /// The TUI session UID assigned by the daemon, if connected.
    pub fn tui_id(&self) -> Option<&str> {
        self.tui_id.as_deref()
    }

    /// The HMAC signature for the TUI session UID.
    pub fn tui_sig(&self) -> Option<&str> {
        self.tui_sig.as_deref()
    }

    /// List all connected TUI sessions from the daemon registry.
    pub async fn tui_list(&self) -> Result<TuiListResult> {
        self.call(method::TUI_LIST, serde_json::json!({})).await
    }

    /// List directory contents on the remote daemon (WSS only).
    /// Returns the structured response from `fs/list_dir`.
    pub async fn fs_list_dir(
        &self,
        path: &std::path::Path,
        show_hidden: bool,
    ) -> Result<FsListDirResponse> {
        self.call(
            method::FS_LIST_DIR,
            serde_json::json!({
                "path": path.to_string_lossy(),
                "show_hidden": show_hidden,
            }),
        )
        .await
    }

    // ── Test-only constructors ────────────────────────────────────

    /// Test-only constructor that skips the Unix socket connect + initialize handshake.
    #[cfg(test)]
    pub fn with_rpc(outbound: Arc<RpcOutbound>) -> Self {
        let (notif_tx, _) = tokio::sync::broadcast::channel(1);
        let (inbound_tx, _) = tokio::sync::broadcast::channel(1);
        Self {
            rpc: outbound,
            _read_task: tokio::spawn(async {}),
            _router_task: tokio::spawn(async {}),
            server_version: "test".to_string(),
            notifications_bcast: notif_tx,
            inbound_requests_bcast: inbound_tx,
            connection_state: Arc::new(Mutex::new(ConnectionState::Connected)),
            tui_id: None,
            tui_sig: None,
            transport: Transport::Local,
        }
    }

    /// Transport protocol of this connection.
    pub fn transport(&self) -> Transport {
        self.transport
    }
}

// ── Response types (client-side, minimal) ────────────────────────

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigListResult {
    pub entries: Vec<ConfigFieldEntry>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigSetResult {}

#[cfg(test)]
mod initialize_version_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn initialize_response_accepts_matching_server_version() {
        let parsed = parse_initialize_response(&json!({
            "server_version": env!("CARGO_PKG_VERSION"),
            "tui_id": "tui_1",
            "tui_sig": "sig_1"
        }))
        .unwrap();

        assert_eq!(parsed.server_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(parsed.tui_id.as_deref(), Some("tui_1"));
        assert_eq!(parsed.tui_sig.as_deref(), Some("sig_1"));
    }

    #[test]
    fn initialize_response_rejects_mismatched_server_version() {
        let err = parse_initialize_response(&json!({
            "server_version": "0.0.0-test"
        }))
        .unwrap_err();
        let mismatch = err
            .downcast_ref::<DaemonVersionMismatch>()
            .expect("mismatched daemon version should be typed");

        assert_eq!(mismatch.client_version(), env!("CARGO_PKG_VERSION"));
        assert_eq!(mismatch.server_version(), "0.0.0-test");
        assert!(err.to_string().contains("Version mismatch"));
    }

    #[test]
    fn initialize_response_rejects_missing_server_version_as_unknown() {
        let err = parse_initialize_response(&json!({})).unwrap_err();
        let mismatch = err
            .downcast_ref::<DaemonVersionMismatch>()
            .expect("missing daemon version should be typed");

        assert_eq!(mismatch.client_version(), env!("CARGO_PKG_VERSION"));
        assert_eq!(mismatch.server_version(), "unknown");
    }
}

#[cfg(test)]
mod initialize_timeout_tests {
    use super::*;

    #[tokio::test]
    async fn initialize_request_times_out_when_transport_never_responds() {
        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(1);
        let rpc = RpcOutbound::new(writer_tx);
        let receiver = tokio::spawn(async move {
            writer_rx.recv().await.expect("initialize request");
            std::future::pending::<()>().await;
        });

        let err = request_initialize(&rpc, serde_json::json!({}), Duration::from_millis(20))
            .await
            .unwrap_err();

        assert!(err.downcast_ref::<DaemonInitializeTimeout>().is_some());
        receiver.abort();
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigDeleteResult {}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigReloadResult {
    #[allow(dead_code)]
    pub reloading: bool,
}

/// One selectable locale (`locales/list`).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct LocaleOption {
    pub code: String,
    pub label: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct LocalesListResult {
    pub locales: Vec<LocaleOption>,
}

/// One fetched catalogue's bytes (`locales/fetch`).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct FetchedCatalog {
    pub name: String,
    pub filename: String,
    pub content: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct LocalesFetchResult {
    #[allow(dead_code)]
    pub locale: String,
    pub catalogs: Vec<FetchedCatalog>,
    pub skipped: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigMapKeysResult {
    pub keys: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigRenameMapKeyResult {
    pub renamed: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigResolveAliasSourceResult {
    pub values: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigSectionsResult {
    pub sections: Vec<ConfigSectionEntry>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigSectionEntry {
    pub key: String,
    pub label: String,
    pub help: String,
    pub completed: bool,
    /// Display group label (`"Foundation"`, `"Tools"`, …) from
    /// `zeroclaw_config::sections::SectionGroup::label()`. Empty when
    /// the daemon predates group plumbing — the sections pane falls
    /// back to the flat ungrouped list.
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub shape: Option<SectionShape>,
    #[serde(default)]
    pub cost_category: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigTemplatesResult {
    pub templates: Vec<ConfigTemplateEntry>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CatalogModelsResult {
    pub models: Vec<String>,
    /// Pricing keyed by upstream model id, when the provider's catalog
    /// returns it. Mirrors the gateway `/api/config/catalog/models` payload
    /// (same RPC) so the Costs tab can pre-fill rate sheets.
    #[serde(default)]
    pub pricing: Option<std::collections::HashMap<String, CatalogModelPricing>>,
    #[serde(default)]
    pub live: bool,
}

/// Per-token USD pricing strings as emitted by the catalog RPC. Field names
/// match `zeroclaw_api::model_provider::ModelPricing`; only the rates the
/// cost-rate sheet consumes are kept.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CatalogModelPricing {
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub completion: Option<String>,
    #[serde(default)]
    pub input_cache_read: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigTemplateEntry {
    pub path: String,
}

// ── Personality types ────────────────────────────────────────────

#[derive(Debug, Clone, serde::Deserialize)]
pub struct PersonalityFileEntry {
    pub filename: String,
    pub exists: bool,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, serde::Deserialize)]
pub struct PersonalityListResult {
    pub files: Vec<PersonalityFileEntry>,
    pub max_chars: usize,
}

#[derive(Debug, serde::Deserialize)]
pub struct PersonalityGetResult {
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct PersonalityPutResult {}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct TemplateFileEntry {
    pub filename: String,
    pub content: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct PersonalityTemplatesResult {
    pub files: Vec<TemplateFileEntry>,
}

// ── Skills types ─────────────────────────────────────────────────

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SkillListEntry {
    pub name: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct SkillsListResult {
    pub skills: Vec<SkillListEntry>,
}

#[derive(Debug, serde::Deserialize)]
pub struct SkillsReadResult {
    pub frontmatter: SkillFrontmatter,
    pub body: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct SkillsWriteResult {}

#[derive(Debug, serde::Deserialize)]
pub struct SkillsDeleteResult {}

// ── Quickstart types ─────────────────────────────────────────────
//
// **Mirror** of the wire shapes defined in
// `zeroclaw_runtime::rpc::types` (the daemon-side single source of
// truth, which itself mirrors the gateway's HTTP route shapes). The
// types live in `zeroclaw-runtime`, but that crate is not on the
// `apps/zerocode` dependency tree — pulling it in would compile the
// entire runtime into the TUI binary. Instead we duplicate the wire
// shape here; the integration drift test enforces equality across
// surfaces, so divergence is a CI failure rather than a silent bug.

/// Mirror of `zeroclaw_runtime::quickstart::Surface` (`snake_case` on the wire).
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuickstartSurface {
    Web,
    Tui,
    Cli,
    Test,
}

/// Mirror of `zeroclaw_runtime::quickstart::QuickstartStep`.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuickstartStep {
    ModelProvider,
    RiskProfile,
    RuntimeProfile,
    Memory,
    Channels,
    PeerGroups,
    Agent,
}

/// Mirror of `zeroclaw_runtime::quickstart::QuickstartError`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct QuickstartError {
    pub step: QuickstartStep,
    pub field: String,
    pub message: String,
}

/// Mirror of `zeroclaw_runtime::quickstart::AppliedAgent`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AppliedAgent {
    pub alias: String,
    pub model_provider: String,
    pub risk_profile: String,
    pub runtime_profile: String,
    pub channels: Vec<String>,
    pub memory_backend: String,
}

/// Mirror of `zeroclaw_runtime::quickstart::FieldSection`.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuickstartFieldSection {
    ModelProvider,
    Channel,
}

/// Mirror of `zeroclaw_config::traits::PropKind` (wire form).
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuickstartFieldKind {
    String,
    Bool,
    Integer,
    Float,
    Enum,
    StringArray,
    ObjectArray,
    Object,
}

/// Mirror of `zeroclaw_runtime::quickstart::FieldDescriptor`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct QuickstartFieldDescriptor {
    pub key: String,
    pub label: String,
    pub help: String,
    pub kind: QuickstartFieldKind,
    pub is_secret: bool,
    pub enum_variants: Option<Vec<String>>,
    pub required: bool,
    pub default: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct QuickstartFieldsResult {
    pub fields: Vec<QuickstartFieldDescriptor>,
}

/// Mirror of `zeroclaw_runtime::quickstart::QuickstartState`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct QuickstartStateResult {
    pub quickstart_completed: bool,
    pub agents: Vec<String>,
    pub risk_profiles: Vec<String>,
    pub runtime_profiles: Vec<String>,
    #[serde(default)]
    pub default_runtime_profile: Option<String>,
    pub model_providers: Vec<String>,
    pub channels: Vec<String>,
    /// Subset of `channels` not yet bound to any agent — safe to
    /// reuse without violating the one-channel-one-agent invariant.
    #[serde(default)]
    pub unassigned_channels: Vec<String>,
    pub storage: Vec<String>,
    /// Picker rows for "Create new model provider" — supplied by the
    /// daemon so the TUI never hardcodes the option list.
    #[serde(default)]
    pub model_provider_types: Vec<QuickstartTypeOption>,
    /// Picker rows for "Create new channel" — supplied by the
    /// daemon so the TUI never hardcodes the option list.
    #[serde(default)]
    pub channel_types: Vec<QuickstartTypeOption>,
    #[serde(default)]
    pub risk_presets: Vec<QuickstartPresetMirror>,
    #[serde(default)]
    pub runtime_presets: Vec<QuickstartPresetMirror>,
    #[serde(default)]
    pub memory_kinds: Vec<String>,
    #[serde(default)]
    pub personality_files: Vec<String>,
}

/// Mirror of `zeroclaw_config::presets::RiskPreset` / `RuntimePreset`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QuickstartPresetMirror {
    pub preset_name: String,
    pub label: String,
    pub help: String,
}

/// Mirror of `zeroclaw_runtime::rpc::types::QuickstartTypeOption`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct QuickstartTypeOption {
    pub kind: String,
    pub display_name: String,
    #[serde(default)]
    pub local: bool,
    #[serde(default)]
    pub default_runtime_profile: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuickstartValidateResult {
    Ok,
    Errors { errors: Vec<QuickstartError> },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuickstartApplyResult {
    Applied {
        agent: AppliedAgent,
        daemon_restarted: bool,
    },
    Errors {
        errors: Vec<QuickstartError>,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct QuickstartDismissResult {
    pub recorded: bool,
}

//

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SopStepKind {
    #[default]
    Execute,
    Checkpoint,
    Capability,
}

impl SopStepKind {
    pub const ALL: [SopStepKind; 3] = [
        SopStepKind::Execute,
        SopStepKind::Checkpoint,
        SopStepKind::Capability,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            SopStepKind::Execute => "execute",
            SopStepKind::Checkpoint => "checkpoint",
            SopStepKind::Capability => "capability",
        }
    }
}

// SOP graph wire types. zerocode is an RPC-only surface: it deserializes these
// off `sops/graph` rather than linking the backend crate that produces them.
// The shape here MUST match `zeroclaw-sop-graph`'s serde projection byte for
// byte (field names, snake_case renames, defaults) or RPC decoding drifts.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinClass {
    Flow,
    Data,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowRole {
    Sequence,
    Dependency,
    Failure,
    Switch,
    Trigger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    #[default]
    Step,
    Trigger,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GraphPin {
    pub class: PinClass,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GraphNode {
    pub step: u32,
    pub title: String,
    #[serde(default)]
    pub kind: NodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_index: Option<u32>,
    pub inputs: Vec<GraphPin>,
    pub outputs: Vec<GraphPin>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GraphWire {
    pub class: PinClass,
    pub from_step: u32,
    pub to_step: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_role: Option<FlowRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_pin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_pin: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GraphDiagnostic {
    pub severity: GraphSeverity,
    pub step: u32,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NodePosition {
    pub step: u32,
    pub col: u32,
    pub row: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
}

pub const LAYOUT_NODE_W: f64 = 210.0;
pub const LAYOUT_NODE_H: f64 = 84.0;
pub const LAYOUT_COL_GAP: f64 = 130.0;
pub const LAYOUT_ROW_GAP: f64 = 46.0;
pub const LAYOUT_ORIGIN: f64 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LayoutGeometry {
    pub node_w: f64,
    pub node_h: f64,
    pub col_gap: f64,
    pub row_gap: f64,
    pub origin: f64,
}

impl LayoutGeometry {
    pub const CANONICAL: Self = Self {
        node_w: LAYOUT_NODE_W,
        node_h: LAYOUT_NODE_H,
        col_gap: LAYOUT_COL_GAP,
        row_gap: LAYOUT_ROW_GAP,
        origin: LAYOUT_ORIGIN,
    };

    pub const fn col_pitch(&self) -> f64 {
        self.node_w + self.col_gap
    }

    pub const fn row_pitch(&self) -> f64 {
        self.node_h + self.row_gap
    }
}

impl Default for LayoutGeometry {
    fn default() -> Self {
        Self::CANONICAL
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GraphLayout {
    #[serde(default)]
    pub positions: Vec<NodePosition>,
    #[serde(default)]
    pub columns: u32,
    #[serde(default)]
    pub rows: u32,
    #[serde(default)]
    pub geometry: LayoutGeometry,
}

impl Default for GraphLayout {
    fn default() -> Self {
        Self {
            positions: Vec::new(),
            columns: 0,
            rows: 0,
            geometry: LayoutGeometry::CANONICAL,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct SopGraphView {
    #[serde(default)]
    pub nodes: Vec<GraphNode>,
    #[serde(default)]
    pub wires: Vec<GraphWire>,
    #[serde(default)]
    pub diagnostics: Vec<GraphDiagnostic>,
    #[serde(default)]
    pub layout: GraphLayout,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeRunState {
    #[default]
    Pending,
    Active,
    Completed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChannelAliasView {
    pub alias: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owning_agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChannelTriggerKindView {
    pub channel: String,
    #[serde(default)]
    pub aliases: Vec<ChannelAliasView>,
    pub configured: bool,
    pub setup_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<PayloadContractView>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerFieldKindView {
    #[default]
    Text,
    List,
    Expression,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TriggerFieldView {
    pub name: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub multi: bool,
    #[serde(default)]
    pub kind: TriggerFieldKindView,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionValueTypeView {
    #[default]
    String,
    Number,
    Bool,
    Enum,
    DateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConditionFieldView {
    pub path: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub value_type: ConditionValueTypeView,
    #[serde(default)]
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PayloadContractView {
    #[serde(default)]
    pub open: bool,
    #[serde(default)]
    pub direct: bool,
    #[serde(default)]
    pub fields: Vec<ConditionFieldView>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConditionOpSpecView {
    pub token: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BoundTriggerSourceView {
    pub source: String,
    #[serde(default)]
    pub fields: Vec<TriggerFieldView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<PayloadContractView>,
}

/// Result shape of `sops/trigger-sources`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TriggerSourceRegistryView {
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub bound: Vec<BoundTriggerSourceView>,
    #[serde(default)]
    pub channels: Vec<ChannelTriggerKindView>,
    #[serde(default)]
    pub operators: Vec<ConditionOpSpecView>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SwitchRule {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goto: Option<u32>,
    #[serde(skip)]
    pub goto_buf: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StepRouting {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub switch: Vec<SwitchRule>,
}

impl StepRouting {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepFailure {
    #[default]
    Fail,
    Retry {
        max: u32,
    },
    Goto {
        step: u32,
    },
}

impl StepFailure {
    pub fn is_fail(&self) -> bool {
        matches!(self, Self::Fail)
    }
}

/// Mirror of runtime `PlannedToolCall`; drift is caught by the
/// `draft_wire_shape` tests on both sides.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlannedToolCall {
    pub tool: String,
    #[serde(default)]
    pub args: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StepPos {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SopStep {
    pub number: u32,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub requires_confirmation: bool,
    #[serde(default)]
    pub kind: SopStepKind,
    #[serde(default, skip_serializing_if = "StepRouting::is_default")]
    pub routing: StepRouting,
    #[serde(default, skip_serializing_if = "StepFailure::is_fail")]
    pub on_failure: StepFailure,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<PlannedToolCall>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub scope: serde_json::Value,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub mode: serde_json::Value,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub agent: serde_json::Value,
    /// Persisted canvas coordinate set by the web Blueprint editor. zerocode
    /// preserves it verbatim on round-trip; its TUI renders from the grid layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<StepPos>,
    /// Editor-local raw JSON text for `calls`; never on the wire.
    #[serde(skip)]
    pub calls_buf: Option<String>,
}

impl Default for SopStep {
    fn default() -> Self {
        Self {
            number: 0,
            title: String::new(),
            body: String::new(),
            suggested_tools: Vec::new(),
            requires_confirmation: false,
            kind: SopStepKind::Execute,
            routing: StepRouting::default(),
            on_failure: StepFailure::Fail,
            calls: Vec::new(),
            schema: serde_json::Value::Null,
            scope: serde_json::Value::Null,
            mode: serde_json::Value::Null,
            agent: serde_json::Value::Null,
            pos: None,
            calls_buf: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SopDraft {
    pub name: String,
    pub description: String,
    pub version: String,
    pub priority: String,
    pub execution_mode: String,
    pub triggers: Vec<SopTriggerDraft>,
    pub steps: Vec<SopStep>,
    pub cooldown_secs: u64,
    pub max_concurrent: u32,
    pub deterministic: bool,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub agent: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SopTriggerDraft {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar_source: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calendar_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_key: Option<String>,
}

impl Default for SopTriggerDraft {
    fn default() -> Self {
        Self {
            kind: "manual".to_string(),
            channel: None,
            alias: None,
            path: None,
            expression: None,
            topic: None,
            condition: None,
            events: Vec::new(),
            board: None,
            signal: None,
            calendar_source: None,
            calendar_ids: Vec::new(),
            routing_key: None,
        }
    }
}

impl Default for SopDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            version: "1.0.0".to_string(),
            priority: "normal".to_string(),
            execution_mode: "supervised".to_string(),
            triggers: vec![SopTriggerDraft {
                kind: "manual".to_string(),
                ..SopTriggerDraft::default()
            }],
            steps: vec![SopStep {
                number: 1,
                ..SopStep::default()
            }],
            cooldown_secs: 0,
            max_concurrent: 1,
            deterministic: false,
            agent: serde_json::Value::Null,
        }
    }
}

// ── Logs types ───────────────────────────────────────────────────

#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct LogsQueryParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_ts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until_ts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until_id: Option<String>,
    /// Byte offset cap passed back from the previous page's
    /// `next_cursor_line_offset`. When set, the reader stops scanning
    /// at this offset so the follow-up page only sees lines strictly
    /// older than the previous one. Independent of id ordering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until_line_offset: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity_min: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default)]
    pub hide_internal: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LogsQueryResult {
    pub events: Vec<serde_json::Value>,
    /// Legacy cursor: `(timestamp, id)` to feed back as `until_ts` +
    /// `until_id` for older. Tie-breaks same-timestamp events by
    /// lexicographic id, which can drop earlier-written events when id
    /// order diverges from file insertion order. Prefer
    /// [`Self::next_cursor_line_offset`] when available — it is
    /// independent of id ordering.
    pub next_cursor: Option<(String, String)>,
    /// Byte offset past the OLDEST event on the current page. Pass back
    /// as [`LogsQueryParams::until_line_offset`] on the next request to
    /// walk older pages deterministically regardless of id ordering.
    /// `None` when the page is empty.
    pub next_cursor_line_offset: Option<u64>,
    pub at_end: bool,
}

/// Mirror of `zeroclaw_runtime::rpc::types::LogsGetResult`. Full log
/// event payload returned by the lazy-load `logs/get` RPC.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LogsGetResult {
    pub event: serde_json::Value,
}

// ── Session / Agents types ───────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionNewResult {
    pub session_id: String,
    #[serde(default)]
    pub workspace_dir: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionCancelResult {}

/// Session-scoped overrides mirror of
/// `zeroclaw_runtime::rpc::session::SessionOverrides`. Sent on
/// `session/configure`; every field is optional and omitted when `None`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionConfigureResult {
    /// Echoed by the daemon; retained to lock the wire shape even though the
    /// TUI keys off the caller's own session id.
    #[allow(dead_code)]
    pub session_id: String,
    #[serde(default)]
    pub overrides: SessionOverrides,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionGitBranchResult {
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub hash: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionApproveResult {}

#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    AllowOnce,
    AllowAlways,
    Reject,
    RejectWithEdit { replacement: String },
}

impl ApprovalDecision {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::AllowOnce => "allow_once",
            Self::AllowAlways => "allow_always",
            Self::Reject => "reject",
            Self::RejectWithEdit { .. } => "reject_with_edit",
        }
    }
}

// ── Dashboard types ──────────────────────────────────────────────

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StatusResult {
    pub server_version: String,
    pub protocol_version: u64,
    pub active_sessions: usize,
    #[serde(default)]
    pub config_dir: Option<String>,
    #[serde(default)]
    pub config_file: Option<String>,
    #[serde(default)]
    pub config_kind: Option<String>,
    #[serde(default)]
    pub local_ipc_endpoint: Option<String>,
}

#[cfg(test)]
mod dashboard_status_tests {
    use super::*;

    #[test]
    fn status_result_decodes_runtime_context_fields() {
        let value = serde_json::json!({
            "server_version": "0.8.4",
            "protocol_version": 1,
            "active_sessions": 2,
            "config_dir": "/tmp/zeroclaw-profile",
            "config_file": "/tmp/zeroclaw-profile/config.toml",
            "config_kind": "temporary",
            "local_ipc_endpoint": "/tmp/zeroclaw-profile/data/daemon.sock"
        });

        let status: StatusResult = serde_json::from_value(value).unwrap();

        assert_eq!(status.config_dir.as_deref(), Some("/tmp/zeroclaw-profile"));
        assert_eq!(
            status.config_file.as_deref(),
            Some("/tmp/zeroclaw-profile/config.toml")
        );
        assert_eq!(status.config_kind.as_deref(), Some("temporary"));
        assert_eq!(
            status.local_ipc_endpoint.as_deref(),
            Some("/tmp/zeroclaw-profile/data/daemon.sock")
        );
    }

    #[test]
    fn status_result_decodes_legacy_payload_without_runtime_context() {
        let value = serde_json::json!({
            "server_version": "0.8.4",
            "protocol_version": 1,
            "active_sessions": 2
        });

        let status: StatusResult = serde_json::from_value(value).unwrap();

        assert_eq!(status.server_version, "0.8.4");
        assert_eq!(status.config_dir, None);
        assert_eq!(status.config_file, None);
        assert_eq!(status.config_kind, None);
        assert_eq!(status.local_ipc_endpoint, None);
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionEntry {
    pub session_id: String,
    pub session_key: String,
    pub created_at: String,
    pub last_activity: String,
    pub message_count: usize,
    #[serde(default)]
    pub agent_alias: Option<String>,
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionListResult {
    pub sessions: Vec<SessionEntry>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentStatusEntry {
    pub alias: String,
    pub enabled: bool,
    #[serde(default)]
    pub live_sessions: usize,
    #[serde(default)]
    pub persisted_sessions: usize,
    #[serde(default)]
    pub channels: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentsStatusResult {
    pub agents: Vec<AgentStatusEntry>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ModelStats {
    pub model: String,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub request_count: usize,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentCostStats {
    pub agent_alias: String,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub request_count: usize,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CostSummaryResult {
    pub session_cost_usd: f64,
    pub daily_cost_usd: f64,
    pub monthly_cost_usd: f64,
    pub total_tokens: u64,
    pub request_count: usize,
    #[serde(default)]
    pub by_model: std::collections::HashMap<String, ModelStats>,
    #[serde(default)]
    pub by_agent: std::collections::HashMap<String, AgentCostStats>,
}

/// One calendar month of organization spend (oldest first; the last entry may
/// be the partial current month).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct OrgMonthCost {
    #[serde(default)]
    pub cost_usd: f64,
}

/// Year-to-date billed totals for a single scope (the user, or the whole org).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct OrgScopeStat {
    #[serde(default)]
    pub ytd_cost_usd: f64,
    #[serde(default)]
    pub ytd_tokens: u64,
    #[serde(default)]
    pub monthly: Vec<OrgMonthCost>,
}

/// Organization-level billed snapshot returned by `cost/org`, deserialized from
/// the daemon's `org_cost.json`. Mirrors a typical billing-export cache shape
/// but is vendor-neutral here; absent on vanilla builds.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct OrgCost {
    #[serde(default)]
    pub year: i32,
    #[serde(default)]
    pub generated: String,
    /// Display label for the organization scope (e.g. "Acme"). Falls back to
    /// "Organization" when absent.
    #[serde(default)]
    pub org_label: Option<String>,
    #[serde(default)]
    pub personal: Option<OrgScopeStat>,
    #[serde(default)]
    pub org: Option<OrgScopeStat>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CronSchedule {
    Cron {
        expr: String,
        #[serde(default)]
        tz: Option<String>,
    },
    At {
        at: String,
    },
    Every {
        every_ms: u64,
    },
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CronJobEntry {
    pub id: String,
    pub schedule: CronSchedule,
    pub command: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub agent_alias: String,
    #[serde(default)]
    pub enabled: bool,
    pub created_at: String,
    pub next_run: String,
    #[serde(default)]
    pub last_run: Option<String>,
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub last_output: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CronListResult {
    pub jobs: Vec<CronJobEntry>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CronRunEntry {
    pub id: i64,
    pub job_id: String,
    pub started_at: String,
    pub finished_at: String,
    pub status: String,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CronRunsResult {
    pub runs: Vec<CronRunEntry>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CronTriggerResult {
    pub id: String,
    pub success: bool,
    pub output: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct MemoryEntryResult {
    pub key: String,
    pub content: String,
    pub category: String,
    pub timestamp: String,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub namespace: String,
    #[serde(default)]
    pub importance: Option<f64>,
    #[serde(default)]
    pub agent_alias: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MemoryListResult {
    pub entries: Vec<MemoryEntryResult>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MemorySearchResult {
    pub entries: Vec<MemoryEntryResult>,
}

/// Mirror of `zeroclaw_runtime::rpc::types::MemoryGetResult`. Full
/// memory entry payload returned by the lazy-load `memory/get` RPC.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MemoryGetResult {
    pub entry: Option<MemoryEntryResult>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionMessagesResult {
    pub messages: Vec<MessageEntry>,
    /// Total persisted messages for the session. With `start`, lets
    /// the Sessions pane size scrollback affordances without keeping
    /// the full history in memory.
    #[serde(default)]
    pub total: usize,
    /// Index of `messages[0]` in the full persisted history.
    #[serde(default)]
    pub start: usize,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MessageEntry {
    pub role: String,
    pub content: String,
}

impl MessageEntry {
    /// Classify the wire `role` string into the closed set the UI renders.
    /// Unknown roles map to [`MessageRole::Other`] so surfaces can fall back
    /// without string-matching at the call site.
    pub fn role(&self) -> MessageRole {
        MessageRole::from_wire(&self.role)
    }
}

/// Closed taxonomy of persisted message roles, as they arrive over the
/// `session/messages` wire. The daemon emits these as strings; this is the
/// single place that maps the wire form into a type the UI matches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Other,
}

impl MessageRole {
    fn from_wire(role: &str) -> Self {
        match role {
            "user" => Self::User,
            "assistant" => Self::Assistant,
            "system" => Self::System,
            _ => Self::Other,
        }
    }
}

// ── TUI identity types ───────────────────────────────────────────

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TuiListEntry {
    pub tui_id: String,
    pub connected_at_unix: i64,
    pub peer_label: String,
    /// Transport protocol: `"unix"` or `"wss"`.
    #[serde(default)]
    pub transport: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TuiListResult {
    pub tuis: Vec<TuiListEntry>,
}

#[cfg(test)]
mod sop_method_tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::mpsc;

    fn make_rpc() -> (Arc<RpcOutbound>, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel::<String>(16);
        (Arc::new(RpcOutbound::new(tx)), rx)
    }

    /// Wire fixture mirrored by `sop::graph` serialization tests in
    /// zeroclaw-runtime. If this shape drifts, fix both sides together.
    fn graph_fixture() -> serde_json::Value {
        json!({
            "nodes": [
                {
                    "step": 1_000_000,
                    "title": "manual",
                    "kind": "trigger",
                    "subtitle": "manual",
                    "trigger_index": 0,
                    "inputs": [],
                    "outputs": [
                        {"class": "flow", "name": "event", "required": false}
                    ]
                },
                {
                    "step": 1,
                    "title": "First",
                    "kind": "step",
                    "inputs": [
                        {"class": "flow", "name": "in", "required": false},
                        {"class": "data", "name": "input", "data_type": "object", "required": true}
                    ],
                    "outputs": [
                        {"class": "flow", "name": "pr", "required": false}
                    ]
                }
            ],
            "wires": [
                {"class": "flow", "from_step": 1_000_000, "to_step": 1, "flow_role": "trigger", "from_pin": "event"},
                {"class": "flow", "from_step": 1, "to_step": 1, "flow_role": "switch", "from_pin": "pr"}
            ],
            "diagnostics": [
                {"severity": "error", "step": 1, "message": "required input `input` has no upstream producer of a compatible type"}
            ],
            "layout": {
                "positions": [
                    {"step": 1, "col": 1, "row": 0},
                    {"step": 1_000_000, "col": 0, "row": 0}
                ],
                "columns": 2,
                "rows": 1
            }
        })
    }

    #[test]
    fn graph_view_parses_runtime_wire_shape() {
        let view: SopGraphView = serde_json::from_value(graph_fixture()).unwrap();

        assert_eq!(view.nodes.len(), 2);
        let trigger = &view.nodes[0];
        assert_eq!(trigger.kind, NodeKind::Trigger);
        assert_eq!(trigger.trigger_index, Some(0));
        assert_eq!(trigger.outputs[0].class, PinClass::Flow);

        let step = &view.nodes[1];
        assert_eq!(step.kind, NodeKind::Step);
        assert_eq!(step.inputs[1].class, PinClass::Data);
        assert_eq!(step.inputs[1].data_type.as_deref(), Some("object"));
        assert!(step.inputs[1].required);

        assert_eq!(view.wires[0].flow_role, Some(FlowRole::Trigger));
        assert_eq!(view.wires[1].flow_role, Some(FlowRole::Switch));
        assert_eq!(view.wires[1].from_pin.as_deref(), Some("pr"));
        assert_eq!(view.diagnostics[0].severity, GraphSeverity::Error);
        assert_eq!(view.layout.columns, 2);
    }

    #[test]
    fn graph_view_roundtrips_without_shape_loss() {
        let view: SopGraphView = serde_json::from_value(graph_fixture()).unwrap();
        let reparsed: SopGraphView =
            serde_json::from_value(serde_json::to_value(&view).unwrap()).unwrap();
        assert_eq!(view, reparsed);
    }

    /// Pins the planned-call wire shape against runtime `PlannedToolCall`
    /// (`sop::types`). The editor-local `calls_buf` must never leak onto
    /// the wire.
    #[test]
    fn step_calls_serialize_to_canonical_wire() {
        let step = SopStep {
            number: 2,
            title: "compute".into(),
            body: "b".into(),
            calls: vec![PlannedToolCall {
                tool: "calculator".into(),
                args: json!({"function": "add", "values": "{{steps.1.value}}"}),
                pinned: Some(json!({"value": 3})),
            }],
            calls_buf: Some("editor scratch".into()),
            ..SopStep::default()
        };
        let value = serde_json::to_value(&step).unwrap();
        assert_eq!(
            value["calls"],
            json!([{
                "tool": "calculator",
                "args": {"function": "add", "values": "{{steps.1.value}}"},
                "pinned": {"value": 3}
            }])
        );
        assert!(
            value.get("calls_buf").is_none(),
            "calls_buf must not hit the wire"
        );

        let reparsed: SopStep = serde_json::from_value(value).unwrap();
        assert_eq!(reparsed.calls, step.calls);
        assert!(reparsed.calls_buf.is_none());
    }

    #[test]
    fn trigger_registry_view_parses_runtime_wire_shape() {
        let view: TriggerSourceRegistryView = serde_json::from_value(json!({
            "sources": ["webhook", "filesystem", "channel", "manual"],
            "bound": [
                {"source": "webhook", "fields": [{"name": "path", "kind": "text"}]},
                {"source": "filesystem", "fields": [
                    {"name": "path", "kind": "text"},
                    {"name": "events", "options": ["created", "modified", "deleted", "renamed"], "multi": true, "kind": "list"},
                    {"name": "condition", "kind": "expression"}
                ]},
                {"source": "manual", "fields": []}
            ],
            "channels": [
                {
                    "channel": "telegram",
                    "aliases": [{"alias": "prod", "enabled": true, "owning_agent": "main"}],
                    "configured": true,
                    "setup_path": "/config/channels/telegram"
                }
            ]
        }))
        .unwrap();

        assert!(
            !view.sources.is_empty(),
            "backend sources walk must survive deserialization so zerocode \
             renders the picker from it without reconstructing"
        );
        assert_eq!(view.bound.len(), 3);
        let fs = &view.bound[1];
        assert_eq!(fs.fields[1].kind, TriggerFieldKindView::List);
        assert!(fs.fields[1].multi);
        assert_eq!(fs.fields[2].kind, TriggerFieldKindView::Expression);
        assert!(view.channels[0].configured);
        assert_eq!(
            view.channels[0].aliases[0].owning_agent.as_deref(),
            Some("main")
        );
    }

    #[tokio::test]
    async fn sops_graph_view_sends_name_and_parses_result() {
        let (rpc, mut write_rx) = make_rpc();
        let client = RpcClient::with_rpc(rpc.clone());

        let task = tokio::spawn(async move { client.sops_graph_view("deploy").await });

        let line = tokio::time::timeout(std::time::Duration::from_secs(2), write_rx.recv())
            .await
            .expect("client.sops_graph_view must send a wire request")
            .unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(req["method"], "sops/graph");
        assert_eq!(req["params"]["name"], "deploy");

        let id = req["id"].as_str().unwrap().to_string();
        rpc.dispatch_response(&id, Some(graph_fixture()), None);

        let view = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("client.sops_graph_view must resolve after the response is dispatched")
            .unwrap()
            .unwrap();
        assert_eq!(view.nodes.len(), 2);
    }

    #[tokio::test]
    async fn sops_wire_draft_sends_sop_and_edit_envelopes() {
        let (rpc, mut write_rx) = make_rpc();
        let client = RpcClient::with_rpc(rpc.clone());

        let sop = json!({"name": "deploy", "steps": []});
        let edit = json!({"op": "connect", "from": 1, "to": 2, "role": "sequence"});
        let task = {
            let (sop, edit) = (sop.clone(), edit.clone());
            tokio::spawn(async move { client.sops_wire_draft(sop, edit).await })
        };

        let line = tokio::time::timeout(std::time::Duration::from_secs(2), write_rx.recv())
            .await
            .expect("client.sops_wire_draft must send a wire request")
            .unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(req["method"], "sops/wire-draft");
        assert_eq!(req["params"]["sop"], sop);
        assert_eq!(req["params"]["edit"], edit);

        let id = req["id"].as_str().unwrap().to_string();
        rpc.dispatch_response(&id, Some(json!({"sop": {"name": "deploy"}})), None);
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("client.sops_wire_draft must resolve after the response is dispatched")
            .unwrap()
            .unwrap();
    }
}

#[cfg(test)]
mod session_method_tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::mpsc;

    fn make_rpc() -> (Arc<RpcOutbound>, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel::<String>(16);
        (Arc::new(RpcOutbound::new(tx)), rx)
    }

    #[tokio::test]
    async fn session_new_sends_correct_wire_params() {
        let (rpc, mut write_rx) = make_rpc();
        let client = RpcClient::with_rpc(rpc.clone());

        let task =
            tokio::spawn(async move { client.session_new("my-agent", Some("/tmp/work")).await });

        let line = tokio::time::timeout(std::time::Duration::from_secs(2), write_rx.recv())
            .await
            .expect("client.session_new must send a wire request; a hang here wedges the TTY")
            .unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(req["method"], "session/new");
        assert_eq!(req["params"]["agent_alias"], "my-agent");
        assert_eq!(req["params"]["cwd"], "/tmp/work");

        let id = req["id"].as_str().unwrap().to_string();
        rpc.dispatch_response(
            &id,
            Some(json!({"session_id":"s42","agent_alias":"my-agent","message_count":0})),
            None,
        );

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("client.session_new must resolve after the response is dispatched")
            .unwrap()
            .unwrap();
        assert_eq!(result.session_id, "s42");
    }

    #[tokio::test]
    async fn session_cancel_sends_session_id() {
        let (rpc, mut write_rx) = make_rpc();
        let client = RpcClient::with_rpc(rpc.clone());

        let task = tokio::spawn(async move { client.session_cancel("s1").await });

        let line = tokio::time::timeout(std::time::Duration::from_secs(2), write_rx.recv())
            .await
            .expect("client.session_cancel must send a wire request; a hang here wedges the TTY")
            .unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(req["method"], "session/cancel");
        assert_eq!(req["params"]["session_id"], "s1");

        let id = req["id"].as_str().unwrap().to_string();
        rpc.dispatch_response(&id, Some(json!({"session_id":"s1","cancelled":true})), None);
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("client.session_cancel must resolve after the response is dispatched")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn cron_runs_sends_job_id_and_limit() {
        let (rpc, mut write_rx) = make_rpc();
        let client = RpcClient::with_rpc(rpc.clone());

        let task = tokio::spawn(async move { client.cron_runs("job-1", Some(3)).await });

        let line = tokio::time::timeout(std::time::Duration::from_secs(2), write_rx.recv())
            .await
            .expect("client.cron_runs must send a wire request; a hang here wedges the TTY")
            .unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(req["method"], "cron/runs");
        assert_eq!(req["params"]["id"], "job-1");
        assert_eq!(req["params"]["limit"], 3);

        let id = req["id"].as_str().unwrap().to_string();
        rpc.dispatch_response(
            &id,
            Some(json!({
                "runs": [{
                    "id": 7,
                    "job_id": "job-1",
                    "started_at": "2026-06-18T00:00:00Z",
                    "finished_at": "2026-06-18T00:00:02Z",
                    "status": "ok",
                    "output": "done",
                    "duration_ms": 2000
                }]
            })),
            None,
        );

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("client.cron_runs must resolve after the response is dispatched")
            .unwrap()
            .unwrap();
        assert_eq!(result.runs.len(), 1);
        assert_eq!(result.runs[0].job_id, "job-1");
        assert_eq!(result.runs[0].duration_ms, Some(2000));
    }

    #[tokio::test]
    async fn cron_trigger_sends_job_id() {
        let (rpc, mut write_rx) = make_rpc();
        let client = RpcClient::with_rpc(rpc.clone());

        let task = tokio::spawn(async move { client.cron_trigger("job-1").await });

        let line = tokio::time::timeout(std::time::Duration::from_secs(2), write_rx.recv())
            .await
            .expect("client.cron_trigger must send a wire request; a hang here wedges the TTY")
            .unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(req["method"], "cron/trigger");
        assert_eq!(req["params"]["id"], "job-1");

        let id = req["id"].as_str().unwrap().to_string();
        rpc.dispatch_response(
            &id,
            Some(json!({"id": "job-1", "success": true, "output": "done"})),
            None,
        );

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("client.cron_trigger must resolve after the response is dispatched")
            .unwrap()
            .unwrap();
        assert_eq!(result.id, "job-1");
        assert!(result.success);
        assert_eq!(result.output, "done");
    }

    #[tokio::test]
    async fn session_approve_sends_decision_and_request_id() {
        let (rpc, mut write_rx) = make_rpc();
        let client = RpcClient::with_rpc(rpc.clone());

        let task = tokio::spawn(async move {
            client
                .session_approve("s1", "req-1", ApprovalDecision::AllowOnce)
                .await
        });

        let line = tokio::time::timeout(std::time::Duration::from_secs(2), write_rx.recv())
            .await
            .expect("client.session_approve must send a wire request; a hang here wedges the TTY")
            .unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(req["method"], "session/approve");
        assert_eq!(req["params"]["decision"], "allow_once");
        assert_eq!(req["params"]["request_id"], "req-1");

        let id = req["id"].as_str().unwrap().to_string();
        rpc.dispatch_response(
            &id,
            Some(json!({"session_id":"s1","request_id":"req-1","acknowledged":true})),
            None,
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("client.session_approve must resolve after the response is dispatched")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn config_map_key_rename_sends_path_and_aliases() {
        assert_eq!(CONFIG_RENAME_TIMEOUT, std::time::Duration::from_secs(120));

        let (rpc, mut write_rx) = make_rpc();
        let client = RpcClient::with_rpc(rpc.clone());

        let task =
            tokio::spawn(async move { client.config_map_key_rename("agents", "old", "new").await });

        let line = tokio::time::timeout(std::time::Duration::from_secs(2), write_rx.recv())
            .await
            .expect("client.config_map_key_rename must send a wire request")
            .unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(req["method"], "config/map-key-rename");
        assert_eq!(req["params"]["path"], "agents");
        assert_eq!(req["params"]["from"], "old");
        assert_eq!(req["params"]["to"], "new");

        let id = req["id"].as_str().unwrap().to_string();
        rpc.dispatch_response(
            &id,
            Some(json!({
                "path": "agents",
                "from": "old",
                "to": "new",
                "renamed": true,
                "warnings": ["workspace move skipped"]
            })),
            None,
        );

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("client.config_map_key_rename must resolve after the response is dispatched")
            .unwrap()
            .unwrap();
        assert!(result.renamed);
        assert_eq!(result.warnings, vec!["workspace move skipped"]);
    }
}

#[cfg(test)]
mod notification_tests {
    use super::*;
    use tokio::sync::{broadcast, mpsc};

    /// Channels handed back by [`route_fixture`]. Aliased to keep the
    /// return type readable (clippy::type_complexity).
    type RouteFixture = (
        Arc<RpcOutbound>,
        broadcast::Sender<RpcNotification>,
        broadcast::Receiver<RpcNotification>,
        broadcast::Sender<RpcInboundRequest>,
        broadcast::Receiver<RpcInboundRequest>,
        mpsc::Receiver<String>,
    );

    /// Build a fresh fixture for routing tests. The writer receiver is
    /// returned (not dropped) so `RpcOutbound`'s writer channel stays
    /// open — dropping it would make every `request`/`respond` fail with
    /// "Writer task closed".
    fn route_fixture() -> RouteFixture {
        let (writer_tx, writer_rx) = mpsc::channel::<String>(16);
        let rpc = Arc::new(RpcOutbound::new(writer_tx));
        let (notif_tx, notif_rx) = broadcast::channel::<RpcNotification>(16);
        let (inbound_tx, inbound_rx) = broadcast::channel::<RpcInboundRequest>(16);
        (rpc, notif_tx, notif_rx, inbound_tx, inbound_rx, writer_rx)
    }

    /// Response frames — id + result/error, no method — should reach the
    /// pending outbound call via `dispatch_response` and emit nothing on
    /// the notification / inbound channels.
    #[tokio::test]
    async fn route_inbound_frame_routes_response_to_pending_call() {
        let (rpc, notif_tx, mut notif_rx, inbound_tx, mut inbound_rx, mut writer_rx) =
            route_fixture();
        // Register a pending outbound call so dispatch_response has a target.
        let call_task = {
            let rpc = Arc::clone(&rpc);
            tokio::spawn(async move { rpc.request("ping", serde_json::Value::Null).await })
        };
        // Drain the one outbound frame the request writes so the spawned
        // task makes progress and registers its pending id (`zc-out-0`,
        // the first id from a fresh RpcOutbound).
        let _outbound = writer_rx.recv().await.expect("request wrote a frame");
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "zc-out-0",
            "result": { "pong": true }
        });
        route_inbound_frame(&rpc, &notif_tx, &inbound_tx, frame);

        let answer = call_task.await.unwrap().unwrap();
        assert_eq!(answer["pong"], true);
        assert!(inbound_rx.try_recv().is_err(), "inbound rx must stay empty");
        assert!(notif_rx.try_recv().is_err(), "notif rx must stay empty");
    }

    /// Notification frames — method, no id — should reach the
    /// notification broadcast and not the inbound-request channel.
    #[tokio::test]
    async fn route_inbound_frame_routes_notification() {
        let (rpc, notif_tx, mut notif_rx, inbound_tx, mut inbound_rx, _writer_rx) = route_fixture();
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": { "type": "agent_message_chunk", "session_id": "s1", "text": "hi" }
        });
        route_inbound_frame(&rpc, &notif_tx, &inbound_tx, frame);
        let notif = notif_rx.try_recv().expect("notification routed");
        assert_eq!(notif.method, "session/update");
        assert!(inbound_rx.try_recv().is_err());
    }

    /// Server-initiated request frames — both id and method — should
    /// reach the inbound-request broadcast and NOT be misclassified
    /// as a response (which would silently drop the elicitation prompt).
    #[tokio::test]
    async fn route_inbound_frame_routes_server_initiated_request() {
        let (rpc, notif_tx, mut notif_rx, inbound_tx, mut inbound_rx, _writer_rx) = route_fixture();
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "elicit-42",
            "method": "elicitation/create",
            "params": {
                "sessionId": "sess-1",
                "mode": "form",
                "message": "Pick one",
                "requestedSchema": { "type": "object", "properties": {} }
            }
        });
        route_inbound_frame(&rpc, &notif_tx, &inbound_tx, frame);
        let req = inbound_rx.try_recv().expect("inbound request routed");
        assert_eq!(req.method, "elicitation/create");
        assert_eq!(req.id, serde_json::Value::String("elicit-42".to_string()));
        assert_eq!(req.params["sessionId"], "sess-1");
        assert!(notif_rx.try_recv().is_err());
    }

    /// Frames with both fields but a numeric id — the JSON-RPC spec
    /// permits int ids, even though the daemon emits strings — must
    /// still route as a server-initiated request (we forward the
    /// `Value` verbatim so the response carries the same shape).
    #[tokio::test]
    async fn route_inbound_frame_handles_numeric_request_id() {
        let (rpc, notif_tx, _notif_rx, inbound_tx, mut inbound_rx, _writer_rx) = route_fixture();
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "elicitation/create",
            "params": {}
        });
        route_inbound_frame(&rpc, &notif_tx, &inbound_tx, frame);
        let req = inbound_rx.try_recv().expect("inbound request routed");
        assert_eq!(req.id, serde_json::json!(7));
    }

    fn make_notification(method: &str, params: serde_json::Value) -> RpcNotification {
        RpcNotification {
            method: method.to_string(),
            params,
        }
    }

    #[tokio::test]
    async fn parse_agent_message_chunk() {
        let params = serde_json::json!({
            "type": "agent_message_chunk",
            "session_id": "s1",
            "text": "hello"
        });
        let update = parse_session_update(&params).unwrap();
        match update {
            SessionUpdate::AgentMessageChunk { session_id, text } => {
                assert_eq!(session_id, "s1");
                assert_eq!(text, "hello");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn parse_approval_request() {
        let params = serde_json::json!({
            "type": "approval_request",
            "session_id": "s2",
            "request_id": "req-1",
            "tool_name": "shell",
            "arguments_summary": "ls /tmp",
            "timeout_secs": 60
        });
        let update = parse_session_update(&params).unwrap();
        assert!(matches!(update, SessionUpdate::ApprovalRequest { .. }));
    }

    #[tokio::test]
    async fn router_converts_session_update_notifications() {
        let (bcast_tx, bcast_rx) = broadcast::channel::<RpcNotification>(16);
        let (update_tx, mut update_rx) = mpsc::channel::<SessionUpdate>(8);
        let _task = spawn_notification_router(bcast_rx, update_tx);

        bcast_tx
            .send(make_notification(
                "session/update",
                serde_json::json!({
                    "type": "agent_message_chunk",
                    "session_id": "s1",
                    "text": "streaming"
                }),
            ))
            .unwrap();

        let update = tokio::time::timeout(std::time::Duration::from_millis(100), update_rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");

        assert!(matches!(update, SessionUpdate::AgentMessageChunk { .. }));
    }

    #[tokio::test]
    async fn router_drops_unknown_method() {
        let (bcast_tx, bcast_rx) = broadcast::channel::<RpcNotification>(16);
        let (update_tx, mut update_rx) = mpsc::channel::<SessionUpdate>(8);
        let _task = spawn_notification_router(bcast_rx, update_tx);

        bcast_tx
            .send(make_notification("other/event", serde_json::json!({})))
            .unwrap();

        let result =
            tokio::time::timeout(std::time::Duration::from_millis(50), update_rx.recv()).await;
        assert!(result.is_err(), "unknown method must be dropped");
    }
}

#[cfg(test)]
mod tls_tests {
    use super::*;

    #[test]
    fn insecure_tls_config_builds_without_panic() {
        let cfg = RpcClient::insecure_tls_config();
        assert!(Arc::strong_count(&cfg) >= 1);
    }
}

#[cfg(test)]
mod plan_parse_tests {
    use super::*;

    #[test]
    fn parses_plan_update() {
        let params = serde_json::json!({
            "type": "plan",
            "session_id": "sess-1",
            "entries": [
                { "content": "A", "status": "completed", "priority": "high" },
                { "content": "B", "status": "in_progress", "activeForm": "Doing B" }
            ]
        });
        let update = parse_session_update(&params).expect("plan parses");
        match update {
            SessionUpdate::Plan {
                session_id,
                entries,
            } => {
                assert_eq!(session_id, "sess-1");
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].status, crate::wire::PlanStatus::Completed);
                assert_eq!(entries[1].active_form.as_deref(), Some("Doing B"));
            }
            _ => panic!("expected SessionUpdate::Plan"),
        }
    }

    #[test]
    fn parses_empty_plan_update_as_clear() {
        let params = serde_json::json!({
            "type": "plan",
            "session_id": "sess-2",
            "entries": []
        });
        match parse_session_update(&params).expect("empty plan parses") {
            SessionUpdate::Plan { entries, .. } => assert!(entries.is_empty()),
            _ => panic!("expected SessionUpdate::Plan"),
        }
    }

    #[test]
    fn parses_history_trimmed_update() {
        let params = serde_json::json!({
            "type": "history_trimmed",
            "session_id": "sess-3",
            "dropped_messages": 12,
            "kept_turns": 3,
            "reason": "history message limit exceeded"
        });

        assert!(matches!(
            parse_session_update(&params),
            Some(SessionUpdate::HistoryTrimmed {
                session_id,
                dropped_messages: 12,
                kept_turns: 3,
                reason,
            }) if session_id == "sess-3" && reason == "history message limit exceeded"
        ));
    }

    #[test]
    fn plan_update_missing_entries_is_none() {
        let params = serde_json::json!({ "type": "plan", "session_id": "s" });
        assert!(parse_session_update(&params).is_none());
    }
}
