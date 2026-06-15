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
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;
pub use zeroclaw_api::jsonrpc::RpcOutbound;
use zeroclaw_api::jsonrpc::error_codes::*;
use zeroclaw_api::jsonrpc::{
    ACP_PROTOCOL_VERSION, JsonRpcError, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
};
use zeroclaw_api::model_provider::ConversationMessage;
use zeroclaw_config::schema::Config;
use zeroclaw_infra::acp_session_store::AcpSessionStore;
use zeroclaw_runtime::agent::agent::{Agent, TurnEvent};
use zeroclaw_runtime::tools::CanvasStore;

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

// ── Session state ────────────────────────────────────────────────

struct Session {
    agent: Agent,
    #[allow(dead_code)] // WIP: intended for session expiry logic
    created_at: Instant,
    last_active: Instant,
    /// Agent alias (e.g. `"clamps"`) for attributable span logs.
    agent_alias: String,
    /// Model-provider ref (e.g. `"anthropic.default"`) for attributable span logs.
    model_provider: String,
    /// Model identifier (e.g. `"claude-sonnet-4-6"`) for attributable span logs.
    model: String,
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
    ///
    /// **Single-turn-per-session invariant:** this map holds at most one token
    /// per `session_id` because the ACP protocol does not pipeline multiple
    /// `session/prompt` calls on the same session — each prompt must complete
    /// (or be cancelled) before the next one is sent. A second prompt is
    /// rejected before it can overwrite the active turn's token. If pipelining
    /// is needed in the future, the key should become `(session_id, turn_id)`.
    cancel_tokens: Arc<std::sync::Mutex<HashMap<String, tokio_util::sync::CancellationToken>>>,
    /// Tracks session IDs currently being loaded/resumed (between the initial
    /// check and the final insert into `sessions`). Used to prevent duplicate
    /// concurrent restores of the same session and to count in-flight slots
    /// against `max_sessions`.
    loading_sessions: Arc<tokio::sync::Mutex<HashSet<String>>>,
    store: Option<Arc<AcpSessionStore>>,
    /// Shared canvas store from the gateway / daemon supervisor.  When set,
    /// agents created by this server write canvas frames to the same store
    /// that `/ws/canvas/:id` WebSocket subscribers read from.  `None` in
    /// standalone `zeroclaw acp` mode where no gateway is running.
    canvas_store: Option<CanvasStore>,
}

impl AcpServer {
    pub fn new(config: Config, acp_config: AcpServerConfig) -> Self {
        let (writer_tx, writer_rx) = mpsc::channel::<String>(256);
        Self::with_writer(config, acp_config, writer_tx, Some(writer_rx), None)
    }

    pub fn new_with_writer(
        config: Config,
        acp_config: AcpServerConfig,
        writer_tx: mpsc::Sender<String>,
    ) -> Self {
        Self::with_writer(config, acp_config, writer_tx, None, None)
    }

    pub fn new_with_store(
        config: Config,
        acp_config: AcpServerConfig,
        store: Arc<AcpSessionStore>,
    ) -> Self {
        let (writer_tx, writer_rx) = mpsc::channel::<String>(256);
        Self::with_writer(config, acp_config, writer_tx, Some(writer_rx), Some(store))
    }

    pub fn new_with_writer_and_store(
        config: Config,
        acp_config: AcpServerConfig,
        writer_tx: mpsc::Sender<String>,
        store: Arc<AcpSessionStore>,
    ) -> Self {
        Self::with_writer(config, acp_config, writer_tx, None, Some(store))
    }

    fn with_writer(
        config: Config,
        acp_config: AcpServerConfig,
        writer_tx: mpsc::Sender<String>,
        writer_rx: Option<mpsc::Receiver<String>>,
        store: Option<Arc<AcpSessionStore>>,
    ) -> Self {
        Self {
            config,
            acp_config,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            rpc: Arc::new(RpcOutbound::new(writer_tx)),
            writer_rx: std::sync::Mutex::new(writer_rx),
            cancel_tokens: Arc::new(std::sync::Mutex::new(HashMap::new())),
            loading_sessions: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            store,
            canvas_store: None,
        }
    }

    /// Attach the shared gateway [`CanvasStore`] so that agents created by
    /// this server write canvas frames to the same store that the
    /// `/ws/canvas/:id` WebSocket endpoint serves.
    pub fn with_canvas_store(mut self, canvas_store: CanvasStore) -> Self {
        self.canvas_store = Some(canvas_store);
        self
    }

    /// Run the ACP server, reading JSON-RPC requests from stdin and writing
    /// responses/notifications to stdout.
    pub async fn run(self: Arc<Self>) -> Result<()> {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_category(::zeroclaw_log::EventCategory::Channel),
            &format!(
                "ACP server starting (max_sessions={}, timeout={}s)",
                self.acp_config.max_sessions, self.acp_config.session_timeout_secs
            )
        );

        // Pull the writer-rx out of self so we can move it into the writer
        // task. Subsequent `run()` calls would have nothing to drive — but
        // `run()` is normally invoked once per process.
        let writer_rx = self
            .writer_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_category(::zeroclaw_log::EventCategory::Channel)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "ACP server writer already started"
                );
                anyhow::Error::msg("ACP server writer already started")
            })?;
        zeroclaw_spawn::spawn!(writer_task(writer_rx));

        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        let mut line = String::new();

        // Spawn session reaper
        let sessions = Arc::clone(&self.sessions);
        let timeout = Duration::from_secs(self.acp_config.session_timeout_secs);
        zeroclaw_spawn::spawn!(async move {
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
                                ::zeroclaw_log::record!(
                                    DEBUG,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_category(::zeroclaw_log::EventCategory::Channel)
                                    .with_attrs(
                                        ::serde_json::json!({
                                            "id": id,
                                            "agent_alias": session.agent_alias,
                                            "model_provider": session.model_provider,
                                            "model": session.model,
                                        })
                                    ),
                                    "Session expired after inactivity"
                                );
                            }
                            !expired
                        }
                        Err(_) => true,
                    }
                });
                let reaped = before - sessions.len();
                if reaped > 0 {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_category(::zeroclaw_log::EventCategory::Channel)
                            .with_attrs(::serde_json::json!({"reaped": reaped})),
                        "Reaped expired session(s)"
                    );
                }
            }
        });

        loop {
            line.clear();
            let bytes_read = reader.read_line(&mut line).await?;
            if bytes_read == 0 {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Channel),
                    "ACP server: stdin closed, shutting down"
                );
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
    /// are supplied by the writer channel passed to [`Self::new_with_writer`]
    /// or [`Self::new_with_writer_and_store`].
    pub async fn run_messages(self: Arc<Self>, mut input_rx: mpsc::Receiver<String>) -> Result<()> {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_category(::zeroclaw_log::EventCategory::Channel),
            "ACP server starting (WebSocket/framed mode)"
        );
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
                // while a prompt turn is in flight. Once `handle_request`
                // resolves session/agent context and attaches an
                // attribution scope, every log record emitted from this
                // task lands attributed in the TUI instead of orphaning.
                let server = Arc::clone(self);
                ::zeroclaw_spawn::spawn!(async move {
                    server.handle_request(request).await;
                });
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Channel)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Failed to parse JSON-RPC request"
                );
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
            "session/load" => self.handle_session_load(&request.params).await,
            "session/resume" => self.handle_session_resume(&request.params).await,
            "session/close" => self.handle_session_close(&request.params).await,
            "session/prompt" => self.handle_session_prompt(&request.params, &id).await,
            "session/stop" => self.handle_session_stop(&request.params).await,
            "session/cancel" => self.handle_session_cancel(&request.params).await,
            "session/event" | "session/update" => self.handle_session_event(&request.params).await,
            _ => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Channel)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"method": request.method})),
                    "ACP method not found"
                );
                Err(RpcError {
                    code: METHOD_NOT_FOUND,
                    message: format!("Method not found: {}", request.method),
                    data: None,
                })
            }
        };

        // Only send response for requests (with id), not notifications
        if !is_notification {
            match result {
                Ok(value) => self.write_result(id, value).await,
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_category(::zeroclaw_log::EventCategory::Channel)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "method": request.method,
                                "error_code": e.code,
                                "error": e.message,
                            })),
                        "ACP request failed"
                    );
                    self.write_error(id, e.code, &e.message).await;
                }
            }
        }
    }

    // ── Method handlers ──────────────────────────────────────────

    fn handle_initialize(&self, _params: &Value) -> RpcResult {
        let default_model = self
            .config
            .providers
            .models
            .iter_entries()
            .find_map(|(_, _, e)| e.model.clone());

        let mut zeroclaw_meta = serde_json::json!({
            "maxSessions": self.acp_config.max_sessions,
            "sessionTimeoutSecs": self.acp_config.session_timeout_secs,
        });
        if let Some(model) = default_model {
            zeroclaw_meta["defaultModel"] = serde_json::json!(model);
        }

        let session_capabilities = if self.store.is_some() {
            serde_json::json!({ "resume": {}, "close": {} })
        } else {
            serde_json::json!({})
        };

        Ok(serde_json::json!({
            "protocolVersion": ACP_PROTOCOL_VERSION,
            "agentCapabilities": {
                "loadSession": self.store.is_some(),
                "promptCapabilities": {
                    "image": false,
                    "audio": false,
                    "embeddedContext": false,
                },
                "mcpCapabilities": {
                    "http": false,
                    "sse": false,
                },
                "sessionCapabilities": session_capabilities,
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

        let loading_count = self.loading_sessions.lock().await.len();
        if sessions.len() + loading_count >= self.acp_config.max_sessions {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_category(::zeroclaw_log::EventCategory::Channel)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "active": sessions.len(),
                        "loading": loading_count,
                        "max": self.acp_config.max_sessions,
                    })),
                "ACP session/new rejected: session limit reached"
            );
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

        // Every ACP session is bound to an explicit agent alias.
        // Accept `agentAlias` (camelCase) or `agent_alias` / `agent`.
        // When the client omits the alias and exactly one agent is configured,
        // auto-select it so single-agent setups work without extra config.
        let agent_alias = params
            .get("agentAlias")
            .or_else(|| params.get("agent_alias"))
            .or_else(|| params.get("agent"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| self.config.acp.default_agent.clone())
            .or_else(|| {
                let mut keys = self.config.agents.keys();
                if self.config.agents.len() == 1 {
                    keys.next().cloned()
                } else {
                    None
                }
            })
            .ok_or_else(|| RpcError {
                code: INVALID_PARAMS,
                message: "session/new requires `agentAlias` (alias of a configured \
                          [agents.<alias>] entry)"
                    .to_string(),
                data: None,
            })?;
        if self.config.agent(&agent_alias).is_none() {
            return Err(RpcError {
                code: INVALID_PARAMS,
                message: format!(
                    "Unknown agent `{agent_alias}` — no [agents.{agent_alias}] entry configured"
                ),
                data: None,
            });
        }

        let session_id = Uuid::new_v4().to_string();

        // Build agent from global config, with the session's cwd pinned as
        // the file/shell sandbox boundary. The agent's data directory
        // (identity, scheduled tasks) still lives under `config.data_dir`.
        // ACP sessions exclude persistent memory — context comes from the
        // persisted session history, not the agent's long-term memory store.
        let agent = Agent::from_config_with_session_cwd_and_mcp_backchannel(
            &self.config,
            &agent_alias,
            Some(std::path::Path::new(&workspace_dir)),
            false,
            true,
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
                agent_alias: agent_alias.clone(),
                model_provider: self
                    .config
                    .agent(&agent_alias)
                    .map(|a| a.model_provider.to_string())
                    .unwrap_or_default(),
                model: self
                    .config
                    .model_provider_for_agent(&agent_alias)
                    .and_then(|mp| mp.model.clone())
                    .unwrap_or_default(),
            })),
        );

        if let Some(store) = &self.store {
            let store = store.clone();
            let sid = session_id.clone();
            let alias = agent_alias.clone();
            let wsd = workspace_dir.clone();
            let created =
                tokio::task::spawn_blocking(move || store.create_session(&sid, &alias, &wsd)).await;
            let error = match created {
                Ok(Ok(_)) => None,
                Ok(Err(e)) => Some(e.to_string()),
                Err(join) => Some(join.to_string()),
            };
            if let Some(detail) = error {
                // Roll back: remove the session we just inserted and surface the error.
                sessions.remove(&session_id);
                return Err(RpcError {
                    code: INTERNAL_ERROR,
                    message: format!("Failed to persist session: {detail}"),
                    data: None,
                });
            }
        }

        let mp = self
            .config
            .agent(&agent_alias)
            .map(|a| a.model_provider.to_string())
            .unwrap_or_default();
        let model_name = self
            .config
            .model_provider_for_agent(&agent_alias)
            .and_then(|mp| mp.model.clone())
            .unwrap_or_default();
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Start)
                .with_category(::zeroclaw_log::EventCategory::Channel)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(::serde_json::json!({
                    "session_id": session_id,
                    "workspace_dir": workspace_dir,
                    "agent_alias": agent_alias,
                    "model_provider": mp,
                    "model": model_name,
                })),
            "ACP session created"
        );

        Ok(serde_json::json!({
            "sessionId": session_id,
            "workspaceDir": workspace_dir,
        }))
    }

    async fn handle_session_load(&self, params: &Value) -> RpcResult {
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

        let store = self.store.as_ref().ok_or_else(|| RpcError {
            code: SESSION_NOT_FOUND,
            message: format!("Session not found: {session_id}"),
            data: None,
        })?;

        // Atomically check and reserve the session slot
        {
            let sessions = self.sessions.lock().await;
            let mut loading = self.loading_sessions.lock().await;
            if sessions.len() + loading.len() >= self.acp_config.max_sessions {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_category(::zeroclaw_log::EventCategory::Channel)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "session_id": session_id,
                            "active": sessions.len(),
                            "loading": loading.len(),
                            "max": self.acp_config.max_sessions,
                        })),
                    "ACP session/load rejected: session limit reached"
                );
                return Err(RpcError {
                    code: SESSION_LIMIT_REACHED,
                    message: format!(
                        "Maximum session limit reached ({})",
                        self.acp_config.max_sessions
                    ),
                    data: None,
                });
            }
            if sessions.contains_key(&session_id) || loading.contains(&session_id) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_category(::zeroclaw_log::EventCategory::Channel)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"session_id": session_id})),
                    "ACP session/load rejected: session already active"
                );
                return Err(RpcError {
                    code: INVALID_PARAMS,
                    message: format!(
                        "Session already active: {session_id}. Call session/close first."
                    ),
                    data: None,
                });
            }
            loading.insert(session_id.clone());
        }

        // Flatten both the SQLite error and the not-found case into a single
        // Result so the cleanup match below runs for every failure after the
        // reservation was inserted.
        let data = store
            .load_session(&session_id)
            .map_err(|e| RpcError {
                code: INTERNAL_ERROR,
                message: format!("Failed to load session: {e}"),
                data: None,
            })
            .and_then(|opt| {
                opt.ok_or_else(|| RpcError {
                    code: SESSION_NOT_FOUND,
                    message: format!("Session not found: {session_id}"),
                    data: None,
                })
            });

        // On error (SQLite failure or not-found), release the reservation.
        let data = match data {
            Ok(d) => d,
            Err(e) => {
                self.loading_sessions.lock().await.remove(&session_id);
                return Err(e);
            }
        };

        let workspace_dir = std::path::PathBuf::from(&data.workspace_dir);

        let restore_alias = self
            .config
            .acp
            .default_agent
            .clone()
            .or_else(|| {
                let mut keys = self.config.agents.keys();
                if self.config.agents.len() == 1 {
                    keys.next().cloned()
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "default".to_string());

        let agent_result = Agent::from_config_with_session_cwd_and_mcp_backchannel(
            &self.config,
            &restore_alias,
            Some(&workspace_dir),
            false,
            true,
        )
        .await
        .map_err(|e| RpcError {
            code: INTERNAL_ERROR,
            message: format!("Failed to create agent: {e}"),
            data: None,
        });

        let mut agent = match agent_result {
            Ok(a) => a,
            Err(e) => {
                self.loading_sessions.lock().await.remove(&session_id);
                return Err(e);
            }
        };

        agent.seed_conversation_history(data.messages.clone());

        let acp_channel = Arc::new(AcpChannel::new(
            "acp",
            session_id.clone(),
            Arc::clone(&self.rpc),
            Duration::from_secs(self.acp_config.session_timeout_secs),
        ));
        agent.channel_handles().register_channel("acp", acp_channel);

        let now = Instant::now();
        // Atomically insert and release reservation
        {
            let mut sessions = self.sessions.lock().await;
            let mut loading = self.loading_sessions.lock().await;
            loading.remove(&session_id);
            sessions.insert(
                session_id.clone(),
                Arc::new(Mutex::new(Session {
                    agent,
                    created_at: now,
                    last_active: now,
                    agent_alias: restore_alias.clone(),
                    model_provider: self
                        .config
                        .agent(&restore_alias)
                        .map(|a| a.model_provider.to_string())
                        .unwrap_or_default(),
                    model: self
                        .config
                        .model_provider_for_agent(&restore_alias)
                        .and_then(|mp| mp.model.clone())
                        .unwrap_or_default(),
                })),
            );
        }

        // Stream conversation history to client as session/update notifications
        for msg in &data.messages {
            for notification in history_notifications_for_message(&session_id, msg) {
                self.write_notification(&notification).await;
            }
        }

        let mp = self
            .config
            .agent(&restore_alias)
            .map(|a| a.model_provider.to_string())
            .unwrap_or_default();
        let model_name = self
            .config
            .model_provider_for_agent(&restore_alias)
            .and_then(|mp| mp.model.clone())
            .unwrap_or_default();
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Start)
                .with_category(::zeroclaw_log::EventCategory::Channel)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(::serde_json::json!({
                    "session_id": session_id,
                    "message_count": data.messages.len(),
                    "agent_alias": restore_alias,
                    "model_provider": mp,
                    "model": model_name,
                })),
            "ACP session loaded"
        );
        Ok(serde_json::json!({}))
    }

    async fn handle_session_resume(&self, params: &Value) -> RpcResult {
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

        let store = self.store.as_ref().ok_or_else(|| RpcError {
            code: SESSION_NOT_FOUND,
            message: format!("Session not found: {session_id}"),
            data: None,
        })?;

        // Atomically check and reserve the session slot
        {
            let sessions = self.sessions.lock().await;
            let mut loading = self.loading_sessions.lock().await;
            if sessions.len() + loading.len() >= self.acp_config.max_sessions {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_category(::zeroclaw_log::EventCategory::Channel)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "session_id": session_id,
                            "active": sessions.len(),
                            "loading": loading.len(),
                            "max": self.acp_config.max_sessions,
                        })),
                    "ACP session/resume rejected: session limit reached"
                );
                return Err(RpcError {
                    code: SESSION_LIMIT_REACHED,
                    message: format!(
                        "Maximum session limit reached ({})",
                        self.acp_config.max_sessions
                    ),
                    data: None,
                });
            }
            if sessions.contains_key(&session_id) || loading.contains(&session_id) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_category(::zeroclaw_log::EventCategory::Channel)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"session_id": session_id})),
                    "ACP session/resume rejected: session already active"
                );
                return Err(RpcError {
                    code: INVALID_PARAMS,
                    message: format!(
                        "Session already active: {session_id}. Call session/close first."
                    ),
                    data: None,
                });
            }
            loading.insert(session_id.clone());
        }

        let data = store
            .load_session(&session_id)
            .map_err(|e| RpcError {
                code: INTERNAL_ERROR,
                message: format!("Failed to load session: {e}"),
                data: None,
            })
            .and_then(|opt| {
                opt.ok_or_else(|| RpcError {
                    code: SESSION_NOT_FOUND,
                    message: format!("Session not found: {session_id}"),
                    data: None,
                })
            });

        // On error (SQLite failure or not-found), release the reservation.
        let data = match data {
            Ok(d) => d,
            Err(e) => {
                self.loading_sessions.lock().await.remove(&session_id);
                return Err(e);
            }
        };

        let workspace_dir = std::path::PathBuf::from(&data.workspace_dir);

        let restore_alias = self
            .config
            .acp
            .default_agent
            .clone()
            .or_else(|| {
                let mut keys = self.config.agents.keys();
                if self.config.agents.len() == 1 {
                    keys.next().cloned()
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "default".to_string());

        let agent_result = Agent::from_config_with_session_cwd_and_mcp_backchannel(
            &self.config,
            &restore_alias,
            Some(&workspace_dir),
            false,
            true,
        )
        .await
        .map_err(|e| RpcError {
            code: INTERNAL_ERROR,
            message: format!("Failed to create agent: {e}"),
            data: None,
        });

        let mut agent = match agent_result {
            Ok(a) => a,
            Err(e) => {
                self.loading_sessions.lock().await.remove(&session_id);
                return Err(e);
            }
        };

        agent.seed_conversation_history(data.messages);

        let acp_channel = Arc::new(AcpChannel::new(
            "acp",
            session_id.clone(),
            Arc::clone(&self.rpc),
            Duration::from_secs(self.acp_config.session_timeout_secs),
        ));
        agent.channel_handles().register_channel("acp", acp_channel);

        let now = Instant::now();
        // Atomically insert and release reservation
        {
            let mut sessions = self.sessions.lock().await;
            let mut loading = self.loading_sessions.lock().await;
            loading.remove(&session_id);
            sessions.insert(
                session_id.clone(),
                Arc::new(Mutex::new(Session {
                    agent,
                    created_at: now,
                    last_active: now,
                    agent_alias: restore_alias.clone(),
                    model_provider: self
                        .config
                        .agent(&restore_alias)
                        .map(|a| a.model_provider.to_string())
                        .unwrap_or_default(),
                    model: self
                        .config
                        .model_provider_for_agent(&restore_alias)
                        .and_then(|mp| mp.model.clone())
                        .unwrap_or_default(),
                })),
            );
        }

        let mp = self
            .config
            .agent(&restore_alias)
            .map(|a| a.model_provider.to_string())
            .unwrap_or_default();
        let model_name = self
            .config
            .model_provider_for_agent(&restore_alias)
            .and_then(|mp| mp.model.clone())
            .unwrap_or_default();
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Start)
                .with_category(::zeroclaw_log::EventCategory::Channel)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(::serde_json::json!({
                    "session_id": session_id,
                    "agent_alias": restore_alias,
                    "model_provider": mp,
                    "model": model_name,
                })),
            "ACP session resumed"
        );
        Ok(serde_json::json!({}))
    }

    /// Handle `session/close` requests (ACP spec §Session Management).
    ///
    /// Closes a session: fires the cancel token to interrupt any in-flight turn,
    /// removes the session from the in-memory map, and unregisters the ACP channel.
    /// The session record in the persistent store is NOT deleted.
    ///
    /// Returns an empty object on success, or SESSION_NOT_FOUND if the session
    /// is not in the in-memory map (it may still exist in the store).
    async fn handle_session_close(&self, params: &Value) -> RpcResult {
        let session_id = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: INVALID_PARAMS,
                message: "Missing required parameter: sessionId".to_string(),
                data: None,
            })?;

        // Fire the cancel token for any in-flight turn before acquiring the session lock.
        let token = self
            .cancel_tokens
            .lock()
            .expect("cancel_tokens lock poisoned — invariant: all guarded critical sections are short, infallible HashMap ops")
            .get(session_id)
            .cloned();
        if let Some(token) = token {
            token.cancel();
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_category(::zeroclaw_log::EventCategory::Channel)
                    .with_attrs(::serde_json::json!({"session_id": session_id})),
                "ACP session/close: cancelled active turn"
            );
        }

        let session_arc = {
            let mut sessions = self.sessions.lock().await;
            sessions.remove(session_id).ok_or_else(|| RpcError {
                code: SESSION_NOT_FOUND,
                message: format!("Session not found: {session_id}"),
                data: None,
            })?
        };

        // Wait for any in-flight turn to finish (the cancel token may have already stopped it).
        let session = session_arc.lock().await;
        let agent_alias = session.agent_alias.clone();
        let model_provider = session.model_provider.clone();
        let model = session.model.clone();
        session.agent.channel_handles().unregister_channel("acp");
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                .with_category(::zeroclaw_log::EventCategory::Channel)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(::serde_json::json!({
                    "session_id": session_id,
                    "agent_alias": agent_alias,
                    "model_provider": model_provider,
                    "model": model,
                })),
            "ACP session closed"
        );

        Ok(serde_json::json!({}))
    }

    fn requested_session_cwd(&self, params: &Value) -> PathBuf {
        params
            .get("cwd")
            .or_else(|| params.get("workspaceDir"))
            .or_else(|| params.get("workspace_dir"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| self.config.data_dir.clone())
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

        // Snapshot attribution fields before releasing the outer lock.
        let (agent_alias, model_provider, model) = {
            // Try-lock: if the inner lock is held by an active turn, we'll
            // reject below via register_cancel_token anyway. Use a brief
            // non-blocking peek so we can log the alias even on the error path.
            if let Ok(s) = session_arc.try_lock() {
                (
                    s.agent_alias.clone(),
                    s.model_provider.clone(),
                    s.model.clone(),
                )
            } else {
                (String::new(), String::new(), String::new())
            }
        };

        // Instrument the rest of the turn so every record! inside lands in
        // the Attribution section of the log viewer with agent_alias,
        // model_provider, and session_key populated.
        // scope! wraps the body with .instrument() internally — no EnteredSpan
        // held across .await points, so the future stays Send.
        // Clone before the macro so the owned values remain available inside
        // the async move block.
        let session_id_s = session_id.clone();
        let agent_alias_s = agent_alias.clone();
        let model_provider_s = model_provider.clone();
        let model_s = model.clone();
        ::zeroclaw_log::scope!(
            agent_alias: agent_alias_s.as_str(),
            model_provider: model_provider_s.as_str(),
            model: model_s.as_str(),
            session_key: session_id_s.as_str(),
            channel: "acp",
        => async move {

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Start).with_category(::zeroclaw_log::EventCategory::Channel)
                .with_attrs(::serde_json::json!({
                    "prompt_len": prompt.len(),
                })),
            "ACP session/prompt turn starting"
        );

        // Create a cancellation token for this turn and register it so that a
        // concurrent `session/cancel` notification can fire it without waiting
        // for the inner session lock (which is held for the full turn duration).
        // The lock can never be poisoned — all critical sections guarded by this
        // mutex are short, infallible HashMap operations (insert/remove/get)
        // that never call user code, panic, or block on I/O.
        let cancel_token = tokio_util::sync::CancellationToken::new();
        self.register_cancel_token(&session_id, cancel_token.clone())?;
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(100);

        // Move the Arc into the spawned task and lock inside it.  The inner
        // Mutex stays locked for the duration of the turn, preventing
        // concurrent stop/reap from touching the agent mid-turn. The outer
        // map entry remains in place.
        let session_id_for_task = session_id.clone();
        let turn_handle = zeroclaw_spawn::spawn!(async move {
            let mut session = session_arc.lock().await;
            let (turn_alias, turn_provider, turn_model) = session.agent.attribution_fields();
            let span_session = session_id_for_task.clone();
            let result = {
                use ::zeroclaw_log::Instrument as _;
                let span = ::zeroclaw_log::info_span!(
                    target: "zeroclaw_log_internal_scope",
                    "zeroclaw_scope",
                    session_key = %span_session,
                    agent_alias = %turn_alias,
                    model_provider = %turn_provider,
                    model = %turn_model,
                    channel = "acp",
                );
                zeroclaw_runtime::agent::loop_::scope_session_key(
                    Some(session_id_for_task),
                    session
                        .agent
                        .turn_streamed(&prompt, event_tx, Some(cancel_token))
                        .instrument(span),
                )
                .await
            };
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
        let mut tool_call_count: u32 = 0;
        while let Some(event) = event_rx.recv().await {
            // ACP has no `session/update` shape for token-usage events; the
            // task-local cost tracker records them out-of-band. We DO use the
            // event to update the per-session `token_count` so the TUI ctx
            // bar resumes accurately. Then skip before dispatching to the
            // notification builder so the helper match can stay exhaustive
            // on the four UI-relevant variants.
            if let TurnEvent::Usage { input_tokens, .. } = &event {
                // Token-count persistence is best-effort UI bookkeeping (it
                // restores the TUI ctx bar on resume). It must never gate the
                // draining of `event_rx`: this loop is the sole consumer of the
                // turn's bounded `event_tx` (capacity 100). The session store
                // wraps a single SQLite connection behind one process-wide
                // mutex, so a concurrent session mid-`append_turn` transaction
                // can stall this write. Awaiting it here would stop draining,
                // fill `event_tx`, and block the agent's unguarded
                // `event_tx.send(...).await` — wedging the turn on "working"
                // with no cancel path. Fire-and-forget keeps the consumer live.
                if let (Some(store), Some(it)) = (&self.store, input_tokens) {
                    let store = store.clone();
                    let sid = session_id.clone();
                    let it = *it;
                    zeroclaw_spawn::spawn!(async move {
                        let persisted =
                            tokio::task::spawn_blocking(move || store.set_token_count(&sid, it))
                                .await;
                        let error = match persisted {
                            Ok(Ok(())) => return,
                            Ok(Err(e)) => e.to_string(),
                            Err(join) => join.to_string(),
                        };
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Write,
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "input_tokens": it,
                                "error": error,
                            })),
                            "Failed to persist ACP session token_count"
                        );
                    });
                }
                continue;
            }
            // Emit attributable span logs for every tool call and result.
            // Attribution (agent_alias, model_provider, session_key) flows
            // from the enclosing spans — not repeated here in attrs.
            match &event {
                TurnEvent::ToolCall { id, name, args } => {
                    tool_call_count += 1;
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Start).with_category(::zeroclaw_log::EventCategory::Channel)
                            .with_attrs(::serde_json::json!({
                                "tool_call_id": id,
                                "tool": name,
                                "args_len": args.to_string().len(),
                            })),
                        "ACP tool call dispatched"
                    );
                }
                TurnEvent::ToolResult { id, name, output } => {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete).with_category(::zeroclaw_log::EventCategory::Channel)
                            .with_outcome(::zeroclaw_log::EventOutcome::Success)
                            .with_attrs(::serde_json::json!({
                                "tool_call_id": id,
                                "tool": name,
                                "output_len": output.len(),
                            })),
                        "ACP tool call completed"
                    );
                }
                TurnEvent::Chunk { delta } => {
                    accumulated_text.push_str(delta);
                }
                _ => {}
            }
            if let Some(notification) = notification_for_turn_event(&session_id, &event) {
                self.write_notification(&notification).await;
            }
        }

        // Remove the cancel token regardless of outcome — the turn is over.
        // Lock poisoned invariant: same as the insert site above.
        self.remove_cancel_token(&session_id);

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
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete).with_category(::zeroclaw_log::EventCategory::Channel)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "tool_calls": tool_call_count,
                        "stop_reason": "cancelled",
                    })),
                "ACP session/prompt turn cancelled"
            );
            return Ok(Self::cancelled_prompt_result(session_id, &accumulated_text));
        }

        let (result_text, new_turn_msgs) = turn_result.map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail).with_category(::zeroclaw_log::EventCategory::Channel)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "error": e.to_string(),
                    })),
                "ACP session/prompt turn failed"
            );
            RpcError {
                code: INTERNAL_ERROR,
                message: format!("Agent turn failed: {e}"),
                data: None,
            }
        })?;

        // Persist new messages on successful, non-cancelled turns.
        if let Some(store) = &self.store
            && !new_turn_msgs.is_empty()
        {
            let store = store.clone();
            let sid = session_id.clone();
            let msgs = new_turn_msgs;
            let persisted =
                tokio::task::spawn_blocking(move || store.append_turn(&sid, &msgs)).await;
            let error = match persisted {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e.to_string()),
                Err(join) => Some(join.to_string()),
            };
            if let Some(detail) = error {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_category(::zeroclaw_log::EventCategory::Channel)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "error": detail,
                        })),
                    "Failed to persist turn; session continues in memory"
                );
            }
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete).with_category(::zeroclaw_log::EventCategory::Channel)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(::serde_json::json!({
                    "tool_calls": tool_call_count,
                    "response_len": result_text.len(),
                    "stop_reason": "end_turn",
                })),
            "ACP session/prompt turn complete"
        );

        Ok(Self::prompt_result(session_id, "end_turn", result_text))

        }).await
    }

    fn register_cancel_token(
        &self,
        session_id: &str,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> std::result::Result<(), RpcError> {
        let mut tokens = self
            .cancel_tokens
            .lock()
            .expect("cancel_tokens lock poisoned — invariant: all guarded critical sections are short, infallible HashMap ops");
        if tokens.contains_key(session_id) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_category(::zeroclaw_log::EventCategory::Channel)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"session_id": session_id})),
                "ACP session/prompt rejected: session already has an active turn"
            );
            return Err(RpcError {
                code: SESSION_BUSY,
                message: format!("Session already has an active prompt turn: {session_id}"),
                data: None,
            });
        }
        tokens.insert(session_id.to_string(), cancel_token);
        Ok(())
    }

    fn remove_cancel_token(&self, session_id: &str) {
        self.cancel_tokens
            .lock()
            .expect("cancel_tokens lock poisoned — invariant: all guarded critical sections are short, infallible HashMap ops")
            .remove(session_id);
    }

    fn prompt_result(session_id: String, stop_reason: &'static str, text: String) -> Value {
        serde_json::json!({
            "sessionId": session_id,
            "stopReason": stop_reason,
            "content": text,
        })
    }

    fn cancelled_prompt_result(session_id: String, accumulated_text: &str) -> Value {
        let marker = zeroclaw_runtime::i18n::get_required_cli_string("turn-interrupted-by-user");
        let content = if accumulated_text.is_empty() {
            marker
        } else {
            format!("{accumulated_text}\n\n{marker}")
        };
        Self::prompt_result(session_id, "cancelled", content)
    }

    fn parse_prompt(params: &Value) -> std::result::Result<String, RpcError> {
        match params.get("prompt") {
            Some(Value::String(s)) => Ok(s.clone()),
            Some(Value::Array(arr)) => {
                let mut joined = String::new();
                for part in arr {
                    let mut added = false;
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        if !joined.is_empty() {
                            joined.push_str("\n\n");
                        }
                        joined.push_str(text);
                        added = true;
                    }
                    // Support ACP resource blocks for @-notation file attachments
                    // (clients send {"type":"resource","resource":{"uri":"...","text":"..."}})
                    if let Some(res) = part.get("resource")
                        && let Some(text) = res.get("text").and_then(|v| v.as_str())
                    {
                        if added || !joined.is_empty() {
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
        let agent_alias = session.agent_alias.clone();
        let model_provider = session.model_provider.clone();
        let model = session.model.clone();
        // Drop the ACP back-channel from each tool's channel map so the
        // session's RpcOutbound clone isn't kept alive by stale entries.
        session.agent.channel_handles().unregister_channel("acp");
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                .with_category(::zeroclaw_log::EventCategory::Channel)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(::serde_json::json!({
                    "session_id": session_id,
                    "agent_alias": agent_alias,
                    "model_provider": model_provider,
                    "model": model,
                })),
            "ACP session stopped"
        );
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
    ///
    /// Cancel-vs-stop interaction: if `session/cancel` and `session/stop` fire
    /// nearly simultaneously, both handlers race — cancel fires the token
    /// (which may or may not interrupt the turn), and stop sets
    /// `session.stopped = true` and awaits the turn handle. The net effect is
    /// harmless: either the turn sees the cancellation token or it doesn't, and
    /// stop always waits for the turn to finish.
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
            .expect("cancel_tokens lock poisoned — invariant: all guarded critical sections are short, infallible HashMap ops")
            .get(session_id)
            .cloned();

        if let Some(token) = token {
            token.cancel();
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_category(::zeroclaw_log::EventCategory::Channel)
                    .with_attrs(::serde_json::json!({"session_id": session_id})),
                "ACP session/cancel: fired cancel token for active turn"
            );
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

        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_category(::zeroclaw_log::EventCategory::Channel)
                .with_attrs(
                    ::serde_json::json!({"event_type": event_type, "session_id": session_id})
                ),
            "Received session update (type=) for session"
        );

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
                if !self.rpc.send_raw(json).await {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_category(::zeroclaw_log::EventCategory::Channel)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "ACP writer task closed; dropping outbound message"
                    );
                }
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_category(::zeroclaw_log::EventCategory::Channel)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Failed to serialize JSON-RPC message"
                );
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
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_category(::zeroclaw_log::EventCategory::Channel)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "Failed to write to stdout"
            );
            continue;
        }
        if let Err(e) = stdout.write_all(b"\n").await {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_category(::zeroclaw_log::EventCategory::Channel)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "Failed to write newline to stdout"
            );
            continue;
        }
        if let Err(e) = stdout.flush().await {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_category(::zeroclaw_log::EventCategory::Channel)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "Failed to flush stdout"
            );
        }
    }
}

/// Translate tool args into the ACP `rawInput` shape.
///
/// For file-editing tools, the ACP Diff schema uses `oldText`/`newText` (camelCase).
/// ZeroClaw's internal tool args use `old_string`/`new_string` (snake_case) for
/// `file_edit` and `content` for `file_write`. Without this translation, ACP clients
/// (Toad, Zed) cannot recognise the Diff shape and fall back to rendering the raw JSON
/// fields as giant strings.
fn to_acp_raw_input(name: &str, args: &Value) -> Value {
    match name {
        "file_edit" => {
            let path = args.get("path").cloned().unwrap_or(Value::Null);
            let old_text = args.get("old_string").cloned().unwrap_or(Value::Null);
            let new_text = args.get("new_string").cloned().unwrap_or(Value::Null);
            serde_json::json!({ "path": path, "oldText": old_text, "newText": new_text })
        }
        "file_write" => {
            let path = args.get("path").cloned().unwrap_or(Value::Null);
            let new_text = args.get("content").cloned().unwrap_or(Value::Null);
            serde_json::json!({ "path": path, "newText": new_text })
        }
        _ => args.clone(),
    }
}

/// Build the ACP `content` array for a tool call notification.
///
/// Zed and Toad render tool call content from the `content` array. For
/// file-editing tools, emit an ACP Diff content item (`{ "type": "diff", ... }`)
/// so clients show a side-by-side diff editor. Non-edit tools return an empty
/// array — their `rawInput` is displayed via the standard `raw_input` fallback.
fn to_acp_content(name: &str, args: &Value) -> Value {
    match name {
        "file_edit" => {
            let path = args.get("path").cloned().unwrap_or(Value::Null);
            let old_text = args.get("old_string").cloned().unwrap_or(Value::Null);
            let new_text = args.get("new_string").cloned().unwrap_or(Value::Null);
            serde_json::json!([{ "type": "diff", "path": path, "oldText": old_text, "newText": new_text }])
        }
        "file_write" => {
            let path = args.get("path").cloned().unwrap_or(Value::Null);
            let new_text = args.get("content").cloned().unwrap_or(Value::Null);
            serde_json::json!([{ "type": "diff", "path": path, "newText": new_text }])
        }
        _ => serde_json::json!([]),
    }
}

fn map_tool_kind(name: &str) -> &'static str {
    match name {
        "ask_user" | "calculator" | "claude_code" | "claude_code_runner" | "codex_cli"
        | "composio" | "delegate" | "escalate_to_human" | "execute_pipeline" | "gemini_cli"
        | "jira" | "llm_task" | "opencode_cli" | "schedule" | "security_ops" | "shell"
        | "sop_advance" | "sop_approve" | "sop_execute" | "vi_verify" => "execute",
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

fn notification_for_turn_event(session_id: &str, event: &TurnEvent) -> Option<JsonRpcNotification> {
    Some(match event {
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
        TurnEvent::ToolCall { id, name, args } => {
            let acp_content = to_acp_content(name, args);
            let mut update = serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": id,
                "name": name,
                "title": name,
                "kind": map_tool_kind(name),
                "rawInput": to_acp_raw_input(name, args),
                "status": "pending"
            });
            if acp_content
                .as_array()
                .is_some_and(|items| !items.is_empty())
            {
                update["content"] = acp_content;
            }
            JsonRpcNotification {
                jsonrpc: "2.0",
                method: "session/update",
                params: serde_json::json!({
                    "sessionId": session_id,
                    "update": update
                }),
            }
        }
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
        // ACP has its own approval mechanism via `session/request_permission`
        // routed through the channel's `request_choice` impl. The agent only
        // emits ApprovalRequest events when a back-channel like the gateway
        // WS is registered to handle them; on ACP-only sessions they should
        // not arrive here.
        TurnEvent::ApprovalRequest { .. } => return None,
        // Usage events are filtered out at every call site (ACP has no
        // `session/update` shape for them; the cost tracker records them
        // out-of-band). Reaching this arm means a caller forgot the filter.
        TurnEvent::Usage { .. } => unreachable!(
            "TurnEvent::Usage must be filtered before notification_for_turn_event; \
             ACP has no session/update notification for token usage"
        ),
    })
}

fn history_notifications_for_message(
    session_id: &str,
    msg: &ConversationMessage,
) -> Vec<JsonRpcNotification> {
    match msg {
        ConversationMessage::Chat(chat) => {
            let update_type = match chat.role.as_str() {
                "user" => "user_message_chunk",
                "assistant" => "agent_message_chunk",
                _ => return vec![],
            };
            vec![JsonRpcNotification {
                jsonrpc: "2.0",
                method: "session/update",
                params: serde_json::json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": update_type,
                        "content": { "type": "text", "text": &chat.content }
                    }
                }),
            }]
        }
        ConversationMessage::AssistantToolCalls {
            text, tool_calls, ..
        } => {
            let mut notifications = Vec::new();
            if let Some(t) = text
                && !t.is_empty()
            {
                notifications.push(JsonRpcNotification {
                    jsonrpc: "2.0",
                    method: "session/update",
                    params: serde_json::json!({
                        "sessionId": session_id,
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": { "type": "text", "text": t }
                        }
                    }),
                });
            }
            for tc in tool_calls {
                let args: serde_json::Value =
                    serde_json::from_str(&tc.arguments).unwrap_or(serde_json::Value::Null);
                let acp_content = to_acp_content(&tc.name, &args);
                let mut update = serde_json::json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": &tc.id,
                    "name": &tc.name,
                    "title": &tc.name,
                    "kind": map_tool_kind(&tc.name),
                    "rawInput": to_acp_raw_input(&tc.name, &args),
                    "status": "completed"
                });
                if acp_content
                    .as_array()
                    .is_some_and(|items| !items.is_empty())
                {
                    update["content"] = acp_content;
                }
                notifications.push(JsonRpcNotification {
                    jsonrpc: "2.0",
                    method: "session/update",
                    params: serde_json::json!({
                        "sessionId": session_id,
                        "update": update
                    }),
                });
            }
            notifications
        }
        ConversationMessage::ToolResults(results) => results
            .iter()
            .map(|r| JsonRpcNotification {
                jsonrpc: "2.0",
                method: "session/update",
                params: serde_json::json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": &r.tool_call_id,
                        "status": "completed",
                        "rawOutput": &r.content,
                        "body": &r.content,
                        "content": [{
                            "type": "content",
                            "content": { "type": "text", "text": &r.content }
                        }]
                    }
                }),
            })
            .collect(),
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
        let rpc = RpcOutbound::new(tx);

        let result = tokio::time::timeout(
            Duration::from_millis(10),
            rpc.request("session/request_permission", serde_json::json!({})),
        )
        .await;

        assert!(result.is_err());
        assert!(rx.recv().await.is_some());
        assert_eq!(rpc.pending_count(), 0);
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
    fn initialize_advertises_load_session_when_store_present() {
        let cwd = tempfile::tempdir().unwrap();
        let store =
            Arc::new(zeroclaw_infra::acp_session_store::AcpSessionStore::new(cwd.path()).unwrap());
        let server = AcpServer::new_with_store(
            make_test_config(cwd.path()),
            AcpServerConfig::default(),
            store,
        );
        let result = server.handle_initialize(&serde_json::json!({})).unwrap();
        assert_eq!(result["agentCapabilities"]["loadSession"], true);
        assert_eq!(
            result["agentCapabilities"]["sessionCapabilities"]["resume"],
            serde_json::json!({})
        );
        assert_eq!(
            result["agentCapabilities"]["sessionCapabilities"]["close"],
            serde_json::json!({})
        );
    }

    #[test]
    fn session_new_defaults_to_launch_cwd_when_client_omits_cwd() {
        let config = Config {
            data_dir: PathBuf::from("/not/the/project"),
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
        let mut config = Config {
            data_dir: cwd.path().to_path_buf(),
            providers: {
                let mut p = zeroclaw_config::providers::Providers::default();
                p.models.openrouter.insert(
                    "default".to_string(),
                    zeroclaw_config::schema::OpenRouterModelProviderConfig {
                        base: zeroclaw_config::schema::ModelProviderConfig {
                            model: Some("test-model".to_string()),
                            ..Default::default()
                        },
                    },
                );
                p
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
        config.risk_profiles.insert(
            "default".to_string(),
            zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        config.agents.insert(
            "test-agent".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "openrouter.default".into(),
                risk_profile: "default".into(),
                ..Default::default()
            },
        );
        let server = AcpServer::new(config, AcpServerConfig::default());

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            server.handle_session_new(&serde_json::json!({
                "cwd": cwd.path().to_string_lossy(),
                "agentAlias": "test-agent",
                "mcpServers": []
            })),
        )
        .await
        .expect("session/new should not block on configured MCP startup")
        .expect("session/new should create a session");

        assert!(result["sessionId"].as_str().is_some());
    }

    #[tokio::test]
    async fn session_new_auto_selects_sole_configured_agent_when_alias_omitted() {
        let cwd = tempfile::tempdir().unwrap();
        let mut config = Config {
            data_dir: cwd.path().to_path_buf(),
            providers: {
                let mut p = zeroclaw_config::providers::Providers::default();
                p.models.openrouter.insert(
                    "default".to_string(),
                    zeroclaw_config::schema::OpenRouterModelProviderConfig {
                        base: zeroclaw_config::schema::ModelProviderConfig {
                            api_key: Some("test-key".to_string()),
                            model: Some("test-model".to_string()),
                            ..Default::default()
                        },
                    },
                );
                p
            },
            ..Default::default()
        };
        config.risk_profiles.insert(
            "default".to_string(),
            zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        config.agents.insert(
            "only-agent".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "openrouter.default".into(),
                risk_profile: "default".into(),
                ..Default::default()
            },
        );
        let server = AcpServer::new(config, AcpServerConfig::default());

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            server.handle_session_new(&serde_json::json!({
                "cwd": cwd.path().to_string_lossy(),
                "mcpServers": []
            })),
        )
        .await
        .expect("session/new should not block")
        .expect("session/new should auto-select the sole configured agent");

        assert!(result["sessionId"].as_str().is_some());
    }

    #[tokio::test]
    async fn session_new_requires_alias_when_multiple_agents_configured() {
        let mut config = Config::default();
        config.agents.insert(
            "agent-one".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig::default(),
        );
        config.agents.insert(
            "agent-two".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig::default(),
        );
        let server = AcpServer::new(config, AcpServerConfig::default());

        let err = server
            .handle_session_new(&serde_json::json!({"mcpServers": []}))
            .await
            .expect_err("session/new without agentAlias should fail when multiple agents exist");

        assert_eq!(err.code, INVALID_PARAMS);
        assert!(
            err.message.contains("agentAlias"),
            "error should mention agentAlias, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn session_new_uses_config_default_agent_when_alias_omitted_and_multiple_agents() {
        let cwd = tempfile::tempdir().unwrap();
        let mut config = Config {
            data_dir: cwd.path().to_path_buf(),
            providers: {
                let mut p = zeroclaw_config::providers::Providers::default();
                p.models.openrouter.insert(
                    "default".to_string(),
                    zeroclaw_config::schema::OpenRouterModelProviderConfig {
                        base: zeroclaw_config::schema::ModelProviderConfig {
                            api_key: Some("test-key".to_string()),
                            model: Some("test-model".to_string()),
                            ..Default::default()
                        },
                    },
                );
                p
            },
            ..Default::default()
        };
        config.risk_profiles.insert(
            "default".to_string(),
            zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        config.agents.insert(
            "agent-alpha".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "openrouter.default".into(),
                risk_profile: "default".into(),
                ..Default::default()
            },
        );
        config.agents.insert(
            "agent-beta".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "openrouter.default".into(),
                risk_profile: "default".into(),
                ..Default::default()
            },
        );
        config.acp.default_agent = Some("agent-alpha".to_string());
        let server = AcpServer::new(config, AcpServerConfig::default());

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            server.handle_session_new(&serde_json::json!({
                "cwd": cwd.path().to_string_lossy(),
                "mcpServers": []
            })),
        )
        .await
        .expect("should not block")
        .expect("should select agent-alpha from config.acp.default_agent");

        assert!(result["sessionId"].as_str().is_some());
    }

    #[tokio::test]
    async fn session_new_explicit_alias_overrides_config_default_agent() {
        let cwd = tempfile::tempdir().unwrap();
        let mut config = Config {
            data_dir: cwd.path().to_path_buf(),
            providers: {
                let mut p = zeroclaw_config::providers::Providers::default();
                p.models.openrouter.insert(
                    "default".to_string(),
                    zeroclaw_config::schema::OpenRouterModelProviderConfig {
                        base: zeroclaw_config::schema::ModelProviderConfig {
                            api_key: Some("test-key".to_string()),
                            model: Some("test-model".to_string()),
                            ..Default::default()
                        },
                    },
                );
                p
            },
            ..Default::default()
        };
        config.risk_profiles.insert(
            "default".to_string(),
            zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        config.agents.insert(
            "agent-alpha".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "openrouter.default".into(),
                risk_profile: "default".into(),
                ..Default::default()
            },
        );
        config.agents.insert(
            "agent-beta".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "openrouter.default".into(),
                risk_profile: "default".into(),
                ..Default::default()
            },
        );
        config.acp.default_agent = Some("agent-alpha".to_string());
        let server = AcpServer::new(config, AcpServerConfig::default());

        // Explicit alias should win over config default
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            server.handle_session_new(&serde_json::json!({
                "agentAlias": "agent-beta",
                "cwd": cwd.path().to_string_lossy(),
                "mcpServers": []
            })),
        )
        .await
        .expect("should not block")
        .expect("should use agent-beta despite default_agent = agent-alpha");

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

        // Array prompt with resource (file @-notation from ACP client)
        let resource_params = serde_json::json!({
            "prompt": [
                {"type": "text", "text": "analyze this file:"},
                {"type": "resource", "resource": {"uri": "file:///tmp/example.rs", "text": "fn main() { println!(\"hi\"); }", "mimeType": "text/rust"}}
            ]
        });
        let result = AcpServer::parse_prompt(&resource_params).unwrap();
        assert!(result.contains("analyze this file:"));
        assert!(result.contains("fn main() { println!(\"hi\"); }"));
    }

    #[test]
    fn handle_initialize_default_model_absent_when_unconfigured() {
        let server = AcpServer::new(Config::default(), AcpServerConfig::default());
        let result = server.handle_initialize(&serde_json::json!({})).unwrap();
        assert!(
            result["_meta"]["zeroclaw"].get("defaultModel").is_none(),
            "defaultModel must be absent when no model_provider is configured, got: {}",
            result["_meta"]["zeroclaw"]["defaultModel"]
        );
    }

    #[test]
    fn handle_initialize_default_model_reflects_configured_provider() {
        use zeroclaw_config::schema::{ModelProviderConfig, OllamaModelProviderConfig};
        let mut config = Config::default();
        config.providers.models.ollama.insert(
            "default".to_string(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("llama3.2".to_string()),
                    ..Default::default()
                },
                ..OllamaModelProviderConfig::default()
            },
        );
        let server = AcpServer::new(config, AcpServerConfig::default());
        let result = server.handle_initialize(&serde_json::json!({})).unwrap();
        assert_eq!(result["_meta"]["zeroclaw"]["defaultModel"], "llama3.2");
    }

    #[test]
    fn prompt_result_preserves_content_string_shape() {
        let result = AcpServer::prompt_result("test-sid".to_string(), "end_turn", "hello".into());
        assert_eq!(result["sessionId"], "test-sid");
        assert_eq!(result["stopReason"], "end_turn");
        assert_eq!(result["content"], "hello");
    }

    #[test]
    fn cancelled_prompt_result_preserves_content_string_shape() {
        let with_partial =
            AcpServer::cancelled_prompt_result("test-sid".to_string(), "partial text");
        assert_eq!(with_partial["sessionId"], "test-sid");
        assert_eq!(with_partial["stopReason"], "cancelled");
        assert_eq!(
            with_partial["content"],
            format!(
                "partial text\n\n{}",
                zeroclaw_runtime::i18n::get_required_cli_string("turn-interrupted-by-user")
            )
        );

        let marker_only = AcpServer::cancelled_prompt_result("test-sid".to_string(), "");
        assert_eq!(
            marker_only["content"],
            zeroclaw_runtime::i18n::get_required_cli_string("turn-interrupted-by-user")
        );
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
    fn file_edit_raw_input_uses_acp_diff_field_names() {
        let call = notification_for_turn_event(
            "sid",
            &TurnEvent::ToolCall {
                id: "tc-1".to_string(),
                name: "file_edit".to_string(),
                args: serde_json::json!({
                    "path": "src/foo.rs",
                    "old_string": "let x = 1;",
                    "new_string": "let x = 2;"
                }),
            },
        );
        let v = serde_json::to_value(call.unwrap()).unwrap();
        let raw = &v["params"]["update"]["rawInput"];
        assert_eq!(raw["path"], "src/foo.rs");
        assert_eq!(raw["oldText"], "let x = 1;");
        assert_eq!(raw["newText"], "let x = 2;");
        assert!(
            raw.get("old_string").is_none(),
            "old_string must not appear in rawInput"
        );
        assert!(
            raw.get("new_string").is_none(),
            "new_string must not appear in rawInput"
        );

        let content = &v["params"]["update"]["content"];
        assert!(content.is_array(), "file_edit must emit a content array");
        let diff = &content[0];
        assert_eq!(diff["type"], "diff");
        assert_eq!(diff["path"], "src/foo.rs");
        assert_eq!(diff["oldText"], "let x = 1;");
        assert_eq!(diff["newText"], "let x = 2;");
    }

    #[test]
    fn file_write_raw_input_uses_acp_diff_field_names() {
        let call = notification_for_turn_event(
            "sid",
            &TurnEvent::ToolCall {
                id: "tc-2".to_string(),
                name: "file_write".to_string(),
                args: serde_json::json!({
                    "path": "src/new.rs",
                    "content": "fn main() {}"
                }),
            },
        );
        let v = serde_json::to_value(call.unwrap()).unwrap();
        let raw = &v["params"]["update"]["rawInput"];
        assert_eq!(raw["path"], "src/new.rs");
        assert_eq!(raw["newText"], "fn main() {}");
        assert!(
            raw.get("oldText").is_none(),
            "oldText must not appear in file_write rawInput"
        );
        assert!(
            raw.get("content").is_none(),
            "content must not appear in rawInput"
        );

        let content = &v["params"]["update"]["content"];
        assert!(content.is_array(), "file_write must emit a content array");
        let diff = &content[0];
        assert_eq!(diff["type"], "diff");
        assert_eq!(diff["path"], "src/new.rs");
        assert_eq!(diff["newText"], "fn main() {}");
        assert!(
            diff.get("oldText").is_none(),
            "oldText must be absent for file_write diff"
        );
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
        let call_value =
            serde_json::to_value(call.expect("ToolCall maps to a notification")).unwrap();
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
        let result_value =
            serde_json::to_value(result.expect("ToolResult maps to a notification")).unwrap();
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
        let mut config = Config {
            data_dir: cwd.path().to_path_buf(),
            providers: {
                let mut p = zeroclaw_config::providers::Providers::default();
                p.models.anthropic.insert(
                    "default".to_string(),
                    zeroclaw_config::schema::AnthropicModelProviderConfig {
                        base: zeroclaw_config::schema::ModelProviderConfig {
                            model: Some("claude-haiku-4-5".to_string()),
                            ..Default::default()
                        },
                    },
                );
                p
            },
            ..Default::default()
        };
        config.risk_profiles.insert(
            "default".to_string(),
            zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        config.agents.insert(
            "test-agent".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "anthropic.default".into(),
                risk_profile: "default".into(),
                ..Default::default()
            },
        );
        let server = Arc::new(AcpServer::new(config, AcpServerConfig::default()));

        // Create a real session via the normal path.
        let new_result = server
            .handle_session_new(&serde_json::json!({
                "cwd": cwd.path().to_string_lossy(),
                "agentAlias": "test-agent"
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

    #[tokio::test]
    async fn session_new_persists_to_store() {
        let cwd = tempfile::tempdir().unwrap();
        let store =
            Arc::new(zeroclaw_infra::acp_session_store::AcpSessionStore::new(cwd.path()).unwrap());
        let server = Arc::new(AcpServer::new_with_store(
            make_test_config(cwd.path()),
            AcpServerConfig::default(),
            Arc::clone(&store),
        ));

        let result = server
            .handle_session_new(&serde_json::json!({
                "cwd": cwd.path().to_string_lossy()
            }))
            .await
            .expect("session/new must succeed");

        let session_id = result["sessionId"].as_str().unwrap();

        // Session must appear in the store
        let data = store.load_session(session_id).unwrap();
        assert!(
            data.is_some(),
            "session/new must persist to AcpSessionStore"
        );
    }

    #[tokio::test]
    async fn session_new_without_store_still_works() {
        let cwd = tempfile::tempdir().unwrap();
        let server = Arc::new(AcpServer::new(
            make_test_config(cwd.path()),
            AcpServerConfig::default(),
        ));

        let result = server
            .handle_session_new(&serde_json::json!({
                "cwd": cwd.path().to_string_lossy()
            }))
            .await
            .expect("session/new must succeed without a store");

        let session_id = result["sessionId"].as_str().unwrap();
        assert!(server.sessions.lock().await.contains_key(session_id));
    }

    fn make_test_config(cwd: &std::path::Path) -> Config {
        let mut cfg = Config {
            data_dir: cwd.to_path_buf(),
            ..Default::default()
        };
        cfg.providers.models.anthropic.insert(
            "default".to_string(),
            zeroclaw_config::schema::AnthropicModelProviderConfig {
                base: zeroclaw_config::schema::ModelProviderConfig {
                    model: Some("claude-haiku-4-5".to_string()),
                    ..Default::default()
                },
            },
        );
        cfg.risk_profiles.insert(
            "default".to_string(),
            zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        cfg.agents.insert(
            "test-agent".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "anthropic.default".into(),
                risk_profile: "default".into(),
                ..Default::default()
            },
        );
        cfg
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
                "cwd": cwd.path().to_string_lossy(),
                "agentAlias": "test-agent"
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
        assert!(
            result.is_ok(),
            "unknown-session cancel must succeed: {result:?}"
        );
    }

    #[tokio::test]
    async fn session_cancel_accepts_snake_case_session_id() {
        let cwd = tempfile::tempdir().unwrap();
        let server = Arc::new(AcpServer::new(
            make_test_config(cwd.path()),
            AcpServerConfig::default(),
        ));

        let session_id = "sess_snake_case_cancel";
        let active_token = tokio_util::sync::CancellationToken::new();
        server
            .register_cancel_token(session_id, active_token.clone())
            .expect("active turn should register token");

        server
            .handle_session_cancel(&serde_json::json!({ "session_id": session_id }))
            .await
            .expect("snake_case session_id should cancel the active turn");

        assert!(active_token.is_cancelled());
    }

    /// A second prompt for the same session must fail before it can overwrite
    /// the active turn's cancellation token.
    #[tokio::test]
    async fn register_cancel_token_rejects_concurrent_prompt_for_session() {
        let cwd = tempfile::tempdir().unwrap();
        let server = Arc::new(AcpServer::new(
            make_test_config(cwd.path()),
            AcpServerConfig::default(),
        ));

        let session_id = "sess_active_turn";
        let active_token = tokio_util::sync::CancellationToken::new();
        let queued_token = tokio_util::sync::CancellationToken::new();

        server
            .register_cancel_token(session_id, active_token.clone())
            .expect("first prompt should register its token");
        let err = server
            .register_cancel_token(session_id, queued_token.clone())
            .expect_err("second prompt must not overwrite active token");

        assert_eq!(err.code, SESSION_BUSY);
        assert!(
            err.message.contains("active prompt turn"),
            "error should explain why prompt was rejected: {}",
            err.message
        );

        server
            .handle_session_cancel(&serde_json::json!({ "sessionId": session_id }))
            .await
            .expect("cancel should still target active token");

        assert!(active_token.is_cancelled());
        assert!(
            !queued_token.is_cancelled(),
            "rejected prompt's token must not become the active cancel target"
        );
    }

    #[tokio::test]
    async fn session_prompt_rejects_concurrent_turn_before_agent_starts() {
        let cwd = tempfile::tempdir().unwrap();
        let server = Arc::new(AcpServer::new(
            make_test_config(cwd.path()),
            AcpServerConfig::default(),
        ));

        let new_result = server
            .handle_session_new(&serde_json::json!({
                "cwd": cwd.path().to_string_lossy(),
                "agentAlias": "test-agent"
            }))
            .await
            .expect("session/new must succeed");
        let session_id = new_result["sessionId"].as_str().unwrap().to_string();
        let active_token = tokio_util::sync::CancellationToken::new();
        server
            .register_cancel_token(&session_id, active_token.clone())
            .expect("simulated active turn should register token");

        let err = server
            .handle_session_prompt(
                &serde_json::json!({
                    "sessionId": session_id.clone(),
                    "prompt": "queued prompt"
                }),
                &serde_json::json!(2),
            )
            .await
            .expect_err("concurrent prompt must be rejected before model_provider work starts");

        assert_eq!(err.code, SESSION_BUSY);
        server
            .handle_session_cancel(&serde_json::json!({ "sessionId": session_id }))
            .await
            .expect("cancel should still target the original active token");
        assert!(active_token.is_cancelled());
    }

    /// Verify that inserting and removing a cancel token from the map works
    /// correctly. This tests map mechanics directly rather than the
    /// `handle_session_prompt` lifecycle, so a regression in the production
    /// path's cleanup wouldn't be caught by this test.
    #[tokio::test]
    async fn cancel_tokens_map_remove_works() {
        let cwd = tempfile::tempdir().unwrap();
        let config = Config {
            data_dir: cwd.path().to_path_buf(),
            ..Default::default()
        };
        let server = Arc::new(AcpServer::new(config, AcpServerConfig::default()));

        // Insert and remove a token directly.
        let session_id = "sess_token_leak_test".to_string();
        let token = tokio_util::sync::CancellationToken::new();
        server
            .cancel_tokens
            .lock()
            .expect("cancel_tokens lock poisoned")
            .insert(session_id.clone(), token);

        // Remove the token.
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

    #[tokio::test]
    async fn session_load_restores_history_and_streams_notifications() {
        use zeroclaw_api::model_provider::{ChatMessage, ConversationMessage};
        let cwd = tempfile::tempdir().unwrap();
        let store =
            Arc::new(zeroclaw_infra::acp_session_store::AcpSessionStore::new(cwd.path()).unwrap());

        let session_id = "sess-load-test";
        store
            .create_session(session_id, "test-agent", &cwd.path().to_string_lossy())
            .unwrap();
        store
            .append_turn(
                session_id,
                &[
                    ConversationMessage::Chat(ChatMessage::user("hello")),
                    ConversationMessage::Chat(ChatMessage::assistant("hi there")),
                ],
            )
            .unwrap();

        let (writer_tx, mut writer_rx) = tokio::sync::mpsc::channel::<String>(64);
        let server = Arc::new(AcpServer::new_with_writer_and_store(
            make_test_config(cwd.path()),
            AcpServerConfig::default(),
            writer_tx,
            Arc::clone(&store),
        ));

        let result = server
            .handle_session_load(&serde_json::json!({
                "sessionId": session_id,
                "cwd": cwd.path().to_string_lossy()
            }))
            .await
            .expect("session/load must succeed");

        assert_eq!(result, serde_json::json!({}));

        // Session must now be in the in-memory map
        assert!(server.sessions.lock().await.contains_key(session_id));

        // Collect notifications (non-blocking drain)
        let mut notifications = Vec::new();
        while let Ok(msg) = writer_rx.try_recv() {
            notifications.push(msg);
        }

        // Expect two session/update notifications: user then assistant
        assert_eq!(
            notifications.len(),
            2,
            "expected 2 notifications, got: {notifications:?}"
        );
        let n0: serde_json::Value = serde_json::from_str(&notifications[0]).unwrap();
        assert_eq!(
            n0["params"]["update"]["sessionUpdate"],
            "user_message_chunk"
        );
        assert_eq!(n0["params"]["update"]["content"]["text"], "hello");
        let n1: serde_json::Value = serde_json::from_str(&notifications[1]).unwrap();
        assert_eq!(
            n1["params"]["update"]["sessionUpdate"],
            "agent_message_chunk"
        );
        assert_eq!(n1["params"]["update"]["content"]["text"], "hi there");
    }

    #[tokio::test]
    async fn session_load_returns_not_found_for_unknown_id() {
        let cwd = tempfile::tempdir().unwrap();
        let store =
            Arc::new(zeroclaw_infra::acp_session_store::AcpSessionStore::new(cwd.path()).unwrap());
        let (writer_tx, _rx) = tokio::sync::mpsc::channel::<String>(8);
        let server = AcpServer::new_with_writer_and_store(
            make_test_config(cwd.path()),
            AcpServerConfig::default(),
            writer_tx,
            store,
        );

        let err = server
            .handle_session_load(&serde_json::json!({ "sessionId": "ghost" }))
            .await
            .expect_err("unknown session must fail");

        assert_eq!(err.code, SESSION_NOT_FOUND);
    }

    #[tokio::test]
    async fn session_load_rejects_already_active_session() {
        let cwd = tempfile::tempdir().unwrap();
        let store =
            Arc::new(zeroclaw_infra::acp_session_store::AcpSessionStore::new(cwd.path()).unwrap());
        let (writer_tx, _rx) = tokio::sync::mpsc::channel::<String>(8);
        let server = Arc::new(AcpServer::new_with_writer_and_store(
            make_test_config(cwd.path()),
            AcpServerConfig::default(),
            writer_tx,
            Arc::clone(&store),
        ));

        // Create and load the session once to put it in memory
        let session_id = "sess-already-active";
        store
            .create_session(session_id, "test-agent", &cwd.path().to_string_lossy())
            .unwrap();
        server
            .handle_session_load(&serde_json::json!({
                "sessionId": session_id,
                "cwd": cwd.path().to_string_lossy()
            }))
            .await
            .unwrap();

        // Second load must be rejected
        let err = server
            .handle_session_load(&serde_json::json!({ "sessionId": session_id }))
            .await
            .expect_err("session/load for active session must fail");

        assert_eq!(err.code, INVALID_PARAMS);
    }

    #[tokio::test]
    async fn session_resume_restores_without_replay() {
        use zeroclaw_api::model_provider::{ChatMessage, ConversationMessage};
        let cwd = tempfile::tempdir().unwrap();
        let store =
            Arc::new(zeroclaw_infra::acp_session_store::AcpSessionStore::new(cwd.path()).unwrap());

        let session_id = "sess-resume-test";
        store
            .create_session(session_id, "test-agent", &cwd.path().to_string_lossy())
            .unwrap();
        store
            .append_turn(
                session_id,
                &[ConversationMessage::Chat(ChatMessage::user("hello"))],
            )
            .unwrap();

        let (writer_tx, mut writer_rx) = tokio::sync::mpsc::channel::<String>(64);
        let server = Arc::new(AcpServer::new_with_writer_and_store(
            make_test_config(cwd.path()),
            AcpServerConfig::default(),
            writer_tx,
            Arc::clone(&store),
        ));

        let result = server
            .handle_session_resume(&serde_json::json!({
                "sessionId": session_id,
                "cwd": cwd.path().to_string_lossy()
            }))
            .await
            .expect("session/resume must succeed");

        // Result is empty object
        assert_eq!(result, serde_json::json!({}));

        // Session must be in memory
        assert!(server.sessions.lock().await.contains_key(session_id));

        // No notifications must have been emitted
        assert!(
            writer_rx.try_recv().is_err(),
            "session/resume must not emit session/update notifications"
        );
    }

    #[tokio::test]
    async fn session_close_releases_memory_but_keeps_store_record() {
        let cwd = tempfile::tempdir().unwrap();
        let store =
            Arc::new(zeroclaw_infra::acp_session_store::AcpSessionStore::new(cwd.path()).unwrap());
        let server = Arc::new(AcpServer::new_with_store(
            make_test_config(cwd.path()),
            AcpServerConfig::default(),
            Arc::clone(&store),
        ));

        let new_result = server
            .handle_session_new(&serde_json::json!({
                "cwd": cwd.path().to_string_lossy()
            }))
            .await
            .expect("session/new must succeed");
        let session_id = new_result["sessionId"].as_str().unwrap().to_string();

        assert!(server.sessions.lock().await.contains_key(&session_id));

        let result = server
            .handle_session_close(&serde_json::json!({ "sessionId": &session_id }))
            .await
            .expect("session/close must succeed");

        assert_eq!(result, serde_json::json!({}));

        // Session gone from in-memory map
        assert!(!server.sessions.lock().await.contains_key(&session_id));

        // Session record still on disk
        let data = store.load_session(&session_id).unwrap();
        assert!(
            data.is_some(),
            "session/close must not delete the DB record"
        );
    }

    #[tokio::test]
    async fn session_close_returns_not_found_for_unknown_session() {
        let cwd = tempfile::tempdir().unwrap();
        let server = AcpServer::new(make_test_config(cwd.path()), AcpServerConfig::default());

        let err = server
            .handle_session_close(&serde_json::json!({ "sessionId": "ghost" }))
            .await
            .expect_err("unknown session must fail");

        assert_eq!(err.code, SESSION_NOT_FOUND);
    }

    /// `session/load` must return SESSION_LIMIT_REACHED when `max_sessions` is
    /// already reached by an active session created via `session/new`.
    #[tokio::test]
    async fn session_load_respects_max_sessions() {
        let cwd = tempfile::tempdir().unwrap();
        let store =
            Arc::new(zeroclaw_infra::acp_session_store::AcpSessionStore::new(cwd.path()).unwrap());

        // Pre-create a stored session that we'll attempt to load
        let stored_id = "sess-load-limit-test";
        store
            .create_session(stored_id, "test-agent", &cwd.path().to_string_lossy())
            .unwrap();

        let (writer_tx, _rx) = tokio::sync::mpsc::channel::<String>(8);
        let server = Arc::new(AcpServer::new_with_writer_and_store(
            make_test_config(cwd.path()),
            AcpServerConfig {
                max_sessions: 1,
                ..AcpServerConfig::default()
            },
            writer_tx,
            Arc::clone(&store),
        ));

        // Fill the one available slot via session/new
        server
            .handle_session_new(&serde_json::json!({
                "cwd": cwd.path().to_string_lossy()
            }))
            .await
            .expect("session/new must succeed when under limit");

        // Now session/load for the stored session must fail with SESSION_LIMIT_REACHED
        let err = server
            .handle_session_load(&serde_json::json!({ "sessionId": stored_id }))
            .await
            .expect_err("session/load must fail when max_sessions reached");

        assert_eq!(
            err.code, SESSION_LIMIT_REACHED,
            "expected SESSION_LIMIT_REACHED, got: {:?}",
            err
        );
    }

    /// `session/resume` must return SESSION_LIMIT_REACHED when `max_sessions` is
    /// already reached by an active session created via `session/new`.
    #[tokio::test]
    async fn session_resume_respects_max_sessions() {
        let cwd = tempfile::tempdir().unwrap();
        let store =
            Arc::new(zeroclaw_infra::acp_session_store::AcpSessionStore::new(cwd.path()).unwrap());

        // Pre-create a stored session that we'll attempt to resume
        let stored_id = "sess-resume-limit-test";
        store
            .create_session(stored_id, "test-agent", &cwd.path().to_string_lossy())
            .unwrap();

        let (writer_tx, _rx) = tokio::sync::mpsc::channel::<String>(8);
        let server = Arc::new(AcpServer::new_with_writer_and_store(
            make_test_config(cwd.path()),
            AcpServerConfig {
                max_sessions: 1,
                ..AcpServerConfig::default()
            },
            writer_tx,
            Arc::clone(&store),
        ));

        // Fill the one available slot via session/new
        server
            .handle_session_new(&serde_json::json!({
                "cwd": cwd.path().to_string_lossy()
            }))
            .await
            .expect("session/new must succeed when under limit");

        // Now session/resume for the stored session must fail with SESSION_LIMIT_REACHED
        let err = server
            .handle_session_resume(&serde_json::json!({ "sessionId": stored_id }))
            .await
            .expect_err("session/resume must fail when max_sessions reached");

        assert_eq!(
            err.code, SESSION_LIMIT_REACHED,
            "expected SESSION_LIMIT_REACHED, got: {:?}",
            err
        );
    }

    /// A SQLite error during `store.load_session` must release the `loading_sessions`
    /// reservation so a subsequent restore attempt is not permanently blocked.
    #[tokio::test]
    async fn session_load_releases_reservation_on_store_error() {
        let cwd = tempfile::tempdir().unwrap();
        let store =
            Arc::new(zeroclaw_infra::acp_session_store::AcpSessionStore::new(cwd.path()).unwrap());

        let session_id = "sess-load-store-err";
        store
            .create_session(session_id, "test-agent", &cwd.path().to_string_lossy())
            .unwrap();

        // Drop the schema via a second connection to force a "no such table"
        // error on the store's next query_row call.
        let db_path = cwd.path().join("sessions/acp-sessions.db");
        {
            let second =
                rusqlite::Connection::open(&db_path).expect("second conn must open same db");
            second
                .execute_batch(
                    "DROP TABLE IF EXISTS acp_messages; DROP TABLE IF EXISTS acp_sessions;",
                )
                .expect("schema drop must succeed on second conn");
        }

        let (writer_tx, _rx) = tokio::sync::mpsc::channel::<String>(8);
        let server = Arc::new(AcpServer::new_with_writer_and_store(
            make_test_config(cwd.path()),
            AcpServerConfig::default(),
            writer_tx,
            Arc::clone(&store),
        ));

        // First call: must fail with INTERNAL_ERROR (SQLite "no such table").
        let first_err = server
            .handle_session_load(&serde_json::json!({ "sessionId": session_id }))
            .await
            .expect_err("session/load must fail when store returns Err");
        assert_eq!(
            first_err.code, INTERNAL_ERROR,
            "expected INTERNAL_ERROR from store failure, got: {:?}",
            first_err
        );

        // Second call for the same session: must also fail with INTERNAL_ERROR,
        // NOT with INVALID_PARAMS ("already active"). A leaked reservation would
        // cause INVALID_PARAMS, proving the slot was never released.
        let second_err = server
            .handle_session_load(&serde_json::json!({ "sessionId": session_id }))
            .await
            .expect_err("second session/load must also fail");
        assert_eq!(
            second_err.code, INTERNAL_ERROR,
            "second load must fail with INTERNAL_ERROR, not INVALID_PARAMS (leaked slot); got: {:?}",
            second_err
        );
    }

    /// Same coverage as `session_load_releases_reservation_on_store_error` but
    /// for the `session/resume` path.
    #[tokio::test]
    async fn session_resume_releases_reservation_on_store_error() {
        let cwd = tempfile::tempdir().unwrap();
        let store =
            Arc::new(zeroclaw_infra::acp_session_store::AcpSessionStore::new(cwd.path()).unwrap());

        let session_id = "sess-resume-store-err";
        store
            .create_session(session_id, "test-agent", &cwd.path().to_string_lossy())
            .unwrap();

        let db_path = cwd.path().join("sessions/acp-sessions.db");
        {
            let second =
                rusqlite::Connection::open(&db_path).expect("second conn must open same db");
            second
                .execute_batch(
                    "DROP TABLE IF EXISTS acp_messages; DROP TABLE IF EXISTS acp_sessions;",
                )
                .expect("schema drop must succeed on second conn");
        }

        let (writer_tx, _rx) = tokio::sync::mpsc::channel::<String>(8);
        let server = Arc::new(AcpServer::new_with_writer_and_store(
            make_test_config(cwd.path()),
            AcpServerConfig::default(),
            writer_tx,
            Arc::clone(&store),
        ));

        let first_err = server
            .handle_session_resume(&serde_json::json!({ "sessionId": session_id }))
            .await
            .expect_err("session/resume must fail when store returns Err");
        assert_eq!(
            first_err.code, INTERNAL_ERROR,
            "expected INTERNAL_ERROR from store failure, got: {:?}",
            first_err
        );

        let second_err = server
            .handle_session_resume(&serde_json::json!({ "sessionId": session_id }))
            .await
            .expect_err("second session/resume must also fail");
        assert_eq!(
            second_err.code, INTERNAL_ERROR,
            "second resume must fail with INTERNAL_ERROR, not INVALID_PARAMS (leaked slot); got: {:?}",
            second_err
        );
    }
}
