//! JSON-RPC 2.0 method dispatch. Transport-agnostic.
//!
//! **No string-literal matching.** Every wire method name is registered
//! exactly once in [`Method::ALL`]. The compiler enforces that every
//! variant has a handler via exhaustive `match`.

use super::context::RpcContext;
use super::transport::RpcTransport;
use super::turn::{TurnAttribution, TurnOutcome, execute_turn};
use super::types::*;
use crate::agent::agent::TurnEvent;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

use zeroclaw_api::jsonrpc::error_codes::*;
use zeroclaw_api::jsonrpc::{
    JSONRPC_VERSION, JsonRpcError, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    RpcOutbound,
};
use zeroclaw_api::model_provider::ChatMessage;

/// Wire protocol version. Bump on breaking changes.
pub const RPC_PROTOCOL_VERSION: u64 = 1;

mod notification {
    pub const SESSION_UPDATE: &str = "session/update";
    pub const LOGS_EVENT: &str = "logs/event";
}

// ── Method registry ──────────────────────────────────────────────
//
// Single source of truth. Every variant maps to exactly one wire
// string. `from_wire` is a table scan — no hand-written string
// matching anywhere in this file.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    // Core
    Initialize,
    Status,
    Health,

    // Sessions (agent chat lives here — session/prompt + session/update
    // notifications is the RPC equivalent of the gateway's ws/chat)
    SessionNew,
    SessionClose,
    SessionPrompt,
    SessionConfigure,
    SessionCancel,
    SessionList,
    SessionListAcp,
    SessionMessages,
    SessionState,
    SessionDelete,
    SessionRename,
    SessionApprove,

    // Memory
    MemoryList,
    MemorySearch,
    MemoryStore,
    MemoryDelete,

    // Cron
    CronList,
    CronGet,
    CronAdd,
    CronPatch,
    CronDelete,
    CronRuns,
    CronTrigger,
    CronSettings,

    // Config
    ConfigGet,
    ConfigSet,
    ConfigValidate,
    ConfigReload,
    ConfigList,
    ConfigDelete,
    ConfigMapKeys,
    ConfigMapKeyCreate,
    ConfigMapKeyDelete,
    ConfigMapKeyRename,
    ConfigTemplates,

    // Agents
    AgentsList,
    AgentsStatus,

    // Cost
    CostQuery,

    // Skills
    SkillsBundles,
    SkillsList,
    SkillsRead,
    SkillsWrite,
    SkillsDelete,

    // Personality
    PersonalityList,
    PersonalityGet,
    PersonalityPut,
    PersonalityTemplates,

    // Config introspection (sections, catalog, status)
    ConfigSections,
    ConfigStatus,
    ConfigCatalog,
    ConfigCatalogModels,

    // Logs / Events
    LogsSubscribe,
    LogsQuery,

    // TUI
    TuiList,

    // Files
    FileAttach,
    FsListDir,

    // Quickstart (TUI mirror of `/api/quickstart/*` HTTP routes)
    QuickstartState,
    QuickstartFields,
    QuickstartValidate,
    QuickstartApply,
    QuickstartDismiss,
}

impl Method {
    /// The single table. Wire name ↔ variant, defined once.
    pub const ALL: &[(Method, &str)] = &[
        (Method::Initialize, "initialize"),
        (Method::Status, "status"),
        (Method::Health, "health"),
        // Sessions
        (Method::SessionNew, "session/new"),
        (Method::SessionClose, "session/close"),
        (Method::SessionPrompt, "session/prompt"),
        (Method::SessionConfigure, "session/configure"),
        (Method::SessionCancel, "session/cancel"),
        (Method::SessionList, "session/list"),
        (Method::SessionListAcp, "session/list-acp"),
        (Method::SessionMessages, "session/messages"),
        (Method::SessionState, "session/state"),
        (Method::SessionDelete, "session/delete"),
        (Method::SessionRename, "session/rename"),
        (Method::SessionApprove, "session/approve"),
        // Memory
        (Method::MemoryList, "memory/list"),
        (Method::MemorySearch, "memory/search"),
        (Method::MemoryStore, "memory/store"),
        (Method::MemoryDelete, "memory/delete"),
        // Cron
        (Method::CronList, "cron/list"),
        (Method::CronGet, "cron/get"),
        (Method::CronAdd, "cron/add"),
        (Method::CronPatch, "cron/patch"),
        (Method::CronDelete, "cron/delete"),
        (Method::CronRuns, "cron/runs"),
        (Method::CronTrigger, "cron/trigger"),
        (Method::CronSettings, "cron/settings"),
        // Config
        (Method::ConfigGet, "config/get"),
        (Method::ConfigSet, "config/set"),
        (Method::ConfigValidate, "config/validate"),
        (Method::ConfigReload, "config/reload"),
        (Method::ConfigList, "config/list"),
        (Method::ConfigDelete, "config/delete"),
        (Method::ConfigMapKeys, "config/map-keys"),
        (Method::ConfigMapKeyCreate, "config/map-key-create"),
        (Method::ConfigMapKeyDelete, "config/map-key-delete"),
        (Method::ConfigMapKeyRename, "config/map-key-rename"),
        (Method::ConfigTemplates, "config/templates"),
        // Agents
        (Method::AgentsList, "agents/list"),
        (Method::AgentsStatus, "agents/status"),
        // Cost
        (Method::CostQuery, "cost/query"),
        // Skills
        (Method::SkillsBundles, "skills/bundles"),
        (Method::SkillsList, "skills/list"),
        (Method::SkillsRead, "skills/read"),
        (Method::SkillsWrite, "skills/write"),
        (Method::SkillsDelete, "skills/delete"),
        // Personality
        (Method::PersonalityList, "personality/list"),
        (Method::PersonalityGet, "personality/get"),
        (Method::PersonalityPut, "personality/put"),
        (Method::PersonalityTemplates, "personality/templates"),
        // Config introspection
        (Method::ConfigSections, "config/sections"),
        (Method::ConfigStatus, "config/status"),
        (Method::ConfigCatalog, "config/catalog"),
        (Method::ConfigCatalogModels, "config/catalog-models"),
        // Logs
        (Method::LogsSubscribe, "logs/subscribe"),
        (Method::LogsQuery, "logs/query"),
        // TUI
        (Method::TuiList, "tui/list"),
        // Files
        (Method::FileAttach, "file/attach"),
        (Method::FsListDir, "fs/list_dir"),
        // Quickstart
        (Method::QuickstartState, "quickstart/state"),
        (Method::QuickstartFields, "quickstart/fields"),
        (Method::QuickstartValidate, "quickstart/validate"),
        (Method::QuickstartApply, "quickstart/apply"),
        (Method::QuickstartDismiss, "quickstart/dismiss"),
    ];

    /// Resolve a wire method name to a variant. Table scan, no hand-written
    /// string matching.
    pub fn from_wire(s: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(_, wire)| *wire == s)
            .map(|(m, _)| *m)
    }

    /// Wire name for this variant.
    pub fn wire_name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(m, _)| *m == self)
            .map(|(_, wire)| *wire)
            .expect("every variant is in ALL")
    }
}

type RpcResult = Result<Value, JsonRpcError>;

fn rpc_err(code: i32, msg: impl Into<String>) -> JsonRpcError {
    JsonRpcError {
        code,
        message: msg.into(),
        data: None,
    }
}

fn not_yet_implemented(method: Method) -> RpcResult {
    Err(rpc_err(
        INTERNAL_ERROR,
        format!("{}: not yet implemented", method.wire_name()),
    ))
}

/// Per-connection dispatcher. Shared state lives in [`RpcContext`].
pub struct RpcDispatcher {
    ctx: Arc<RpcContext>,
    rpc: Arc<RpcOutbound>,
    authenticated: bool,
    /// TUI session UID assigned during `initialize`. Used for registry
    /// cleanup on disconnect.
    tui_id: Option<String>,
    /// Transport-level peer label (e.g. `unix:pid=1234,uid=1000`).
    peer_label: String,
}

impl RpcDispatcher {
    pub fn new(ctx: Arc<RpcContext>, writer_tx: mpsc::Sender<String>, peer_label: String) -> Self {
        Self {
            ctx,
            rpc: Arc::new(RpcOutbound::new(writer_tx)),
            authenticated: false,
            tui_id: None,
            peer_label,
        }
    }

    /// TUI ID assigned during initialize, if any.
    pub fn tui_id(&self) -> Option<&str> {
        self.tui_id.as_deref()
    }

    /// Construct a pre-authenticated dispatcher sharing the same context and
    /// RPC outbound as `self`. Used to run long-lived methods (e.g.
    /// `session/prompt`) in a spawned task so the read loop remains live.
    fn spawn_handle(&self) -> Self {
        Self {
            ctx: Arc::clone(&self.ctx),
            rpc: Arc::clone(&self.rpc),
            authenticated: true,
            tui_id: self.tui_id.clone(),
            peer_label: self.peer_label.clone(),
        }
    }

    /// Flush dirty config paths to disk. Clone the config out of the
    /// lock (parking_lot guards are !Send), save to disk, then write
    /// the clone (with cleared dirty set) back.
    async fn flush_config(&self) -> Result<(), JsonRpcError> {
        let mut snapshot = self.ctx.config.read().clone();
        snapshot
            .save_dirty()
            .await
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Config save failed: {e}")))?;
        *self.ctx.config.write() = snapshot;
        Ok(())
    }

    /// Read frames from transport, dispatch, repeat.
    pub async fn run(&mut self, transport: &mut (dyn RpcTransport + Send)) {
        while let Some(line) = transport.next_frame().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            self.process_line(trimmed).await;
        }
    }

    async fn process_line(&mut self, line: &str) {
        let req: JsonRpcRequest = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                self.send_error(Value::Null, PARSE_ERROR, &format!("Parse error: {e}"))
                    .await;
                return;
            }
        };

        // Bidirectional RPC: responses to our outbound requests.
        if req.method.is_empty() {
            if let Some(id) = req.id.as_ref().and_then(Value::as_str) {
                self.rpc.dispatch_response(id, Some(req.params), None);
            }
            return;
        }

        let id = req.id.clone().unwrap_or(Value::Null);
        let is_notification = req.id.is_none();

        let method = match Method::from_wire(&req.method) {
            Some(m) => m,
            None => {
                if !is_notification {
                    self.send_error(
                        id,
                        METHOD_NOT_FOUND,
                        &format!("Unknown method: {}", req.method),
                    )
                    .await;
                }
                return;
            }
        };

        if !self.authenticated && method != Method::Initialize {
            if !is_notification {
                self.send_error(id, AUTH_REQUIRED, "First call must be 'initialize'")
                    .await;
            }
            return;
        }

        // Exhaustive match — compiler enforces every Method has a handler.
        let result = match method {
            // Core
            Method::Initialize => self.handle_initialize(&req.params).await,
            Method::Status => self.handle_status().await,
            Method::Health => self.handle_health(),

            // Sessions
            Method::SessionNew => self.handle_session_new(&req.params).await,
            Method::SessionClose => self.handle_session_close(&req.params).await,
            Method::SessionPrompt => {
                // Spawn so the read loop stays live for session/approve while
                // the turn is in flight — serial dispatch would deadlock.
                if !is_notification {
                    let handle = self.spawn_handle();
                    let id_clone = id;
                    let params_clone = req.params.clone();
                    zeroclaw_spawn::spawn!(async move {
                        let result = handle.handle_session_prompt(&params_clone).await;
                        match result {
                            Ok(v) => handle.send_result(id_clone, v).await,
                            Err(e) => handle.send_error(id_clone, e.code, &e.message).await,
                        }
                    });
                }
                return;
            }
            Method::SessionConfigure => self.handle_session_configure(&req.params).await,
            Method::SessionCancel => self.handle_session_cancel(&req.params),
            Method::SessionList => self.handle_session_list(&req.params).await,
            Method::SessionListAcp => self.handle_session_list_acp(&req.params).await,
            Method::SessionMessages => self.handle_session_messages(&req.params).await,
            Method::SessionState => self.handle_session_state(&req.params).await,
            Method::SessionDelete => self.handle_session_delete(&req.params).await,
            Method::SessionRename => self.handle_session_rename(&req.params).await,
            Method::SessionApprove => self.handle_session_approve(&req.params),

            // Memory
            Method::MemoryList => self.handle_memory_list(&req.params).await,
            Method::MemorySearch => self.handle_memory_search(&req.params).await,
            Method::MemoryStore => self.handle_memory_store(&req.params).await,
            Method::MemoryDelete => self.handle_memory_delete(&req.params).await,

            // Cron
            Method::CronList => self.handle_cron_list().await,
            Method::CronGet => self.handle_cron_get(&req.params).await,
            Method::CronAdd => self.handle_cron_add(&req.params).await,
            Method::CronPatch => self.handle_cron_patch(&req.params).await,
            Method::CronDelete => self.handle_cron_delete(&req.params).await,
            Method::CronRuns => self.handle_cron_runs(&req.params).await,
            Method::CronTrigger => self.handle_cron_trigger(&req.params).await,
            Method::CronSettings => self.handle_cron_settings(&req.params).await,

            // Config
            Method::ConfigGet => self.handle_config_get(&req.params),
            Method::ConfigSet => self.handle_config_set(&req.params).await,
            Method::ConfigValidate => self.handle_config_validate(),
            Method::ConfigReload => self.handle_config_reload(),
            Method::ConfigList => self.handle_config_list(&req.params),
            Method::ConfigDelete => self.handle_config_delete(&req.params).await,
            Method::ConfigMapKeys => self.handle_config_map_keys(&req.params),
            Method::ConfigMapKeyCreate => self.handle_config_map_key_create(&req.params).await,
            Method::ConfigMapKeyDelete => self.handle_config_map_key_delete(&req.params).await,
            Method::ConfigMapKeyRename => self.handle_config_map_key_rename(&req.params).await,
            Method::ConfigTemplates => self.handle_config_templates(),

            // Agents
            Method::AgentsList => self.handle_agents_list(),
            Method::AgentsStatus => self.handle_agents_status().await,

            // Cost
            Method::CostQuery => self.handle_cost_query(&req.params),

            // Skills
            Method::SkillsBundles => self.handle_skills_bundles(),
            Method::SkillsList => self.handle_skills_list(&req.params),
            Method::SkillsRead => self.handle_skills_read(&req.params),
            Method::SkillsWrite => self.handle_skills_write(&req.params),
            Method::SkillsDelete => self.handle_skills_delete(&req.params),

            // Personality
            Method::PersonalityList => self.handle_personality_list(&req.params),
            Method::PersonalityGet => self.handle_personality_get(&req.params),
            Method::PersonalityPut => self.handle_personality_put(&req.params),
            Method::PersonalityTemplates => self.handle_personality_templates(&req.params),

            // Config introspection
            Method::ConfigSections => self.handle_config_sections(),
            Method::ConfigStatus => self.handle_config_status(),
            Method::ConfigCatalog => self.handle_config_catalog(),
            Method::ConfigCatalogModels => self.handle_config_catalog_models(&req.params).await,

            // Logs
            Method::LogsSubscribe => self.handle_logs_subscribe().await,
            Method::LogsQuery => self.handle_logs_query(&req.params).await,

            // TUI
            Method::TuiList => self.handle_tui_list(),

            // Files
            Method::FileAttach => self.handle_file_attach(&req.params).await,
            Method::FsListDir => super::fs::handle_fs_list_dir(&req.params).await,

            // Quickstart
            Method::QuickstartState => self.handle_quickstart_state(),
            Method::QuickstartFields => self.handle_quickstart_fields(&req.params),
            Method::QuickstartValidate => self.handle_quickstart_validate(&req.params),
            Method::QuickstartApply => self.handle_quickstart_apply(&req.params).await,
            Method::QuickstartDismiss => self.handle_quickstart_dismiss(&req.params),
        };

        if is_notification {
            return;
        }

        match result {
            Ok(v) => self.send_result(id, v).await,
            Err(e) => self.send_error(id, e.code, &e.message).await,
        }
    }

    // ── Core handlers ────────────────────────────────────────────

    async fn handle_initialize(&mut self, params: &Value) -> RpcResult {
        let req: InitializeParams = parse_params(params)?;

        if req.protocol_version != RPC_PROTOCOL_VERSION {
            return Err(rpc_err(
                VERSION_MISMATCH,
                format!(
                    "Protocol version mismatch: server={RPC_PROTOCOL_VERSION}, client={}",
                    req.protocol_version,
                ),
            ));
        }

        // TUI identity: reconnect with previous credentials or generate new
        let tui_id = if let (Some(claimed_id), Some(sig)) =
            (req.tui_id.as_deref(), req.tui_sig.as_deref())
        {
            // Client presents ID + signature — verify
            if !self.ctx.tui_registry.verify(claimed_id, sig) {
                return Err(rpc_err(AUTH_REQUIRED, "Invalid TUI signature"));
            }
            // Remove stale entry from previous connection before re-registering
            self.ctx.tui_registry.unregister(claimed_id);
            claimed_id.to_string()
        } else if let Some(claimed_id) = req.tui_id.as_deref() {
            // Client claims ID but no signature — accept only if signing disabled
            if self.ctx.tui_registry.signing_is_enabled() {
                return Err(rpc_err(AUTH_REQUIRED, "TUI signature required"));
            }
            self.ctx.tui_registry.unregister(claimed_id);
            claimed_id.to_string()
        } else {
            // Fresh connection — generate new ID
            self.ctx.tui_registry.generate_unique_tui_id()
        };

        let tui_sig = self.ctx.tui_registry.sign(&tui_id);
        self.ctx
            .tui_registry
            .register(super::tui_identity::TuiEntry {
                tui_id: tui_id.clone(),
                connected_at: chrono::Utc::now(),
                transport: self
                    .peer_label
                    .split_once(':')
                    .map_or("unknown", |(proto, _)| proto)
                    .to_string(),
                peer_label: self.peer_label.clone(),
                env: req.env,
            });
        self.tui_id = Some(tui_id.clone());

        self.authenticated = true;

        let capabilities: Vec<String> = Method::ALL
            .iter()
            .map(|(_, name)| (*name).to_string())
            .collect();

        to_result(InitializeResult {
            protocol_version: RPC_PROTOCOL_VERSION,
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            tui_id: Some(tui_id),
            tui_sig,
            capabilities,
        })
    }

    async fn handle_status(&self) -> RpcResult {
        let ids = self.ctx.sessions.list_ids().await;
        // Count persisted sessions (channel-originated) that aren't already
        // in the in-memory RPC store.
        let persisted_count = self
            .ctx
            .session_backend
            .as_ref()
            .map(|b| b.list_sessions_with_metadata().len())
            .unwrap_or(0);
        let total = ids.len().max(persisted_count);
        to_result(StatusResult {
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: RPC_PROTOCOL_VERSION,
            active_sessions: total,
            session_ids: ids,
        })
    }

    fn handle_health(&self) -> RpcResult {
        let mut val = crate::health::snapshot_json();
        if let Some(obj) = val.as_object_mut() {
            let stats = crate::process_stats::sample();
            obj.insert(
                "process".to_string(),
                serde_json::to_value(&stats).unwrap_or_default(),
            );
        }
        Ok(val)
    }

    // ── TUI handlers ─────────────────────────────────────────────

    fn handle_tui_list(&self) -> RpcResult {
        let entries = self.ctx.tui_registry.list();
        to_result(TuiListResult {
            tuis: entries
                .into_iter()
                .map(|e| TuiListEntry {
                    tui_id: e.tui_id,
                    connected_at: e.connected_at.to_rfc3339(),
                    connected_at_unix: e.connected_at.timestamp(),
                    peer_label: e.peer_label,
                    transport: e.transport,
                })
                .collect(),
        })
    }

    // ── Session handlers ─────────────────────────────────────────

    /// Test-only: call `handle_session_new` directly, bypassing the
    /// authentication gate in the `run` loop.  This lets integration tests
    /// drive the full agent-creation path without spinning up a transport.
    #[cfg(test)]
    pub async fn handle_session_new_for_test(&self, params: &Value) -> RpcResult {
        self.handle_session_new(params).await
    }

    async fn handle_session_new(&self, params: &Value) -> RpcResult {
        let req: SessionNewParams = parse_params(params)?;
        let session_id = req
            .session_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let config = self.ctx.config.read().clone();
        let cwd_path = req.cwd.as_deref().map(std::path::Path::new);
        let tui_env = req
            .tui_id
            .as_deref()
            .and_then(|id| self.ctx.tui_registry.get_env(id));
        let agent = crate::agent::agent::Agent::from_config_with_tui_env(
            &config,
            &req.agent_alias,
            cwd_path,
            false,
            req.exclude_memory.unwrap_or(false),
            tui_env,
        )
        .await
        .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Failed to create agent: {e}")))?;

        let approval_ch = Arc::new(crate::rpc::approval_channel::RpcApprovalChannel::new(
            "rpc",
            session_id.clone(),
            Arc::clone(&self.rpc),
            Arc::clone(&self.ctx.approval_pending),
        ));
        agent.channel_handles().register_channel("rpc", approval_ch);

        let cwd = req.cwd.clone().unwrap_or_else(|| {
            config
                .agent_workspace_dir(&req.agent_alias)
                .to_string_lossy()
                .to_string()
        });
        let chat_mode = req
            .chat_mode
            .clone()
            .unwrap_or(crate::rpc::types::ChatMode::Chat);
        self.ctx
            .sessions
            .insert(
                session_id.clone(),
                super::session::RpcSession::new(agent, &req.agent_alias, &cwd, chat_mode.clone()),
            )
            .await
            .map_err(|_| rpc_err(SESSION_LIMIT_REACHED, "Session limit reached"))?;

        let mut message_count = 0;
        match chat_mode {
            crate::rpc::types::ChatMode::Acp => {
                if let Some(ref store) = self.ctx.acp_session_store {
                    match store.load_session(&session_id) {
                        Ok(Some(data)) => {
                            message_count = data.messages.len();
                            self.ctx
                                .sessions
                                .seed_conversation_history(&session_id, data.messages)
                                .await;
                        }
                        Ok(None) => {
                            if let Err(e) =
                                store.create_session(&session_id, &req.agent_alias, &cwd)
                            {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                        .with_attrs(::serde_json::json!({"session_id": session_id, "error": e.to_string()})),
                                    "Failed to create ACP session row"
                                );
                            }
                        }
                        Err(e) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(::serde_json::json!({"session_id": session_id, "error": e.to_string()})),
                                "Failed to load ACP session"
                            );
                        }
                    }
                }
            }
            crate::rpc::types::ChatMode::Chat => {
                if let Some(ref backend) = self.ctx.session_backend {
                    let session_key = format!("rpc_{session_id}");
                    let _ = backend.set_session_agent_alias(&session_key, &req.agent_alias);
                    let stored = backend.load(&session_key);
                    if !stored.is_empty() {
                        self.ctx.sessions.seed_history(&session_id, &stored).await;
                        message_count = stored.len();
                    }
                }
            }
        }

        to_result(SessionNewResult {
            session_id,
            agent_alias: req.agent_alias,
            message_count,
            workspace_dir: cwd,
        })
    }

    async fn handle_session_close(&self, params: &Value) -> RpcResult {
        let req: SessionIdParams = parse_params(params)?;
        if !self.ctx.sessions.remove(&req.session_id).await {
            return Err(rpc_err(SESSION_NOT_FOUND, "Session not found"));
        }
        to_result(SessionCloseResult {
            session_id: req.session_id,
            closed: true,
        })
    }

    async fn handle_session_prompt(&self, params: &Value) -> RpcResult {
        let req: SessionPromptParams = parse_params(params)?;
        let sid = &req.session_id;

        // Reject blank turns at the RPC boundary. A turn must carry SOMETHING
        // — either prose or an attachment — for the agent to act on. Letting
        // an empty `{prompt: "", attachments: []}` through would push a user
        // message that contains only the runtime's timestamp prefix into the
        // model context; Claude in particular then narrates the trailing
        // `<<HUMAN_CONVERSATION_START>>` template sentinel instead of
        // responding, and that bleeds into the visible transcript. The
        // duplicate guard inside `Agent::turn_streamed` is the load-bearing
        // one (any code path that reaches the agent is covered); this one
        // gives RPC callers a clean error code instead of a generic agent
        // failure surfaced after queue acquisition.
        if req.prompt.trim().is_empty() && req.attachments.is_empty() {
            return Err(rpc_err(
                INVALID_PARAMS,
                "session/prompt requires a non-empty `prompt` or at least one attachment",
            ));
        }

        let agent = self
            .ctx
            .sessions
            .get_agent(sid)
            .await
            .ok_or_else(|| rpc_err(SESSION_NOT_FOUND, "Session not found"))?;

        // Process inline attachments: upload each, append markers to prompt.
        let mut prompt = req.prompt.clone();
        if !req.attachments.is_empty() {
            use super::attachments::process_file_entry;

            // Uploads go to the AGENT's workspace dir, not the session cwd.
            // The session cwd is often the user's project/git working tree
            // (e.g. when the TUI is launched from inside a repo), and we
            // don't want to splatter binary blobs into their source tree.
            // The per-agent workspace (`<config_dir>/agents/<alias>/workspace`)
            // is the canonical home for agent-owned files.
            let agent_alias = self
                .ctx
                .sessions
                .get_agent_alias(sid)
                .await
                .ok_or_else(|| rpc_err(SESSION_NOT_FOUND, "Session not found"))?;
            let upload_root = self
                .ctx
                .config
                .read()
                .agent_workspace_dir(&agent_alias)
                .to_string_lossy()
                .to_string();
            let is_wss = self.peer_label.starts_with("wss:");
            // Only insert a newline separator if there's existing text.
            // An attachment-only turn must not start with a leading "\n"
            // because that produces a user message whose only non-marker
            // content is whitespace — same failure mode the top-of-fn
            // guard prevents, just at one layer down.
            if !prompt.is_empty() {
                prompt.push('\n');
            }
            for (idx, entry) in req.attachments.iter().enumerate() {
                let result =
                    process_file_entry(entry, sid, &upload_root, is_wss, &self.ctx.sessions)
                        .await?;
                if idx > 0 {
                    prompt.push('\n');
                }
                prompt.push_str(&result.marker);
            }
        }

        let _guard = self
            .ctx
            .sessions
            .session_queue
            .acquire(sid)
            .await
            .map_err(|e| rpc_err(SESSION_BUSY, format!("Session busy: {e}")))?;

        let cancel = tokio_util::sync::CancellationToken::new();
        self.ctx.sessions.register_cancel_token(sid, cancel.clone());
        self.ctx.sessions.touch(sid).await;

        let chat_mode = self
            .ctx
            .sessions
            .chat_mode(sid)
            .await
            .unwrap_or(crate::rpc::types::ChatMode::Chat);
        let pre_history_len = if matches!(chat_mode, crate::rpc::types::ChatMode::Acp) {
            self.ctx.sessions.history_len(sid).await.unwrap_or(0)
        } else {
            0
        };

        // Capture attribution fields and max_context_tokens for the turn span.
        let (agent_alias, model_provider, model, max_ctx) = {
            let alias = self
                .ctx
                .sessions
                .get_agent_alias(sid)
                .await
                .unwrap_or_default();
            let cfg = self.ctx.config.read().clone();
            let mp = cfg
                .agent(&alias)
                .map(|a| a.model_provider.to_string())
                .unwrap_or_default();
            let m = cfg
                .model_provider_for_agent(&alias)
                .and_then(|p| p.model.clone())
                .unwrap_or_default();
            let max_ctx = Some(cfg.effective_max_context_tokens(&alias) as u64);
            (alias, mp, m, max_ctx)
        };

        let rpc = self.rpc.clone();
        let sid_owned = sid.to_string();
        let acp_token_store = if matches!(chat_mode, crate::rpc::types::ChatMode::Acp) {
            self.ctx.acp_session_store.clone()
        } else {
            None
        };
        let outcome = execute_turn(
            agent,
            prompt.clone(),
            cancel,
            TurnAttribution {
                session_key: Some(sid.to_string()),
                agent_alias,
                model_provider,
                model,
                channel: "rpc",
            },
            move |event| {
                let rpc = rpc.clone();
                let sid = sid_owned.clone();
                let acp_token_store = acp_token_store.clone();
                async move {
                    if let (
                        Some(store),
                        TurnEvent::Usage {
                            input_tokens: Some(it),
                            ..
                        },
                    ) = (acp_token_store.as_ref(), &event)
                    {
                        let _ = store.set_token_count(&sid, *it);
                    }
                    if let Some(n) = notification_for_turn_event(&sid, &event, max_ctx) {
                        let _ = rpc.send_raw(n).await;
                    }
                }
            },
        )
        .await;

        self.ctx.sessions.remove_cancel_token(sid);

        match chat_mode {
            crate::rpc::types::ChatMode::Acp => {
                if let Some(ref store) = self.ctx.acp_session_store
                    && matches!(
                        outcome,
                        Ok(TurnOutcome::Completed { .. }) | Ok(TurnOutcome::Cancelled { .. })
                    )
                    && let Some(new_msgs) = self
                        .ctx
                        .sessions
                        .history_slice_from(sid, pre_history_len)
                        .await
                    && !new_msgs.is_empty()
                    && let Err(e) = store.append_turn(sid, &new_msgs)
                {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"session_id": sid, "error": e.to_string()})
                            ),
                        "Failed to persist ACP turn"
                    );
                }
            }
            crate::rpc::types::ChatMode::Chat => {
                if let Some(ref backend) = self.ctx.session_backend {
                    let key = format!("rpc_{sid}");
                    let _ = backend.append(&key, &ChatMessage::user(&prompt));
                    match &outcome {
                        Ok(TurnOutcome::Completed { text, .. }) => {
                            let _ = backend.append(&key, &ChatMessage::assistant(text));
                        }
                        Ok(TurnOutcome::Cancelled { partial_text }) if !partial_text.is_empty() => {
                            let _ = backend.append(&key, &ChatMessage::assistant(partial_text));
                        }
                        _ => {}
                    }
                }
            }
        }

        match outcome {
            Ok(TurnOutcome::Completed { text, .. }) => to_result(SessionPromptResult {
                session_id: req.session_id,
                stop_reason: "end_turn".to_string(),
                content: text,
            }),
            Ok(TurnOutcome::Cancelled { partial_text }) => to_result(SessionPromptResult {
                session_id: req.session_id,
                stop_reason: "cancelled".to_string(),
                content: partial_text,
            }),
            Err(e) => Err(rpc_err(INTERNAL_ERROR, e.to_string())),
        }
    }

    async fn handle_session_configure(&self, params: &Value) -> RpcResult {
        let req: SessionConfigureParams = parse_params(params)?;

        let merged = self
            .ctx
            .sessions
            .set_overrides(&req.session_id, req.overrides)
            .await
            .ok_or_else(|| rpc_err(SESSION_NOT_FOUND, "Session not found"))?;

        to_result(SessionConfigureResult {
            session_id: req.session_id,
            overrides: merged,
        })
    }

    fn handle_session_cancel(&self, params: &Value) -> RpcResult {
        let req: SessionIdParams = parse_params(params)?;
        if self.ctx.sessions.cancel_session(&req.session_id) {
            to_result(SessionCancelResult {
                session_id: req.session_id,
                cancelled: true,
            })
        } else {
            Err(rpc_err(
                SESSION_NOT_FOUND,
                "No active turn for this session",
            ))
        }
    }

    async fn handle_session_list(&self, params: &Value) -> RpcResult {
        let backend = self
            .ctx
            .session_backend
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "Session persistence is disabled"))?;
        let req: SessionListParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();

        // Use FTS when a query is provided, plain list otherwise.
        let all = if let Some(ref keyword) = req.query {
            if keyword.trim().is_empty() {
                backend.list_sessions_with_metadata()
            } else {
                use zeroclaw_infra::session_backend::SessionQuery;
                backend.search(&SessionQuery {
                    keyword: Some(keyword.clone()),
                    limit: req.limit,
                })
            }
        } else {
            backend.list_sessions_with_metadata()
        };

        let sessions: Vec<SessionEntry> = all
            .into_iter()
            .filter(|meta| meta.agent_alias.is_some() || meta.channel_id.is_some())
            .map(|meta| {
                let agent_alias = meta.agent_alias.clone().or_else(|| {
                    meta.channel_id
                        .as_deref()
                        .and_then(|c| config.agent_for_channel(c))
                        .map(str::to_string)
                });
                let session_id = meta
                    .key
                    .strip_prefix("rpc_")
                    .or_else(|| meta.key.strip_prefix("gw_"))
                    .map(str::to_string)
                    .unwrap_or_else(|| meta.key.clone());
                SessionEntry {
                    session_id,
                    session_key: meta.key,
                    created_at: meta.created_at.to_rfc3339(),
                    last_activity: meta.last_activity.to_rfc3339(),
                    message_count: meta.message_count,
                    agent_alias,
                    channel_id: meta.channel_id,
                    name: meta.name,
                }
            })
            .collect();
        to_result(SessionListResult { sessions })
    }

    /// List ACP sessions from the dedicated ACP session store. The Code (ACP)
    /// pane in the TUI calls this instead of `session/list` so its picker only
    /// shows sessions that came from `acp-sessions.db` — chat-pane sessions
    /// live in the unified `session_backend` and must not appear here.
    async fn handle_session_list_acp(&self, _params: &Value) -> RpcResult {
        let store = self
            .ctx
            .acp_session_store
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "ACP session store is not available"))?;

        let summaries = store
            .list_sessions()
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("acp session list failed: {e}")))?;

        let sessions: Vec<SessionEntry> = summaries
            .into_iter()
            .map(|s| SessionEntry {
                session_id: s.session_uuid.clone(),
                // ACP sessions are keyed by their UUID directly — no `rpc_`/`gw_`
                // prefix exists in this store, so session_id == session_key.
                session_key: s.session_uuid,
                created_at: s.created_at.to_rfc3339(),
                last_activity: s.last_activity.to_rfc3339(),
                message_count: s.message_count,
                agent_alias: Some(s.agent_alias),
                channel_id: None,
                // ACP sessions don't carry a user-set display name today; the
                // picker falls back to `session_id` when this is None.
                name: None,
            })
            .collect();

        to_result(SessionListResult { sessions })
    }

    async fn handle_session_messages(&self, params: &Value) -> RpcResult {
        let req: SessionIdParams = parse_params(params)?;
        let backend = self
            .ctx
            .session_backend
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "Session persistence is disabled"))?;

        // Try the raw id first (channel sessions store as-is), then
        // prefixed variants for RPC/gateway-originated sessions.
        let candidates = [
            req.session_id.clone(),
            format!("rpc_{}", req.session_id),
            format!("gw_{}", req.session_id),
        ];
        let mut messages = Vec::new();
        for key in &candidates {
            let loaded = backend.load(key);
            if !loaded.is_empty() {
                messages = loaded
                    .iter()
                    .map(|m| MessageEntry {
                        role: m.role.clone(),
                        content: m.content.clone(),
                    })
                    .collect();
                break;
            }
        }

        to_result(SessionMessagesResult {
            session_id: req.session_id,
            messages,
        })
    }

    async fn handle_session_state(&self, params: &Value) -> RpcResult {
        let req: SessionIdParams = parse_params(params)?;
        let backend = self
            .ctx
            .session_backend
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "Session persistence is disabled"))?;
        let candidates = [
            req.session_id.clone(),
            format!("rpc_{}", req.session_id),
            format!("gw_{}", req.session_id),
        ];
        for key in &candidates {
            match backend.get_session_state(key) {
                Ok(Some(ss)) => {
                    return to_result(SessionStateResult {
                        session_id: req.session_id,
                        state: ss.state,
                        turn_id: ss.turn_id,
                        turn_started_at: ss.turn_started_at.map(|t| t.to_rfc3339()),
                    });
                }
                Ok(None) => continue,
                Err(e) => {
                    return Err(rpc_err(
                        INTERNAL_ERROR,
                        format!("Failed to get session state: {e}"),
                    ));
                }
            }
        }
        Err(rpc_err(SESSION_NOT_FOUND, "Session not found"))
    }

    async fn handle_session_delete(&self, params: &Value) -> RpcResult {
        let req: SessionIdParams = parse_params(params)?;
        // Remove from in-memory store.
        self.ctx.sessions.remove(&req.session_id).await;
        // Remove from persistent backend — try raw id, then prefixed variants.
        if let Some(ref backend) = self.ctx.session_backend {
            for key in &[
                req.session_id.clone(),
                format!("rpc_{}", req.session_id),
                format!("gw_{}", req.session_id),
            ] {
                let _ = backend.delete_session(key);
            }
        }
        to_result(SessionDeleteResult {
            session_id: req.session_id,
            deleted: true,
        })
    }

    async fn handle_session_rename(&self, params: &Value) -> RpcResult {
        let req: SessionRenameParams = parse_params(params)?;
        let backend = self
            .ctx
            .session_backend
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "Session persistence is disabled"))?;
        // Try all candidate keys — UPDATE on a missing key is a no-op.
        for key in &[
            req.session_id.clone(),
            format!("rpc_{}", req.session_id),
            format!("gw_{}", req.session_id),
        ] {
            backend
                .set_session_name(key, &req.name)
                .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Rename failed: {e}")))?;
        }
        to_result(SessionRenameResult {
            session_id: req.session_id,
            name: req.name,
        })
    }

    fn handle_session_approve(&self, params: &Value) -> RpcResult {
        let p: SessionApproveParams = parse_params(params)?;

        let response = match p.decision.as_str() {
            "allow_once" => zeroclaw_api::channel::ChannelApprovalResponse::Approve,
            "allow_always" => zeroclaw_api::channel::ChannelApprovalResponse::AlwaysApprove,
            "reject" | "reject_once" => zeroclaw_api::channel::ChannelApprovalResponse::Deny,
            "reject_with_edit" => {
                let replacement = p.replacement.unwrap_or_default();
                zeroclaw_api::channel::ChannelApprovalResponse::DenyWithEdit { replacement }
            }
            other => {
                return Err(rpc_err(
                    INVALID_PARAMS,
                    format!("unknown decision: {other}"),
                ));
            }
        };

        self.ctx.approval_pending.resolve(&p.request_id, response);

        to_result(SessionApproveResult {
            session_id: p.session_id,
            request_id: p.request_id,
            acknowledged: true,
        })
    }

    // ── Memory handlers ──────────────────────────────────────────

    async fn handle_memory_list(&self, params: &Value) -> RpcResult {
        let mem = self
            .ctx
            .memory
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "Memory subsystem is not available"))?;
        let req: MemoryListParams = parse_params(params)?;
        let category = req
            .category
            .as_deref()
            .map(|s| MemoryCategory::Custom(s.to_string()));
        let entries = mem
            .list(category.as_ref(), req.session_id.as_deref())
            .await
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Memory list failed: {e}")))?;
        let count = entries.len();
        to_result(MemoryListResult { entries, count })
    }

    async fn handle_memory_search(&self, params: &Value) -> RpcResult {
        let mem = self
            .ctx
            .memory
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "Memory subsystem is not available"))?;
        let req: MemorySearchParams = parse_params(params)?;
        let entries = mem
            .recall(
                &req.query,
                req.limit,
                req.session_id.as_deref(),
                req.since.as_deref(),
                req.until.as_deref(),
            )
            .await
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Memory search failed: {e}")))?;
        let count = entries.len();
        to_result(MemorySearchResult { entries, count })
    }

    async fn handle_memory_store(&self, params: &Value) -> RpcResult {
        let mem = self
            .ctx
            .memory
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "Memory subsystem is not available"))?;
        let req: MemoryStoreParams = parse_params(params)?;
        let category = req
            .category
            .as_deref()
            .map(|s| MemoryCategory::Custom(s.to_string()))
            .unwrap_or(MemoryCategory::Custom("user".into()));
        mem.store(&req.key, &req.content, category, req.session_id.as_deref())
            .await
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Memory store failed: {e}")))?;
        to_result(MemoryStoreResult {
            key: req.key,
            stored: true,
        })
    }

    async fn handle_memory_delete(&self, params: &Value) -> RpcResult {
        let mem = self
            .ctx
            .memory
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "Memory subsystem is not available"))?;
        let req: MemoryDeleteParams = parse_params(params)?;
        mem.forget(&req.key)
            .await
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Memory delete failed: {e}")))?;
        to_result(MemoryDeleteResult {
            key: req.key,
            deleted: true,
        })
    }

    // ── Cron handlers ────────────────────────────────────────────

    async fn handle_cron_list(&self) -> RpcResult {
        let config = self.ctx.config.read().clone();
        let jobs = crate::cron::list_jobs(&config)
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Cron list failed: {e}")))?;
        to_result(CronListResult { jobs })
    }

    async fn handle_cron_get(&self, params: &Value) -> RpcResult {
        let req: CronIdParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();
        let job = crate::cron::get_job(&config, &req.id)
            .map_err(|e| rpc_err(INVALID_PARAMS, format!("Cron job not found: {e}")))?;
        to_result(job)
    }

    async fn handle_cron_add(&self, params: &Value) -> RpcResult {
        let req: CronAddParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();
        let schedule = Schedule::Cron {
            expr: req.schedule,
            tz: req.tz,
        };
        let job = crate::cron::add_shell_job_with_approval(
            &config,
            &req.agent,
            req.name,
            schedule,
            req.command.as_deref().unwrap_or(""),
            req.delivery,
            true, // RPC calls are pre-approved
        )
        .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Cron add failed: {e}")))?;
        to_result(job)
    }

    async fn handle_cron_patch(&self, params: &Value) -> RpcResult {
        let req: CronPatchParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();
        let patch = CronJobPatch {
            schedule: req.schedule.map(|s| Schedule::Cron {
                expr: s,
                tz: if req.clear_tz == Some(true) {
                    None
                } else {
                    req.tz
                },
            }),
            command: req.command,
            prompt: req.prompt,
            name: req.name,
            ..Default::default()
        };
        let job = crate::cron::update_job(&config, &req.id, patch)
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Cron patch failed: {e}")))?;
        to_result(job)
    }

    async fn handle_cron_delete(&self, params: &Value) -> RpcResult {
        let req: CronIdParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();
        crate::cron::remove_job(&config, &req.id)
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Cron delete failed: {e}")))?;
        to_result(CronDeleteResult {
            id: req.id,
            deleted: true,
        })
    }

    async fn handle_cron_runs(&self, params: &Value) -> RpcResult {
        let req: CronRunsParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();
        let limit = req.limit.unwrap_or(20) as usize;
        let runs = crate::cron::list_runs(&config, &req.id, limit)
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Cron runs failed: {e}")))?;
        to_result(CronRunsResult { runs })
    }

    async fn handle_cron_trigger(&self, params: &Value) -> RpcResult {
        let req: CronIdParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();
        let job = crate::cron::get_job(&config, &req.id)
            .map_err(|e| rpc_err(INVALID_PARAMS, format!("Cron job not found: {e}")))?;
        let (success, output) = crate::cron::scheduler::execute_job_now(&config, &job).await;
        to_result(CronTriggerResult {
            id: req.id,
            success,
            output,
        })
    }

    async fn handle_cron_settings(&self, params: &Value) -> RpcResult {
        let config = self.ctx.config.read().clone();
        // If a "patch" field is present, this is a write; otherwise read.
        if params.get("patch").is_some() {
            not_yet_implemented(Method::CronSettings)
        } else {
            Ok(serde_json::to_value(&config.scheduler).unwrap_or(Value::Null))
        }
    }

    // ── Config handlers ──────────────────────────────────────────

    fn handle_config_get(&self, params: &Value) -> RpcResult {
        use zeroclaw_config::traits::MaskSecrets;
        let req: ConfigGetParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();
        if let Some(prop) = req.prop {
            let val = config
                .get_prop(&prop)
                .map_err(|e| rpc_err(INVALID_PARAMS, format!("Unknown prop: {e}")))?;
            to_result(ConfigGetPropResult { prop, value: val })
        } else {
            // Return full config, masked.
            let mut masked = config;
            masked.mask_secrets();
            Ok(serde_json::to_value(&masked).unwrap_or(Value::Null))
        }
    }

    async fn handle_config_set(&self, params: &Value) -> RpcResult {
        let req: ConfigSetParams = parse_params(params)?;
        {
            let mut config = self.ctx.config.write();
            let info = config
                .prop_fields()
                .into_iter()
                .find(|f| f.name == req.prop);
            // Polymorphic value: strings pass through, everything else coerced.
            let value_str = match &req.value {
                Value::String(s) => s.clone(),
                other => zeroclaw_config::typed_value::coerce_for_set_prop(
                    other,
                    info.as_ref().map(|i| i.kind),
                )
                .map_err(|e| rpc_err(INVALID_PARAMS, e.message))?,
            };
            // Reject the masked sentinel for secrets — surfaces echo the
            // masked display value back when no real edit happened, and
            // letting that through silently clobbers the live secret with
            // the literal masked string.
            if info
                .as_ref()
                .is_some_and(|i| i.is_secret || i.derived_from_secret)
                && (value_str == zeroclaw_config::traits::MASKED_SECRET
                    || value_str == "****"
                    || value_str.is_empty())
            {
                return Err(rpc_err(
                    INVALID_PARAMS,
                    format!(
                        "Refusing to overwrite secret `{}` with a masked or empty value",
                        req.prop
                    ),
                ));
            }
            config
                .set_prop_persistent(&req.prop, &value_str)
                .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Config set failed: {e}")))?;
        }
        self.flush_config().await?;
        to_result(ConfigSetResult {
            prop: req.prop,
            set: true,
        })
    }

    fn handle_config_validate(&self) -> RpcResult {
        let config = self.ctx.config.read().clone();
        match config.validate() {
            Ok(()) => to_result(ConfigValidateResult {
                valid: true,
                error: None,
            }),
            Err(e) => to_result(ConfigValidateResult {
                valid: false,
                error: Some(e.to_string()),
            }),
        }
    }

    fn handle_config_reload(&self) -> RpcResult {
        if let Some(ref tx) = self.ctx.reload_tx {
            let _ = tx.send(true);
            to_result(ConfigReloadResult { reloading: true })
        } else {
            Err(rpc_err(INTERNAL_ERROR, "Reload not available"))
        }
    }

    fn handle_config_list(&self, params: &Value) -> RpcResult {
        use zeroclaw_config::field_visibility;
        use zeroclaw_config::traits::ConfigFieldEntry;
        let req: ConfigListParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();
        let prefix = req.prefix.as_deref();
        let excluded = field_visibility::excluded_paths(&config, prefix.unwrap_or(""));
        let entries: Vec<ConfigFieldEntry> = config
            .prop_fields()
            .into_iter()
            .filter(|info| match prefix {
                Some(p) => info.name.starts_with(p),
                None => true,
            })
            .filter(|info| !field_visibility::is_excluded(&info.name, &excluded))
            .map(|info| {
                let env = config.prop_is_env_overridden(&info.name);
                ConfigFieldEntry::from_prop_field(info, env)
            })
            .collect();
        to_result(ConfigListResult { entries })
    }

    async fn handle_config_delete(&self, params: &Value) -> RpcResult {
        let req: ConfigDeleteParams = parse_params(params)?;
        {
            let mut config = self.ctx.config.write();
            config
                .set_prop_persistent(&req.prop, "")
                .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Config delete failed: {e}")))?;
        }
        self.flush_config().await?;
        to_result(ConfigDeleteResult {
            prop: req.prop,
            deleted: true,
        })
    }

    fn handle_config_map_keys(&self, params: &Value) -> RpcResult {
        let req: ConfigMapKeysParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();
        let keys = config.get_map_keys(&req.path).ok_or_else(|| {
            rpc_err(
                INVALID_PARAMS,
                format!("No map-keyed section at `{}`", req.path),
            )
        })?;
        to_result(ConfigMapKeysResult {
            path: req.path,
            keys,
        })
    }

    async fn handle_config_map_key_create(&self, params: &Value) -> RpcResult {
        let req: ConfigMapKeyCreateParams = parse_params(params)?;
        let created = {
            let mut config = self.ctx.config.write();
            let created = config
                .create_map_key(&req.path, &req.key)
                .map_err(|e| rpc_err(INVALID_PARAMS, e))?;
            if created {
                config.mark_dirty(&format!("{}.{}", req.path, req.key));
            }
            created
        };
        if created {
            self.flush_config().await?;
        }
        to_result(ConfigMapKeyCreateResult {
            path: req.path,
            key: req.key,
            created,
        })
    }

    async fn handle_config_map_key_delete(&self, params: &Value) -> RpcResult {
        let req: ConfigMapKeyDeleteParams = parse_params(params)?;
        let deleted = {
            let mut config = self.ctx.config.write();
            let deleted = config
                .delete_map_key(&req.path, &req.key)
                .map_err(|e| rpc_err(INVALID_PARAMS, e))?;
            if deleted {
                config.mark_dirty(&format!("{}.{}", req.path, req.key));
            }
            deleted
        };
        if deleted {
            self.flush_config().await?;
        }
        to_result(ConfigMapKeyDeleteResult {
            path: req.path,
            key: req.key,
            deleted,
        })
    }

    async fn handle_config_map_key_rename(&self, params: &Value) -> RpcResult {
        let req: ConfigMapKeyRenameParams = parse_params(params)?;
        let renamed = {
            let mut config = self.ctx.config.write();
            let renamed = config
                .rename_map_key(&req.path, &req.from, &req.to)
                .map_err(|e| rpc_err(INVALID_PARAMS, e))?;
            if renamed {
                config.mark_dirty(&format!("{}.{}", req.path, req.from));
                config.mark_dirty(&format!("{}.{}", req.path, req.to));
            }
            renamed
        };
        if renamed {
            self.flush_config().await?;
        }
        to_result(ConfigMapKeyRenameResult {
            path: req.path,
            from: req.from,
            to: req.to,
            renamed,
        })
    }

    fn handle_config_templates(&self) -> RpcResult {
        use zeroclaw_config::schema::Config;
        let templates: Vec<ConfigTemplateEntry> = Config::map_key_sections()
            .into_iter()
            .map(Into::into)
            .collect();
        to_result(ConfigTemplatesResult { templates })
    }

    // ── Agents handlers ──────────────────────────────────────────

    fn handle_agents_list(&self) -> RpcResult {
        let config = self.ctx.config.read().clone();
        let agents: Vec<AgentEntry> = config
            .agents
            .iter()
            .map(|(alias, agent_cfg)| AgentEntry {
                alias: alias.clone(),
                enabled: agent_cfg.enabled,
                channels: agent_cfg.channels.iter().map(|c| c.to_string()).collect(),
            })
            .collect();
        to_result(AgentsListResult { agents })
    }

    async fn handle_agents_status(&self) -> RpcResult {
        let config = self.ctx.config.read().clone();

        // Count sessions from the persisted backend (covers channel-originated
        // sessions) + in-memory RPC sessions, deduped by taking the max.
        let rpc_counts = self.ctx.sessions.count_by_agent().await;
        let mut backend_counts = std::collections::HashMap::<String, usize>::new();
        if let Some(ref backend) = self.ctx.session_backend {
            for meta in backend.list_sessions_with_metadata() {
                let alias = meta.agent_alias.or_else(|| {
                    meta.channel_id
                        .as_deref()
                        .and_then(|c| config.agent_for_channel(c))
                        .map(str::to_string)
                });
                if let Some(a) = alias {
                    *backend_counts.entry(a).or_default() += 1;
                }
            }
        }

        let agents: Vec<AgentStatusEntry> = config
            .agents
            .iter()
            .map(|(alias, agent_cfg)| {
                let rpc = *rpc_counts.get(alias).unwrap_or(&0);
                let persisted = *backend_counts.get(alias).unwrap_or(&0);
                AgentStatusEntry {
                    alias: alias.clone(),
                    enabled: agent_cfg.enabled,
                    active_sessions: rpc.max(persisted),
                    channels: agent_cfg.channels.iter().map(|c| c.to_string()).collect(),
                }
            })
            .collect();
        to_result(AgentsStatusResult { agents })
    }

    // ── Cost handler ─────────────────────────────────────────────

    fn handle_cost_query(&self, params: &Value) -> RpcResult {
        let tracker = self
            .ctx
            .cost_tracker
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "Cost tracking is not available"))?;
        let req: CostQueryParams = parse_params(params)?;
        let summary = if let Some(agent) = req.agent {
            tracker
                .get_summary_for_agent(&agent)
                .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Cost query failed: {e}")))?
        } else {
            tracker
                .get_summary()
                .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Cost query failed: {e}")))?
        };
        to_result(summary)
    }

    // ── Skills handlers ──────────────────────────────────────────

    fn handle_skills_bundles(&self) -> RpcResult {
        let config = self.ctx.config.read().clone();
        let root = config.install_root_dir();
        let svc = crate::skills::service::SkillsService::new(&config, &root);
        let bundles: Vec<SkillBundleEntry> = svc
            .list_bundles()
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Skills bundles failed: {e}")))?
            .into_iter()
            .map(|b| SkillBundleEntry {
                alias: b.alias,
                directory: b.directory.to_string_lossy().to_string(),
                include: b.include,
                exclude: b.exclude,
            })
            .collect();
        to_result(SkillsBundlesResult { bundles })
    }

    fn handle_skills_list(&self, params: &Value) -> RpcResult {
        let req: SkillsListParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();
        let root = config.install_root_dir();
        let svc = crate::skills::service::SkillsService::new(&config, &root);
        let skills: Vec<SkillListEntry> = svc
            .list_skills(req.bundle.as_deref())
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Skills list failed: {e}")))?
            .into_iter()
            .map(|s| SkillListEntry {
                bundle: s.r#ref.bundle().to_string(),
                name: s.r#ref.name().to_string(),
                directory: s.directory.to_string_lossy().to_string(),
                frontmatter: s.frontmatter,
            })
            .collect();
        to_result(SkillsListResult { skills })
    }

    fn handle_skills_read(&self, params: &Value) -> RpcResult {
        let req: SkillsReadParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();
        let root = config.install_root_dir();
        let svc = crate::skills::service::SkillsService::new(&config, &root);
        let skill_ref = svc
            .resolve_ref(&req.name, Some(&req.bundle))
            .map_err(|e| rpc_err(INVALID_PARAMS, format!("Invalid skill ref: {e}")))?;
        let doc = svc
            .read_skill(&skill_ref)
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Skill read failed: {e}")))?;
        to_result(SkillsReadResult {
            bundle: req.bundle,
            name: req.name,
            frontmatter: doc.frontmatter,
            body: doc.body,
        })
    }

    fn handle_skills_write(&self, params: &Value) -> RpcResult {
        let req: SkillsWriteParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();
        let root = config.install_root_dir();
        let svc = crate::skills::service::SkillsService::new(&config, &root);
        let skill_ref = svc
            .resolve_ref(&req.name, Some(&req.bundle))
            .map_err(|e| rpc_err(INVALID_PARAMS, format!("Invalid skill ref: {e}")))?;
        let doc = crate::skills::document::SkillDocument {
            frontmatter: req.frontmatter,
            body: req.body,
        };
        svc.write_skill(&skill_ref, &doc)
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Skill write failed: {e}")))?;
        to_result(SkillsWriteResult {
            bundle: req.bundle,
            name: req.name,
            written: true,
        })
    }

    fn handle_skills_delete(&self, params: &Value) -> RpcResult {
        let req: SkillsDeleteParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();
        let root = config.install_root_dir();
        let svc = crate::skills::service::SkillsService::new(&config, &root);
        let skill_ref = svc
            .resolve_ref(&req.name, Some(&req.bundle))
            .map_err(|e| rpc_err(INVALID_PARAMS, format!("Invalid skill ref: {e}")))?;
        svc.remove_skill(&skill_ref, crate::skills::service::RemoveMode::Archive)
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Skill delete failed: {e}")))?;
        to_result(SkillsDeleteResult {
            bundle: req.bundle,
            name: req.name,
            deleted: true,
        })
    }

    // ── Personality handlers ─────────────────────────────────────

    fn handle_personality_list(&self, params: &Value) -> RpcResult {
        let req: PersonalityListParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();
        let workspace = req.agent.as_deref().map(|a| config.agent_workspace_dir(a));
        let files: Vec<PersonalityFileEntry> =
            crate::agent::personality::EDITABLE_PERSONALITY_FILES
                .iter()
                .map(|&filename| {
                    let (exists, size, mtime_ms) = workspace
                        .as_ref()
                        .and_then(|dir| {
                            let path = dir.join(filename);
                            let meta = std::fs::metadata(&path).ok()?;
                            let mtime = meta
                                .modified()
                                .ok()
                                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                .map(|d| d.as_millis() as i64);
                            Some((true, meta.len(), mtime))
                        })
                        .unwrap_or((false, 0, None));
                    PersonalityFileEntry {
                        filename: filename.to_string(),
                        exists,
                        size,
                        mtime_ms,
                    }
                })
                .collect();
        to_result(PersonalityListResult {
            files,
            max_chars: crate::agent::personality::MAX_FILE_CHARS,
        })
    }

    fn handle_personality_get(&self, params: &Value) -> RpcResult {
        let req: PersonalityGetParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();

        // Sandbox: only allow files from the allowlist.
        if !crate::agent::personality::EDITABLE_PERSONALITY_FILES.contains(&req.filename.as_str()) {
            return Err(rpc_err(
                INVALID_PARAMS,
                format!("Not an editable file: {}", req.filename),
            ));
        }
        let workspace = config.agent_workspace_dir(&req.agent);
        let path = workspace.join(&req.filename);
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let mtime_ms = std::fs::metadata(&path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64);
                let truncated = content.chars().count() > crate::agent::personality::MAX_FILE_CHARS;
                to_result(PersonalityGetResult {
                    filename: req.filename,
                    content: Some(content),
                    exists: true,
                    truncated,
                    mtime_ms,
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => to_result(PersonalityGetResult {
                filename: req.filename,
                content: None,
                exists: false,
                truncated: false,
                mtime_ms: None,
            }),
            Err(e) => Err(rpc_err(INTERNAL_ERROR, format!("Read failed: {e}"))),
        }
    }

    fn handle_personality_put(&self, params: &Value) -> RpcResult {
        let req: PersonalityPutParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();

        if !crate::agent::personality::EDITABLE_PERSONALITY_FILES.contains(&req.filename.as_str()) {
            return Err(rpc_err(
                INVALID_PARAMS,
                format!("Not an editable file: {}", req.filename),
            ));
        }
        if req.content.chars().count() > crate::agent::personality::MAX_FILE_CHARS {
            return Err(rpc_err(
                INVALID_PARAMS,
                format!(
                    "Content exceeds {} char limit",
                    crate::agent::personality::MAX_FILE_CHARS
                ),
            ));
        }
        let workspace = config.agent_workspace_dir(&req.agent);
        let path = workspace.join(&req.filename);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, &req.content)
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Write failed: {e}")))?;
        let bytes_written = req.content.len() as u64;
        let mtime_ms = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64);
        to_result(PersonalityPutResult {
            bytes_written,
            mtime_ms,
        })
    }

    fn handle_personality_templates(&self, params: &Value) -> RpcResult {
        let req: PersonalityTemplatesParams = parse_params(params)?;
        let agent_alias = req.agent.as_deref().unwrap_or("default");
        let config = self.ctx.config.read().clone();
        let ctx = crate::agent::personality_templates::TemplateContext {
            agent: config
                .agent(agent_alias)
                .map(|_| agent_alias.to_string())
                .unwrap_or_else(|| "ZeroClaw".to_string()),
            include_memory: config.agent(agent_alias).is_some(),
            ..Default::default()
        };
        let templates = crate::agent::personality_templates::render_preset_default(&ctx);
        let files: Vec<TemplateFileEntry> = templates
            .into_iter()
            .map(|(name, content)| TemplateFileEntry {
                filename: name.to_string(),
                content,
            })
            .collect();
        to_result(PersonalityTemplatesResult {
            preset: "default".to_string(),
            files,
        })
    }

    // ── Config introspection handlers ───────────────────────────

    fn handle_config_sections(&self) -> RpcResult {
        use zeroclaw_config::schema::Config;
        use zeroclaw_config::sections::{
            QUICKSTART_SECTIONS, Section, SectionShape, section_help, section_index_for_key,
        };

        let config = self.ctx.config.read().clone();

        // Schema-driven: walk Config::prop_fields() to discover ALL
        // top-level section roots, not just QUICKSTART_SECTIONS.
        let mut roots: std::collections::BTreeSet<String> = config
            .prop_fields()
            .iter()
            .filter_map(|f| f.name.split('.').next().map(str::to_string))
            .collect();

        // Hidden system fields the user never edits.
        const HIDDEN: &[&str] = &[
            "schema_version",
            "onboard_state",
            "onboard-state",
            "config_path",
            "workspace_dir",
            "env_overridden_paths",
            "pre_override_snapshots",
        ];
        for h in HIDDEN {
            roots.remove(*h);
        }

        // Map-keyed sections surface even when empty.
        let all_map_paths: Vec<&'static str> =
            Config::map_key_sections().iter().map(|s| s.path).collect();
        for &prefix in &all_map_paths
            .iter()
            .filter_map(|p| p.split('.').next())
            .collect::<std::collections::HashSet<_>>()
        {
            roots.insert(prefix.to_string());
        }

        // Inject synthetic onboarding sections (e.g. personality).
        for s in QUICKSTART_SECTIONS {
            roots.insert(s.as_str().to_string());
        }

        // Drop bare parents when a dotted child exists
        // (`providers` vanishes once `providers.models` is present).
        let parents_with_children: std::collections::HashSet<String> = roots
            .iter()
            .filter_map(|k| k.split_once('.').map(|(p, _)| p.to_string()))
            .collect();
        roots.retain(|k| k.contains('.') || !parents_with_children.contains(k));

        // Hide cost.rates subtree.
        roots.retain(|k| !k.starts_with("cost.rates"));

        // Sort: onboarding sections in canonical order first, rest alpha.
        let mut ordered: Vec<String> = roots.into_iter().collect();
        ordered.sort_by(
            |a, b| match (section_index_for_key(a), section_index_for_key(b)) {
                (Some(ai), Some(bi)) => ai.cmp(&bi),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.cmp(b),
            },
        );

        // Picker eligibility: map-keyed section or onboarding section
        // with a picker shape.
        let section_has_picker_for_key = |key: &str| -> bool {
            let key_dot = format!("{key}.");
            all_map_paths.iter().any(|p| {
                *p == key
                    || p.strip_prefix(&key_dot)
                        .is_some_and(|rest| !rest.contains('.'))
            })
        };

        let sections: Vec<ConfigSectionEntry> = ordered
            .into_iter()
            .map(|key| {
                let wizard = Section::from_key(&key);
                let has_picker = match wizard {
                    Some(w) => matches!(
                        w.shape(),
                        SectionShape::TypedFamilyMap | SectionShape::OneTierAliasMap
                    ),
                    None => section_has_picker_for_key(&key),
                };
                let completed = wizard
                    .map(|w| zeroclaw_config::sections::section_has_signal(&config, w))
                    .unwrap_or(false);
                let label = humanize_section_key(&key);
                ConfigSectionEntry {
                    help: section_help(&key).to_string(),
                    has_picker,
                    completed,
                    ready: false,
                    group: String::new(),
                    is_quickstart: wizard.is_some(),
                    shape: wizard.map(Section::shape),
                    label,
                    key,
                }
            })
            .collect();
        to_result(ConfigSectionsResult { sections })
    }

    fn handle_config_status(&self) -> RpcResult {
        use zeroclaw_config::sections::QUICKSTART_SECTIONS;
        let config = self.ctx.config.read().clone();
        let missing: Vec<String> = QUICKSTART_SECTIONS
            .iter()
            .filter(|&&s| !zeroclaw_config::sections::section_has_signal(&config, s))
            .map(|s| s.as_str().to_string())
            .collect();
        let needs_quickstart = !missing.is_empty();
        let reason = if needs_quickstart {
            format!("{} section(s) incomplete", missing.len())
        } else {
            "all sections complete".to_string()
        };
        to_result(ConfigStatusResult {
            needs_quickstart,
            reason,
            has_partial_state: false,
            missing,
        })
    }

    fn handle_config_catalog(&self) -> RpcResult {
        let providers: Vec<CatalogModelProvider> = zeroclaw_providers::list_model_providers()
            .into_iter()
            .map(|p| CatalogModelProvider {
                name: p.name.to_string(),
                display_name: p.display_name.to_string(),
                local: p.local,
            })
            .collect();
        to_result(CatalogResponse {
            model_providers: providers,
        })
    }

    async fn handle_config_catalog_models(&self, params: &Value) -> RpcResult {
        let req: CatalogModelsParams = parse_params(params)?;
        let family = &req.model_provider;
        let local = matches!(
            family.as_str(),
            "ollama" | "llamacpp" | "lmstudio" | "vllm" | "sglang"
        );
        let models = zeroclaw_providers::catalog::list_models_for_family(family)
            .await
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Catalog models failed: {e}")))?;
        to_result(CatalogModelsResult {
            model_provider: req.model_provider,
            models,
            local,
            live: true,
        })
    }

    // ── Logs handler ─────────────────────────────────────────────

    async fn handle_logs_subscribe(&self) -> RpcResult {
        let event_tx = self
            .ctx
            .event_tx
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "Event streaming is not available"))?;
        let mut rx = event_tx.subscribe();
        let rpc = self.rpc.clone();
        // Spawn a forwarding task that lives until the subscriber drops.
        zeroclaw_spawn::spawn!(async move {
            while let Ok(event) = rx.recv().await {
                let notification = JsonRpcNotification::new(notification::LOGS_EVENT, event);
                if let Ok(json) = serde_json::to_string(&notification)
                    && !rpc.send_raw(json).await
                {
                    break;
                }
            }
        });
        to_result(LogsSubscribeResult { subscribed: true })
    }

    async fn handle_logs_query(&self, params: &Value) -> RpcResult {
        let p: LogsQueryParams = parse_params(params)?;

        let path = zeroclaw_log::current_log_path()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "Log persistence is not enabled"))?;

        let filter = zeroclaw_log::LogFilter {
            since_ts: p.since_ts,
            until_ts: p.until_ts,
            until_id: p.until_id,
            action: p.action,
            category: p.category,
            outcome: p.outcome,
            severity_min: p.severity_min,
            trace_id: p.trace_id,
            q: p.q,
            hide_internal: p.hide_internal,
            field_eq: std::collections::BTreeMap::new(),
        };

        let limit = p.limit.unwrap_or(200);

        let page = zeroclaw_log::load_page(&path, &filter, limit)
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Log read failed: {e:#}")))?;

        let events: Vec<serde_json::Value> = page
            .events
            .into_iter()
            .filter_map(|evt| serde_json::to_value(evt).ok())
            .collect();

        to_result(LogsQueryResult {
            events,
            next_cursor: page.next_cursor,
            at_end: page.at_end,
        })
    }

    // ── File attachment handler ────────────────────────────────

    async fn handle_file_attach(&self, params: &Value) -> RpcResult {
        use super::attachments::{MAX_REQUEST_BYTES, process_file_entry};

        let req: FileAttachParams = parse_params(params)?;
        let sid = &req.session_id;

        // Uploads land in the per-agent workspace, not the session cwd.
        // See `handle_send_message` for the rationale.
        let agent_alias = self
            .ctx
            .sessions
            .get_agent_alias(sid)
            .await
            .ok_or_else(|| rpc_err(SESSION_NOT_FOUND, "Session not found"))?;
        let upload_root = self
            .ctx
            .config
            .read()
            .agent_workspace_dir(&agent_alias)
            .to_string_lossy()
            .to_string();

        let is_wss = self.peer_label.starts_with("wss:");

        let mut total_bytes: u64 = 0;
        let mut results = Vec::with_capacity(req.files.len());

        for entry in &req.files {
            let result =
                process_file_entry(entry, sid, &upload_root, is_wss, &self.ctx.sessions).await?;
            total_bytes += result.size_bytes;
            if total_bytes > MAX_REQUEST_BYTES {
                return Err(rpc_err(
                    INVALID_PARAMS,
                    format!(
                        "Total attachment size exceeds {} MB limit",
                        MAX_REQUEST_BYTES / (1024 * 1024)
                    ),
                ));
            }
            results.push(result);
        }

        to_result(FileAttachResult { files: results })
    }

    // ── Wire helpers ─────────────────────────────────────────────

    async fn send_result(&self, id: Value, result: Value) {
        let resp = JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION,
            result: Some(result),
            error: None,
            id,
        };
        if let Ok(json) = serde_json::to_string(&resp) {
            let _ = self.rpc.send_raw(json).await;
        }
    }

    async fn send_error(&self, id: Value, code: i32, message: &str) {
        let resp = JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
                data: None,
            }),
            id,
        };
        if let Ok(json) = serde_json::to_string(&resp) {
            let _ = self.rpc.send_raw(json).await;
        }
    }

    // ── Quickstart ───────────────────────────────────────────────
    //
    // RPC mirror of the HTTP `/api/quickstart/*` routes in
    // `zeroclaw-gateway`. All business logic lives in
    // `zeroclaw_runtime::quickstart`; these handlers are call-the-runtime
    // plumbing only — they MUST stay byte-equivalent to the HTTP routes
    // so the drift test holds.

    fn handle_quickstart_state(&self) -> RpcResult {
        let cfg = self.ctx.config.read().clone();
        to_result(crate::quickstart::snapshot_state(&cfg))
    }

    fn handle_quickstart_fields(&self, params: &Value) -> RpcResult {
        let req: QuickstartFieldsParams = parse_params(params)?;
        let descriptors = crate::quickstart::field_shape(req.section, &req.type_key);
        to_result(QuickstartFieldsResult {
            fields: descriptors,
        })
    }

    fn handle_quickstart_validate(&self, params: &Value) -> RpcResult {
        let req: QuickstartValidateParams = parse_params(params)?;
        let cfg = self.ctx.config.read().clone();
        let body = match crate::quickstart::validate_only_with_surface(
            &req.submission,
            &cfg,
            crate::quickstart::Surface::Tui,
        ) {
            Ok(()) => QuickstartValidateResult::Ok,
            Err(errors) => QuickstartValidateResult::Errors { errors },
        };
        to_result(body)
    }

    async fn handle_quickstart_apply(&self, params: &Value) -> RpcResult {
        let req: QuickstartApplyParams = parse_params(params)?;
        // Clone out of the lock to satisfy `&mut Config`. On success
        // write the mutated snapshot back, mirroring `flush_config`
        // and the gateway's `handle_apply`.
        let mut working = self.ctx.config.read().clone();
        let result = crate::quickstart::apply_with_surface(
            req.submission,
            &mut working,
            crate::quickstart::Surface::Tui,
        )
        .await;
        let body = match result {
            Ok(agent) => {
                *self.ctx.config.write() = working;
                let reload_signalled = self.signal_daemon_reload();
                QuickstartApplyResult::Applied {
                    agent,
                    daemon_restarted: reload_signalled,
                }
            }
            Err(errors) => QuickstartApplyResult::Errors { errors },
        };
        to_result(body)
    }

    fn handle_quickstart_dismiss(&self, params: &Value) -> RpcResult {
        let req: QuickstartDismissParams = parse_params(params)?;
        crate::quickstart::record_dismissed(&req.run_id, req.surface, req.last_step);
        to_result(QuickstartDismissResult { recorded: true })
    }

    /// Signal the in-place daemon reload using the same `reload_tx`
    /// watch channel `/admin/reload` and the gateway's quickstart route
    /// use. Returns `true` when the supervisor was notified, `false`
    /// when no supervisor is attached (e.g. test harness).
    fn signal_daemon_reload(&self) -> bool {
        let Some(reload_tx) = self.ctx.reload_tx.clone() else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "reason": "no_supervisor",
                        "surface": crate::quickstart::Surface::Tui.as_str(),
                    })),
                "quickstart: daemon reload not available (standalone daemon)"
            );
            return false;
        };
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Start).with_attrs(
                ::serde_json::json!({
                    "surface": crate::quickstart::Surface::Tui.as_str(),
                })
            ),
            "quickstart: daemon reload signalled"
        );
        let started = std::time::Instant::now();
        zeroclaw_spawn::spawn!(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let _ = reload_tx.send(true);
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_attrs(::serde_json::json!({
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                        "surface": crate::quickstart::Surface::Tui.as_str(),
                    })),
                "quickstart: daemon reload dispatched"
            );
        });
        true
    }
}

// ── Helpers ──────────────────────────────────────────────────────

/// Humanize a section key for display (`risk-profiles` → `Risk profiles`).
fn humanize_section_key(key: &str) -> String {
    match key {
        "providers.models" => return "Model providers".to_string(),
        "providers.tts" => return "TTS providers".to_string(),
        "providers.transcription" => return "Transcription providers".to_string(),
        _ => {}
    }
    let mut s = key.replace(['_', '-'], " ");
    if let Some(c) = s.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    s
}

fn parse_params<T: DeserializeOwned>(params: &Value) -> Result<T, JsonRpcError> {
    serde_json::from_value(params.clone()).map_err(|e| rpc_err(INVALID_PARAMS, e.to_string()))
}

fn to_result<T: Serialize>(val: T) -> RpcResult {
    serde_json::to_value(val).map_err(|e| rpc_err(INTERNAL_ERROR, e.to_string()))
}

fn notification_for_turn_event(
    session_id: &str,
    event: &TurnEvent,
    max_context_tokens: Option<u64>,
) -> Option<String> {
    let update = match event {
        TurnEvent::Chunk { delta } => SessionUpdateEvent::AgentMessageChunk {
            session_id: session_id.to_string(),
            text: delta.clone(),
        },
        TurnEvent::Thinking { delta } => SessionUpdateEvent::AgentThoughtChunk {
            session_id: session_id.to_string(),
            text: delta.clone(),
        },
        TurnEvent::ToolCall { id, name, args } => SessionUpdateEvent::ToolCall {
            session_id: session_id.to_string(),
            tool_call_id: id.clone(),
            name: name.clone(),
            raw_input: args.clone(),
        },
        TurnEvent::ToolResult { id, name, output } => SessionUpdateEvent::ToolResult {
            session_id: session_id.to_string(),
            tool_call_id: id.clone(),
            name: name.clone(),
            raw_output: output.clone(),
        },
        TurnEvent::ApprovalRequest {
            request_id,
            tool_name,
            arguments_summary,
            timeout_secs,
        } => SessionUpdateEvent::ApprovalRequest {
            session_id: session_id.to_string(),
            request_id: request_id.clone(),
            tool_name: tool_name.clone(),
            arguments_summary: arguments_summary.clone(),
            timeout_secs: *timeout_secs,
        },
        TurnEvent::Usage {
            input_tokens,
            cached_input_tokens: _,
            output_tokens: _,
            ..
        } => {
            // `input_tokens` per TokenUsage contract is the *total* prompt
            // size (uncached + cached). `cached_input_tokens` is a subset
            // and must NOT be added — doing so double-counts cache reads
            // and inflates the displayed context size (was showing ~2× the
            // real value on Anthropic / OpenAI sessions with prompt cache).
            SessionUpdateEvent::ContextUsage {
                session_id: session_id.to_string(),
                input_tokens: *input_tokens,
                max_context_tokens,
            }
        }
    };

    let params = serde_json::to_value(update).ok()?;
    let n = JsonRpcNotification::new(notification::SESSION_UPDATE, params);
    serde_json::to_string(&n).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn method_from_wire_roundtrip() {
        for (method, wire) in Method::ALL {
            assert_eq!(
                Method::from_wire(wire),
                Some(*method),
                "from_wire({wire}) should resolve"
            );
            assert_eq!(method.wire_name(), *wire, "wire_name roundtrip for {wire}");
        }
    }

    #[test]
    fn method_from_wire_unknown() {
        assert_eq!(Method::from_wire("nonexistent/method"), None);
    }

    #[test]
    fn method_all_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for (_, wire) in Method::ALL {
            assert!(seen.insert(*wire), "duplicate wire name: {wire}");
        }
    }

    #[test]
    fn chunk_notification() {
        let event = TurnEvent::Chunk {
            delta: "hello".into(),
        };
        let json = notification_for_turn_event("s1", &event, None).unwrap();
        let v = parse(&json);
        assert_eq!(v["jsonrpc"], JSONRPC_VERSION);
        assert_eq!(v["method"], notification::SESSION_UPDATE);
        assert_eq!(v["params"]["session_id"], "s1");
        assert_eq!(v["params"]["type"], "agent_message_chunk");
        assert_eq!(v["params"]["text"], "hello");
    }

    #[test]
    fn thinking_notification() {
        let event = TurnEvent::Thinking {
            delta: "hmm".into(),
        };
        let json = notification_for_turn_event("s1", &event, None).unwrap();
        let v = parse(&json);
        assert_eq!(v["params"]["type"], "agent_thought_chunk");
        assert_eq!(v["params"]["text"], "hmm");
    }

    #[test]
    fn tool_call_notification() {
        let event = TurnEvent::ToolCall {
            id: "tc_1".into(),
            name: "bash".into(),
            args: json!({"cmd": "ls"}),
        };
        let json = notification_for_turn_event("s1", &event, None).unwrap();
        let v = parse(&json);
        assert_eq!(v["params"]["type"], "tool_call");
        assert_eq!(v["params"]["tool_call_id"], "tc_1");
        assert_eq!(v["params"]["name"], "bash");
        assert_eq!(v["params"]["raw_input"]["cmd"], "ls");
    }

    #[test]
    fn tool_result_notification() {
        let event = TurnEvent::ToolResult {
            id: "tc_1".into(),
            name: "bash".into(),
            output: "file.txt".into(),
        };
        let json = notification_for_turn_event("s1", &event, None).unwrap();
        let v = parse(&json);
        assert_eq!(v["params"]["type"], "tool_result");
        assert_eq!(v["params"]["tool_call_id"], "tc_1");
        assert_eq!(v["params"]["raw_output"], "file.txt");
    }

    #[test]
    fn approval_request_notification() {
        let event = TurnEvent::ApprovalRequest {
            request_id: "ar_1".into(),
            tool_name: "bash".into(),
            arguments_summary: "rm -rf /".into(),
            timeout_secs: 30,
        };
        let json = notification_for_turn_event("s1", &event, None).unwrap();
        let v = parse(&json);
        assert_eq!(v["params"]["type"], "approval_request");
        assert_eq!(v["params"]["request_id"], "ar_1");
        assert_eq!(v["params"]["tool_name"], "bash");
        assert_eq!(v["params"]["timeout_secs"], 30);
    }

    #[test]
    fn usage_event_emits_context_usage_notification() {
        let event = TurnEvent::Usage {
            input_tokens: Some(100),
            cached_input_tokens: None,
            output_tokens: Some(50),
            cost_usd: Some(0.01),
        };
        let json = notification_for_turn_event("s1", &event, Some(32_000)).unwrap();
        let v = parse(&json);
        assert_eq!(v["params"]["type"], "context_usage");
        assert_eq!(v["params"]["session_id"], "s1");
        // Context size is the prompt the model just consumed = input_tokens.
        // Output tokens are the model's reply, not part of the prompt size.
        // cached_input_tokens is a *subset* of input_tokens per the
        // TokenUsage contract and must NOT be added (double-counts).
        assert_eq!(v["params"]["input_tokens"], 100);
        assert_eq!(v["params"]["max_context_tokens"], 32_000);
    }

    #[test]
    fn usage_event_without_input_tokens_emits_null() {
        let event = TurnEvent::Usage {
            input_tokens: None,
            cached_input_tokens: None,
            output_tokens: Some(50),
            cost_usd: None,
        };
        let json = notification_for_turn_event("s1", &event, None).unwrap();
        let v = parse(&json);
        assert_eq!(v["params"]["type"], "context_usage");
        // No input_tokens reported → field omitted (skip_serializing_if).
        assert!(
            v["params"].get("input_tokens").is_none(),
            "absent input_tokens should not be synthesized from output_tokens"
        );
    }

    #[test]
    fn usage_event_does_not_double_count_cached_subset() {
        // Per TokenUsage contract, cached_input_tokens is a *subset* of
        // input_tokens. The ACP ContextUsage notification must report
        // input_tokens as-is — the cached subset is already included.
        //
        // Realistic OpenAI-shape: prompt_tokens = 25_000 (already total),
        // cached_tokens = 15_000 (subset). Context size = 25_000, NOT 40_000.
        let event = TurnEvent::Usage {
            input_tokens: Some(25_000),
            cached_input_tokens: Some(15_000),
            output_tokens: Some(200),
            cost_usd: None,
        };
        let json = notification_for_turn_event("s1", &event, Some(200_000)).unwrap();
        let v = parse(&json);
        assert_eq!(v["params"]["type"], "context_usage");
        assert_eq!(
            v["params"]["input_tokens"], 25_000,
            "input_tokens is reported as-is — cached subset must not be added"
        );
    }

    #[test]
    fn usage_event_only_cached_tokens_emits_null() {
        // Edge case: provider reports only cached without input total.
        // Without a known total this is ambiguous, so we don't synthesize one.
        let event = TurnEvent::Usage {
            input_tokens: None,
            cached_input_tokens: Some(80_000),
            output_tokens: Some(100),
            cost_usd: None,
        };
        let json = notification_for_turn_event("s1", &event, Some(100_000)).unwrap();
        let v = parse(&json);
        assert!(
            v["params"].get("input_tokens").is_none(),
            "cached-only is ambiguous; do not fabricate a total"
        );
    }

    #[test]
    fn parse_params_valid() {
        let v = json!({"session_id": "s1"});
        let p: SessionIdParams = parse_params(&v).unwrap();
        assert_eq!(p.session_id, "s1");
    }

    #[test]
    fn parse_params_missing_required() {
        let v = json!({});
        let err = parse_params::<SessionIdParams>(&v).unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
    }

    #[test]
    fn to_result_roundtrip() {
        let r = InitializeResult {
            protocol_version: 1,
            server_version: "0.1.0".into(),
            tui_id: None,
            tui_sig: None,
            capabilities: vec![],
        };
        let val = to_result(r).unwrap();
        assert_eq!(val["protocol_version"], 1);
        assert_eq!(val["server_version"], "0.1.0");
    }

    // -----------------------------------------------------------------------
    // ACP session/new — memory-tool exclusion
    // -----------------------------------------------------------------------
    //
    // These tests verify that `session/new` with `exclude_memory: true` strips
    // all five memory tools from the agent, while `exclude_memory: false` leaves
    // at least one memory tool present.
    //
    // They live here (not in `tests/`) because they depend on `#[cfg(test)]`
    // helpers: `RpcContext::minimal`, `RpcDispatcher::handle_session_new_for_test`,
    // and `Agent::tool_names`.

    const MEMORY_TOOLS: &[&str] = &[
        "memory_recall",
        "memory_store",
        "memory_forget",
        "memory_export",
        "memory_purge",
    ];

    fn make_acp_test_config(tmp: &tempfile::TempDir) -> zeroclaw_config::schema::Config {
        use std::collections::HashMap;
        use zeroclaw_config::schema::{AliasedAgentConfig, RiskProfileConfig};

        let workspace_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();

        let mut providers = zeroclaw_config::providers::Providers::default();
        {
            let base = providers
                .models
                .ensure("openai", "test-provider")
                .expect("`openai` slot must exist");
            base.api_key = Some("test-key".into());
            base.model = Some("test-model".into());
            base.uri = Some("http://127.0.0.1:1".into());
        }

        let mut agents = HashMap::new();
        agents.insert(
            "test-agent".to_string(),
            AliasedAgentConfig {
                enabled: true,
                model_provider: "openai.test-provider".into(),
                risk_profile: "test-profile".to_string(),
                ..Default::default()
            },
        );

        let mut risk_profiles = HashMap::new();
        risk_profiles.insert("test-profile".to_string(), RiskProfileConfig::default());

        zeroclaw_config::schema::Config {
            data_dir: workspace_dir,
            config_path: tmp.path().join("config.toml"),
            providers,
            agents,
            risk_profiles,
            ..zeroclaw_config::schema::Config::default()
        }
    }

    fn make_acp_test_dispatcher(
        config: zeroclaw_config::schema::Config,
    ) -> (RpcDispatcher, Arc<crate::rpc::session::SessionStore>) {
        use zeroclaw_infra::session_queue::SessionActorQueue;
        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let ctx = RpcContext::minimal(config, Arc::clone(&sessions));
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let dispatcher = RpcDispatcher::new(ctx, tx, "test-peer".into());
        (dispatcher, sessions)
    }

    #[tokio::test]
    async fn acp_session_new_exposes_no_memory_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let (dispatcher, sessions) = make_acp_test_dispatcher(config);

        let params = json!({
            "agent_alias": "test-agent",
            "exclude_memory": true,
            "session_id": "acp-test-session-001"
        });

        let result = dispatcher.handle_session_new_for_test(&params).await;
        assert!(
            result.is_ok(),
            "session/new should succeed; got: {:?}",
            result.err()
        );

        let agent_arc = sessions
            .get_agent("acp-test-session-001")
            .await
            .expect("session must be registered in the store after session/new");

        let agent = agent_arc.lock().await;
        let tool_names = agent.tool_names();

        for &mem_tool in MEMORY_TOOLS {
            assert!(
                !tool_names.contains(&mem_tool),
                "ACP session must NOT expose `{mem_tool}` — found in tool list: {tool_names:?}"
            );
        }
    }

    #[tokio::test]
    async fn non_acp_session_new_exposes_memory_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let (dispatcher, sessions) = make_acp_test_dispatcher(config);

        let params = json!({
            "agent_alias": "test-agent",
            "exclude_memory": false,
            "session_id": "chat-test-session-001"
        });

        let result = dispatcher.handle_session_new_for_test(&params).await;
        assert!(
            result.is_ok(),
            "session/new should succeed; got: {:?}",
            result.err()
        );

        let agent_arc = sessions
            .get_agent("chat-test-session-001")
            .await
            .expect("session must be registered in the store after session/new");

        let agent = agent_arc.lock().await;
        let tool_names = agent.tool_names();

        let has_any_memory_tool = MEMORY_TOOLS.iter().any(|&t| tool_names.contains(&t));
        assert!(
            has_any_memory_tool,
            "non-ACP session MUST expose at least one memory tool — tool list: {tool_names:?}"
        );
    }

    // -----------------------------------------------------------------------
    // chat_mode persistence routing: ACP vs Chat must not cross stores
    // -----------------------------------------------------------------------

    use zeroclaw_infra::session_backend::SessionBackend;

    fn make_persistence_test_dispatcher(
        config: zeroclaw_config::schema::Config,
        data_dir: &std::path::Path,
    ) -> (
        RpcDispatcher,
        Arc<crate::rpc::session::SessionStore>,
        Arc<zeroclaw_infra::session_sqlite::SqliteSessionBackend>,
        Arc<zeroclaw_infra::acp_session_store::AcpSessionStore>,
    ) {
        use zeroclaw_infra::session_queue::SessionActorQueue;
        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let chat_backend =
            Arc::new(zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(data_dir).unwrap());
        let acp_store =
            Arc::new(zeroclaw_infra::acp_session_store::AcpSessionStore::new(data_dir).unwrap());
        let ctx = RpcContext::for_persistence_tests(
            config,
            Arc::clone(&sessions),
            Some(chat_backend.clone() as Arc<dyn zeroclaw_infra::session_backend::SessionBackend>),
            Some(Arc::clone(&acp_store)),
        );
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let dispatcher = RpcDispatcher::new(ctx, tx, "test-peer".into());
        (dispatcher, sessions, chat_backend, acp_store)
    }

    /// chat_mode=acp creates a row in acp-sessions.db, sessions.db stays empty
    /// for that session_id.
    #[tokio::test]
    async fn acp_session_new_writes_to_acp_store_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let data_dir = config.data_dir.clone();
        let (dispatcher, _sessions, chat_backend, acp_store) =
            make_persistence_test_dispatcher(config, &data_dir);

        let sid = "acp-routing-001";
        let params = json!({
            "agent_alias": "test-agent",
            "exclude_memory": true,
            "chat_mode": "acp",
            "session_id": sid,
        });

        dispatcher
            .handle_session_new_for_test(&params)
            .await
            .expect("session/new should succeed");

        assert!(
            acp_store.load_session(sid).unwrap().is_some(),
            "ACP session must be persisted to acp_session_store"
        );

        assert!(
            chat_backend.load(&format!("rpc_{sid}")).is_empty(),
            "ACP session must NOT touch chat session_backend"
        );
    }

    /// chat_mode omitted (or =chat) creates rows via session_backend,
    /// acp-sessions.db stays empty for that session_id.
    #[tokio::test]
    async fn chat_session_new_writes_to_chat_backend_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let data_dir = config.data_dir.clone();
        let (dispatcher, _sessions, chat_backend, acp_store) =
            make_persistence_test_dispatcher(config, &data_dir);

        let sid = "chat-routing-001";
        let params = json!({
            "agent_alias": "test-agent",
            "session_id": sid,
        });

        dispatcher
            .handle_session_new_for_test(&params)
            .await
            .expect("session/new should succeed");

        assert!(
            acp_store.load_session(sid).unwrap().is_none(),
            "Chat session must NOT touch acp_session_store"
        );

        let key = format!("rpc_{sid}");
        let metadata = chat_backend.list_sessions_with_metadata();
        let entry = metadata
            .iter()
            .find(|m| m.key == key)
            .expect("Chat session must be registered in session_backend metadata");
        assert_eq!(
            entry.agent_alias.as_deref(),
            Some("test-agent"),
            "Chat session must stamp its agent_alias in session_backend (got: {:?})",
            entry.agent_alias
        );
    }

    // ── config/set secret-routing ────────────────────────────────

    fn make_config_set_test_dispatcher(config: zeroclaw_config::schema::Config) -> RpcDispatcher {
        use zeroclaw_infra::session_queue::SessionActorQueue;
        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let ctx = RpcContext::minimal(config, Arc::clone(&sessions));
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let mut dispatcher = RpcDispatcher::new(ctx, tx, "test-peer".into());
        dispatcher.authenticated = true;
        dispatcher
    }

    /// Mint a config with `providers.models.anthropic.default` so we can
    /// poke its `#[secret]` `api-key` field through `config/set`.
    fn make_secret_test_config() -> zeroclaw_config::schema::Config {
        let mut cfg = zeroclaw_config::schema::Config::default();
        cfg.create_map_key("providers.models.anthropic", "default")
            .expect("create anthropic.default");
        cfg
    }

    #[tokio::test]
    async fn config_set_writes_real_secret_through_set_prop() {
        let dispatcher = make_config_set_test_dispatcher(make_secret_test_config());
        let params = json!({
            "prop": "providers.models.anthropic.default.api-key",
            "value": "sk-real-test-key"
        });
        let res = dispatcher.handle_config_set(&params).await;
        assert!(res.is_ok(), "config/set must accept a real secret: {res:?}");
        let cfg = dispatcher.ctx.config.read().clone();
        let stored = cfg
            .providers
            .models
            .anthropic
            .get("default")
            .and_then(|e| e.base.api_key.clone());
        assert_eq!(
            stored.as_deref(),
            Some("sk-real-test-key"),
            "real secret must land in memory as plaintext"
        );
    }

    #[tokio::test]
    async fn config_set_rejects_masked_secret_value() {
        let mut cfg = make_secret_test_config();
        cfg.providers
            .models
            .anthropic
            .get_mut("default")
            .unwrap()
            .base
            .api_key = Some("sk-live-secret".into());
        let dispatcher = make_config_set_test_dispatcher(cfg);

        for masked in [zeroclaw_config::traits::MASKED_SECRET, "****", ""] {
            let params = json!({
                "prop": "providers.models.anthropic.default.api-key",
                "value": masked
            });
            let res = dispatcher.handle_config_set(&params).await;
            assert!(
                res.is_err(),
                "config/set must refuse masked/empty secret (`{masked}`), got: {res:?}"
            );
        }

        let cfg_after = dispatcher.ctx.config.read().clone();
        let stored = cfg_after
            .providers
            .models
            .anthropic
            .get("default")
            .and_then(|e| e.base.api_key.clone());
        assert_eq!(
            stored.as_deref(),
            Some("sk-live-secret"),
            "live secret must NOT be clobbered by a masked write"
        );
    }

    #[tokio::test]
    async fn config_set_non_secret_field_still_uses_set_prop() {
        let dispatcher = make_config_set_test_dispatcher(make_secret_test_config());
        let params = json!({
            "prop": "providers.models.anthropic.default.model",
            "value": "claude-sonnet-4-5"
        });
        let res = dispatcher.handle_config_set(&params).await;
        assert!(res.is_ok(), "non-secret set must succeed: {res:?}");
        let cfg = dispatcher.ctx.config.read().clone();
        let stored = cfg
            .providers
            .models
            .anthropic
            .get("default")
            .and_then(|e| e.base.model.clone());
        assert_eq!(stored.as_deref(), Some("claude-sonnet-4-5"));
    }
}
