//! JSON-RPC 2.0 client over Unix socket (NDJSON) or WebSocket (WSS).
//!
//! Wraps [`RpcOutbound`] from `zeroclaw-api` — the same request/response
//! plumbing the daemon uses for bidirectional calls.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, mpsc};

use zeroclaw_api::jsonrpc::{self, JsonRpcError, RpcOutbound, field};
use zeroclaw_config::sections::SectionShape;
use zeroclaw_config::traits::{ConfigFieldEntry, MapKeyKind};

// ── Wire method names used by the TUI ────────────────────────────

pub mod method {
    pub const INITIALIZE: &str = "initialize";
    pub const CONFIG_LIST: &str = "config/list";
    pub const CONFIG_SET: &str = "config/set";
    pub const CONFIG_DELETE: &str = "config/delete";
    pub const CONFIG_MAP_KEYS: &str = "config/map-keys";
    pub const CONFIG_MAP_KEY_CREATE: &str = "config/map-key-create";
    pub const CONFIG_MAP_KEY_DELETE: &str = "config/map-key-delete";
    pub const CONFIG_MAP_KEY_RENAME: &str = "config/map-key-rename";
    pub const CONFIG_TEMPLATES: &str = "config/templates";
    pub const CONFIG_VALIDATE: &str = "config/validate";
    pub const CONFIG_SECTIONS: &str = "config/sections";
    pub const CONFIG_STATUS: &str = "config/status";
    pub const CONFIG_CATALOG_MODELS: &str = "config/catalog-models";
    // Personality
    pub const PERSONALITY_LIST: &str = "personality/list";
    pub const PERSONALITY_GET: &str = "personality/get";
    pub const PERSONALITY_PUT: &str = "personality/put";
    pub const PERSONALITY_TEMPLATES: &str = "personality/templates";
    // Skills
    pub const SKILLS_LIST: &str = "skills/list";
    pub const SKILLS_READ: &str = "skills/read";
    pub const SKILLS_WRITE: &str = "skills/write";
    pub const SKILLS_CREATE: &str = "skills/write";
    pub const SKILLS_DELETE: &str = "skills/delete";
    // Session
    pub const SESSION_NEW: &str = "session/new";
    pub const SESSION_PROMPT: &str = "session/prompt";
    pub const SESSION_CANCEL: &str = "session/cancel";
    pub const SESSION_CLOSE: &str = "session/close";
    pub const SESSION_APPROVE: &str = "session/approve";
    pub const SESSION_RENAME: &str = "session/rename";
    // Dashboard
    pub const STATUS: &str = "status";
    pub const HEALTH: &str = "health";
    pub const COST_QUERY: &str = "cost/query";
    pub const SESSION_LIST: &str = "session/list";
    pub const AGENTS_STATUS: &str = "agents/status";
    pub const CRON_LIST: &str = "cron/list";
    pub const MEMORY_LIST: &str = "memory/list";
    pub const MEMORY_SEARCH: &str = "memory/search";
    pub const SESSION_MESSAGES: &str = "session/messages";
    // TUI identity
    pub const TUI_LIST: &str = "tui/list";
}

// ── Socket path resolution ───────────────────────────────────────

/// Resolve the daemon socket path.
/// CLI flag > `$ZEROCLAW_SOCKET` > `<config_dir>/data/daemon.sock`.
pub fn resolve_socket_path(config_dir: &Path) -> Result<PathBuf> {
    if let Ok(p) = std::env::var("ZEROCLAW_SOCKET") {
        let p = p.trim();
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    Ok(config_dir.join("data").join("daemon.sock"))
}

/// Resolve config dir: CLI flag > `$ZEROCLAW_CONFIG_DIR` > `~/.zeroclaw`.
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
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".zeroclaw"))
}

// ── Notifications ────────────────────────────────────────────────

/// A server-initiated notification (no `id` field).
#[derive(Debug, Clone)]
pub struct RpcNotification {
    pub method: String,
    pub params: Value,
}

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
        name: String,
        raw_output: String,
    },
    ApprovalRequest {
        session_id: String,
        request_id: String,
        tool_name: String,
        arguments_summary: String,
        timeout_secs: u64,
    },
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
            name: params.get("name")?.as_str()?.to_string(),
            raw_output: params.get("raw_output")?.as_str()?.to_string(),
        }),
        "approval_request" => Some(SessionUpdate::ApprovalRequest {
            session_id: sid,
            request_id: params.get("request_id")?.as_str()?.to_string(),
            tool_name: params.get("tool_name")?.as_str()?.to_string(),
            arguments_summary: params.get("arguments_summary")?.as_str()?.to_string(),
            timeout_secs: params.get("timeout_secs")?.as_u64().unwrap_or(30),
        }),
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
                    if let Some(update) = parse_session_update(&notif.params) {
                        if update_tx.send(update).await.is_err() {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

// ── Connection state ──────────────────────────────────────────────

/// Observable connection state, written by the socket read task.
/// This is the single source of truth for daemon connectivity.
#[derive(Clone, Debug)]
pub enum ConnectionState {
    Connected,
    Disconnected { reason: String },
}

// ── Client ───────────────────────────────────────────────────────

pub struct RpcClient {
    pub(crate) rpc: Arc<RpcOutbound>,
    _read_task: tokio::task::JoinHandle<()>,
    _router_task: tokio::task::JoinHandle<()>,
    pub server_version: String,
    notifications_bcast: broadcast::Sender<RpcNotification>,
    pub notifications: mpsc::Receiver<SessionUpdate>,
    connection_state: Arc<Mutex<ConnectionState>>,
    /// TUI session UID assigned by the daemon during initialize.
    pub tui_id: Option<String>,
    /// HMAC signature for reconnection. Pass back in next initialize.
    pub tui_sig: Option<String>,
}

impl RpcClient {
    /// Connect to the daemon socket and complete the `initialize` handshake.
    ///
    /// Pass previous `tui_id` and `tui_sig` on reconnect to reclaim
    /// the same identity. Pass `None` for both on first connect.
    pub async fn connect(
        socket: &Path,
        prev_tui_id: Option<&str>,
        prev_tui_sig: Option<&str>,
    ) -> Result<Self> {
        let stream = UnixStream::connect(socket)
            .await
            .with_context(|| format!("connecting to {}", socket.display()))?;
        let (read_half, write_half) = stream.into_split();

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
                if let Some(id) = frame.get(field::ID).and_then(Value::as_str) {
                    let result = frame.get(field::RESULT).cloned();
                    let error: Option<JsonRpcError> = frame
                        .get(field::ERROR)
                        .and_then(|e| serde_json::from_value(e.clone()).ok());
                    rpc_for_reader.dispatch_response(id, result, error);
                } else if let Some(method) = frame.get(field::METHOD).and_then(Value::as_str) {
                    let params = frame.get("params").cloned().unwrap_or(Value::Null);
                    let _ = notif_tx_for_reader.send(RpcNotification {
                        method: method.to_string(),
                        params,
                    });
                }
            }
        });

        let mut init_params = serde_json::json!({
            "protocol_version": jsonrpc::ACP_PROTOCOL_VERSION
        });
        if let Some(id) = prev_tui_id {
            init_params["tui_id"] = serde_json::Value::String(id.to_string());
        }
        if let Some(sig) = prev_tui_sig {
            init_params["tui_sig"] = serde_json::Value::String(sig.to_string());
        }
        let resp = rpc
            .request(method::INITIALIZE, init_params)
            .await
            .map_err(|e| anyhow::Error::msg(format!("initialize: {} ({})", e.message, e.code)))?;

        let server_version = resp
            .get("server_version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();

        let tui_id = resp.get("tui_id").and_then(Value::as_str).map(String::from);
        let tui_sig = resp
            .get("tui_sig")
            .and_then(Value::as_str)
            .map(String::from);

        let bcast_rx = notif_tx.subscribe();
        let (update_tx, update_rx) = mpsc::channel::<SessionUpdate>(64);
        let router_task = spawn_notification_router(bcast_rx, update_tx);

        Ok(Self {
            rpc,
            _read_task: read_task,
            _router_task: router_task,
            server_version,
            notifications_bcast: notif_tx,
            notifications: update_rx,
            connection_state: conn_state,
            tui_id,
            tui_sig,
        })
    }

    /// Connect to the daemon via WebSocket Secure (WSS).
    ///
    /// Same handshake and reconnect semantics as [`connect`] — pass
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
                        if let Some(id) = frame.get(jsonrpc::field::ID).and_then(Value::as_str) {
                            let result = frame.get(jsonrpc::field::RESULT).cloned();
                            let error: Option<jsonrpc::JsonRpcError> = frame
                                .get(jsonrpc::field::ERROR)
                                .and_then(|e| serde_json::from_value(e.clone()).ok());
                            rpc_for_reader.dispatch_response(id, result, error);
                        } else if let Some(method) =
                            frame.get(jsonrpc::field::METHOD).and_then(Value::as_str)
                        {
                            let params = frame.get("params").cloned().unwrap_or(Value::Null);
                            let _ = notif_tx_for_reader.send(RpcNotification {
                                method: method.to_string(),
                                params,
                            });
                        }
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
            "protocol_version": jsonrpc::ACP_PROTOCOL_VERSION
        });
        if let Some(id) = prev_tui_id {
            init_params["tui_id"] = serde_json::Value::String(id.to_string());
        }
        if let Some(sig) = prev_tui_sig {
            init_params["tui_sig"] = serde_json::Value::String(sig.to_string());
        }
        let resp = rpc
            .request(method::INITIALIZE, init_params)
            .await
            .map_err(|e| anyhow::Error::msg(format!("initialize: {} ({})", e.message, e.code)))?;

        let server_version = resp
            .get("server_version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();

        let tui_id = resp.get("tui_id").and_then(Value::as_str).map(String::from);
        let tui_sig = resp
            .get("tui_sig")
            .and_then(Value::as_str)
            .map(String::from);

        let bcast_rx = notif_tx.subscribe();
        let (update_tx, update_rx) = mpsc::channel::<SessionUpdate>(64);
        let router_task = spawn_notification_router(bcast_rx, update_tx);

        Ok(Self {
            rpc,
            _read_task: read_task,
            _router_task: router_task,
            server_version,
            notifications_bcast: notif_tx,
            notifications: update_rx,
            connection_state: conn_state,
            tui_id,
            tui_sig,
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

        let config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth();

        Arc::new(config)
    }

    pub async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        // Timeout prevents indefinite hangs when the daemon dies between
        // the connection-state check and the actual RPC send/recv.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.rpc.request(method, params),
        )
        .await
        .map_err(|_| anyhow::Error::msg(format!("RPC {method}: timed out after 5s")))?
        .map_err(|e| anyhow::Error::msg(format!("RPC {method}: {} ({})", e.message, e.code)))?;
        serde_json::from_value(result).with_context(|| format!("deserializing {method} result"))
    }

    /// Call an RPC method using a shared Arc<RpcOutbound> — usable from spawned tasks.
    pub async fn call_static<T: serde::de::DeserializeOwned + Send + 'static>(
        rpc: &Arc<RpcOutbound>,
        method: &'static str,
        params: serde_json::Value,
    ) -> anyhow::Result<T> {
        let result = rpc
            .request(method, params)
            .await
            .map_err(|e| anyhow::Error::msg(format!("RPC {method}: {} ({})", e.message, e.code)))?;
        serde_json::from_value(result)
            .map_err(|e| anyhow::Error::msg(format!("deserializing {method} result: {e}")))
    }

    // ── Connection state ─────────────────────────────────────────

    /// Current connection state. Cheap mutex read, safe to call on every frame.
    pub fn connection_state(&self) -> ConnectionState {
        self.connection_state.lock().unwrap().clone()
    }

    /// Returns `true` when the daemon connection is known to be dead.
    pub fn is_disconnected(&self) -> bool {
        matches!(
            self.connection_state(),
            ConnectionState::Disconnected { .. }
        )
    }

    // ── Notifications ─────────────────────────────────────────────

    /// Get a receiver for server-initiated notifications.
    pub fn subscribe_notifications(&self) -> broadcast::Receiver<RpcNotification> {
        self.notifications_bcast.subscribe()
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

    pub async fn config_templates(&self) -> Result<Vec<ConfigTemplateEntry>> {
        let result: ConfigTemplatesResult = self
            .call(method::CONFIG_TEMPLATES, serde_json::json!({}))
            .await?;
        Ok(result.templates)
    }

    pub async fn catalog_models(&self, provider: &str) -> Result<Vec<String>> {
        let result: CatalogModelsResult = self
            .call(
                method::CONFIG_CATALOG_MODELS,
                serde_json::json!({ "model_provider": provider }),
            )
            .await?;
        Ok(result.models)
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

    // ── Session methods ──────────────────────────────────────────

    pub async fn session_new(
        &self,
        agent_alias: &str,
        cwd: Option<&str>,
    ) -> Result<SessionNewResult> {
        self.session_new_with_id(agent_alias, cwd, None).await
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
        self.call(
            method::SESSION_NEW,
            serde_json::json!({ "agent_alias": agent_alias, "cwd": cwd, "session_id": session_id }),
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

    pub async fn session_close(&self, session_id: &str) -> Result<()> {
        let _: serde_json::Value = self
            .call(
                method::SESSION_CLOSE,
                serde_json::json!({ "session_id": session_id }),
            )
            .await?;
        Ok(())
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

    pub async fn session_rename(
        &self,
        session_id: &str,
        name: &str,
    ) -> Result<SessionRenameResult> {
        self.call(
            method::SESSION_RENAME,
            serde_json::json!({ "session_id": session_id, "name": name }),
        )
        .await
    }

    // ── Dashboard helpers ────────────────────────────────────────

    pub async fn status(&self) -> Result<StatusResult> {
        self.call(method::STATUS, serde_json::json!({})).await
    }

    pub async fn health(&self) -> Result<Value> {
        self.call(method::HEALTH, serde_json::json!({})).await
    }

    pub async fn cost_query(&self, agent: Option<&str>) -> Result<CostSummaryResult> {
        self.call(method::COST_QUERY, serde_json::json!({ "agent": agent }))
            .await
    }

    pub async fn session_list(&self, query: Option<&str>) -> Result<SessionListResult> {
        self.call(method::SESSION_LIST, serde_json::json!({ "query": query }))
            .await
    }

    pub async fn agents_status(&self) -> Result<AgentsStatusResult> {
        self.call(method::AGENTS_STATUS, serde_json::json!({}))
            .await
    }

    pub async fn cron_list(&self) -> Result<CronListResult> {
        self.call(method::CRON_LIST, serde_json::json!({})).await
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

    pub async fn session_messages(&self, session_id: &str) -> Result<SessionMessagesResult> {
        self.call(
            method::SESSION_MESSAGES,
            serde_json::json!({ "session_id": session_id }),
        )
        .await
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

    // ── Test-only constructors ────────────────────────────────────

    /// Test-only constructor that skips the Unix socket connect + initialize handshake.
    #[cfg(test)]
    pub fn with_rpc(rpc: Arc<RpcOutbound>) -> Self {
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        let (notif_tx, _) = tokio::sync::broadcast::channel(1);
        Self {
            rpc,
            _read_task: tokio::spawn(async {}),
            _router_task: tokio::spawn(async {}),
            server_version: "test".to_string(),
            notifications_bcast: notif_tx,
            notifications: rx,
            connection_state: Arc::new(Mutex::new(ConnectionState::Connected)),
            tui_id: None,
            tui_sig: None,
        }
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
pub struct ConfigSetResult {
    pub prop: String,
    pub set: bool,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigDeleteResult {
    pub prop: String,
    pub deleted: bool,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigMapKeysResult {
    pub path: String,
    pub keys: Vec<String>,
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
    pub has_picker: bool,
    pub completed: bool,
    #[serde(default)]
    pub shape: Option<SectionShape>,
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
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ConfigTemplateEntry {
    pub path: String,
    pub kind: MapKeyKind,
    pub value_type: String,
    pub description: String,
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
    pub filename: String,
    #[serde(default)]
    pub content: Option<String>,
    pub exists: bool,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, serde::Deserialize)]
pub struct PersonalityPutResult {
    pub bytes_written: u64,
}

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
    pub bundle: String,
    pub name: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct SkillsListResult {
    pub skills: Vec<SkillListEntry>,
}

#[derive(Debug, serde::Deserialize)]
pub struct SkillsReadResult {
    pub bundle: String,
    pub name: String,
    pub frontmatter: SkillFrontmatter,
    pub body: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct SkillsWriteResult {
    pub bundle: String,
    pub name: String,
    pub written: bool,
}

#[derive(Debug, serde::Deserialize)]
pub struct SkillsDeleteResult {
    pub bundle: String,
    pub name: String,
    pub deleted: bool,
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
    pub next_cursor: Option<(String, String)>,
    pub at_end: bool,
}

// ── Session / Agents types ───────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionNewResult {
    pub session_id: String,
    pub agent_alias: String,
    pub message_count: usize,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionPromptResult {
    pub session_id: String,
    pub stop_reason: String,
    pub content: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionCancelResult {
    pub session_id: String,
    pub cancelled: bool,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionApproveResult {
    pub session_id: String,
    pub request_id: String,
    pub acknowledged: bool,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionRenameResult {
    pub session_id: String,
    pub name: String,
}

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
    pub session_ids: Vec<String>,
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
    pub active_sessions: usize,
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
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    pub request_count: usize,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentCostStats {
    pub agent_alias: String,
    pub cost_usd: f64,
    pub total_tokens: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
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
    pub expression: String,
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
pub struct MemoryEntryResult {
    pub id: String,
    pub key: String,
    pub content: String,
    pub category: String,
    pub timestamp: String,
    #[serde(default)]
    pub session_id: Option<String>,
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
    pub count: usize,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MemorySearchResult {
    pub entries: Vec<MemoryEntryResult>,
    pub count: usize,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionMessagesResult {
    pub session_id: String,
    pub messages: Vec<MessageEntry>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MessageEntry {
    pub role: String,
    pub content: String,
}

// ── TUI identity types ───────────────────────────────────────────

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TuiListEntry {
    pub tui_id: String,
    pub connected_at: String,
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

// ── Tests ────────────────────────────────────────────────────────

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

        let line = write_rx.recv().await.unwrap();
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

        let result = task.await.unwrap().unwrap();
        assert_eq!(result.session_id, "s42");
    }

    #[tokio::test]
    async fn session_cancel_sends_session_id() {
        let (rpc, mut write_rx) = make_rpc();
        let client = RpcClient::with_rpc(rpc.clone());

        let task = tokio::spawn(async move { client.session_cancel("s1").await });

        let line = write_rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(req["method"], "session/cancel");
        assert_eq!(req["params"]["session_id"], "s1");

        let id = req["id"].as_str().unwrap().to_string();
        rpc.dispatch_response(&id, Some(json!({"session_id":"s1","cancelled":true})), None);
        task.await.unwrap().unwrap();
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

        let line = write_rx.recv().await.unwrap();
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
        let result = task.await.unwrap().unwrap();
        assert!(result.acknowledged);
    }

    #[tokio::test]
    async fn session_approve_reject_with_edit_sends_replacement() {
        let (rpc, mut write_rx) = make_rpc();
        let client = RpcClient::with_rpc(rpc.clone());

        let task = tokio::spawn(async move {
            client
                .session_approve(
                    "s1",
                    "req-2",
                    ApprovalDecision::RejectWithEdit {
                        replacement: "let x = 99;".to_string(),
                    },
                )
                .await
        });

        let line = write_rx.recv().await.unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(req["params"]["decision"], "reject_with_edit");
        assert_eq!(req["params"]["replacement"], "let x = 99;");

        let id = req["id"].as_str().unwrap().to_string();
        rpc.dispatch_response(
            &id,
            Some(serde_json::json!({"session_id":"s1","request_id":"req-2","acknowledged":true})),
            None,
        );
        task.await.unwrap().unwrap();
    }
}

#[cfg(test)]
mod notification_tests {
    use super::*;
    use tokio::sync::{broadcast, mpsc};

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
