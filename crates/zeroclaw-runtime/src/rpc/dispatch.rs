//! JSON-RPC 2.0 method dispatch. Transport-agnostic.
//!
//! **No string-literal matching.** Every wire method name is registered
//! exactly once in [`Method::ALL`]. The compiler enforces that every
//! variant has a handler via exhaustive `match`.

use super::context::RpcContext;
use super::transport::RpcTransport;
use super::turn::{TurnOutcome, execute_turn};
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
    SessionMessages,
    SessionState,
    SessionDelete,
    SessionRename,

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
        (Method::SessionMessages, "session/messages"),
        (Method::SessionState, "session/state"),
        (Method::SessionDelete, "session/delete"),
        (Method::SessionRename, "session/rename"),
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
}

impl RpcDispatcher {
    pub fn new(ctx: Arc<RpcContext>, writer_tx: mpsc::Sender<String>) -> Self {
        Self {
            ctx,
            rpc: Arc::new(RpcOutbound::new(writer_tx)),
            authenticated: false,
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
            Method::SessionPrompt => self.handle_session_prompt(&req.params).await,
            Method::SessionConfigure => self.handle_session_configure(&req.params).await,
            Method::SessionCancel => self.handle_session_cancel(&req.params),
            Method::SessionList => self.handle_session_list().await,
            Method::SessionMessages => self.handle_session_messages(&req.params).await,
            Method::SessionState => self.handle_session_state(&req.params).await,
            Method::SessionDelete => self.handle_session_delete(&req.params).await,
            Method::SessionRename => self.handle_session_rename(&req.params).await,

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

        self.authenticated = true;

        to_result(InitializeResult {
            protocol_version: RPC_PROTOCOL_VERSION,
            server_version: env!("CARGO_PKG_VERSION").to_string(),
        })
    }

    async fn handle_status(&self) -> RpcResult {
        let ids = self.ctx.sessions.list_ids().await;
        to_result(StatusResult {
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: RPC_PROTOCOL_VERSION,
            active_sessions: ids.len(),
            session_ids: ids,
        })
    }

    fn handle_health(&self) -> RpcResult {
        Ok(crate::health::snapshot_json())
    }

    // ── Session handlers ─────────────────────────────────────────

    async fn handle_session_new(&self, params: &Value) -> RpcResult {
        let req: SessionNewParams = parse_params(params)?;
        let session_id = req
            .session_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let config = self.ctx.config.read().clone();
        let cwd_path = req.cwd.as_deref().map(std::path::Path::new);
        let agent = crate::agent::agent::Agent::from_config_with_session_cwd_and_mcp(
            &config,
            &req.agent_alias,
            cwd_path,
            false,
        )
        .await
        .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Failed to create agent: {e}")))?;

        let cwd = req.cwd.as_deref().unwrap_or(".");
        self.ctx
            .sessions
            .insert(
                session_id.clone(),
                super::session::RpcSession::new(agent, &req.agent_alias, cwd),
            )
            .await
            .map_err(|_| rpc_err(SESSION_LIMIT_REACHED, "Session limit reached"))?;

        // Restore persisted history if available.
        let mut message_count = 0;
        if let Some(ref backend) = self.ctx.session_backend {
            let stored = backend.load(&format!("rpc_{session_id}"));
            if !stored.is_empty() {
                self.ctx.sessions.seed_history(&session_id, &stored).await;
                message_count = stored.len();
            }
        }

        to_result(SessionNewResult {
            session_id,
            agent_alias: req.agent_alias,
            message_count,
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

        let agent = self
            .ctx
            .sessions
            .get_agent(sid)
            .await
            .ok_or_else(|| rpc_err(SESSION_NOT_FOUND, "Session not found"))?;

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

        let rpc = self.rpc.clone();
        let sid_owned = sid.to_string();
        let outcome = execute_turn(
            agent,
            req.prompt.clone(),
            cancel,
            Some(format!("rpc_{sid}")),
            move |event| {
                let rpc = rpc.clone();
                let sid = sid_owned.clone();
                async move {
                    if let Some(n) = notification_for_turn_event(&sid, &event) {
                        let _ = rpc.send_raw(n).await;
                    }
                }
            },
        )
        .await;

        self.ctx.sessions.remove_cancel_token(sid);

        // Persist.
        if let Some(ref backend) = self.ctx.session_backend {
            let key = format!("rpc_{sid}");
            let _ = backend.append(&key, &ChatMessage::user(&req.prompt));
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

    async fn handle_session_list(&self) -> RpcResult {
        let backend = self
            .ctx
            .session_backend
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "Session persistence is disabled"))?;
        let config = self.ctx.config.read().clone();
        let all = backend.list_sessions_with_metadata();
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

    async fn handle_session_messages(&self, params: &Value) -> RpcResult {
        let req: SessionIdParams = parse_params(params)?;
        let backend = self
            .ctx
            .session_backend
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "Session persistence is disabled"))?;
        let session_key = format!("rpc_{}", req.session_id);
        let messages: Vec<MessageEntry> = backend
            .load(&session_key)
            .iter()
            .map(|m| MessageEntry {
                role: m.role.clone(),
                content: m.content.clone(),
            })
            .collect();
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
        let session_key = format!("rpc_{}", req.session_id);
        match backend.get_session_state(&session_key) {
            Ok(Some(ss)) => to_result(SessionStateResult {
                session_id: req.session_id,
                state: ss.state,
                turn_id: ss.turn_id,
                turn_started_at: ss.turn_started_at.map(|t| t.to_rfc3339()),
            }),
            Ok(None) => Err(rpc_err(SESSION_NOT_FOUND, "Session not found")),
            Err(e) => Err(rpc_err(
                INTERNAL_ERROR,
                format!("Failed to get session state: {e}"),
            )),
        }
    }

    async fn handle_session_delete(&self, params: &Value) -> RpcResult {
        let req: SessionIdParams = parse_params(params)?;
        // Remove from in-memory store.
        self.ctx.sessions.remove(&req.session_id).await;
        // Remove from persistent backend.
        if let Some(ref backend) = self.ctx.session_backend {
            let session_key = format!("rpc_{}", req.session_id);
            let _ = backend.delete_session(&session_key);
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
        let session_key = format!("rpc_{}", req.session_id);
        backend
            .set_session_name(&session_key, &req.name)
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Rename failed: {e}")))?;
        to_result(SessionRenameResult {
            session_id: req.session_id,
            name: req.name,
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
            // Polymorphic value: strings pass through, everything else coerced.
            let info = config
                .prop_fields()
                .into_iter()
                .find(|f| f.name == req.prop);
            let value_str = match &req.value {
                Value::String(s) => s.clone(),
                other => zeroclaw_config::typed_value::coerce_for_set_prop(
                    other,
                    info.as_ref().map(|i| i.kind),
                )
                .map_err(|e| rpc_err(INVALID_PARAMS, e.message))?,
            };
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
        use crate::onboard::field_visibility;
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
        let _session_ids = self.ctx.sessions.list_ids().await;
        let agents: Vec<AgentStatusEntry> = config
            .agents
            .iter()
            .map(|(alias, agent_cfg)| {
                // TODO: filter session_ids by agent_alias once sessions track it
                AgentStatusEntry {
                    alias: alias.clone(),
                    enabled: agent_cfg.enabled,
                    active_sessions: 0,
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
        use zeroclaw_config::sections::{ONBOARDING_SECTIONS, SectionShape};
        let config = self.ctx.config.read().clone();
        let sections: Vec<ConfigSectionEntry> = ONBOARDING_SECTIONS
            .iter()
            .map(|&section| {
                let completed = crate::onboard::section_has_signal(&config, section);
                let has_picker = matches!(
                    section.shape(),
                    SectionShape::TypedFamilyMap | SectionShape::OneTierAliasMap
                );
                ConfigSectionEntry {
                    key: section.as_str().to_string(),
                    label: section.as_str().replace(['-', '_'], " "),
                    help: section.help().to_string(),
                    has_picker,
                    completed,
                    ready: false,
                    group: String::new(),
                    is_onboarding: true,
                    shape: Some(section.shape()),
                }
            })
            .collect();
        to_result(ConfigSectionsResult { sections })
    }

    fn handle_config_status(&self) -> RpcResult {
        use zeroclaw_config::sections::ONBOARDING_SECTIONS;
        let config = self.ctx.config.read().clone();
        let missing: Vec<String> = ONBOARDING_SECTIONS
            .iter()
            .filter(|&&s| !crate::onboard::section_has_signal(&config, s))
            .map(|s| s.as_str().to_string())
            .collect();
        let needs_onboarding = !missing.is_empty();
        let reason = if needs_onboarding {
            format!("{} section(s) incomplete", missing.len())
        } else {
            "all sections complete".to_string()
        };
        to_result(ConfigStatusResult {
            needs_onboarding,
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
        tokio::spawn(async move {
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
}

// ── Helpers ──────────────────────────────────────────────────────

fn parse_params<T: DeserializeOwned>(params: &Value) -> Result<T, JsonRpcError> {
    serde_json::from_value(params.clone()).map_err(|e| rpc_err(INVALID_PARAMS, e.to_string()))
}

fn to_result<T: Serialize>(val: T) -> RpcResult {
    serde_json::to_value(val).map_err(|e| rpc_err(INTERNAL_ERROR, e.to_string()))
}

fn notification_for_turn_event(session_id: &str, event: &TurnEvent) -> Option<String> {
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
        TurnEvent::Usage { .. } => return None,
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
        let json = notification_for_turn_event("s1", &event).unwrap();
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
        let json = notification_for_turn_event("s1", &event).unwrap();
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
        let json = notification_for_turn_event("s1", &event).unwrap();
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
        let json = notification_for_turn_event("s1", &event).unwrap();
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
        let json = notification_for_turn_event("s1", &event).unwrap();
        let v = parse(&json);
        assert_eq!(v["params"]["type"], "approval_request");
        assert_eq!(v["params"]["request_id"], "ar_1");
        assert_eq!(v["params"]["tool_name"], "bash");
        assert_eq!(v["params"]["timeout_secs"], 30);
    }

    #[test]
    fn usage_event_returns_none() {
        let event = TurnEvent::Usage {
            input_tokens: Some(100),
            output_tokens: Some(50),
            cost_usd: Some(0.01),
        };
        assert!(notification_for_turn_event("s1", &event).is_none());
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
        };
        let val = to_result(r).unwrap();
        assert_eq!(val["protocol_version"], 1);
        assert_eq!(val["server_version"], "0.1.0");
    }
}
