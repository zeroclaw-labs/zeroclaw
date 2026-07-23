//! JSON-RPC 2.0 method dispatch. Transport-agnostic.

use super::context::RpcContext;
use super::transport::RpcTransport;
use super::turn::{TurnAttribution, TurnOutcome, execute_turn};
use super::types::*;

const RPC_RELOAD_REPLY_FLUSH_DELAY: std::time::Duration = std::time::Duration::from_millis(200);
const RPC_RELOAD_GATEWAY_SHUTDOWN_DELAY: std::time::Duration =
    std::time::Duration::from_millis(200);
use crate::agent::agent::TurnEvent;
use crate::sop::SopGraphExt;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;
use zeroclaw_config::schema::Config;

use zeroclaw_api::jsonrpc::error_codes::*;
use zeroclaw_api::jsonrpc::{
    JSONRPC_VERSION, JsonRpcError, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    RpcOutbound, SopDecideRequest, SopRunOverlayRequest, SopRunRequest, SopRunResponse,
    SopRunsRequest, SopSaveRequest, SopSelectRequest,
};
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_api::runtime_status::RuntimeConfigKind;

/// Wire protocol version. Bump on breaking changes.
pub const RPC_PROTOCOL_VERSION: u64 = 1;

mod notification {
    pub const SESSION_UPDATE: &str = "session/update";
    pub const LOGS_EVENT: &str = "logs/event";
}

struct StatusRuntimeContext {
    config_dir: String,
    config_file: String,
    config_kind: RuntimeConfigKind,
    local_ipc_endpoint: String,
}

fn status_runtime_context(config: &Config, config_kind: RuntimeConfigKind) -> StatusRuntimeContext {
    let config_file = config.config_path.display().to_string();
    let config_dir = config
        .config_path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let local_ipc_endpoint = super::local::socket_path(config).display().to_string();

    StatusRuntimeContext {
        config_dir,
        config_file,
        config_kind,
        local_ipc_endpoint,
    }
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
    DoctorRun,

    // Sessions (agent chat lives here — session/prompt + session/update
    // notifications is the RPC equivalent of the gateway's ws/chat)
    SessionNew,
    SessionClose,
    SessionPrompt,
    SessionConfigure,
    SessionCancel,
    SessionGitBranch,
    SessionList,
    SessionListAcp,
    SessionMessages,
    SessionState,
    SessionDelete,
    SessionApprove,
    SessionKill,

    // Memory
    MemoryList,
    MemorySearch,
    MemoryGet,
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
    ConfigResolveAliasSource,
    ConfigMapKeyCreate,
    ConfigMapKeyDelete,
    ConfigMapKeyRename,
    ConfigTemplates,

    // Agents
    AgentsList,
    AgentsStatus,

    // Cost
    CostQuery,
    CostOrg,

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
    LogsGet,

    // TUI
    TuiList,

    // Files
    FileAttach,
    FsListDir,

    // Locales
    LocalesList,
    LocalesFetch,

    // Quickstart (TUI mirror of `/api/quickstart/*` HTTP routes)
    QuickstartState,
    QuickstartFields,
    QuickstartValidate,
    QuickstartApply,
    QuickstartDismiss,

    SopsList,
    SopsGet,
    SopsGraph,
    SopsRun,
    SopsRuns,
    SopsRunOverlay,
    SopsValidate,
    SopsSave,
    SopsCreate,
    SopsDelete,
    SopsDecide,
    SopsWireDraft,
    SopsGraphDraft,
    SopsTriggerSources,
    ToolsParamOptions,
}

impl Method {
    /// The single table. Wire name ↔ variant, defined once.
    pub const ALL: &[(Method, &str)] = &[
        (Method::Initialize, "initialize"),
        (Method::Status, "status"),
        (Method::Health, "health"),
        (Method::DoctorRun, "doctor/run"),
        // Sessions
        (Method::SessionNew, "session/new"),
        (Method::SessionClose, "session/close"),
        (Method::SessionPrompt, "session/prompt"),
        (Method::SessionConfigure, "session/configure"),
        (Method::SessionCancel, "session/cancel"),
        (Method::SessionGitBranch, "session/git_branch"),
        (Method::SessionList, "session/list"),
        (Method::SessionListAcp, "session/list-acp"),
        (Method::SessionMessages, "session/messages"),
        (Method::SessionState, "session/state"),
        (Method::SessionDelete, "session/delete"),
        (Method::SessionApprove, "session/approve"),
        (Method::SessionKill, "session/kill"),
        // Memory
        (Method::MemoryList, "memory/list"),
        (Method::MemorySearch, "memory/search"),
        (Method::MemoryGet, "memory/get"),
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
        (
            Method::ConfigResolveAliasSource,
            "config/resolve-alias-source",
        ),
        (Method::ConfigMapKeyCreate, "config/map-key-create"),
        (Method::ConfigMapKeyDelete, "config/map-key-delete"),
        (Method::ConfigMapKeyRename, "config/map-key-rename"),
        (Method::ConfigTemplates, "config/templates"),
        // Agents
        (Method::AgentsList, "agents/list"),
        (Method::AgentsStatus, "agents/status"),
        // Cost
        (Method::CostQuery, "cost/query"),
        (Method::CostOrg, "cost/org"),
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
        (Method::LogsGet, "logs/get"),
        // TUI
        (Method::TuiList, "tui/list"),
        // Files
        (Method::FileAttach, "file/attach"),
        (Method::FsListDir, "fs/list_dir"),
        // Locales
        (Method::LocalesList, "locales/list"),
        (Method::LocalesFetch, "locales/fetch"),
        // Quickstart
        (Method::QuickstartState, "quickstart/state"),
        (Method::QuickstartFields, "quickstart/fields"),
        (Method::QuickstartValidate, "quickstart/validate"),
        (Method::QuickstartApply, "quickstart/apply"),
        (Method::QuickstartDismiss, "quickstart/dismiss"),
        (Method::SopsList, "sops/list"),
        (Method::SopsGet, "sops/get"),
        (Method::SopsGraph, "sops/graph"),
        (Method::SopsRun, "sops/run"),
        (Method::SopsRuns, "sops/runs"),
        (Method::SopsRunOverlay, "sops/run-overlay"),
        (Method::SopsValidate, "sops/validate"),
        (Method::SopsSave, "sops/save"),
        (Method::SopsCreate, "sops/create"),
        (Method::SopsDelete, "sops/delete"),
        (Method::SopsDecide, "sops/decide"),
        (Method::SopsWireDraft, "sops/wire-draft"),
        (Method::SopsGraphDraft, "sops/graph-draft"),
        (Method::SopsTriggerSources, "sops/trigger-sources"),
        (Method::ToolsParamOptions, "tools/param-options"),
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
type BoxRpcFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = RpcResult> + Send + 'a>>;

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

fn doctor_summary(results: &[DiagResult]) -> DoctorSummary {
    DoctorSummary {
        ok: results
            .iter()
            .filter(|r| r.severity == crate::doctor::Severity::Ok)
            .count(),
        warnings: results
            .iter()
            .filter(|r| r.severity == crate::doctor::Severity::Warn)
            .count(),
        errors: results
            .iter()
            .filter(|r| r.severity == crate::doctor::Severity::Error)
            .count(),
    }
}

fn personality_template_context(
    config: &zeroclaw_config::schema::Config,
    req: &PersonalityTemplatesParams,
) -> crate::agent::personality_templates::TemplateContext {
    let agent_requested = req.agent.is_some();
    let requested_agent = req
        .agent
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let agent_alias = requested_agent.unwrap_or("default");
    let configured_agent_exists = config.agent(agent_alias).is_some();

    crate::agent::personality_templates::TemplateContext {
        agent: requested_agent
            .map(str::to_string)
            .or_else(|| configured_agent_exists.then(|| agent_alias.to_string()))
            .unwrap_or_else(|| "ZeroClaw".to_string()),
        include_memory: configured_agent_exists || agent_requested,
        ..Default::default()
    }
}

fn model_provider_ref_from_provider_profile_prop(prop: &str) -> Option<String> {
    let rest = prop.strip_prefix("providers.models.")?;
    let (provider_type, rest) = rest.split_once('.')?;
    let (provider_alias, field) = rest.split_once('.')?;
    if provider_type.is_empty() || provider_alias.is_empty() || field.is_empty() {
        None
    } else {
        Some(format!("{provider_type}.{provider_alias}"))
    }
}

/// Extract the agent alias from an `agents.<alias>.model_provider` prop path.
/// A live change to an agent's bound provider must rebuild that agent's live
/// session boxes the same way a `providers.models.*` edit does, so any
/// `config/set agents.<alias>.model_provider` caller (the config pane and other
/// RPC/config-set clients) gets a live refresh.
fn agent_alias_from_model_provider_prop(prop: &str) -> Option<String> {
    let rest = prop.strip_prefix("agents.")?;
    let (alias, field) = rest.split_once('.')?;
    if alias.is_empty() || field != "model_provider" {
        None
    } else {
        Some(alias.to_string())
    }
}

/// Session-selection predicate for an agent-scoped `model_provider` refresh
/// (`config/set agents.<alias>.model_provider`). Only sessions bound to the
/// edited agent are eligible, and a session that carries its own
/// `model_provider` override is excluded so unrelated agents and overridden
/// sessions are never rebuilt.
fn agent_scoped_refresh_selects(
    edited_agent: &str,
    session_agent: &str,
    overrides: &SessionOverrides,
) -> bool {
    session_agent == edited_agent && overrides.model_provider.is_none()
}

/// Session-selection predicate for a provider-scoped refresh
/// (`providers.models.*` edit). A session is eligible when its own
/// `model_provider` override matches the edited provider, or when it has no
/// override and thus inherits the agent's provider (final provider match is
/// resolved separately against config).
fn provider_scoped_refresh_selects(target_ref: &str, overrides: &SessionOverrides) -> bool {
    overrides
        .model_provider
        .as_deref()
        .map(|r| r == target_ref)
        .unwrap_or(true)
}

/// Whether memory embeddings resolve from the given `<type>.<alias>` provider
/// profile — either the base `[memory].embedding_provider` reference or any
/// `[[embedding_routes]]` entry. Gates the memory-embedder refresh on a
/// `config/set` provider-profile change
fn memory_embeddings_use_provider(
    config: &zeroclaw_config::schema::Config,
    model_provider_ref: &str,
) -> bool {
    config.memory.embedding_provider.trim() == model_provider_ref
        || config
            .embedding_routes
            .iter()
            .any(|route| route.model_provider.trim() == model_provider_ref)
}

fn rename_error_to_rpc(
    path: &str,
    from: &str,
    err: zeroclaw_config::alias_refs::RenameError,
) -> JsonRpcError {
    use zeroclaw_config::alias_refs::RenameError;
    let code = match err {
        RenameError::PostCondition(_) => INTERNAL_ERROR,
        _ => INVALID_PARAMS,
    };
    rpc_err(code, format!("{path}.{from}: {err}"))
}

async fn move_renamed_agent_workspace(
    old_workspace: &std::path::Path,
    new_workspace: &std::path::Path,
) -> Option<String> {
    if old_workspace == new_workspace || !old_workspace.exists() {
        return None;
    }
    if let Some(parent) = new_workspace.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    match tokio::fs::rename(old_workspace, new_workspace).await {
        Ok(()) => None,
        Err(err) => Some(format!(
            "workspace move {} -> {} failed: {err}",
            old_workspace.display(),
            new_workspace.display()
        )),
    }
}

fn session_should_initialize_mcp(chat_mode: &crate::rpc::types::ChatMode) -> bool {
    !matches!(chat_mode, crate::rpc::types::ChatMode::Acp)
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
    client_elicitation_caps: zeroclaw_api::elicitation::ElicitationCapabilities,
}

impl RpcDispatcher {
    pub fn new(ctx: Arc<RpcContext>, writer_tx: mpsc::Sender<String>, peer_label: String) -> Self {
        Self {
            ctx,
            rpc: Arc::new(RpcOutbound::new(writer_tx)),
            authenticated: false,
            tui_id: None,
            peer_label,
            client_elicitation_caps: zeroclaw_api::elicitation::ElicitationCapabilities::default(),
        }
    }

    /// TUI ID assigned during initialize, if any.
    pub fn tui_id(&self) -> Option<&str> {
        self.tui_id.as_deref()
    }

    #[cfg(test)]
    pub fn set_tui_id_for_test(&mut self, tui_id: Option<String>) {
        self.tui_id = tui_id;
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
            client_elicitation_caps: self.client_elicitation_caps,
        }
    }

    async fn forward_seed_event(&self, session_id: &str, event: Option<TurnEvent>) {
        if let Some(event) = event
            && let Some(notification) = notification_for_turn_event(session_id, &event, None)
        {
            let _ = self.rpc.send_raw(notification).await;
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

    async fn save_and_swap_config(
        &self,
        mut snapshot: zeroclaw_config::schema::Config,
    ) -> Result<(), JsonRpcError> {
        snapshot
            .save_dirty()
            .await
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Config save failed: {e}")))?;
        *self.ctx.config.write() = snapshot;
        Ok(())
    }

    async fn agent_rename_residue_exists(
        &self,
        config: &zeroclaw_config::schema::Config,
        from: &str,
    ) -> bool {
        if config.agent_workspace_dir(from).exists() {
            return true;
        }
        if crate::cron::list_jobs_by_agent(config, from)
            .map(|jobs| !jobs.is_empty())
            .unwrap_or(false)
        {
            return true;
        }
        if let Some(store) = self.ctx.acp_session_store.as_ref()
            && store
                .list_sessions_by_agent(from)
                .map(|sessions| !sessions.is_empty())
                .unwrap_or(false)
        {
            return true;
        }
        if let Some(mem) = self.ctx.memory.as_ref()
            && mem.count_agent(from).await.unwrap_or(0) > 0
        {
            return true;
        }
        if let Some(backend) = self.ctx.session_backend.as_ref()
            && backend.count_agent_attribution(from).unwrap_or(0) > 0
        {
            return true;
        }
        false
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
        if self.dispatch_inbound_response(line) {
            return;
        }

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
            Method::DoctorRun => self.handle_doctor_run().await,

            // Sessions
            Method::SessionNew => self.handle_session_new(&req.params).await,
            Method::SessionClose => self.handle_session_close(&req.params).await,
            Method::SessionPrompt => {
                // Always spawn — turn completion is signaled by a
                // TurnComplete notification, not by this method's response.
                // The response (empty {} or error) is kept only so legacy
                // request-form callers don't park forever.
                let handle = self.spawn_handle();
                let id_clone = id;
                let params_clone = req.params.clone();
                let is_notif = is_notification;
                zeroclaw_spawn::spawn!(async move {
                    let result = handle.handle_session_prompt(&params_clone).await;
                    if !is_notif {
                        match result {
                            Ok(_) => handle.send_result(id_clone, serde_json::json!({})).await,
                            Err(e) => handle.send_error(id_clone, e.code, &e.message).await,
                        }
                    }
                });
                return;
            }
            Method::SessionConfigure => self.handle_session_configure(&req.params).await,
            Method::SessionCancel => self.handle_session_cancel(&req.params).await,
            Method::SessionGitBranch => self.handle_session_git_branch(&req.params).await,
            Method::SessionList => self.handle_session_list(&req.params).await,
            Method::SessionListAcp => self.handle_session_list_acp(&req.params).await,
            Method::SessionMessages => self.handle_session_messages(&req.params).await,
            Method::SessionState => self.handle_session_state(&req.params).await,
            Method::SessionDelete => self.handle_session_delete(&req.params).await,
            Method::SessionApprove => self.handle_session_approve(&req.params),
            Method::SessionKill => self.handle_session_kill(&req.params).await,

            // Memory
            Method::MemoryList => self.handle_memory_list(&req.params).await,
            Method::MemorySearch => self.handle_memory_search(&req.params).await,
            Method::MemoryGet => self.handle_memory_get(&req.params).await,
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
            Method::ConfigResolveAliasSource => {
                self.handle_config_resolve_alias_source(&req.params)
            }
            Method::ConfigMapKeyCreate => self.handle_config_map_key_create(&req.params).await,
            Method::ConfigMapKeyDelete => self.handle_config_map_key_delete(&req.params).await,
            Method::ConfigMapKeyRename => self.handle_config_map_key_rename(&req.params).await,
            Method::ConfigTemplates => self.handle_config_templates(),

            // Agents
            Method::AgentsList => self.handle_agents_list(),
            Method::AgentsStatus => self.handle_agents_status().await,

            // Cost
            Method::CostQuery => self.handle_cost_query(&req.params),
            Method::CostOrg => self.handle_cost_org(),

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
            Method::LogsGet => self.handle_logs_get(&req.params).await,

            // TUI
            Method::TuiList => self.handle_tui_list(),

            // Files
            Method::FileAttach => self.handle_file_attach(&req.params).await,
            Method::FsListDir => super::fs::handle_fs_list_dir(&req.params).await,

            // Locales
            Method::LocalesList => super::locales::handle_locales_list(self.tui_id()),
            Method::LocalesFetch => {
                super::locales::handle_locales_fetch(&req.params, self.tui_id()).await
            }

            // Quickstart
            Method::QuickstartState => self.handle_quickstart_state(),
            Method::QuickstartFields => self.handle_quickstart_fields(&req.params),
            Method::QuickstartValidate => self.handle_quickstart_validate(&req.params),
            Method::QuickstartApply => self.handle_quickstart_apply(&req.params).await,
            Method::QuickstartDismiss => self.handle_quickstart_dismiss(&req.params),

            Method::SopsList => self.handle_sops_list(),
            Method::SopsGet => self.handle_sops_get(&req.params),
            Method::SopsGraph => self.handle_sops_graph(&req.params),
            Method::SopsRun => self.handle_sops_run(&req.params).await,
            Method::SopsRuns => self.handle_sops_runs(&req.params),
            Method::SopsRunOverlay => self.handle_sops_run_overlay(&req.params),
            Method::SopsValidate => self.handle_sops_validate(&req.params),
            Method::SopsSave => self.handle_sops_save(&req.params),
            Method::SopsCreate => self.handle_sops_create(&req.params),
            Method::SopsDelete => self.handle_sops_delete(&req.params),
            Method::SopsDecide => self.handle_sops_decide(&req.params).await,
            Method::SopsWireDraft => self.handle_sops_wire_draft(&req.params),
            Method::SopsGraphDraft => self.handle_sops_graph_draft(&req.params),
            Method::SopsTriggerSources => self.handle_sops_trigger_sources(),
            Method::ToolsParamOptions => self.handle_tools_param_options(&req.params),
        };

        if is_notification {
            return;
        }

        match result {
            Ok(v) => self.send_result(id, v).await,
            Err(e) => self.send_error(id, e.code, &e.message).await,
        }
    }

    fn dispatch_inbound_response(&self, line: &str) -> bool {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        if !value.is_object()
            || value.get("method").is_some()
            || (value.get("result").is_none() && value.get("error").is_none())
        {
            return false;
        }
        let Some(id) = value.get("id") else {
            return false;
        };
        let id_str = id
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| id.to_string());
        let result = value.get("result").cloned();
        let error: Option<JsonRpcError> = value
            .get("error")
            .and_then(|e| serde_json::from_value(e.clone()).ok());
        self.rpc.dispatch_response(&id_str, result, error);
        true
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

        let elicitation = req
            .client_capabilities
            .as_ref()
            .and_then(|c| c.get("elicitation"));
        self.client_elicitation_caps =
            zeroclaw_api::elicitation::ElicitationCapabilities::from_value(elicitation);

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
        let config_path = self.ctx.config.read().config_path.clone();
        let config_kind = zeroclaw_config::schema::classify_runtime_config_kind(&config_path).await;
        let runtime_context = {
            let config = self.ctx.config.read();
            status_runtime_context(&config, config_kind)
        };
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
            config_dir: Some(runtime_context.config_dir),
            config_file: Some(runtime_context.config_file),
            config_kind: Some(runtime_context.config_kind),
            local_ipc_endpoint: Some(runtime_context.local_ipc_endpoint),
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

    async fn handle_doctor_run(&self) -> RpcResult {
        let config = self.ctx.config.read().clone();
        let results = crate::doctor::run_structured(&config).await;
        let summary = doctor_summary(&results);
        to_result(DoctorRunResult { results, summary })
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

    #[cfg(test)]
    pub async fn handle_session_new_for_test(&self, params: &Value) -> RpcResult {
        self.handle_session_new(params).await
    }

    #[cfg(test)]
    pub async fn handle_session_messages_for_test(&self, params: &Value) -> RpcResult {
        self.handle_session_messages(params).await
    }

    /// Drive a full JSON-RPC request line through the dispatcher from an
    /// external integration test, including notification emission on the
    /// outbound channel. Mirrors the transport `process_line` path.
    pub async fn process_line_for_test(&mut self, line: &str) {
        self.process_line(line).await;
    }

    async fn handle_session_new(&self, params: &Value) -> RpcResult {
        let req: SessionNewParams = parse_params(params)?;
        let resuming = req.session_id.is_some();
        let session_id = req
            .session_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let config = self.ctx.config.read().clone();
        let chat_mode = req
            .chat_mode
            .clone()
            .unwrap_or(crate::rpc::types::ChatMode::Chat);

        // Resuming an ACP session with no caller cwd: recover the original
        // working directory from the persisted store so the rehydrated session
        // keeps its own cwd instead of falling back to the agent workspace dir.
        // The loaded data is reused below so history is not fetched twice.
        let mut preloaded_acp: Option<zeroclaw_infra::acp_session_store::AcpSessionData> = None;
        if resuming
            && req.cwd.is_none()
            && matches!(chat_mode, crate::rpc::types::ChatMode::Acp)
            && let Some(ref store) = self.ctx.acp_session_store
        {
            let store_cloned = store.clone();
            let sid = session_id.clone();
            match tokio::task::spawn_blocking(move || store_cloned.load_session_for_restore(&sid))
                .await
            {
                Ok(Ok(zeroclaw_infra::acp_session_store::AcpSessionRestore::Restorable(data))) => {
                    if data.agent_alias != req.agent_alias {
                        return Err(rpc_err(
                            INVALID_PARAMS,
                            "ACP session belongs to a different agent",
                        ));
                    }
                    preloaded_acp = Some(data);
                }
                Ok(Ok(zeroclaw_infra::acp_session_store::AcpSessionRestore::Missing)) => {}
                Ok(Ok(zeroclaw_infra::acp_session_store::AcpSessionRestore::Killed)) => {
                    return Err(rpc_err(SESSION_NOT_FOUND, "Session not found"));
                }
                Ok(Err(e)) => {
                    return Err(rpc_err(
                        INTERNAL_ERROR,
                        format!("Failed to load ACP session: {e}"),
                    ));
                }
                Err(join) => {
                    return Err(rpc_err(
                        INTERNAL_ERROR,
                        format!("Failed to load ACP session: {join}"),
                    ));
                }
            }
        }

        // The session cwd: caller-supplied wins, then a resumed ACP session's
        // persisted cwd, then the agent's workspace dir.
        let cwd = req
            .cwd
            .clone()
            .or_else(|| preloaded_acp.as_ref().map(|d| d.workspace_dir.clone()))
            .unwrap_or_else(|| {
                config
                    .agent_workspace_dir(&req.agent_alias)
                    .to_string_lossy()
                    .to_string()
            });

        let cwd_path = Some(std::path::Path::new(&cwd));
        let tui_env = req
            .tui_id
            .as_deref()
            .and_then(|id| self.ctx.tui_registry.get_env(id));
        let chat_mode = req
            .chat_mode
            .clone()
            .unwrap_or(crate::rpc::types::ChatMode::Chat);
        let exclude_memory = matches!(chat_mode, crate::rpc::types::ChatMode::Acp)
            || req.exclude_memory == Some(true);
        // Chat sessions initialize MCP so the TUI sees the same MCP tools the
        // gateway exposes for this agent; ACP (Code) sessions skip it to keep
        // `session/new` prompt
        let initialize_mcp = session_should_initialize_mcp(&chat_mode);
        let agent = crate::agent::agent::Agent::from_live_config_with_tui_env(
            Arc::clone(&self.ctx.config),
            &req.agent_alias,
            cwd_path,
            initialize_mcp,
            exclude_memory,
            tui_env,
            self.ctx.sop_engine.clone(),
            self.ctx.sop_audit.clone(),
        )
        .await
        .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Failed to create agent: {e}")))?;

        let approval_ch = Arc::new(crate::rpc::approval_channel::RpcApprovalChannel::new(
            "rpc",
            session_id.clone(),
            Arc::clone(&self.rpc),
            Arc::clone(&self.ctx.approval_pending),
            self.client_elicitation_caps,
        ));
        agent.channel_handles().register_channel("rpc", approval_ch);

        self.ctx
            .sessions
            .insert(
                session_id.clone(),
                super::session::RpcSession::new(agent, &req.agent_alias, &cwd, chat_mode.clone())
                    .with_owner(self.tui_id.clone()),
            )
            .await
            .map_err(|_| rpc_err(SESSION_LIMIT_REACHED, "Session limit reached"))?;

        if let Some(ref tui_id) = self.tui_id {
            let evicted = self
                .ctx
                .sessions
                .evict_same_mode_sibling(tui_id, &chat_mode, &session_id)
                .await;
            if !evicted.is_empty() {
                if let Some(ref hooks) = self.ctx.hooks {
                    for (sid, _) in &evicted {
                        hooks.fire_session_end(sid, "rpc").await;
                    }
                }
                let span = ::zeroclaw_log::info_span!(
                    target: "zeroclaw_log_internal_scope",
                    "zeroclaw_scope",
                    session_key = %session_id,
                    agent_alias = %req.agent_alias,
                    channel = "rpc",
                );
                let _guard = span.enter();
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Agent)
                        .with_outcome(::zeroclaw_log::EventOutcome::Success)
                        .with_attrs(::serde_json::json!({
                            "tui_id": tui_id,
                            "evicted": evicted.iter().map(|(id, _)| id).collect::<Vec<_>>(),
                        })),
                    "Evicted abandoned same-mode session(s) on session/new"
                );
                // Every evicted session was idle (no in-flight turn), so its
                // removal above dropped the last Agent strong ref and freed the
                // history. Trimming now actually returns those pages.
                crate::util::release_freed_heap();
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Agent)
                        .with_outcome(::zeroclaw_log::EventOutcome::Success)
                        .with_attrs(::serde_json::json!({
                            "evicted_count": evicted.len(),
                        })),
                    "Trimmed glibc arenas after same-mode session eviction"
                );
            }
        }

        enum AcpSessionNewLoad {
            Restored(zeroclaw_infra::acp_session_store::AcpSessionData),
            Created,
            Killed,
        }

        let mut message_count = 0;
        match chat_mode {
            crate::rpc::types::ChatMode::Acp => {
                // Reuse the data already loaded for cwd recovery on resume so the
                // store isn't hit twice; otherwise fall through to the restore-
                // aware load-or-create path below.
                let loaded = if let Some(data) = preloaded_acp.take() {
                    Ok(Ok(AcpSessionNewLoad::Restored(data)))
                } else {
                    let Some(ref store) = self.ctx.acp_session_store else {
                        if let Some(ref hooks) = self.ctx.hooks {
                            hooks.fire_session_end(&session_id, "rpc").await;
                        }
                        self.ctx.sessions.remove(&session_id).await;
                        return Err(rpc_err(
                            INTERNAL_ERROR,
                            "ACP session store is not available",
                        ));
                    };

                    let store_cloned = store.clone();
                    let sid = session_id.clone();
                    let alias = req.agent_alias.clone();
                    let cwd_owned = cwd.clone();
                    tokio::task::spawn_blocking(move || -> anyhow::Result<AcpSessionNewLoad> {
                        match store_cloned.load_session_for_restore(&sid)? {
                            zeroclaw_infra::acp_session_store::AcpSessionRestore::Restorable(
                                data,
                            ) => Ok(AcpSessionNewLoad::Restored(data)),
                            zeroclaw_infra::acp_session_store::AcpSessionRestore::Missing => {
                                store_cloned.create_session(&sid, &alias, &cwd_owned)?;
                                Ok(AcpSessionNewLoad::Created)
                            }
                            zeroclaw_infra::acp_session_store::AcpSessionRestore::Killed => {
                                Ok(AcpSessionNewLoad::Killed)
                            }
                        }
                    })
                    .await
                };
                match loaded {
                    Ok(Ok(AcpSessionNewLoad::Restored(data))) => {
                        if data.agent_alias != req.agent_alias {
                            if let Some(ref hooks) = self.ctx.hooks {
                                hooks.fire_session_end(&session_id, "rpc").await;
                            }
                            self.ctx.sessions.remove(&session_id).await;
                            return Err(rpc_err(
                                INVALID_PARAMS,
                                "ACP session belongs to a different agent",
                            ));
                        }
                        message_count = data.messages.len();
                        let seed_event = self
                            .ctx
                            .sessions
                            .seed_conversation_history_with_event(&session_id, data.messages)
                            .await;
                        self.forward_seed_event(&session_id, seed_event).await;
                        // Restore the durable TodoWrite plan into the fresh
                        // in-memory session and re-emit it so the resuming /
                        // reconnecting client's tracker repopulates without a
                        // model round-trip. Robust against tmux detach, socket
                        // drop, suspend/resume, and daemon restart.
                        if let Some(ref store) = self.ctx.acp_session_store {
                            let store = store.clone();
                            let sid = session_id.clone();
                            let plan = tokio::task::spawn_blocking(move || {
                                store.get_plan(&sid).unwrap_or_default()
                            })
                            .await
                            .unwrap_or_default();
                            if !plan.is_empty() {
                                self.ctx.sessions.set_plan(&session_id, plan.clone()).await;
                                if let Some(n) = plan_replay_notification(&session_id, &plan) {
                                    let _ = self.rpc.send_raw(n).await;
                                }
                            }
                        }
                    }
                    Ok(Ok(AcpSessionNewLoad::Created)) => {}
                    Ok(Ok(AcpSessionNewLoad::Killed)) => {
                        if let Some(ref hooks) = self.ctx.hooks {
                            hooks.fire_session_end(&session_id, "rpc").await;
                        }
                        self.ctx.sessions.remove(&session_id).await;
                        return Err(rpc_err(SESSION_NOT_FOUND, "Session not found"));
                    }
                    Ok(Err(e)) => {
                        if let Some(ref hooks) = self.ctx.hooks {
                            hooks.fire_session_end(&session_id, "rpc").await;
                        }
                        self.ctx.sessions.remove(&session_id).await;
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({"session_id": session_id, "error": e.to_string()})),
                            "Failed to load or create ACP session"
                        );
                        return Err(rpc_err(
                            INTERNAL_ERROR,
                            format!("Failed to load or create ACP session: {e}"),
                        ));
                    }
                    Err(join) => {
                        if let Some(ref hooks) = self.ctx.hooks {
                            hooks.fire_session_end(&session_id, "rpc").await;
                        }
                        self.ctx.sessions.remove(&session_id).await;
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({"session_id": session_id, "error": join.to_string()})),
                            "ACP session load task failed"
                        );
                        return Err(rpc_err(
                            INTERNAL_ERROR,
                            format!("ACP session load task failed: {join}"),
                        ));
                    }
                }
            }
            crate::rpc::types::ChatMode::Chat => {
                if let Some(ref backend) = self.ctx.session_backend {
                    let session_key = format!("rpc_{session_id}");
                    let _ = backend.set_session_agent_alias(&session_key, &req.agent_alias);
                    let stored = backend.load(&session_key);
                    if !stored.is_empty() {
                        let seed_event = self
                            .ctx
                            .sessions
                            .seed_history_with_event(&session_id, &stored)
                            .await;
                        self.forward_seed_event(&session_id, seed_event).await;
                        message_count = stored.len();
                    }
                }
            }
        }

        if let Some(ref hooks) = self.ctx.hooks {
            hooks.fire_session_start(&session_id, "rpc").await;
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
        if let Some(agent) = self.ctx.sessions.get_agent(&req.session_id).await {
            agent
                .lock()
                .await
                .channel_handles()
                .unregister_channel("rpc");
            let strong = std::sync::Arc::strong_count(&agent);
            let agent_alias = self
                .ctx
                .sessions
                .get_agent_alias(&req.session_id)
                .await
                .unwrap_or_default();
            let span = ::zeroclaw_log::info_span!(
                target: "zeroclaw_log_internal_scope",
                "zeroclaw_scope",
                session_key = %req.session_id,
                agent_alias = %agent_alias,
                channel = "rpc",
            );
            let _guard = span.enter();
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_attrs(::serde_json::json!({
                        "agent_arc_strong_count_before_remove": strong,
                    })),
                "session close: dropping local Agent handle before remove"
            );
            // Drop our clone explicitly so the session map holds the last
            // strong ref; `remove` then frees the Agent at removal time
            // rather than at end-of-scope, letting the allocator reclaim
            // promptly.
            drop(agent);
        }
        if !self.ctx.sessions.remove(&req.session_id).await {
            return Err(rpc_err(SESSION_NOT_FOUND, "Session not found"));
        }
        if let Some(ref hooks) = self.ctx.hooks {
            hooks.fire_session_end(&req.session_id, "rpc").await;
        }
        crate::util::release_freed_heap();
        {
            let span = ::zeroclaw_log::info_span!(
                target: "zeroclaw_log_internal_scope",
                "zeroclaw_scope",
                session_key = %req.session_id,
                channel = "rpc",
            );
            let _guard = span.enter();
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success),
                "Trimmed glibc arenas after session close"
            );
        }
        to_result(SessionCloseResult {
            session_id: req.session_id,
            closed: true,
        })
    }

    async fn handle_session_kill(&self, params: &Value) -> RpcResult {
        let req: SessionKillParams = parse_params(params)?;
        let sid = &req.session_id;

        let chat_mode = self
            .ctx
            .sessions
            .chat_mode(sid)
            .await
            .ok_or_else(|| rpc_err(SESSION_NOT_FOUND, "Session not found"))?;

        let agent_alias = self
            .ctx
            .sessions
            .get_agent_alias(sid)
            .await
            .unwrap_or_default();
        let span = ::zeroclaw_log::info_span!(
            target: "zeroclaw_log_internal_scope",
            "zeroclaw_scope",
            session_key = %sid,
            agent_alias = %agent_alias,
            channel = "rpc",
        );
        let _guard = span.enter();

        if matches!(chat_mode, ChatMode::Acp) {
            let store = self
                .ctx
                .acp_session_store
                .clone()
                .ok_or_else(|| rpc_err(INTERNAL_ERROR, "ACP session store is not available"))?;
            let sid_owned = sid.to_string();
            let marked =
                tokio::task::spawn_blocking(move || store.mark_session_killed(&sid_owned)).await;
            match marked {
                Ok(Ok(true)) => {}
                Ok(Ok(false)) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_category(::zeroclaw_log::EventCategory::Agent)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        "session/kill: live ACP session had no durable row to tombstone"
                    );
                }
                Ok(Err(e)) => {
                    return Err(rpc_err(
                        INTERNAL_ERROR,
                        format!("Failed to mark ACP session killed: {e}"),
                    ));
                }
                Err(e) => {
                    return Err(rpc_err(
                        INTERNAL_ERROR,
                        format!("Failed to mark ACP session killed: {e}"),
                    ));
                }
            }
        }

        let killed = self.ctx.sessions.kill_session(sid).await;
        if killed {
            if let Some(ref hooks) = self.ctx.hooks {
                hooks.fire_session_end(sid, "rpc").await;
            }
            crate::util::release_freed_heap();
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success),
                "session/kill: session terminated by admin"
            );
        } else {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "session/kill: session vanished between existence check and kill (concurrent close?)"
            );
        }

        to_result(SessionKillResult {
            session_id: req.session_id,
            killed,
        })
    }

    /// Rebuild a reaped ACP session from a restorable durable row so a fresh
    /// prompt recovers to a working session instead of hanging. Returns the
    /// live agent on success; returns `None` for missing, killed, or unreadable
    /// durable state.
    async fn rehydrate_reaped_session(
        &self,
        sid: &str,
    ) -> Option<Arc<tokio::sync::Mutex<crate::agent::agent::Agent>>> {
        let store = self.ctx.acp_session_store.clone()?;
        let sid_owned = sid.to_string();
        let loaded =
            tokio::task::spawn_blocking(move || store.load_session_for_restore(&sid_owned)).await;
        let data = match loaded {
            Ok(Ok(zeroclaw_infra::acp_session_store::AcpSessionRestore::Restorable(data))) => data,
            Ok(Ok(zeroclaw_infra::acp_session_store::AcpSessionRestore::Killed)) => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Agent)
                        .with_outcome(::zeroclaw_log::EventOutcome::Success),
                    "session/prompt: refusing to rehydrate admin-killed ACP session"
                );
                return None;
            }
            Ok(Err(e)) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Agent)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "session_id": sid,
                            "error": e.to_string(),
                        })),
                    "session/prompt: failed to query ACP killed marker before rehydrate"
                );
                return None;
            }
            Ok(Ok(zeroclaw_infra::acp_session_store::AcpSessionRestore::Missing)) => return None,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Agent)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "session_id": sid,
                            "error": e.to_string(),
                        })),
                    "session/prompt: ACP killed-marker query task failed before rehydrate"
                );
                return None;
            }
        };

        let cwd_path = Some(std::path::Path::new(&data.workspace_dir));
        let tui_env = self
            .tui_id
            .as_deref()
            .and_then(|id| self.ctx.tui_registry.get_env(id));
        let exclude_memory = true;
        // Reaped sessions always rehydrate as ACP, which skips eager MCP init to
        // stay prompt — matching `session_should_initialize_mcp(ChatMode::Acp)`.
        let agent = crate::agent::agent::Agent::from_live_config_with_tui_env(
            Arc::clone(&self.ctx.config),
            &data.agent_alias,
            cwd_path,
            false,
            exclude_memory,
            tui_env,
            self.ctx.sop_engine.clone(),
            self.ctx.sop_audit.clone(),
        )
        .await
        .ok()?;

        let approval_ch = Arc::new(crate::rpc::approval_channel::RpcApprovalChannel::new(
            "rpc",
            sid.to_string(),
            Arc::clone(&self.rpc),
            Arc::clone(&self.ctx.approval_pending),
            self.client_elicitation_caps,
        ));
        agent.channel_handles().register_channel("rpc", approval_ch);

        let message_count = data.messages.len();
        self.ctx
            .sessions
            .insert(
                sid.to_string(),
                super::session::RpcSession::new(
                    agent,
                    &data.agent_alias,
                    &data.workspace_dir,
                    crate::rpc::types::ChatMode::Acp,
                )
                .with_owner(self.tui_id.clone()),
            )
            .await
            .ok()?;
        let seed_event = self
            .ctx
            .sessions
            .seed_conversation_history_with_event(sid, data.messages)
            .await;
        self.forward_seed_event(sid, seed_event).await;
        self.ctx.sessions.touch(sid).await;

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_category(::zeroclaw_log::EventCategory::Agent)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(::serde_json::json!({
                    "session_id": sid,
                    "agent_alias": data.agent_alias,
                    "messages_restored": message_count,
                })),
            "rehydrated reaped session from durable store; turn continues on a working session"
        );

        self.ctx.sessions.get_agent(sid).await
    }

    async fn handle_session_prompt(&self, params: &Value) -> RpcResult {
        let req: SessionPromptParams = parse_params(params)?;
        let sid = &req.session_id;

        if req.prompt.trim().is_empty() && req.attachments.is_empty() {
            return Err(rpc_err(
                INVALID_PARAMS,
                "session/prompt requires a non-empty `prompt` or at least one attachment",
            ));
        }

        let agent = match self.ctx.sessions.get_agent(sid).await {
            Some(a) => a,
            None => match self.rehydrate_reaped_session(sid).await {
                Some(a) => a,
                None => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail,)
                            .with_category(::zeroclaw_log::EventCategory::Agent)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({ "session_id": sid })),
                        "session/prompt on a session absent from memory and the durable store; emitting TurnComplete so the client exits the working state"
                    );
                    self.emit_turn_complete(
                        sid,
                        crate::rpc::types::TurnCompletionOutcome::Failed,
                        "turn cancelled by daemon: session_not_found".to_string(),
                    )
                    .await;
                    return Err(rpc_err(SESSION_NOT_FOUND, "Session not found"));
                }
            },
        };

        // Process inline attachments: upload each, append markers to prompt.
        let mut prompt = req.prompt.clone();
        if !req.attachments.is_empty() {
            use super::attachments::process_file_entry;

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
        let cancel_generation = self.ctx.sessions.register_cancel_token(sid, cancel.clone());
        self.ctx.sessions.touch(sid).await;
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Invoke)
                .with_category(::zeroclaw_log::EventCategory::Agent)
                .with_attrs(::serde_json::json!({ "session_id": sid })),
            "turn dispatch: registered cancel token, starting turn"
        );

        let chat_mode = self
            .ctx
            .sessions
            .chat_mode(sid)
            .await
            .unwrap_or(crate::rpc::types::ChatMode::Chat);
        // Capture live attribution fields and max_context_tokens for the turn span.
        // Zerocode's context meter field is named `max_context_tokens` and must
        // reflect the runtime-profile budget (`[runtime_profiles.<name>]
        // max_context_tokens`), not the provider model-window helper (which
        // falls back to 32_000 when `context_window` is unset).
        let (agent_alias, model_provider, model, max_ctx) = {
            let alias = self
                .ctx
                .sessions
                .get_agent_alias(sid)
                .await
                .unwrap_or_default();
            let (mp, m) = if let Some(agent) = self.ctx.sessions.get_agent(sid).await {
                let (_, model_provider, model) = agent.lock().await.attribution_fields();
                (model_provider, model)
            } else {
                (String::new(), String::new())
            };
            let max_ctx = {
                let cfg = self.ctx.config.read();
                Some(context_usage_max_tokens(&cfg, &alias))
            };
            (alias, mp, m, max_ctx)
        };

        let rpc = self.rpc.clone();
        let sid_owned = sid.to_string();
        // Clone of the session store so the turn-event closure can persist
        // the latest TodoWrite plan (store-then-emit) before the plan
        // notification goes out. See `persist_plan_if_any`.
        let sessions_for_plan = self.ctx.sessions.clone();
        let acp_token_store = if matches!(chat_mode, crate::rpc::types::ChatMode::Acp) {
            self.ctx.acp_session_store.clone()
        } else {
            None
        };
        let attribution_agent_alias = agent_alias.clone();
        let attribution_model_provider = model_provider.clone();
        let attribution_model = model.clone();
        // Cost-tracking context for this turn. Built from the daemon-scoped
        // tracker + the live pricing map and stamped with the agent alias so
        // `execute_turn` can persist token usage and attribute spend. `None`
        // when cost tracking is disabled (no tracker wired).
        let cost_context = self.ctx.cost_tracker.as_ref().map(|tracker| {
            let cfg_guard = self.ctx.config.read();
            let pricing = crate::agent::cost::build_model_provider_pricing(&cfg_guard);
            crate::agent::cost::ToolLoopCostTrackingContext::new(
                tracker.clone(),
                std::sync::Arc::new(pricing),
            )
            .with_agent_alias(&attribution_agent_alias)
        });
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
            cost_context,
            move |event| {
                let rpc = rpc.clone();
                let sid = sid_owned.clone();
                let acp_token_store = acp_token_store.clone();
                let sessions_for_plan = sessions_for_plan.clone();
                async move {
                    if let (
                        Some(store),
                        TurnEvent::Usage {
                            input_tokens: Some(it),
                            ..
                        },
                    ) = (acp_token_store.as_ref(), &event)
                    {
                        let store = store.clone();
                        let sid = sid.clone();
                        let it = *it;
                        let _ =
                            tokio::task::spawn_blocking(move || store.set_token_count(&sid, it))
                                .await;
                    }
                    persist_plan_if_any(&sessions_for_plan, acp_token_store.as_ref(), &sid, &event)
                        .await;
                    if let Some(n) = notification_for_turn_event(&sid, &event, max_ctx) {
                        let _ = rpc.send_raw(n).await;
                    }
                }
            },
        )
        .await;

        // Drain the cancel cause BEFORE removing the token (removal clears the
        // cause map). Every cancel firing site records its cause before firing;
        // a cancel with no recorded cause is a bug, not user attribution.
        let cancel_cause = self.ctx.sessions.take_cancel_cause(sid);
        self.ctx
            .sessions
            .remove_cancel_token(sid, cancel_generation);

        // ── Durable turn-verdict audit row ───────────────────────────────
        // Every turn termination writes one attributed row to the ACP session
        // store's event log so a cancel verdict is diagnosable after the trace
        // log rotates. Fire-and-forget on a blocking task.
        if matches!(chat_mode, crate::rpc::types::ChatMode::Acp)
            && let Some(store) = self.ctx.acp_session_store.clone()
        {
            let (action, event_outcome, payload) = match &outcome {
                Ok(crate::rpc::turn::TurnOutcome::Completed { .. }) => (
                    ::zeroclaw_log::Action::Complete,
                    ::zeroclaw_log::EventOutcome::Success,
                    None,
                ),
                Ok(crate::rpc::turn::TurnOutcome::Cancelled { .. }) => (
                    ::zeroclaw_log::Action::Cancel,
                    ::zeroclaw_log::EventOutcome::Unknown,
                    Some(
                        ::serde_json::json!({
                            "cancel_cause": cancel_cause.map(|c| c.as_str()),
                        })
                        .to_string(),
                    ),
                ),
                Err(e) => (
                    ::zeroclaw_log::Action::Fail,
                    ::zeroclaw_log::EventOutcome::Failure,
                    Some(::serde_json::json!({ "error": e.to_string() }).to_string()),
                ),
            };
            let sid_owned = sid.to_string();
            let span_session = sid.to_string();
            let span_alias = attribution_agent_alias.clone();
            let span_provider = attribution_model_provider.clone();
            let span_model = attribution_model.clone();
            zeroclaw_spawn::spawn!(async move {
                use ::zeroclaw_log::Instrument as _;
                let span = ::zeroclaw_log::info_span!(
                    target: "zeroclaw_log_internal_scope",
                    "zeroclaw_scope",
                    session_key = %span_session,
                    agent_alias = %span_alias,
                    model_provider = %span_provider,
                    model = %span_model,
                    channel = "rpc",
                );
                async move {
                    let persisted = tokio::task::spawn_blocking(move || {
                        store.append_event(&sid_owned, action, event_outcome, payload.as_deref())
                    })
                    .await;
                    let error = match persisted {
                        Ok(Ok(())) => return,
                        Ok(Err(e)) => e.to_string(),
                        Err(join) => join.to_string(),
                    };
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
                            .with_category(::zeroclaw_log::EventCategory::Agent)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({ "error": error })),
                        "Failed to persist ACP turn-verdict audit event"
                    );
                }
                .instrument(span)
                .await;
            });
        }

        match chat_mode {
            crate::rpc::types::ChatMode::Acp => {
                if let Some(ref store) = self.ctx.acp_session_store
                    && let Some(detail) = persist_acp_turn(store, sid, &outcome).await
                {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"session_id": sid, "error": detail})),
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
                        Ok(TurnOutcome::Cancelled { partial_text, .. })
                            if !partial_text.is_empty() =>
                        {
                            let _ = backend.append(&key, &ChatMessage::assistant(partial_text));
                        }
                        _ => {}
                    }
                }
            }
        }

        match outcome {
            Ok(TurnOutcome::Completed { text, .. }) => {
                self.emit_turn_complete(
                    &req.session_id,
                    crate::rpc::types::TurnCompletionOutcome::Completed,
                    text.clone(),
                )
                .await;
                to_result(SessionPromptResult {
                    session_id: req.session_id,
                    stop_reason: "end_turn".to_string(),
                    content: text,
                })
            }
            Ok(TurnOutcome::Cancelled { partial_text, .. }) => {
                let cancel_message = match cancel_cause {
                    Some(cause) => {
                        format!(
                            "turn cancelled via {} in RPC_SESSION {}",
                            cause.as_str(),
                            req.session_id
                        )
                    }
                    None => {
                        format!(
                            "turn cancelled (cause unattributed) in RPC_SESSION {}",
                            req.session_id
                        )
                    }
                };
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Cancel)
                        .with_category(::zeroclaw_log::EventCategory::Agent)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "session_id": req.session_id,
                            "agent_alias": attribution_agent_alias,
                            "model_provider": attribution_model_provider,
                            "model": attribution_model,
                            "chat_mode": format!("{chat_mode:?}"),
                            "cancel_cause": cancel_cause.map(|c| c.as_str()),
                        })),
                    "turn cancelled; emitting attributed TurnComplete so the client exits the working state"
                );
                self.emit_turn_complete(
                    &req.session_id,
                    crate::rpc::types::TurnCompletionOutcome::Cancelled,
                    cancel_message,
                )
                .await;
                to_result(SessionPromptResult {
                    session_id: req.session_id,
                    stop_reason: "cancelled".to_string(),
                    content: partial_text,
                })
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_category(::zeroclaw_log::EventCategory::Agent)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "session_id": req.session_id,
                            "agent_alias": attribution_agent_alias,
                            "model_provider": attribution_model_provider,
                            "model": attribution_model,
                            "chat_mode": format!("{chat_mode:?}"),
                            "error": e.to_string(),
                        })),
                    "turn failed; emitting TurnComplete so the client exits the working state"
                );
                self.emit_turn_complete(
                    &req.session_id,
                    crate::rpc::types::TurnCompletionOutcome::Failed,
                    format!("turn failed: {e}"),
                )
                .await;
                Err(rpc_err(INTERNAL_ERROR, e.to_string()))
            }
        }
    }

    /// Emit the terminal `session/update` notification for a turn.
    /// The TUI uses this — not the JSON-RPC response — to flip
    /// `turn_in_flight` back to false.
    async fn emit_turn_complete(
        &self,
        session_id: &str,
        outcome: crate::rpc::types::TurnCompletionOutcome,
        content: String,
    ) {
        let update = SessionUpdateEvent::TurnComplete {
            session_id: session_id.to_string(),
            outcome,
            content,
        };
        if let Ok(params) = serde_json::to_value(update) {
            let n = JsonRpcNotification::new(notification::SESSION_UPDATE, params);
            if let Ok(s) = serde_json::to_string(&n) {
                let _ = self.rpc.send_raw(s).await;
            }
        }
    }

    async fn handle_session_configure(&self, params: &Value) -> RpcResult {
        let req: SessionConfigureParams = parse_params(params)?;
        validate_session_configure_overrides(&req.overrides)?;

        let merged = self
            .ctx
            .sessions
            .preview_overrides(&req.session_id, &req.overrides)
            .await
            .ok_or_else(|| rpc_err(SESSION_NOT_FOUND, "Session not found"))?;

        // Model/model_provider overrides need a live provider-box rebuild,
        // which requires Config — held here, not in the session store. Resolve
        // the provider from the prospective merged override or configured
        // agent, build the box, and only then commit the override.
        let built_model_provider = if merged.model_provider.is_some() || merged.model.is_some() {
            let agent_alias = self
                .ctx
                .sessions
                .get_agent_alias(&req.session_id)
                .await
                .ok_or_else(|| rpc_err(SESSION_NOT_FOUND, "Session not found"))?;
            let built = {
                let config = self.ctx.config.read();
                let agent_cfg = config
                    .resolved_agent_config(&agent_alias)
                    .or_else(|| config.agent(&agent_alias).cloned())
                    .ok_or_else(|| {
                        rpc_err(
                            INVALID_PARAMS,
                            format!("Agent `{agent_alias}` is not configured"),
                        )
                    })?;
                let model_provider_ref = merged
                    .model_provider
                    .as_deref()
                    .unwrap_or_else(|| agent_cfg.model_provider.as_str());
                let (model_provider, model_provider_name, model_name) =
                    crate::agent::agent::build_session_model_provider(
                        &config,
                        model_provider_ref,
                        merged.model.as_deref(),
                    )
                    .map_err(|e| rpc_err(INVALID_PARAMS, e.to_string()))?;
                let tool_dispatcher = crate::agent::agent::tool_dispatcher_for_provider(
                    &agent_cfg,
                    model_provider.as_ref(),
                );
                (
                    model_provider,
                    model_provider_name,
                    model_name,
                    tool_dispatcher,
                )
            };
            Some(built)
        } else {
            None
        };

        let merged = self
            .ctx
            .sessions
            .set_overrides(&req.session_id, req.overrides)
            .await
            .ok_or_else(|| rpc_err(SESSION_NOT_FOUND, "Session not found"))?;

        if let Some((model_provider, model_provider_name, model_name, tool_dispatcher)) =
            built_model_provider
        {
            self.ctx
                .sessions
                .apply_model_provider(
                    &req.session_id,
                    model_provider,
                    model_provider_name,
                    model_name,
                    tool_dispatcher,
                )
                .await
                .then_some(())
                .ok_or_else(|| rpc_err(SESSION_NOT_FOUND, "Session not found"))?;
        }

        to_result(SessionConfigureResult {
            session_id: req.session_id,
            overrides: merged,
        })
    }

    async fn handle_session_cancel(&self, params: &Value) -> RpcResult {
        let req: SessionIdParams = parse_params(params)?;
        let owner = self
            .ctx
            .sessions
            .session_owner_tui_id(&req.session_id)
            .await;
        let allowed = match (
            owner.as_ref().and_then(|o| o.as_deref()),
            self.tui_id.as_deref(),
        ) {
            (Some(o), Some(c)) => o == c,
            _ => false,
        };
        if !allowed {
            let (agent_alias, model_provider, model) =
                match self.ctx.sessions.get_agent(&req.session_id).await {
                    Some(agent) => agent.lock().await.attribution_fields(),
                    None => (String::new(), String::new(), String::new()),
                };
            let span = ::zeroclaw_log::info_span!(
                target: "zeroclaw_log_internal_scope",
                "zeroclaw_scope",
                session_key = %req.session_id,
                agent_alias = %agent_alias,
                model_provider = %model_provider,
                model = %model,
                channel = "rpc",
            );
            let _guard = span.enter();
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_category(::zeroclaw_log::EventCategory::Channel)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "caller_tui_id": self.tui_id.as_deref().unwrap_or("<none>"),
                        "owner_tui_id": owner
                            .as_ref()
                            .and_then(|o| o.as_deref())
                            .unwrap_or("<none>"),
                        "peer_label": &self.peer_label,
                    })),
                "session/cancel refused: caller does not own the session"
            );
            return Err(rpc_err(
                SESSION_NOT_OWNED,
                "Caller does not own this session",
            ));
        }
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

    async fn handle_session_git_branch(&self, params: &Value) -> RpcResult {
        let req: SessionIdParams = parse_params(params)?;
        let cwd = self
            .ctx
            .sessions
            .get_workspace_dir(&req.session_id)
            .await
            .ok_or_else(|| rpc_err(SESSION_NOT_FOUND, "session not found"))?;
        let info = crate::rpc::git::head_info(std::path::Path::new(&cwd)).unwrap_or_default();
        to_result(SessionGitBranchResult {
            session_id: req.session_id,
            branch: info.branch,
            hash: info.hash,
        })
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
        let req: SessionMessagesParams = parse_params(params)?;
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
        let mut raw: Vec<zeroclaw_api::model_provider::ChatMessage> = Vec::new();
        for key in &candidates {
            let loaded = backend.load(key);
            if !loaded.is_empty() {
                raw = loaded;
                break;
            }
        }

        if raw.is_empty()
            && let Some(store) = self.ctx.acp_session_store.as_ref()
        {
            match store.load_session(&req.session_id) {
                Ok(Some(data)) => {
                    raw = data
                        .messages
                        .into_iter()
                        .filter_map(|m| {
                            match m {
                            zeroclaw_api::model_provider::ConversationMessage::Chat(c) => Some(c),
                            zeroclaw_api::model_provider::ConversationMessage::AssistantToolCalls {
                                text: Some(t),
                                ..
                            } if !t.is_empty() => {
                                Some(zeroclaw_api::model_provider::ChatMessage::assistant(t))
                            }
                            zeroclaw_api::model_provider::ConversationMessage::AssistantToolCalls {
                                ..
                            }
                            | zeroclaw_api::model_provider::ConversationMessage::ToolResults(_) => {
                                None
                            }
                        }
                        })
                        .collect();
                }
                Ok(None) => {}
                Err(e) => {
                    return Err(rpc_err(
                        INTERNAL_ERROR,
                        format!("Failed to load ACP session messages: {e}"),
                    ));
                }
            }
        }

        let total = raw.len();
        let limit = req.limit.unwrap_or(total);
        let end = req.before_index.map(|i| i.min(total)).unwrap_or(total);
        let start = end.saturating_sub(limit);
        let messages: Vec<MessageEntry> = raw[start..end]
            .iter()
            .map(|m| MessageEntry {
                role: m.role.clone(),
                content: m.content.clone(),
            })
            .collect();

        to_result(SessionMessagesResult {
            session_id: req.session_id,
            messages,
            total,
            start,
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
        if let Some(agent) = self.ctx.sessions.get_agent(&req.session_id).await {
            agent
                .lock()
                .await
                .channel_handles()
                .unregister_channel("rpc");
        }
        let existed = self.ctx.sessions.remove(&req.session_id).await;
        if existed && let Some(ref hooks) = self.ctx.hooks {
            hooks.fire_session_end(&req.session_id, "rpc").await;
        }
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
        let entries = truncate_memory_previews(entries);
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
        let entries = truncate_memory_previews(entries);
        to_result(MemorySearchResult { entries, count })
    }

    /// `memory/get { key } → MemoryEntry`. Returns the full memory
    /// entry for one key so the Memory pane can keep only preview
    /// rows in memory and fetch the full `content` only when the
    /// detail pane opens. Dropped on detail close.
    async fn handle_memory_get(&self, params: &Value) -> RpcResult {
        let mem = self
            .ctx
            .memory
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "Memory subsystem is not available"))?;
        let req: MemoryGetParams = parse_params(params)?;
        let entry = mem
            .get(&req.key)
            .await
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Memory get failed: {e}")))?;
        match entry {
            Some(e) => to_result(MemoryGetResult { entry: Some(e) }),
            None => Err(rpc_err(
                INTERNAL_ERROR,
                format!("Memory key `{}` not found", req.key),
            )),
        }
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
        let event_tx = self.ctx.event_tx.clone();
        let result = crate::cron::scheduler::run_manual_job(
            &config,
            &job,
            crate::cron::scheduler::CronDeliveryContext::RpcManual,
            &event_tx,
        )
        .await;
        to_result(CronTriggerResult {
            id: result.job_id,
            success: result.success,
            status: result.status,
            output: result.output,
            duration_ms: result.duration_ms,
            started_at: result.started_at.to_rfc3339(),
            finished_at: result.finished_at.to_rfc3339(),
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
        let refresh_model_provider_ref = model_provider_ref_from_provider_profile_prop(&req.prop);
        {
            let mut config = self.ctx.config.write();
            if config.ensure_map_key_for_path(&req.prop) {
                // Refused to vivify the reserved `default` agent: return a
                // reserved error rather than a downstream "Unknown property".
                return Err(rpc_err(
                    INVALID_PARAMS,
                    "alias `default` is reserved and cannot be created",
                ));
            }
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
            let is_secret_prop = info
                .as_ref()
                .is_some_and(|i| i.is_secret || i.derived_from_secret)
                || zeroclaw_config::schema::Config::prop_is_secret(&req.prop);
            if is_secret_prop
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
        if let Some(model_provider_ref) = refresh_model_provider_ref {
            self.refresh_memory_embedder_for_model_provider(&model_provider_ref);
            self.schedule_live_sessions_refresh_for_model_provider(model_provider_ref);
        }
        if let Some(agent_alias) = agent_alias_from_model_provider_prop(&req.prop) {
            self.schedule_live_sessions_refresh_for_agent(agent_alias);
        }
        to_result(ConfigSetResult {
            prop: req.prop,
            set: true,
        })
    }

    fn refresh_memory_embedder_for_model_provider(&self, model_provider_ref: &str) {
        let resolved = {
            let config = self.ctx.config.read();
            if !memory_embeddings_use_provider(&config, model_provider_ref) {
                return;
            }
            // Match daemon-boot resolution (`create_memory_with_storage_and_routes`
            // is called with `api_key = None`): keys come from the per-route /
            // `[memory]` override or the referenced profile, never an inherited seed.
            zeroclaw_memory::resolve_embedding_settings(
                &config.memory,
                &config.embedding_routes,
                None,
                Some(&config.providers.models),
            )
        };
        // 1. Install-wide RPC memory handle.
        if let Some(memory) = self.ctx.memory.as_ref() {
            memory.refresh_embedder(
                &resolved.model_provider,
                resolved.api_key.as_deref(),
                &resolved.model,
                resolved.dimensions,
            );
        }
        self.schedule_live_agent_memory_refresh(resolved);
    }

    fn schedule_live_agent_memory_refresh(&self, resolved: zeroclaw_memory::EmbeddingSettings) {
        let ctx = Arc::clone(&self.ctx);
        zeroclaw_spawn::spawn!(async move {
            Self::refresh_live_agent_memory(ctx, resolved).await;
        });
    }

    async fn refresh_live_agent_memory(
        ctx: Arc<RpcContext>,
        resolved: zeroclaw_memory::EmbeddingSettings,
    ) {
        for session_id in ctx.sessions.list_ids().await {
            if let Some(agent) = ctx.sessions.get_agent(&session_id).await {
                agent.lock().await.refresh_memory_embedder(
                    &resolved.model_provider,
                    resolved.api_key.as_deref(),
                    &resolved.model,
                    resolved.dimensions,
                );
            }
        }
    }

    fn schedule_live_sessions_refresh_for_model_provider(&self, model_provider_ref: String) {
        let ctx = Arc::clone(&self.ctx);
        zeroclaw_spawn::spawn!(async move {
            Self::refresh_live_sessions_for_model_provider(ctx, &model_provider_ref).await;
        });
    }

    /// Rebuild the live agent box for every session bound to `agent_alias`,
    /// resolving the agent's currently-configured `model_provider` from config.
    /// Fired when `agents.<alias>.model_provider` changes via `config/set` so a
    /// provider switch takes effect on the running session without a restart —
    /// the same refresh a `providers.models.*` edit triggers. Only sessions
    /// bound to the edited agent are rebuilt; sessions belonging to other
    /// agents, and sessions that carry their own `model_provider` override, are
    /// left untouched even when they resolve to the same provider.
    fn schedule_live_sessions_refresh_for_agent(&self, agent_alias: String) {
        let ctx = Arc::clone(&self.ctx);
        zeroclaw_spawn::spawn!(async move {
            let provider_ref = {
                let config = ctx.config.read();
                config
                    .agent(&agent_alias)
                    .map(|agent| agent.model_provider.to_string())
            };
            let Some(provider_ref) = provider_ref else {
                return;
            };
            Self::refresh_live_sessions_matching(ctx, &provider_ref, |session_agent, overrides| {
                agent_scoped_refresh_selects(&agent_alias, session_agent, overrides)
            })
            .await;
        });
    }

    async fn refresh_live_sessions_for_model_provider(
        ctx: Arc<RpcContext>,
        model_provider_ref: &str,
    ) {
        let target_ref = model_provider_ref.to_string();
        Self::refresh_live_sessions_matching(ctx, model_provider_ref, move |_agent, overrides| {
            provider_scoped_refresh_selects(&target_ref, overrides)
        })
        .await;
    }

    async fn refresh_live_sessions_matching<F>(
        ctx: Arc<RpcContext>,
        model_provider_ref: &str,
        select: F,
    ) where
        F: Fn(&str, &SessionOverrides) -> bool,
    {
        let session_ids = ctx.sessions.list_ids().await;
        for session_id in session_ids {
            let Some(agent_alias) = ctx.sessions.get_agent_alias(&session_id).await else {
                continue;
            };
            let Some(overrides) = ctx.sessions.get_overrides(&session_id).await else {
                continue;
            };
            if !select(&agent_alias, &overrides) {
                continue;
            }
            let resolves_provider = {
                let config = ctx.config.read();
                let effective_ref = overrides.model_provider.as_deref().or_else(|| {
                    config
                        .agent(&agent_alias)
                        .map(|agent| agent.model_provider.as_str())
                });
                effective_ref == Some(model_provider_ref)
            };
            if !resolves_provider {
                continue;
            }

            let (model_provider, model_provider_name, model_name, tool_dispatcher, temperature) = {
                let config = ctx.config.read();
                let provider_temperature = model_provider_ref.split_once('.').and_then(
                    |(provider_type, provider_alias)| {
                        config
                            .providers
                            .models
                            .find(provider_type, provider_alias)
                            .and_then(|entry| entry.temperature)
                    },
                );
                let Some(agent_cfg) = config
                    .resolved_agent_config(&agent_alias)
                    .or_else(|| config.agent(&agent_alias).cloned())
                else {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "session_id": session_id,
                                "agent_alias": agent_alias,
                                "model_provider": model_provider_ref,
                            })),
                        "config/set saved provider profile but live session refresh could not resolve agent config"
                    );
                    continue;
                };
                match crate::agent::agent::build_session_model_provider(
                    &config,
                    model_provider_ref,
                    overrides.model.as_deref(),
                ) {
                    Ok((model_provider, model_provider_name, model_name)) => {
                        let tool_dispatcher = crate::agent::agent::tool_dispatcher_for_provider(
                            &agent_cfg,
                            model_provider.as_ref(),
                        );
                        (
                            model_provider,
                            model_provider_name,
                            model_name,
                            tool_dispatcher,
                            overrides.temperature.or(provider_temperature),
                        )
                    }
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "session_id": session_id,
                                "agent_alias": agent_alias,
                                "model_provider": model_provider_ref,
                                "error": e.to_string(),
                            })),
                            "config/set saved provider profile but live session refresh failed"
                        );
                        continue;
                    }
                }
            };
            if ctx
                .sessions
                .apply_model_provider(
                    &session_id,
                    model_provider,
                    model_provider_name,
                    model_name,
                    tool_dispatcher,
                )
                .await
                && let Some(agent) = ctx.sessions.get_agent(&session_id).await
            {
                let mut agent = agent.lock().await;
                agent.set_temperature(temperature);
            }
        }
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
        if !self.schedule_daemon_reload("config") {
            return Err(rpc_err(INTERNAL_ERROR, "Reload not available"));
        }
        to_result(ConfigReloadResult { reloading: true })
    }

    fn schedule_daemon_reload(&self, surface: &'static str) -> bool {
        let Some(reload_tx) = self.ctx.reload_tx.clone() else {
            return false;
        };
        let gateway_shutdown_tx = self.ctx.gateway_shutdown_tx.clone();
        zeroclaw_spawn::spawn!(async move {
            tokio::time::sleep(RPC_RELOAD_REPLY_FLUSH_DELAY).await;
            if let Some(gateway_shutdown_tx) = gateway_shutdown_tx {
                let _ = gateway_shutdown_tx.send(true);
                tokio::time::sleep(RPC_RELOAD_GATEWAY_SHUTDOWN_DELAY).await;
            }
            let _ = reload_tx.send(true);
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_attrs(::serde_json::json!({ "surface": surface })),
                "daemon reload dispatched"
            );
        });
        true
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
                Some(p) => field_visibility::path_matches_prefix(&info.name, p),
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
        let refresh_model_provider_ref = model_provider_ref_from_provider_profile_prop(&req.prop);
        {
            let mut config = self.ctx.config.write();
            config
                .set_prop_persistent(&req.prop, "")
                .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Config delete failed: {e}")))?;
        }
        self.flush_config().await?;
        if let Some(model_provider_ref) = refresh_model_provider_ref {
            self.refresh_memory_embedder_for_model_provider(&model_provider_ref);
            self.schedule_live_sessions_refresh_for_model_provider(model_provider_ref);
        }
        to_result(ConfigDeleteResult {
            prop: req.prop,
            deleted: true,
        })
    }

    fn handle_config_resolve_alias_source(&self, params: &Value) -> RpcResult {
        let req: ConfigResolveAliasSourceParams = parse_params(params)?;
        let config = self.ctx.config.read().clone();
        let values = config.resolve_alias_source(req.source);
        to_result(ConfigResolveAliasSourceResult {
            source: req.source,
            values,
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
            // Shared guarded boundary: enforces the reserved-agent rule (the
            // `default` runtime fallback) on this surface too, so the RPC create
            // path cannot author an `agents.default` the rename guard then traps.
            let created = zeroclaw_config::alias_refs::create_map_key_checked(
                &mut config,
                &req.path,
                &req.key,
            )
            .map_err(|e| rpc_err(INVALID_PARAMS, e.to_string()))?;
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

    fn handle_config_map_key_rename<'a>(&'a self, params: &'a Value) -> BoxRpcFuture<'a> {
        let req: ConfigMapKeyRenameParams = match parse_params(params) {
            Ok(req) => req,
            Err(err) => return Box::pin(std::future::ready(Err(err))),
        };
        if let Some(kind) = zeroclaw_config::alias_refs::alias_kind_for_map_path(&req.path) {
            return self.handle_config_alias_rename(req, kind);
        }

        Box::pin(async move {
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
                warnings: Vec::new(),
            })
        })
    }

    fn handle_config_alias_rename<'a>(
        &'a self,
        req: ConfigMapKeyRenameParams,
        kind: zeroclaw_config::alias_refs::AliasKind,
    ) -> BoxRpcFuture<'a> {
        Box::pin(async move {
            let is_agent = matches!(kind, zeroclaw_config::alias_refs::AliasKind::Agent);
            if is_agent {
                // Live RPC sessions hold the selected agent alias in memory; refuse
                // rather than letting them recreate old-alias state after the rename.
                let active = self
                    .ctx
                    .sessions
                    .count_by_agent()
                    .await
                    .get(&req.from)
                    .copied()
                    .unwrap_or(0);
                if active > 0 {
                    return Err(rpc_err(
                        INVALID_PARAMS,
                        format!(
                            "{}.{}: cannot rename agent with {active} active RPC session(s); close those sessions first",
                            req.path, req.from
                        ),
                    ));
                }
            }

            let mut working = self.ctx.config.read().clone();
            let old_workspace = is_agent.then(|| working.agent_workspace_dir(&req.from));
            // If a prior call saved config as `to` but crashed before side effects,
            // re-running `from -> to` should converge lagging owned state instead
            // of failing because `from` is no longer a config key.
            let resume_committed_to = is_agent
                && working.agent(&req.from).is_none()
                && working.agent(&req.to).is_some()
                && self.agent_rename_residue_exists(&working, &req.from).await;

            if !resume_committed_to {
                let report = zeroclaw_config::alias_refs::rename_with_cascade(
                    &mut working,
                    &kind,
                    &req.from,
                    &req.to,
                )
                .map_err(|e| rename_error_to_rpc(&req.path, &req.from, e))?;
                for path in &report.dirty_paths {
                    working.mark_dirty(path);
                }
                self.save_and_swap_config(working.clone()).await?;
            }
            let new_workspace = is_agent.then(|| working.agent_workspace_dir(&req.to));

            let mut warnings = Vec::new();
            if let (Some(old_workspace), Some(new_workspace)) = (old_workspace, new_workspace) {
                warnings.extend(move_renamed_agent_workspace(&old_workspace, &new_workspace).await);
                warnings.extend(
                    self.rename_agent_owned_state(&working, &req.from, &req.to)
                        .await,
                );
            }

            to_result(ConfigMapKeyRenameResult {
                path: req.path,
                from: req.from,
                to: req.to,
                renamed: true,
                warnings,
            })
        })
    }

    async fn rename_agent_owned_state(
        &self,
        config: &zeroclaw_config::schema::Config,
        from: &str,
        to: &str,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut memory_rows = 0usize;
        let mut cron_jobs = 0usize;
        let mut acp_sessions = 0usize;
        let mut sessions_repointed = 0usize;

        if let Some(mem) = &self.ctx.memory {
            match mem.rename_agent(from, to).await {
                Ok(n) => memory_rows = n,
                Err(e) => warnings.push(format!("memory rename: {e}")),
            }
        }

        match crate::cron::rename_jobs_by_agent(config, from, to) {
            Ok(n) => cron_jobs = n,
            Err(e) => warnings.push(format!("cron rename: {e}")),
        }

        match &self.ctx.acp_session_store {
            Some(store) => match store.rename_sessions_by_agent(from, to) {
                Ok(n) => acp_sessions = n,
                Err(e) => warnings.push(format!("acp rename: {e}")),
            },
            None => warnings.push("acp store unavailable".to_string()),
        }

        if let Some(backend) = &self.ctx.session_backend {
            match backend.rename_agent_attribution(from, to) {
                Ok(n) => sessions_repointed = n,
                Err(e) => warnings.push(format!("session attribution rename: {e}")),
            }
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "from": from,
                    "to": to,
                    "memory": memory_rows,
                    "cron": cron_jobs,
                    "acp": acp_sessions,
                    "sessions": sessions_repointed,
                    "warnings": warnings.clone(),
                })
            ),
            "agent renamed with RPC owned-state cascade"
        );

        warnings
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
                    live_sessions: rpc,
                    persisted_sessions: persisted,
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
        // Optional `[from, to)` window (RFC3339). Lets callers (the dashboard's
        // Reports view, or an external CLI report) pull day/month/quarter/YTD
        // scalars rather than only the daemon's today/this-month aggregates.
        let parse_bound = |raw: &str| -> Result<chrono::DateTime<chrono::Utc>, _> {
            chrono::DateTime::parse_from_rfc3339(raw)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| rpc_err(INVALID_PARAMS, format!("invalid date {raw:?}: {e}")))
        };
        let from = req.from.as_deref().map(parse_bound).transpose()?;
        let to = req.to.as_deref().map(parse_bound).transpose()?;
        // Precedence (inherited from the existing per-agent path): an explicit
        // `agent` selects that agent's summary and the [from, to) window does
        // NOT apply; the window scopes only the fleet-wide summary.
        let summary = if let Some(agent) = req.agent {
            tracker
                .get_summary_for_agent(&agent)
                .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Cost query failed: {e}")))?
        } else if from.is_some() || to.is_some() {
            tracker
                .get_summary_in_bounds(from, to)
                .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Cost query failed: {e}")))?
        } else {
            tracker
                .get_summary()
                .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Cost query failed: {e}")))?
        };
        to_result(summary)
    }

    fn handle_cost_org(&self) -> RpcResult {
        let path = self.ctx.config.read().data_dir.join("org_cost.json");
        match std::fs::read_to_string(&path) {
            Ok(raw) => {
                let value: Value = serde_json::from_str(&raw).map_err(|e| {
                    rpc_err(
                        INTERNAL_ERROR,
                        format!("org_cost.json is not valid JSON: {e}"),
                    )
                })?;
                Ok(value)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
            Err(e) => Err(rpc_err(
                INTERNAL_ERROR,
                format!("failed to read org_cost.json: {e}"),
            )),
        }
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
        let config = self.ctx.config.read().clone();
        let ctx = personality_template_context(&config, &req);
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

        let direct_scalar_parents: std::collections::HashSet<String> = config
            .prop_fields()
            .iter()
            .filter_map(|f| {
                let mut segs = f.name.split('.');
                let root = segs.next()?;
                // exactly one more segment past root = direct child scalar
                segs.next()?;
                if segs.next().is_some() {
                    return None;
                }
                Some(root.to_string())
            })
            .collect();
        let parents_with_children: std::collections::HashSet<String> = roots
            .iter()
            .filter_map(|k| k.split_once('.').map(|(p, _)| p.to_string()))
            .collect();
        roots.retain(|k| {
            k.contains('.')
                || !parents_with_children.contains(k)
                || direct_scalar_parents.contains(k)
        });

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
                let label = zeroclaw_config::sections::humanize_section_key(&key);
                ConfigSectionEntry {
                    help: section_help(&key).to_string(),
                    has_picker,
                    completed,
                    ready: false,
                    group: zeroclaw_config::sections::section_group_for_key(&key)
                        .label()
                        .to_string(),
                    is_quickstart: wizard.is_some(),
                    shape: wizard.map(Section::shape),
                    cost_category: zeroclaw_config::schema::cost_category_for_provider_section(
                        &key,
                    )
                    .unwrap_or_default()
                    .to_string(),
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
        let local = crate::quickstart::model_provider_is_local(&req.model_provider);
        // Snapshot config so the catalog can resolve the alias credential and
        // reach the native /models endpoint (surfacing new native-only models
        // that models.dev may not carry yet) rather than silently falling back.
        let config = self.ctx.config.read().clone();
        let (models, pricing, live) =
            crate::quickstart::model_catalog_with_config(Some(&config), &req.model_provider).await;
        to_result(CatalogModelsResult {
            model_provider: req.model_provider,
            models,
            pricing,
            local,
            live,
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
        zeroclaw_spawn::spawn!(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = rpc.closed() => break,
                    event = rx.recv() => match event {
                        Ok(mut event) => {
                            // Pairing secrets (QR payloads, one-shot pair codes)
                            // ride the shared broadcast bus stamped with the
                            // ephemeral marker. `logs/subscribe` is NOT the
                            // bearer-authenticated SSE surface those credentials
                            // are scoped to — a fresh remote RPC client can
                            // `initialize` and subscribe over WSS without the
                            // gateway bearer check — so fail closed: withhold
                            // marked frames entirely and strip the internal
                            // marker from everything else (public shape
                            // unchanged). See `zeroclaw_gateway::sse`.
                            if zeroclaw_log::frame_carries_ephemeral_credentials(&event) {
                                continue;
                            }
                            zeroclaw_log::strip_ephemeral_broadcast_marker(&mut event);
                            let notification =
                                JsonRpcNotification::new(notification::LOGS_EVENT, event);
                            if let Ok(json) = serde_json::to_string(&notification)
                                && !rpc.send_raw(json).await
                            {
                                break;
                            }
                        }
                        Err(_) => break,
                    },
                }
            }
        });
        to_result(LogsSubscribeResult { subscribed: true })
    }

    #[allow(deprecated)] // we still forward the legacy cursor for backwards compat
    async fn handle_logs_query(&self, params: &Value) -> RpcResult {
        let p: LogsQueryParams = parse_params(params)?;

        let path = zeroclaw_log::current_log_path()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "Log persistence is not enabled"))?;

        let filter = zeroclaw_log::LogFilter {
            since_ts: p.since_ts,
            until_ts: p.until_ts,
            until_id: p.until_id,
            until_line_offset: p.until_line_offset,
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
            next_cursor_line_offset: page.next_cursor_line_offset,
            at_end: page.at_end,
        })
    }

    /// `logs/get { id } → LogEvent`. Loads one full event by id from
    /// the persistent JSONL log so the Logs pane can keep only preview
    /// fields in memory and lazy-fetch the full payload only when the
    /// user opens the detail pane.
    async fn handle_logs_get(&self, params: &Value) -> RpcResult {
        let p: LogsGetParams = parse_params(params)?;
        let path = zeroclaw_log::current_log_path()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "Log persistence is not enabled"))?;
        let event = zeroclaw_log::find_event_by_id(&path, &p.id)
            .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Log read failed: {e:#}")))?;
        match event {
            Some(evt) => {
                let event = serde_json::to_value(evt).map_err(|e| {
                    rpc_err(INTERNAL_ERROR, format!("Failed to serialize event: {e}"))
                })?;
                to_result(LogsGetResult { event })
            }
            None => Err(rpc_err(
                INTERNAL_ERROR,
                format!("Log id `{}` not found", p.id),
            )),
        }
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

    fn sops_dir_and_mode(&self) -> (std::path::PathBuf, crate::sop::SopExecutionMode) {
        let config = self.ctx.config.read();
        let workspace = config.shared_workspace_dir();
        let dir = crate::sop::resolve_sops_dir(&workspace, config.sop.sops_dir.as_deref());
        let mode = crate::sop::parse_execution_mode(&config.sop.default_execution_mode);
        (dir, mode)
    }

    fn parse_sop(value: &Value) -> Result<crate::sop::Sop, JsonRpcError> {
        serde_json::from_value(value.clone()).map_err(|e| rpc_err(INVALID_PARAMS, e.to_string()))
    }

    fn handle_sops_list(&self) -> RpcResult {
        let (dir, mode) = self.sops_dir_and_mode();
        let sops = crate::sop::load_sops_from_directory(&dir, mode);
        to_result(sops)
    }

    fn handle_sops_get(&self, params: &Value) -> RpcResult {
        let req: SopSelectRequest = parse_params(params)?;
        let (dir, mode) = self.sops_dir_and_mode();
        let sop = crate::sop::load_sop_by_name(&dir, &req.name, mode)
            .map_err(|e| rpc_err(INVALID_PARAMS, format!("SOP '{}': {e}", req.name)))?;
        to_result(sop)
    }

    fn handle_sops_graph(&self, params: &Value) -> RpcResult {
        let req: SopSelectRequest = parse_params(params)?;
        let (dir, mode) = self.sops_dir_and_mode();
        let sop = crate::sop::load_sop_by_name(&dir, &req.name, mode)
            .map_err(|e| rpc_err(INVALID_PARAMS, format!("SOP '{}': {e}", req.name)))?;
        to_result(crate::sop::SopGraph::from_sop_with_specs(
            &sop,
            &self.sop_tool_specs(),
        ))
    }

    async fn handle_sops_run(&self, params: &Value) -> RpcResult {
        let req: SopRunRequest = parse_params(params)?;

        if let Some(payload) = req.payload.as_deref()
            && !payload.trim().is_empty()
            && serde_json::from_str::<Value>(payload).is_err()
        {
            return Err(rpc_err(INVALID_PARAMS, "payload is not valid JSON"));
        }

        let engine = self
            .ctx
            .sop_engine
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "SOP subsystem not enabled"))?;
        let audit = self
            .ctx
            .sop_audit
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "SOP subsystem not enabled"))?;

        let payload = req
            .payload
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string);

        let event = crate::sop::SopEvent {
            source: crate::sop::SopTriggerSource::Manual,
            topic: None,
            payload,
            timestamp: crate::sop::engine::now_iso8601(),
        };

        let results =
            crate::sop::dispatch::dispatch_sop_event_to(engine, audit, event, &req.name).await;
        crate::sop::dispatch::process_headless_results(&results);

        for result in &results {
            match result {
                crate::sop::dispatch::DispatchResult::Started { run_id, .. } => {
                    return to_result(SopRunResponse {
                        run_id: run_id.clone(),
                    });
                }
                crate::sop::dispatch::DispatchResult::Skipped { reason, .. }
                | crate::sop::dispatch::DispatchResult::BlockedUnsafe { reason, .. } => {
                    return Err(rpc_err(INVALID_PARAMS, reason.clone()));
                }
                crate::sop::dispatch::DispatchResult::Deferred { reason, .. } => {
                    return Err(rpc_err(INVALID_PARAMS, reason.clone()));
                }
                crate::sop::dispatch::DispatchResult::Coalesced {
                    existing_run_id, ..
                } => {
                    return to_result(SopRunResponse {
                        run_id: existing_run_id.clone(),
                    });
                }
                crate::sop::dispatch::DispatchResult::NoMatch => {}
            }
        }

        Err(rpc_err(
            INVALID_PARAMS,
            format!("SOP '{}' has no matching manual trigger", req.name),
        ))
    }

    fn handle_sops_runs(&self, params: &Value) -> RpcResult {
        let req: SopRunsRequest = parse_params(params)?;
        let engine = self
            .ctx
            .sop_engine
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "SOP subsystem not enabled"))?;
        let runs = crate::sop::run_summaries_for(engine, req.sop.as_deref())
            .map_err(|e| rpc_err(INTERNAL_ERROR, e.to_string()))?;
        to_result(serde_json::json!({ "runs": runs }))
    }

    fn handle_sops_run_overlay(&self, params: &Value) -> RpcResult {
        let req: SopRunOverlayRequest = parse_params(params)?;
        let (dir, mode) = self.sops_dir_and_mode();
        let sop = crate::sop::load_sop_by_name(&dir, &req.name, mode)
            .map_err(|e| rpc_err(INVALID_PARAMS, format!("SOP '{}': {e}", req.name)))?;
        let engine = self
            .ctx
            .sop_engine
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "SOP subsystem not enabled"))?;
        let overlay = crate::sop::run_overlay_for(&sop, engine, &req.run_id).map_err(|e| {
            let msg = e.to_string();
            let code = if msg.contains("not found") {
                INVALID_PARAMS
            } else {
                INTERNAL_ERROR
            };
            rpc_err(code, msg)
        })?;
        to_result(overlay)
    }

    async fn handle_sops_decide(&self, params: &Value) -> RpcResult {
        let req: SopDecideRequest = parse_params(params)?;
        let decision: crate::sop::approval::ApprovalDecision =
            serde_json::from_value(req.decision.clone()).map_err(|e| {
                rpc_err(
                    INVALID_PARAMS,
                    format!("decision is not a valid approval decision: {e}"),
                )
            })?;

        let (dir, mode) = self.sops_dir_and_mode();
        let sop = crate::sop::load_sop_by_name(&dir, &req.name, mode)
            .map_err(|e| rpc_err(INVALID_PARAMS, format!("SOP '{}': {e}", req.name)))?;
        let engine = self
            .ctx
            .sop_engine
            .as_ref()
            .ok_or_else(|| rpc_err(INTERNAL_ERROR, "SOP subsystem not enabled"))?
            .clone();

        let agent_alias = sop.agent.clone().unwrap_or_default();
        let span = ::zeroclaw_log::info_span!(
            target: "zeroclaw_log_internal_scope",
            "zeroclaw_scope",
            session_key = %req.run_id,
            agent_alias = %agent_alias,
            channel = "rpc",
        );
        let _guard = span.enter();

        let mut resolved_outcome = None;
        {
            let mut guard = engine
                .lock()
                .map_err(|_| rpc_err(INTERNAL_ERROR, "SOP engine lock poisoned"))?;
            let run_sop_name = guard
                .get_run(&req.run_id)
                .map(|run| run.sop_name.clone())
                .ok_or_else(|| {
                    rpc_err(INVALID_PARAMS, format!("run '{}' not found", req.run_id))
                })?;
            if run_sop_name != req.name {
                return Err(rpc_err(
                    INVALID_PARAMS,
                    format!(
                        "run '{}' belongs to SOP '{}', not '{}'",
                        req.run_id, run_sop_name, req.name
                    ),
                ));
            }
            use crate::sop::approval::{BrokerOutcome, ResolveOutcome};
            let principal = crate::sop::approval::ApprovalPrincipal::cli(self.tui_id.clone());
            match guard
                .resolve_via_broker(&req.run_id, decision, principal)
                .map_err(|e| rpc_err(INTERNAL_ERROR, e.to_string()))?
            {
                outcome @ BrokerOutcome::Resolved(ResolveOutcome::Resumed(_)) => {
                    resolved_outcome = Some(outcome);
                }
                BrokerOutcome::Resolved(
                    ResolveOutcome::Denied
                    | ResolveOutcome::AlreadyResolved
                    | ResolveOutcome::Revised,
                )
                | BrokerOutcome::PendingQuorum { .. } => {}
                BrokerOutcome::Resolved(
                    ResolveOutcome::NotWaiting | ResolveOutcome::DeferredAtCapacity,
                )
                | BrokerOutcome::NotWaiting => {
                    return Err(rpc_err(
                        INVALID_PARAMS,
                        crate::i18n::get_required_cli_string_with_args(
                            "sop-rpc-decision-invalid-state",
                            &[("run_id", req.run_id.as_str())],
                        ),
                    ));
                }
                BrokerOutcome::Resolved(ResolveOutcome::RejectedSelfApproval)
                | BrokerOutcome::NotAuthorized { .. } => {
                    return Err(rpc_err(
                        AUTH_REQUIRED,
                        crate::i18n::get_required_cli_string("sop-rpc-decision-unauthorized"),
                    ));
                }
                BrokerOutcome::PolicyMissing { name } => {
                    return Err(rpc_err(
                        INTERNAL_ERROR,
                        crate::i18n::get_required_cli_string_with_args(
                            "sop-rpc-policy-missing",
                            &[("name", name.as_str())],
                        ),
                    ));
                }
                BrokerOutcome::PolicyUnavailable { reason } => {
                    return Err(rpc_err(
                        INTERNAL_ERROR,
                        crate::i18n::get_required_cli_string_with_args(
                            "sop-rpc-policy-unavailable",
                            &[("reason", reason.as_str())],
                        ),
                    ));
                }
            }
        }

        if let Some(outcome) = resolved_outcome {
            let config = self.ctx.config.read();
            crate::sop::drive_resumed_broker_action(
                &config,
                Arc::clone(&engine),
                self.ctx.sop_audit.clone(),
                &outcome,
            );
        }

        let overlay = crate::sop::run_overlay_for(&sop, &engine, &req.run_id).map_err(|e| {
            let msg = e.to_string();
            let code = if msg.contains("not found") {
                INVALID_PARAMS
            } else {
                INTERNAL_ERROR
            };
            rpc_err(code, msg)
        })?;
        to_result(overlay)
    }

    fn handle_sops_validate(&self, params: &Value) -> RpcResult {
        let sop = if params.get("sop").is_some() {
            let req: SopSaveRequest = parse_params(params)?;
            Self::parse_sop(&req.sop)?
        } else {
            let req: SopSelectRequest = parse_params(params)?;
            let (dir, mode) = self.sops_dir_and_mode();
            crate::sop::load_sop_by_name(&dir, &req.name, mode)
                .map_err(|e| rpc_err(INVALID_PARAMS, format!("SOP '{}': {e}", req.name)))?
        };
        let v = crate::sop::validate_sop_strict(&sop);
        to_result(serde_json::json!({
            "blocking": v.blocking,
            "warnings": v.warnings,
            "ok": v.is_ok(),
        }))
    }

    fn handle_sops_save(&self, params: &Value) -> RpcResult {
        let req: SopSaveRequest = parse_params(params)?;
        let sop = Self::parse_sop(&req.sop)?;
        if let Some(original) = req.original_name.as_deref()
            && !original.is_empty()
            && original != sop.name
        {
            return Err(rpc_err(
                INVALID_PARAMS,
                format!(
                    "rename not supported: SOP '{original}' cannot be saved as '{}'",
                    sop.name
                ),
            ));
        }
        let (dir, _mode) = self.sops_dir_and_mode();
        crate::sop::save_sop(&dir, &sop).map_err(|e| rpc_err(INVALID_PARAMS, e.to_string()))?;
        to_result(serde_json::json!({ "saved": sop.name }))
    }

    fn handle_sops_create(&self, params: &Value) -> RpcResult {
        let req: SopSaveRequest = parse_params(params)?;
        let sop = Self::parse_sop(&req.sop)?;
        let (dir, _mode) = self.sops_dir_and_mode();
        crate::sop::create_sop_typed(&dir, &sop).map_err(|e| {
            let code = match e {
                crate::sop::SopAuthorError::AlreadyExists(_) => SOP_ALREADY_EXISTS,
                _ => INVALID_PARAMS,
            };
            rpc_err(code, e.to_string())
        })?;
        to_result(serde_json::json!({ "created": sop.name }))
    }

    fn handle_sops_delete(&self, params: &Value) -> RpcResult {
        let req: SopSelectRequest = parse_params(params)?;
        let (dir, _mode) = self.sops_dir_and_mode();
        crate::sop::delete_sop_typed(&dir, &req.name).map_err(|e| {
            let code = match e {
                crate::sop::SopAuthorError::NotFound(_) => SOP_NOT_FOUND,
                _ => INTERNAL_ERROR,
            };
            rpc_err(code, e.to_string())
        })?;
        to_result(serde_json::json!({ "deleted": req.name }))
    }

    fn handle_sops_wire_draft(&self, params: &Value) -> RpcResult {
        let sop_val = params
            .get("sop")
            .ok_or_else(|| rpc_err(INVALID_PARAMS, "missing 'sop'"))?;
        let edit_val = params
            .get("edit")
            .ok_or_else(|| rpc_err(INVALID_PARAMS, "missing 'edit'"))?;
        let mut sop = Self::parse_sop(sop_val)?;
        let edit: crate::sop::WireEdit = serde_json::from_value(edit_val.clone())
            .map_err(|e| rpc_err(INVALID_PARAMS, format!("invalid wire edit: {e}")))?;
        crate::sop::apply_wire(&mut sop, &edit)
            .map_err(|e| rpc_err(INVALID_PARAMS, e.to_string()))?;
        to_result(serde_json::json!({
            "sop": sop,
            "graph": crate::sop::SopGraph::from_sop_with_specs(&sop, &self.sop_tool_specs()),
        }))
    }

    fn handle_sops_graph_draft(&self, params: &Value) -> RpcResult {
        let sop_val = params
            .get("sop")
            .ok_or_else(|| rpc_err(INVALID_PARAMS, "missing 'sop'"))?;
        let sop = Self::parse_sop(sop_val)?;
        to_result(crate::sop::SopGraph::from_sop_with_specs(
            &sop,
            &self.sop_tool_specs(),
        ))
    }

    fn sop_tool_specs(&self) -> crate::sop::ToolSpecs {
        let config = self.ctx.config.read();
        let agent = config.agents.keys().min().cloned().unwrap_or_default();
        crate::sop::tool_specs_from_config(&config, &agent)
    }

    fn handle_sops_trigger_sources(&self) -> RpcResult {
        let registry = {
            let config = self.ctx.config.read();
            crate::sop::registry_from_config(&config)
        };
        to_result(registry)
    }

    /// Resolve selectable values for a domain-typed tool parameter.
    /// Params: `{ domain, agent?, args? }`. `domain` is an
    /// `OptionDomain` wire name (e.g. `peer_targets`); `agent` scopes
    /// agent-relative domains; `args` carries sibling arguments already
    /// chosen so cascading domains can narrow.
    fn handle_tools_param_options(&self, params: &Value) -> RpcResult {
        #[derive(serde::Deserialize)]
        struct ParamOptionsParams {
            domain: zeroclaw_api::tool::OptionDomain,
            #[serde(default)]
            agent: Option<String>,
            #[serde(default)]
            args: Value,
        }
        let req: ParamOptionsParams = parse_params(params)?;
        let config = self.ctx.config.read();
        let agent_alias = req
            .agent
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_string)
            .or_else(|| config.agents.keys().min().cloned())
            .unwrap_or_default();

        let entries = if req.domain == zeroclaw_api::tool::OptionDomain::ToolNames {
            let security = std::sync::Arc::new(
                zeroclaw_config::policy::SecurityPolicy::for_agent(&config, &agent_alias)
                    .unwrap_or_default(),
            );
            let tools = crate::tools::default_tools(security);
            let refs: Vec<&dyn zeroclaw_api::tool::Tool> =
                tools.iter().map(std::convert::AsRef::as_ref).collect();
            crate::tools::param_options::resolve_options(
                req.domain,
                &config,
                &agent_alias,
                &req.args,
                &refs,
            )
        } else {
            crate::tools::param_options::resolve_options(
                req.domain,
                &config,
                &agent_alias,
                &req.args,
                &[],
            )
        };
        to_result(serde_json::json!({ "options": entries }))
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
        if self.ctx.reload_tx.is_none() {
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
        self.schedule_daemon_reload(crate::quickstart::Surface::Tui.as_str())
    }
}

// ── Helpers ──────────────────────────────────────────────────────

fn parse_params<T: DeserializeOwned>(params: &Value) -> Result<T, JsonRpcError> {
    serde_json::from_value(params.clone()).map_err(|e| rpc_err(INVALID_PARAMS, e.to_string()))
}

fn validate_session_configure_overrides(overrides: &SessionOverrides) -> Result<(), JsonRpcError> {
    if overrides
        .model
        .as_deref()
        .is_some_and(|model| model.trim().is_empty())
    {
        return Err(rpc_err(INVALID_PARAMS, "model must not be blank"));
    }
    if overrides
        .model_provider
        .as_deref()
        .is_some_and(|provider| provider.trim().is_empty())
    {
        return Err(rpc_err(INVALID_PARAMS, "model_provider must not be blank"));
    }
    Ok(())
}

fn to_result<T: Serialize>(val: T) -> RpcResult {
    serde_json::to_value(val).map_err(|e| rpc_err(INTERNAL_ERROR, e.to_string()))
}

const MEMORY_PREVIEW_CONTENT_BYTES: usize = 200;

/// Truncate each entry's `content` to the preview budget. Operates
/// in place to avoid a second allocation per entry.
fn truncate_memory_previews(
    mut entries: Vec<zeroclaw_api::memory_traits::MemoryEntry>,
) -> Vec<zeroclaw_api::memory_traits::MemoryEntry> {
    for entry in &mut entries {
        if entry.content.len() > MEMORY_PREVIEW_CONTENT_BYTES {
            // Truncate on a char boundary so we never split a UTF-8 sequence.
            let mut end = MEMORY_PREVIEW_CONTENT_BYTES;
            while end > 0 && !entry.content.is_char_boundary(end) {
                end -= 1;
            }
            entry.content.truncate(end);
            entry.content.push('…');
        }
    }
    entries
}

/// Resolve the max-token ceiling shown on Zerocode's context usage meter.
///
/// The wire field is named `max_context_tokens` and must track the operator's
/// runtime-profile budget (`[runtime_profiles.<name>] max_context_tokens`),
/// which is also the preemptive history-trim budget. Using the provider
/// model-window helper here is wrong: that path ignores the runtime profile
/// and falls back to 32_000 when `providers.models.*.context_window` is unset,
/// so the meter freezes at the default even when the profile is set higher.
fn context_usage_max_tokens(cfg: &zeroclaw_config::schema::Config, agent_alias: &str) -> u64 {
    cfg.effective_max_context_tokens(agent_alias) as u64
}

/// Persist the exact turn delta captured before structured history trimming.
/// Empty and failed turns intentionally remain no-ops.
async fn persist_acp_turn(
    store: &Arc<zeroclaw_infra::acp_session_store::AcpSessionStore>,
    session_id: &str,
    outcome: &Result<TurnOutcome, crate::rpc::turn::TurnError>,
) -> Option<String> {
    let messages = match outcome {
        Ok(TurnOutcome::Completed { messages, .. })
        | Ok(TurnOutcome::Cancelled { messages, .. })
            if !messages.is_empty() =>
        {
            messages.clone()
        }
        _ => return None,
    };
    let store = Arc::clone(store);
    let session_id = session_id.to_string();
    match tokio::task::spawn_blocking(move || store.append_turn(&session_id, &messages)).await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(join) => Some(join.to_string()),
    }
}

/// Persist a `TurnEvent::Plan` before it is emitted, so a racing
/// reconnect — or a later `session/resume` — reads a consistent plan.
/// Writes both the in-memory live cache (`sessions`) and, when an ACP
/// durable store is present, the on-disk `plan_json` column (via
/// `spawn_blocking`, since SQLite is synchronous). No-op for every
/// other event. Durable-write failures are logged-and-swallowed: the
/// in-memory cache is still authoritative for the live session.
async fn persist_plan_if_any(
    sessions: &crate::rpc::session::SessionStore,
    acp_store: Option<&std::sync::Arc<zeroclaw_infra::acp_session_store::AcpSessionStore>>,
    session_id: &str,
    event: &TurnEvent,
) {
    let TurnEvent::Plan { entries } = event else {
        return;
    };
    sessions.set_plan(session_id, entries.clone()).await;
    if let Some(store) = acp_store {
        let store = store.clone();
        let sid = session_id.to_string();
        let entries = entries.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Err(e) = store.set_plan(&sid, &entries) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "session_id": sid,
                            "error": e.to_string(),
                        })),
                    "Failed to persist TodoWrite plan to ACP session store"
                );
            }
        })
        .await;
    }
}

fn plan_replay_notification(
    session_id: &str,
    entries: &[zeroclaw_api::plan::PlanEntry],
) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let event = TurnEvent::Plan {
        entries: entries.to_vec(),
    };
    notification_for_turn_event(session_id, &event, None)
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
        TurnEvent::HistoryTrimmed {
            dropped_messages,
            kept_turns,
            reason,
        } => SessionUpdateEvent::HistoryTrimmed {
            session_id: session_id.to_string(),
            dropped_messages: *dropped_messages,
            kept_turns: *kept_turns,
            reason: reason.clone(),
        },
        TurnEvent::Usage {
            input_tokens,
            cached_input_tokens: _,
            output_tokens: _,
            ..
        } => SessionUpdateEvent::ContextUsage {
            session_id: session_id.to_string(),
            input_tokens: *input_tokens,
            max_context_tokens,
        },
        TurnEvent::Plan { entries } => SessionUpdateEvent::Plan {
            session_id: session_id.to_string(),
            entries: entries.clone(),
        },
    };

    let params = serde_json::to_value(update).ok()?;
    let n = JsonRpcNotification::new(notification::SESSION_UPDATE, params);
    serde_json::to_string(&n).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn memory_embeddings_use_provider_matches_base_ref_and_routes() {
        use zeroclaw_config::schema::{Config, EmbeddingRouteConfig};

        let mut config = Config::default();
        config.memory.embedding_provider = "openai.default".into();
        config.embedding_routes = vec![EmbeddingRouteConfig {
            hint: "semantic".into(),
            model_provider: "openrouter.alt".into(),
            model: "embed".into(),
            dimensions: Some(1024),
            api_key: None,
        }];

        // Base `[memory].embedding_provider` reference.
        assert!(memory_embeddings_use_provider(&config, "openai.default"));
        // Any `[[embedding_routes]]` reference.
        assert!(memory_embeddings_use_provider(&config, "openrouter.alt"));
        // An unrelated provider must not trigger a memory-embedder refresh.
        assert!(!memory_embeddings_use_provider(
            &config,
            "anthropic.default"
        ));
    }

    #[test]
    fn agent_alias_from_model_provider_prop_matches_only_the_bound_provider_field() {
        // The config pane and other `config/set agents.<alias>.model_provider`
        // callers write this path; it must map back to the alias so the live
        // session refresh fires. The zerocode picker takes the `session/configure`
        // path instead and is not a caller here.
        assert_eq!(
            agent_alias_from_model_provider_prop("agents.fred.model_provider"),
            Some("fred".to_string())
        );
        // Any other agent field must not trigger a provider rebuild.
        assert_eq!(
            agent_alias_from_model_provider_prop("agents.fred.risk_profile"),
            None
        );
        // A provider-profile edit is handled by the other refresh path, not this one.
        assert_eq!(
            agent_alias_from_model_provider_prop("providers.models.anthropic.default.model"),
            None
        );
        // Empty alias is rejected.
        assert_eq!(
            agent_alias_from_model_provider_prop("agents..model_provider"),
            None
        );
    }

    #[test]
    fn agent_scoped_refresh_selects_only_edited_agent_without_override() {
        use crate::rpc::session::SessionOverrides;
        let no_override = SessionOverrides::default();
        let with_override = SessionOverrides {
            model_provider: Some("anthropic.other".to_string()),
            ..Default::default()
        };

        // A session bound to the edited agent with no override is rebuilt.
        assert!(agent_scoped_refresh_selects("fred", "fred", &no_override));
        // A session belonging to a different agent is never rebuilt, even
        // when it resolves to the same provider.
        assert!(!agent_scoped_refresh_selects("fred", "wilma", &no_override));
        // The edited agent's own session is left untouched when it carries a
        // `model_provider` override.
        assert!(!agent_scoped_refresh_selects(
            "fred",
            "fred",
            &with_override
        ));
        // A different agent with an override is likewise excluded.
        assert!(!agent_scoped_refresh_selects(
            "fred",
            "wilma",
            &with_override
        ));
    }

    #[test]
    fn provider_scoped_refresh_selects_inheritors_and_matching_overrides() {
        use crate::rpc::session::SessionOverrides;
        let no_override = SessionOverrides::default();
        let matching_override = SessionOverrides {
            model_provider: Some("anthropic.default".to_string()),
            ..Default::default()
        };
        let other_override = SessionOverrides {
            model_provider: Some("openai.default".to_string()),
            ..Default::default()
        };

        // No override: inherits the agent provider, so it is a candidate
        // (final config match is resolved by the caller).
        assert!(provider_scoped_refresh_selects(
            "anthropic.default",
            &no_override
        ));
        // Override that names the edited provider is a candidate.
        assert!(provider_scoped_refresh_selects(
            "anthropic.default",
            &matching_override
        ));
        // Override that names a different provider is excluded.
        assert!(!provider_scoped_refresh_selects(
            "anthropic.default",
            &other_override
        ));
    }

    #[test]
    fn session_initializes_mcp_for_chat_but_not_acp() {
        use crate::rpc::types::ChatMode;
        // Chat sessions must initialize MCP so the Zerocode TUI sees the same
        // MCP tools (and the deferred-loading `tool_search`) the gateway
        // already exposes for the agent
        assert!(
            session_should_initialize_mcp(&ChatMode::Chat),
            "Chat sessions must eagerly initialize MCP"
        );
        // ACP (Code) sessions intentionally skip eager MCP init to keep
        // `session/new` prompt.
        assert!(
            !session_should_initialize_mcp(&ChatMode::Acp),
            "ACP sessions must skip eager MCP init"
        );
    }

    /// Spin up a wiremock server that speaks the minimum MCP HTTP handshake
    /// (`initialize` → `notifications/initialized` → `tools/list`) and advertises
    /// a single tool. The dotted `tool_name` exercises spec-valid names that
    /// must survive `<server>__<tool>` prefixing
    async fn start_mock_mcp_http_server(tool_name: &str) -> wiremock::MockServer {
        use wiremock::matchers::{body_partial_json, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method": "initialize"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Mcp-Session-Id", "sess-1")
                    .set_body_json(json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {
                            "protocolVersion": "2024-11-05",
                            "capabilities": {"tools": {}},
                            "serverInfo": {"name": "remote", "version": "0.1.0"}
                        }
                    })),
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
                "jsonrpc": "2.0",
                "id": 2,
                "result": {"tools": [{
                    "name": tool_name,
                    "description": "List domains",
                    "inputSchema": {"type": "object"}
                }]}
            })))
            .mount(&server)
            .await;
        server
    }

    /// `make_acp_test_config` plus an MCP server granted to `test-agent` via an
    /// `mcp_bundles` grant, pointed at `mock_uri`. `deferred` selects
    /// deferred-loading (`tool_search`) vs eager registration.
    fn make_mcp_granting_config(
        tmp: &tempfile::TempDir,
        mock_uri: String,
        deferred: bool,
    ) -> zeroclaw_config::schema::Config {
        use zeroclaw_config::schema::{McpBundleConfig, McpServerConfig, McpTransport};

        let mut config = make_acp_test_config(tmp);
        config.mcp.enabled = true;
        config.mcp.deferred_loading = deferred;
        config.mcp.servers = vec![McpServerConfig {
            name: "remote".into(),
            transport: McpTransport::Http,
            url: Some(mock_uri),
            ..Default::default()
        }];
        config.mcp_bundles.insert(
            "b1".into(),
            McpBundleConfig {
                servers: vec!["remote".into()],
                exclude: vec![],
            },
        );
        config
            .agents
            .get_mut("test-agent")
            .expect("test-agent must exist")
            .mcp_bundles = vec!["b1".into()];
        config
    }

    #[tokio::test]
    async fn chat_session_new_exposes_tool_search_in_deferred_mcp_mode() {
        let tmp = tempfile::TempDir::new().unwrap();
        let server = start_mock_mcp_http_server("domains.list").await;
        let config = make_mcp_granting_config(&tmp, server.uri(), true);
        let (dispatcher, sessions) = make_acp_test_dispatcher(config);

        let params = json!({
            "agent_alias": "test-agent",
            "chat_mode": "chat",
            "session_id": "chat-mcp-deferred-001"
        });
        let result = dispatcher.handle_session_new_for_test(&params).await;
        assert!(
            result.is_ok(),
            "session/new should succeed; got: {:?}",
            result.err()
        );

        let agent_arc = sessions
            .get_agent("chat-mcp-deferred-001")
            .await
            .expect("session must be registered after session/new");
        let agent = agent_arc.lock().await;
        let names = agent.tool_names();
        assert!(
            names.contains(&"tool_search"),
            "Chat session with deferred MCP must expose `tool_search`; tools: {names:?}"
        );
    }

    #[tokio::test]
    async fn chat_session_new_excluded_tool_search_is_dropped_in_deferred_mcp_mode() {
        let tmp = tempfile::TempDir::new().unwrap();
        let server = start_mock_mcp_http_server("domains.list").await;
        let mut config = make_mcp_granting_config(&tmp, server.uri(), true);
        // Deny the deferred-MCP discovery tool by name.
        config
            .risk_profiles
            .get_mut("test-profile")
            .expect("test-profile must exist")
            .excluded_tools = vec!["tool_search".into()];
        let (dispatcher, sessions) = make_acp_test_dispatcher(config);

        let params = json!({
            "agent_alias": "test-agent",
            "chat_mode": "chat",
            "session_id": "chat-mcp-deferred-excl-001"
        });
        let result = dispatcher.handle_session_new_for_test(&params).await;
        assert!(
            result.is_ok(),
            "session/new should succeed; got: {:?}",
            result.err()
        );

        let agent_arc = sessions
            .get_agent("chat-mcp-deferred-excl-001")
            .await
            .expect("session must be registered after session/new");
        let agent = agent_arc.lock().await;
        let names = agent.tool_names();
        assert!(
            !names.contains(&"tool_search"),
            "excluded_tools = [\"tool_search\"] must drop the deferred tool_search \
             wrapper (excluded_tools always subtracts); tools: {names:?}"
        );
        // The registry and prompt surfaces must move together: the system prompt
        // must not instruct the model to call a tool the policy just removed.
        let prompt = agent
            .system_prompt_for_test()
            .expect("system prompt must render");
        assert!(
            !prompt.contains("tool_search"),
            "excluded tool_search must not be advertised in the system prompt; prompt: {prompt}"
        );
        assert!(
            !prompt.contains("## Deferred Tools"),
            "excluded tool_search must suppress the deferred-tools section entirely; prompt: {prompt}"
        );
        assert!(
            !prompt.contains("remote__domains.list"),
            "excluded tool_search must not leak the deferred stub it would have activated; prompt: {prompt}"
        );
    }

    #[tokio::test]
    async fn chat_session_new_advertises_deferred_mcp_section_in_system_prompt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let server = start_mock_mcp_http_server("domains.list").await;
        let config = make_mcp_granting_config(&tmp, server.uri(), true);
        let (dispatcher, sessions) = make_acp_test_dispatcher(config);

        let params = json!({
            "agent_alias": "test-agent",
            "chat_mode": "chat",
            "session_id": "chat-mcp-deferred-prompt-001"
        });
        let result = dispatcher.handle_session_new_for_test(&params).await;
        assert!(
            result.is_ok(),
            "session/new should succeed; got: {:?}",
            result.err()
        );

        let agent_arc = sessions
            .get_agent("chat-mcp-deferred-prompt-001")
            .await
            .expect("session must be registered after session/new");
        let agent = agent_arc.lock().await;
        let prompt = agent
            .system_prompt_for_test()
            .expect("system prompt must render");
        assert!(
            prompt.contains("## Deferred Tools"),
            "system prompt must include the deferred-tools section; prompt: {prompt}"
        );
        assert!(
            prompt.contains("tool_search"),
            "system prompt must instruct the model to call `tool_search`; prompt: {prompt}"
        );
        assert!(
            prompt.contains("remote__domains.list"),
            "system prompt must advertise the dotted `<server>__<tool>` stub; prompt: {prompt}"
        );
    }

    #[tokio::test]
    async fn chat_session_new_tool_search_returns_granted_mcp_tool_in_deferred_mode() {
        let tmp = tempfile::TempDir::new().unwrap();
        let server = start_mock_mcp_http_server("domains.list").await;
        let config = make_mcp_granting_config(&tmp, server.uri(), true);
        let (dispatcher, sessions) = make_acp_test_dispatcher(config);

        let params = json!({
            "agent_alias": "test-agent",
            "chat_mode": "chat",
            "session_id": "chat-mcp-deferred-search-001"
        });
        let result = dispatcher.handle_session_new_for_test(&params).await;
        assert!(
            result.is_ok(),
            "session/new should succeed; got: {:?}",
            result.err()
        );

        let agent_arc = sessions
            .get_agent("chat-mcp-deferred-search-001")
            .await
            .expect("session must be registered after session/new");
        let agent = agent_arc.lock().await;

        let tool_result = agent
            .execute_tool_for_test("tool_search", json!({ "query": "domains" }))
            .await
            .expect("deferred Chat session must expose `tool_search`")
            .expect("tool_search must execute without error");

        assert!(
            tool_result.success,
            "tool_search should succeed; error: {:?}",
            tool_result.error
        );
        assert!(
            tool_result.output.contains("remote__domains.list"),
            "tool_search must resolve the granted `<server>__<tool>` stub, not just \
             be present; output: {}",
            tool_result.output
        );
    }

    #[tokio::test]
    async fn chat_session_new_exposes_prefixed_mcp_tool_in_eager_mode() {
        let tmp = tempfile::TempDir::new().unwrap();
        let server = start_mock_mcp_http_server("domains.list").await;
        let config = make_mcp_granting_config(&tmp, server.uri(), false);
        let (dispatcher, sessions) = make_acp_test_dispatcher(config);

        let params = json!({
            "agent_alias": "test-agent",
            "chat_mode": "chat",
            "session_id": "chat-mcp-eager-001"
        });
        let result = dispatcher.handle_session_new_for_test(&params).await;
        assert!(
            result.is_ok(),
            "session/new should succeed; got: {:?}",
            result.err()
        );

        let agent_arc = sessions
            .get_agent("chat-mcp-eager-001")
            .await
            .expect("session must be registered after session/new");
        let agent = agent_arc.lock().await;
        let names = agent.tool_names();
        // Eager mode registers the MCP tool directly; the dotted suffix keeps
        // its `<server>__<tool>` prefix.
        assert!(
            names.contains(&"remote__domains.list"),
            "Chat session with eager MCP must expose `remote__domains.list`; tools: {names:?}"
        );
    }

    #[tokio::test]
    async fn chat_session_new_omits_mcp_tools_when_agent_has_no_bundles_deferred() {
        use zeroclaw_config::schema::AliasedAgentConfig;

        let tmp = tempfile::TempDir::new().unwrap();
        let server = start_mock_mcp_http_server("domains.list").await;
        let mut config = make_mcp_granting_config(&tmp, server.uri(), true);

        // Add a SECOND agent with no `mcp_bundles`. Reuse `test-agent`'s
        // model_provider/risk_profile so the agent is fully constructible
        // without touching providers/risk_profiles.
        let template = config
            .agents
            .get("test-agent")
            .cloned()
            .expect("test-agent must exist in make_mcp_granting_config");
        config.agents.insert(
            "unscoped-agent".to_string(),
            AliasedAgentConfig {
                enabled: true,
                model_provider: template.model_provider.clone(),
                risk_profile: template.risk_profile.clone(),
                mcp_bundles: Vec::new(), // explicit: no grant
                ..AliasedAgentConfig::default()
            },
        );

        let (dispatcher, sessions) = make_acp_test_dispatcher(config);

        let params = json!({
            "agent_alias": "unscoped-agent",
            "chat_mode": "chat",
            "session_id": "chat-mcp-unscoped-deferred-001"
        });
        let result = dispatcher.handle_session_new_for_test(&params).await;
        assert!(
            result.is_ok(),
            "session/new for an unscoped agent should still succeed; got: {:?}",
            result.err()
        );

        let agent_arc = sessions
            .get_agent("chat-mcp-unscoped-deferred-001")
            .await
            .expect("session must be registered after session/new");
        let agent = agent_arc.lock().await;
        let names = agent.tool_names();
        assert!(
            !names.contains(&"tool_search"),
            "Unscoped agent must NOT expose `tool_search` in deferred mode \
             (mcp_bundles is empty -> no MCP connection -> no deferred \
             registry -> no tool_search). Tools were: {names:?}"
        );
        // And, defensively, no prefixed MCP tool either.
        assert!(
            !names.iter().any(|n| n.contains("__")),
            "Unscoped agent must expose zero `<server>__<tool>` MCP tools; \
             tools were: {names:?}"
        );
    }

    #[tokio::test]
    async fn chat_session_new_omits_mcp_tools_when_agent_has_no_bundles_eager() {
        use zeroclaw_config::schema::AliasedAgentConfig;

        let tmp = tempfile::TempDir::new().unwrap();
        let server = start_mock_mcp_http_server("domains.list").await;
        let mut config = make_mcp_granting_config(&tmp, server.uri(), false);

        let template = config
            .agents
            .get("test-agent")
            .cloned()
            .expect("test-agent must exist in make_mcp_granting_config");
        config.agents.insert(
            "unscoped-agent".to_string(),
            AliasedAgentConfig {
                enabled: true,
                model_provider: template.model_provider.clone(),
                risk_profile: template.risk_profile.clone(),
                mcp_bundles: Vec::new(),
                ..AliasedAgentConfig::default()
            },
        );

        let (dispatcher, sessions) = make_acp_test_dispatcher(config);

        let params = json!({
            "agent_alias": "unscoped-agent",
            "chat_mode": "chat",
            "session_id": "chat-mcp-unscoped-eager-001"
        });
        let result = dispatcher.handle_session_new_for_test(&params).await;
        assert!(
            result.is_ok(),
            "session/new for an unscoped agent should still succeed; got: {:?}",
            result.err()
        );

        let agent_arc = sessions
            .get_agent("chat-mcp-unscoped-eager-001")
            .await
            .expect("session must be registered after session/new");
        let agent = agent_arc.lock().await;
        let names = agent.tool_names();
        assert!(
            !names.contains(&"remote__domains.list"),
            "Unscoped agent must NOT expose `remote__domains.list` in \
             eager mode (mcp_bundles is empty -> no MCP connection -> \
             no eager registration). Tools were: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.starts_with("remote__")),
            "No `remote__*` tool may leak to an unscoped agent; tools \
             were: {names:?}"
        );
    }

    #[tokio::test]
    async fn acp_session_new_skips_mcp_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let server = start_mock_mcp_http_server("domains.list").await;
        // Deferred mode would register `tool_search` for a Chat session; an ACP
        // session must skip MCP init entirely regardless. ACP `session/new`
        // requires the persistence dispatcher (it touches the ACP store).
        let config = make_mcp_granting_config(&tmp, server.uri(), true);
        let data_dir = config.data_dir.clone();
        let (dispatcher, sessions, _chat_backend, _acp_store) =
            make_persistence_test_dispatcher(config, &data_dir);

        let params = json!({
            "agent_alias": "test-agent",
            "chat_mode": "acp",
            "session_id": "acp-mcp-001"
        });
        let result = dispatcher.handle_session_new_for_test(&params).await;
        assert!(
            result.is_ok(),
            "session/new should succeed; got: {:?}",
            result.err()
        );

        let agent_arc = sessions
            .get_agent("acp-mcp-001")
            .await
            .expect("session must be registered after session/new");
        let agent = agent_arc.lock().await;
        let names = agent.tool_names();
        assert!(
            !names.contains(&"tool_search") && !names.contains(&"remote__domains.list"),
            "ACP session must skip MCP init (no `tool_search`, no MCP tools); tools: {names:?}"
        );
    }

    /// Blocking regression: a fresh remote RPC client that reaches
    /// `logs/subscribe` (an unauthenticated surface — a new WSS client can
    /// `initialize` and subscribe without the gateway bearer) must never
    /// receive a pairing credential off the shared broadcast bus, while
    /// ordinary log frames still forward with the internal marker stripped.
    #[tokio::test]
    async fn logs_subscribe_fails_closed_on_pairing_credentials() {
        use zeroclaw_infra::session_queue::SessionActorQueue;
        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let config = zeroclaw_config::schema::Config::default();
        let (event_tx, _rx0) = tokio::sync::broadcast::channel(16);
        let ctx = RpcContext::minimal_with_event_tx(config, sessions, event_tx.clone());
        let (writer_tx, mut writer_rx) = tokio::sync::mpsc::channel::<String>(64);
        let d = RpcDispatcher::new(ctx, writer_tx, "remote:wss=1,uid=anon".into());

        assert!(
            d.handle_logs_subscribe().await.is_ok(),
            "a fresh client should be able to subscribe"
        );

        // Marker-stamped credential frame (as `record_event` stamps a QR login
        // event) followed by an ordinary lifecycle frame.
        let credential = serde_json::json!({
            "source": "observability",
            "attributes": { "login": { "state": "qr", "qr_payload": "SECRET-QR-PAYLOAD" } },
            zeroclaw_log::EPHEMERAL_BROADCAST_MARKER: true,
        });
        let plain = serde_json::json!({
            "source": "observability",
            "type": "tool_call",
            "tool": "SENTINEL-LIVE",
        });
        event_tx.send(credential).expect("send credential frame");
        event_tx.send(plain).expect("send plain frame");

        // Collect forwarded notifications until the sentinel arrives or the
        // budget elapses.
        let mut seen = String::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, writer_rx.recv()).await {
                Ok(Some(msg)) => {
                    let hit = msg.contains("SENTINEL-LIVE");
                    seen.push_str(&msg);
                    if hit {
                        break;
                    }
                }
                _ => break,
            }
        }

        assert!(
            seen.contains("SENTINEL-LIVE"),
            "an ordinary lifecycle frame must still forward over logs/subscribe: {seen:?}"
        );
        assert!(
            !seen.contains("SECRET-QR-PAYLOAD"),
            "a remote RPC client must never obtain a pairing credential via logs/subscribe: {seen:?}"
        );
        assert!(
            !seen.contains(zeroclaw_log::EPHEMERAL_BROADCAST_MARKER),
            "the internal fail-closed marker must be stripped from forwarded frames: {seen:?}"
        );
    }

    fn make_cost_query_test_dispatcher(data_dir: &std::path::Path) -> RpcDispatcher {
        use zeroclaw_infra::session_queue::SessionActorQueue;
        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let tracker = Arc::new(
            zeroclaw_config::cost::tracker::CostTracker::new(
                zeroclaw_config::schema::CostConfig {
                    enabled: true,
                    ..Default::default()
                },
                data_dir,
            )
            .unwrap(),
        );
        let config = zeroclaw_config::schema::Config {
            data_dir: data_dir.to_path_buf(),
            ..Default::default()
        };
        let ctx = RpcContext::minimal_with_cost_tracker(config, sessions, tracker);
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        RpcDispatcher::new(ctx, tx, "test-peer-costquery:pid=1".into())
    }

    #[test]
    fn cost_query_invalid_rfc3339_bound_is_invalid_params() {
        let tmp = tempfile::TempDir::new().unwrap();
        let d = make_cost_query_test_dispatcher(tmp.path());
        let err = d
            .handle_cost_query(&serde_json::json!({ "from": "not-a-real-date" }))
            .expect_err("an invalid RFC3339 bound must be rejected");
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(err.message.contains("invalid date"), "got: {}", err.message);
    }

    #[test]
    fn cost_query_valid_bounds_reach_in_bounds_summary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let d = make_cost_query_test_dispatcher(tmp.path());
        let res = d.handle_cost_query(&serde_json::json!({
            "from": "2026-01-01T00:00:00Z",
            "to": "2026-07-01T00:00:00Z"
        }));
        assert!(
            res.is_ok(),
            "a valid bounded cost/query must reach get_summary_in_bounds: {res:?}"
        );
    }

    fn make_cost_test_dispatcher(data_dir: &std::path::Path) -> RpcDispatcher {
        use zeroclaw_infra::session_queue::SessionActorQueue;
        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let config = zeroclaw_config::schema::Config {
            data_dir: data_dir.to_path_buf(),
            ..Default::default()
        };
        let ctx = RpcContext::minimal(config, sessions);
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        RpcDispatcher::new(ctx, tx, "test-peer-cost:pid=1".into())
    }

    // cost/org: null only for a genuinely-absent snapshot; any other read failure
    // (unreadable file, a directory at the path, bad JSON) surfaces as an error so a
    // broken deployment is not mistaken for a vanilla one. (Audacity88/JordanTheJet,)
    #[test]
    fn cost_org_absent_returns_null() {
        let tmp = tempfile::TempDir::new().unwrap();
        let d = make_cost_test_dispatcher(tmp.path());
        assert_eq!(d.handle_cost_org().unwrap(), serde_json::Value::Null);
    }

    #[test]
    fn cost_org_present_returns_snapshot_verbatim() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("org_cost.json"),
            r#"{"org":"acme","billed_usd":12.5}"#,
        )
        .unwrap();
        let d = make_cost_test_dispatcher(tmp.path());
        let v = d.handle_cost_org().unwrap();
        assert_eq!(v["org"], serde_json::json!("acme"));
        assert_eq!(v["billed_usd"], serde_json::json!(12.5));
    }

    #[test]
    fn cost_org_invalid_json_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("org_cost.json"), "not valid json{").unwrap();
        let d = make_cost_test_dispatcher(tmp.path());
        assert!(d.handle_cost_org().is_err());
    }

    // The `sops/trigger-sources` RPC response must carry the full
    // ordered `SopTriggerSource` walk so authoring surfaces (web + zerocode)
    // render the picker from the backend list instead of reconstructing it.
    // Any new trigger source variant appears here automatically; a surface that
    // reconstructs its own list would drift while this guard would not, so the
    // contract is pinned at the transport boundary every surface reads from.
    #[test]
    fn sops_trigger_sources_rpc_carries_full_trigger_source_walk() {
        use crate::sop::types::SopTriggerSource;
        use strum::IntoEnumIterator;

        let tmp = tempfile::TempDir::new().unwrap();
        let d = make_cost_test_dispatcher(tmp.path());
        let value = d
            .handle_sops_trigger_sources()
            .expect("sops/trigger-sources must succeed on a default config");
        let sources: Vec<String> = value
            .get("sources")
            .and_then(|s| serde_json::from_value(s.clone()).ok())
            .expect("response must carry a `sources` array");
        let expected: Vec<String> = SopTriggerSource::iter().map(|s| s.to_string()).collect();
        assert_eq!(
            sources, expected,
            "RPC `sources` must equal the complete SopTriggerSource walk so \
             surfaces cannot drift by reconstructing their own list"
        );
    }

    #[tokio::test]
    async fn sops_run_rejects_malformed_payload_before_engine() {
        // Payload validation runs before the engine lookup, so a malformed JSON
        // string is rejected with INVALID_PARAMS even on a dispatcher with no
        // SOP engine wired. This pins the "surface a clear error on malformed
        // JSON rather than failing the run opaquely" contract.
        let tmp = tempfile::TempDir::new().unwrap();
        let d = make_cost_test_dispatcher(tmp.path());
        let err = d
            .handle_sops_run(&serde_json::json!({ "name": "any", "payload": "{not json" }))
            .await
            .expect_err("malformed payload must be rejected");
        assert_eq!(err.code, INVALID_PARAMS);
    }

    #[tokio::test]
    async fn sops_run_requires_engine() {
        // A well-formed request against a dispatcher with no SOP engine reports
        // the subsystem as unavailable rather than panicking or silently
        // succeeding.
        let tmp = tempfile::TempDir::new().unwrap();
        let d = make_cost_test_dispatcher(tmp.path());
        let err = d
            .handle_sops_run(&serde_json::json!({ "name": "any", "payload": "{\"k\":1}" }))
            .await
            .expect_err("missing engine must error");
        assert_eq!(err.code, INTERNAL_ERROR);
    }

    #[test]
    fn sops_runs_requires_engine() {
        // Listing runs against a dispatcher with no SOP engine reports the
        // subsystem as unavailable rather than returning a bogus empty list.
        let tmp = tempfile::TempDir::new().unwrap();
        let d = make_cost_test_dispatcher(tmp.path());
        let err = d
            .handle_sops_runs(&serde_json::json!({}))
            .expect_err("missing engine must error");
        assert_eq!(err.code, INTERNAL_ERROR);
    }

    #[test]
    fn sops_runs_accepts_optional_sop_filter() {
        // The request parses with or without the `sop` field; both fail only on
        // the engine guard, not on param parsing.
        let tmp = tempfile::TempDir::new().unwrap();
        let d = make_cost_test_dispatcher(tmp.path());
        let err = d
            .handle_sops_runs(&serde_json::json!({ "sop": "some-sop" }))
            .expect_err("missing engine must error");
        assert_eq!(err.code, INTERNAL_ERROR);
    }

    fn make_checkpoint_rpc_dispatcher(
        quorum: u32,
        members: &[&str],
        tui_id: &str,
    ) -> (
        RpcDispatcher,
        Arc<std::sync::Mutex<crate::sop::SopEngine>>,
        String,
        tempfile::TempDir,
    ) {
        use crate::sop::types::{
            Sop, SopAdmissionPolicy, SopEvent, SopExecutionMode, SopPriority, SopRunAction,
            SopStep, SopStepKind, SopTrigger, SopTriggerSource,
        };
        use std::collections::HashMap;
        use zeroclaw_config::schema::{
            ApprovalGroupConfig, ApprovalPolicyConfig, Config, SopApprovalConfig,
        };
        use zeroclaw_infra::session_queue::SessionActorQueue;

        let temp = tempfile::TempDir::new().unwrap();
        let sops_dir = temp.path().join("sops");
        let sop = Sop {
            name: "rpc-checkpoint".into(),
            description: "checkpoint RPC authorization test".into(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Deterministic,
            triggers: vec![SopTrigger::Manual],
            steps: vec![
                SopStep {
                    number: 1,
                    title: "authorize".into(),
                    kind: SopStepKind::Checkpoint,
                    policy: Some("prod".into()),
                    ..SopStep::default()
                },
                SopStep {
                    number: 2,
                    title: "continue".into(),
                    kind: SopStepKind::Execute,
                    ..SopStep::default()
                },
            ],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
            admission_policy: SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
            agent: None,
        };
        crate::sop::save_sop(&sops_dir, &sop).unwrap();
        let mut groups = HashMap::new();
        groups.insert(
            "release".to_string(),
            ApprovalGroupConfig {
                members: members.iter().map(|member| (*member).to_string()).collect(),
            },
        );
        let mut policies = HashMap::new();
        policies.insert(
            "prod".to_string(),
            ApprovalPolicyConfig {
                required_group: Some("release".into()),
                quorum,
                request_route: None,
                escalation_route: None,
            },
        );
        let mut config = Config::default();
        config.sop.sops_dir = Some(sops_dir.to_string_lossy().into_owned());
        config.sop.approval = SopApprovalConfig { groups, policies };

        let mut engine = crate::sop::SopEngine::new(config.sop.clone())
            .with_approval_broker(Arc::new(crate::sop::approval::ApprovalBroker::disabled()));
        engine.set_sops_for_test(vec![sop]);
        let action = engine
            .start_run(
                "rpc-checkpoint",
                SopEvent {
                    source: SopTriggerSource::Manual,
                    topic: None,
                    payload: None,
                    timestamp: crate::sop::engine::now_iso8601(),
                },
            )
            .unwrap();
        let run_id = match action {
            SopRunAction::CheckpointWait { run_id, .. } => run_id,
            other => panic!("expected checkpoint wait, got {other:?}"),
        };
        let engine = Arc::new(std::sync::Mutex::new(engine));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(
            16,
            Arc::new(SessionActorQueue::new(4, 10, 60)),
        ));
        let ctx = RpcContext::minimal_with_sop_engine(config, sessions, Arc::clone(&engine));
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let mut dispatcher = RpcDispatcher::new(ctx, tx, "local:test".into());
        dispatcher.set_tui_id_for_test(Some(tui_id.to_string()));
        (dispatcher, engine, run_id, temp)
    }

    #[tokio::test]
    async fn sops_decide_rpc_enforces_checkpoint_membership_and_quorum() {
        use crate::sop::types::SopRunStatus;

        let (unauthorized, engine, run_id, _temp) =
            make_checkpoint_rpc_dispatcher(1, &["cli:ZeroClawOperator"], "ZeroClawAgent");
        let error = unauthorized
            .handle_sops_decide(&json!({
                "name": "rpc-checkpoint",
                "run_id": run_id.clone(),
                "decision": "approve",
            }))
            .await
            .expect_err("unauthorized RPC principal must be rejected");
        assert_eq!(error.code, AUTH_REQUIRED);
        assert_eq!(
            engine
                .lock()
                .unwrap()
                .get_run(&run_id)
                .map(|run| run.status),
            Some(SopRunStatus::PausedCheckpoint)
        );

        let (pending, engine, run_id, _temp) = make_checkpoint_rpc_dispatcher(
            2,
            &["cli:ZeroClawOperator", "cli:ZeroClawMaintainer"],
            "ZeroClawOperator",
        );
        pending
            .handle_sops_decide(&json!({
                "name": "rpc-checkpoint",
                "run_id": run_id.clone(),
                "decision": "approve",
            }))
            .await
            .expect("an authorized first vote returns the still-parked overlay");
        assert_eq!(
            engine
                .lock()
                .unwrap()
                .get_run(&run_id)
                .map(|run| run.status),
            Some(SopRunStatus::PausedCheckpoint)
        );
    }

    #[tokio::test]
    async fn sops_decide_drives_resumed_execute_step() {
        use crate::sop::{
            Sop, SopEvent, SopExecutionMode, SopPriority, SopRunAction, SopRunStatus, SopStep,
            SopStepKind, SopTrigger, SopTriggerSource,
        };
        use std::sync::{Arc, Mutex};
        use zeroclaw_config::schema::{Config, SopConfig};
        use zeroclaw_infra::session_queue::SessionActorQueue;

        let tmp = tempfile::TempDir::new().expect("temporary SOP directory");
        let sops_dir = tmp.path().join("sops");
        let sop_config = SopConfig {
            sops_dir: Some(sops_dir.to_string_lossy().into_owned()),
            ..SopConfig::default()
        };
        let config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            sop: sop_config.clone(),
            ..Config::default()
        };
        let sop = Sop {
            name: "rpc-resumed-execute".to_string(),
            description: "RPC resume driver regression".to_string(),
            version: "1.0.0".to_string(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Supervised,
            triggers: vec![SopTrigger::Manual],
            steps: vec![SopStep {
                number: 1,
                title: "Execute after approval".to_string(),
                kind: SopStepKind::Execute,
                ..SopStep::default()
            }],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
            agent: None,
            admission_policy: crate::sop::types::SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
        };
        crate::sop::save_sop(&sops_dir, &sop).expect("save temporary SOP");

        let mut engine = crate::sop::SopEngine::new(sop_config);
        engine.reload(tmp.path());
        let engine = Arc::new(Mutex::new(engine));
        let run_id = {
            let mut guard = engine.lock().expect("engine lock");
            let action = guard
                .start_run(
                    "rpc-resumed-execute",
                    SopEvent {
                        source: SopTriggerSource::Manual,
                        topic: None,
                        payload: None,
                        timestamp: crate::sop::engine::now_iso8601(),
                    },
                )
                .expect("start approval-gated SOP");
            let SopRunAction::WaitApproval { run_id, .. } = action else {
                panic!("supervised Execute step must park for approval: {action:?}");
            };
            run_id
        };

        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let ctx = RpcContext::minimal_with_sop_engine(config, sessions, Arc::clone(&engine));
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let dispatcher = RpcDispatcher::new(ctx, tx, "test-peer-rpc:pid=1".to_string());

        dispatcher
            .handle_sops_decide(&serde_json::json!({
                "name": "rpc-resumed-execute",
                "run_id": run_id,
                "decision": "approve",
            }))
            .await
            .expect("RPC approval must accept the parked run");

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let status = engine
                    .lock()
                    .expect("engine lock")
                    .get_run(&run_id)
                    .map(|run| run.status);
                if status == Some(SopRunStatus::Failed) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("RPC approval must schedule the resumed ExecuteStep");
    }

    #[tokio::test]
    async fn sops_decide_rejects_approval_mode_rejection() {
        use crate::sop::{
            Sop, SopEvent, SopExecutionMode, SopPriority, SopRunAction, SopRunStatus, SopStep,
            SopStepKind, SopTrigger, SopTriggerSource,
        };
        use std::sync::{Arc, Mutex};
        use zeroclaw_config::schema::{ApprovalMode, Config, SopConfig};
        use zeroclaw_infra::session_queue::SessionActorQueue;

        fn dispatcher_with_sop_engine(
            config: Config,
            engine: Arc<Mutex<crate::sop::SopEngine>>,
        ) -> RpcDispatcher {
            let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
            let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
            let ctx = RpcContext::minimal_with_sop_engine(config, sessions, engine);
            let (tx, _rx) = tokio::sync::mpsc::channel(64);
            let mut dispatcher = RpcDispatcher::new(ctx, tx, "test-peer-rpc:pid=1".to_string());
            dispatcher.set_tui_id_for_test(Some("alice".to_string()));
            dispatcher
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let sops_dir = tmp.path().join("sops");
        let sop_config = SopConfig {
            sops_dir: Some(sops_dir.to_string_lossy().into_owned()),
            default_execution_mode: "deterministic".to_string(),
            approval_mode: ApprovalMode::AgentTool,
            ..SopConfig::default()
        };
        let config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            sop: sop_config.clone(),
            ..Config::default()
        };

        let sop = Sop {
            name: "rpc-agent-tool-only".to_string(),
            description: "rpc approval-mode checkpoint".to_string(),
            version: "1.0.0".to_string(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Deterministic,
            triggers: vec![SopTrigger::Manual],
            steps: vec![SopStep {
                number: 1,
                title: "Policy gate".to_string(),
                kind: SopStepKind::Checkpoint,
                ..SopStep::default()
            }],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: true,
            agent: None,
            admission_policy: crate::sop::types::SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
        };
        crate::sop::save_sop(&sops_dir, &sop).expect("save temp SOP");

        let mut engine = crate::sop::SopEngine::new(sop_config);
        engine.reload(tmp.path());
        let engine = Arc::new(Mutex::new(engine));
        let run_id = {
            let mut guard = engine.lock().expect("engine lock");
            let action = guard
                .start_run(
                    "rpc-agent-tool-only",
                    SopEvent {
                        source: SopTriggerSource::Manual,
                        topic: None,
                        payload: None,
                        timestamp: crate::sop::engine::now_iso8601(),
                    },
                )
                .expect("start approval-mode SOP");
            let SopRunAction::CheckpointWait { run_id, .. } = action else {
                panic!("approval-mode SOP must park at checkpoint, got {action:?}");
            };
            run_id
        };

        let dispatcher = dispatcher_with_sop_engine(config, Arc::clone(&engine));
        let err = dispatcher
            .handle_sops_decide(&serde_json::json!({
                "name": "rpc-agent-tool-only",
                "run_id": run_id,
                "decision": "approve",
            }))
            .await
            .expect_err("RPC principal must be rejected by approval_mode=agent_tool");
        assert_eq!(err.code, AUTH_REQUIRED);
        assert!(
            err.message.contains(&crate::i18n::get_required_cli_string(
                "sop-rpc-decision-unauthorized",
            )),
            "approval_mode rejection must surface, got: {}",
            err.message
        );
        let guard = engine.lock().expect("engine lock");
        assert_eq!(
            guard.get_run(&run_id).expect("run still active").status,
            SopRunStatus::PausedCheckpoint
        );
        assert!(
            !guard
                .run_events(&run_id)
                .unwrap_or_default()
                .iter()
                .any(|event| event.kind == "gate_resolved"),
            "rejected RPC decision must not append a gate_resolved row"
        );
    }

    #[tokio::test]
    async fn sops_decide_rejects_run_id_from_different_sop_before_broker_resolution() {
        use crate::sop::{
            Sop, SopEvent, SopExecutionMode, SopPriority, SopRunAction, SopRunStatus, SopStep,
            SopStepKind, SopTrigger, SopTriggerSource,
        };
        use std::sync::{Arc, Mutex};
        use zeroclaw_config::schema::{Config, SopConfig};
        use zeroclaw_infra::session_queue::SessionActorQueue;

        fn dispatcher_with_sop_engine(
            config: Config,
            engine: Arc<Mutex<crate::sop::SopEngine>>,
        ) -> RpcDispatcher {
            let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
            let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
            let ctx = RpcContext::minimal_with_sop_engine(config, sessions, engine);
            let (tx, _rx) = tokio::sync::mpsc::channel(64);
            let mut dispatcher = RpcDispatcher::new(ctx, tx, "test-peer-rpc:pid=1".to_string());
            dispatcher.set_tui_id_for_test(Some("alice".to_string()));
            dispatcher
        }

        fn checkpoint_sop(name: &str) -> Sop {
            Sop {
                name: name.to_string(),
                description: format!("{name} checkpoint"),
                version: "1.0.0".to_string(),
                priority: SopPriority::Normal,
                execution_mode: SopExecutionMode::Deterministic,
                triggers: vec![SopTrigger::Manual],
                steps: vec![SopStep {
                    number: 1,
                    title: "Gate".to_string(),
                    kind: SopStepKind::Checkpoint,
                    ..SopStep::default()
                }],
                cooldown_secs: 0,
                max_concurrent: 1,
                location: None,
                deterministic: true,
                agent: None,
                admission_policy: crate::sop::types::SopAdmissionPolicy::Parallel,
                max_pending_approvals: 0,
            }
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let sops_dir = tmp.path().join("sops");
        let sop_config = SopConfig {
            sops_dir: Some(sops_dir.to_string_lossy().into_owned()),
            default_execution_mode: "deterministic".to_string(),
            ..SopConfig::default()
        };
        let config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            sop: sop_config.clone(),
            ..Config::default()
        };

        crate::sop::save_sop(&sops_dir, &checkpoint_sop("rpc-a")).expect("save rpc-a");
        crate::sop::save_sop(&sops_dir, &checkpoint_sop("rpc-b")).expect("save rpc-b");

        let mut engine = crate::sop::SopEngine::new(sop_config);
        engine.reload(tmp.path());
        assert_eq!(engine.sops().len(), 2, "both temp SOPs should load");
        let engine = Arc::new(Mutex::new(engine));

        let run_id = {
            let mut guard = engine.lock().expect("engine lock");
            let action = guard
                .start_run(
                    "rpc-b",
                    SopEvent {
                        source: SopTriggerSource::Manual,
                        topic: None,
                        payload: None,
                        timestamp: crate::sop::engine::now_iso8601(),
                    },
                )
                .expect("start rpc-b SOP");
            let SopRunAction::CheckpointWait { run_id, .. } = action else {
                panic!("rpc-b must park at checkpoint, got {action:?}");
            };
            run_id
        };

        let dispatcher = dispatcher_with_sop_engine(config, Arc::clone(&engine));
        let err = dispatcher
            .handle_sops_decide(&serde_json::json!({
                "name": "rpc-a",
                "run_id": run_id,
                "decision": "approve",
            }))
            .await
            .expect_err("mismatched name/run_id must be rejected before broker resolution");
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(
            err.message.contains("belongs to SOP 'rpc-b', not 'rpc-a'"),
            "mismatch rejection must name both SOPs, got: {}",
            err.message
        );

        let guard = engine.lock().expect("engine lock");
        let run = guard.get_run(&run_id).expect("rpc-b run still active");
        assert_eq!(run.sop_name, "rpc-b");
        assert_eq!(run.status, SopRunStatus::PausedCheckpoint);
        assert!(
            !guard
                .run_events(&run_id)
                .unwrap_or_default()
                .iter()
                .any(|event| event.kind == "gate_resolved"),
            "mismatched RPC decision must not append a gate_resolved row"
        );
    }

    #[test]
    fn cost_org_unreadable_non_notfound_errors() {
        // A directory at the snapshot path produces a non-NotFound read error; it must
        // surface as an RPC error, not masquerade as "no snapshot configured".
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("org_cost.json")).unwrap();
        let d = make_cost_test_dispatcher(tmp.path());
        assert!(
            d.handle_cost_org().is_err(),
            "an unreadable snapshot must not be reported as absent"
        );
    }

    fn make_approval_test_dispatcher() -> RpcDispatcher {
        use zeroclaw_infra::session_queue::SessionActorQueue;
        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let ctx = RpcContext::minimal(zeroclaw_config::schema::Config::default(), sessions);
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        RpcDispatcher::new(ctx, tx, "test-peer-approval:pid=1".into())
    }

    fn make_response_frame_test_dispatcher() -> (RpcDispatcher, tokio::sync::mpsc::Receiver<String>)
    {
        use zeroclaw_infra::session_queue::SessionActorQueue;
        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let ctx = RpcContext::minimal(zeroclaw_config::schema::Config::default(), sessions);
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        (
            RpcDispatcher::new(ctx, tx, "test-peer-response:pid=1".into()),
            rx,
        )
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
    fn doctor_run_method_is_registered() {
        assert_eq!(Method::from_wire("doctor/run"), Some(Method::DoctorRun));
        assert_eq!(Method::DoctorRun.wire_name(), "doctor/run");
    }

    #[tokio::test]
    async fn process_line_routes_standard_response_frames_to_pending_outbound_request() {
        let (mut dispatcher, mut write_rx) = make_response_frame_test_dispatcher();
        let rpc = Arc::clone(&dispatcher.rpc);

        let pending = zeroclaw_spawn::spawn!(async move {
            rpc.request("client/ping", json!({"ping": true})).await
        });

        let outbound = tokio::time::timeout(std::time::Duration::from_secs(1), write_rx.recv())
            .await
            .expect("outbound request should be written")
            .expect("writer channel should stay open");
        let outbound_frame: Value = serde_json::from_str(&outbound).unwrap();
        assert_eq!(outbound_frame["method"], "client/ping");
        let id = outbound_frame["id"].clone();

        dispatcher
            .process_line_for_test(
                &json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {"ok": true}
                })
                .to_string(),
            )
            .await;

        let result = tokio::time::timeout(std::time::Duration::from_secs(1), pending)
            .await
            .expect("standard response frame should release pending outbound request")
            .expect("outbound request task should not panic")
            .expect("response should be successful");
        assert_eq!(result, json!({"ok": true}));
        assert!(matches!(
            write_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn process_line_routes_standard_error_frames_to_pending_outbound_request() {
        let (mut dispatcher, mut write_rx) = make_response_frame_test_dispatcher();
        let rpc = Arc::clone(&dispatcher.rpc);

        let pending =
            zeroclaw_spawn::spawn!(async move { rpc.request("client/fail", json!({})).await });

        let outbound = tokio::time::timeout(std::time::Duration::from_secs(1), write_rx.recv())
            .await
            .expect("outbound request should be written")
            .expect("writer channel should stay open");
        let outbound_frame: Value = serde_json::from_str(&outbound).unwrap();
        assert_eq!(outbound_frame["method"], "client/fail");
        let id = outbound_frame["id"].clone();

        dispatcher
            .process_line_for_test(
                &json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32603, "message": "client failed"}
                })
                .to_string(),
            )
            .await;

        let err = tokio::time::timeout(std::time::Duration::from_secs(1), pending)
            .await
            .expect("standard error frame should release pending outbound request")
            .expect("outbound request task should not panic")
            .expect_err("response should be an error");
        assert_eq!(err.code, INTERNAL_ERROR);
        assert_eq!(err.message, "client failed");
        assert!(matches!(
            write_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn config_reload_shuts_down_gateway_before_daemon_reload() {
        use zeroclaw_infra::session_queue::SessionActorQueue;

        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let (gateway_shutdown_tx, mut gateway_shutdown_rx) = tokio::sync::watch::channel(false);
        let (reload_tx, mut reload_rx) = tokio::sync::watch::channel(false);
        let ctx = RpcContext::minimal_with_reload_controls(
            zeroclaw_config::schema::Config::default(),
            sessions,
            Some(gateway_shutdown_tx),
            Some(reload_tx),
        );
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let dispatcher = RpcDispatcher::new(ctx, tx, "test-peer-reload:pid=1".into());

        let result = dispatcher.handle_config_reload();
        assert!(
            result.is_ok(),
            "config/reload should accept reload-capable contexts"
        );

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            gateway_shutdown_rx.changed(),
        )
        .await
        .expect("gateway shutdown must be signalled before daemon reload")
        .expect("gateway shutdown sender should stay alive");
        assert!(*gateway_shutdown_rx.borrow_and_update());
        assert!(
            !*reload_rx.borrow(),
            "daemon reload must wait until the gateway listener has been asked to shut down"
        );

        tokio::time::timeout(std::time::Duration::from_secs(1), reload_rx.changed())
            .await
            .expect("daemon reload should follow gateway shutdown")
            .expect("reload sender should stay alive");
        assert!(*reload_rx.borrow_and_update());
    }

    #[tokio::test]
    async fn quickstart_apply_shuts_down_gateway_before_daemon_reload() {
        use zeroclaw_config::presets::{
            AgentIdentity, BuilderSubmission, MemoryChoice, ModelProviderChoice, SelectorChoice,
        };
        use zeroclaw_infra::session_queue::SessionActorQueue;

        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();

        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let (gateway_shutdown_tx, mut gateway_shutdown_rx) = tokio::sync::watch::channel(false);
        let (reload_tx, mut reload_rx) = tokio::sync::watch::channel(false);
        let ctx = RpcContext::minimal_with_reload_controls(
            config,
            sessions,
            Some(gateway_shutdown_tx),
            Some(reload_tx),
        );
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let dispatcher = RpcDispatcher::new(ctx, tx, "test-peer-quickstart-reload:pid=1".into());

        let submission = BuilderSubmission {
            model_provider: SelectorChoice::Fresh(ModelProviderChoice {
                provider_type: "anthropic".into(),
                alias: "anthropic".into(),
                model: "claude-sonnet-4-5".into(),
                fields: std::collections::HashMap::from([(
                    "api_key".to_string(),
                    "sk-test".to_string(),
                )]),
            }),
            risk_profile: SelectorChoice::Fresh("balanced".into()),
            runtime_profile: SelectorChoice::Fresh("balanced".into()),
            memory: SelectorChoice::Fresh(MemoryChoice::Sqlite),
            channels: vec![],
            peer_groups: vec![],
            agent: AgentIdentity {
                name: "quickstart_bot".into(),
                system_prompt: "You are helpful.".into(),
                personality_file: None,
                personality_files: vec![],
            },
        };

        let result = dispatcher
            .handle_quickstart_apply(&json!({ "submission": submission }))
            .await
            .expect("quickstart/apply should accept reload-capable contexts");
        assert_eq!(
            result["kind"], "applied",
            "quickstart/apply result: {result:#?}"
        );
        assert_eq!(result["daemon_restarted"], true);

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            gateway_shutdown_rx.changed(),
        )
        .await
        .expect("quickstart/apply must signal gateway shutdown before daemon reload")
        .expect("gateway shutdown sender should stay alive");
        assert!(*gateway_shutdown_rx.borrow_and_update());
        assert!(
            !*reload_rx.borrow(),
            "quickstart/apply daemon reload must wait until the gateway listener has been asked to shut down"
        );

        tokio::time::timeout(std::time::Duration::from_secs(1), reload_rx.changed())
            .await
            .expect("quickstart/apply daemon reload should follow gateway shutdown")
            .expect("reload sender should stay alive");
        assert!(*reload_rx.borrow_and_update());
    }

    #[test]
    fn doctor_summary_counts_each_severity_bucket() {
        let results = vec![
            DiagResult {
                severity: crate::doctor::Severity::Ok,
                category: "config".to_string(),
                message: "ok".to_string(),
            },
            DiagResult {
                severity: crate::doctor::Severity::Warn,
                category: "config".to_string(),
                message: "warning".to_string(),
            },
            DiagResult {
                severity: crate::doctor::Severity::Warn,
                category: "runtime".to_string(),
                message: "warning".to_string(),
            },
            DiagResult {
                severity: crate::doctor::Severity::Error,
                category: "workspace".to_string(),
                message: "error".to_string(),
            },
        ];

        let summary = doctor_summary(&results);

        assert_eq!(summary.ok, 1);
        assert_eq!(summary.warnings, 2);
        assert_eq!(summary.errors, 1);
    }

    #[test]
    fn method_all_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for (_, wire) in Method::ALL {
            assert!(seen.insert(*wire), "duplicate wire name: {wire}");
        }
    }

    #[test]
    fn session_approve_resolves_pending_request() {
        let dispatcher = make_approval_test_dispatcher();
        let (tx, mut rx) =
            tokio::sync::oneshot::channel::<zeroclaw_api::channel::ChannelApprovalResponse>();
        dispatcher
            .ctx
            .approval_pending
            .insert("req-allow".to_string(), tx);

        let result = dispatcher
            .handle_session_approve(&json!({
                "session_id": "sess-1",
                "request_id": "req-allow",
                "decision": "allow_once"
            }))
            .unwrap();

        assert_eq!(result["session_id"], "sess-1");
        assert_eq!(result["request_id"], "req-allow");
        assert_eq!(result["acknowledged"], true);
        assert_eq!(
            rx.try_recv().unwrap(),
            zeroclaw_api::channel::ChannelApprovalResponse::Approve
        );
        assert!(!dispatcher.ctx.approval_pending.contains("req-allow"));
    }

    #[test]
    fn session_approve_unknown_request_is_acknowledged_noop() {
        let dispatcher = make_approval_test_dispatcher();

        let result = dispatcher
            .handle_session_approve(&json!({
                "session_id": "sess-1",
                "request_id": "timed-out-req",
                "decision": "allow_once"
            }))
            .unwrap();

        assert_eq!(result["session_id"], "sess-1");
        assert_eq!(result["request_id"], "timed-out-req");
        assert_eq!(result["acknowledged"], true);
        assert!(!dispatcher.ctx.approval_pending.contains("timed-out-req"));
    }

    #[test]
    fn personality_templates_use_requested_agent_name_before_config_exists() {
        let req = PersonalityTemplatesParams {
            agent: Some(" bob ".to_string()),
        };
        let ctx = personality_template_context(&zeroclaw_config::schema::Config::default(), &req);

        assert_eq!(ctx.agent, "bob");
        assert!(ctx.include_memory);
    }

    #[test]
    fn personality_templates_without_agent_stay_generic_and_memoryless() {
        let req = PersonalityTemplatesParams { agent: None };
        let ctx = personality_template_context(&zeroclaw_config::schema::Config::default(), &req);

        assert_eq!(ctx.agent, "ZeroClaw");
        assert!(!ctx.include_memory);
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
    fn plan_turn_event_maps_to_plan_notification() {
        use zeroclaw_api::plan::{PlanEntry, PlanPriority, PlanStatus};

        let event = TurnEvent::Plan {
            entries: vec![PlanEntry {
                content: "Analyze codebase".to_string(),
                status: PlanStatus::InProgress,
                priority: PlanPriority::High,
                active_form: Some("Analyzing codebase".to_string()),
            }],
        };
        let json = notification_for_turn_event("sess-1", &event, None)
            .expect("plan yields a notification");
        let v = parse(&json);
        assert_eq!(v["method"], "session/update");
        assert_eq!(v["params"]["type"], "plan");
        assert_eq!(v["params"]["session_id"], "sess-1");
        assert_eq!(v["params"]["entries"][0]["content"], "Analyze codebase");
        assert_eq!(v["params"]["entries"][0]["status"], "in_progress");
        assert_eq!(v["params"]["entries"][0]["priority"], "high");
        assert_eq!(
            v["params"]["entries"][0]["activeForm"],
            "Analyzing codebase"
        );
    }

    #[test]
    fn empty_plan_turn_event_maps_to_empty_entries() {
        let event = TurnEvent::Plan { entries: vec![] };
        let json =
            notification_for_turn_event("sess-2", &event, None).expect("empty plan still notifies");
        let v = parse(&json);
        assert_eq!(v["params"]["type"], "plan");
        assert!(v["params"]["entries"].as_array().unwrap().is_empty());
    }

    #[test]
    fn resume_plan_notification_built_for_nonempty_plan() {
        use zeroclaw_api::plan::{PlanEntry, PlanPriority, PlanStatus};
        let entries = vec![PlanEntry {
            content: "Resume me".to_string(),
            status: PlanStatus::Pending,
            priority: PlanPriority::Medium,
            active_form: None,
        }];
        let json = plan_replay_notification("sess-9", &entries).expect("nonempty plan replays");
        let v = parse(&json);
        assert_eq!(v["method"], "session/update");
        assert_eq!(v["params"]["type"], "plan");
        assert_eq!(v["params"]["session_id"], "sess-9");
        assert_eq!(v["params"]["entries"][0]["content"], "Resume me");
    }

    #[test]
    fn resume_plan_notification_absent_for_empty_plan() {
        assert!(plan_replay_notification("sess-9", &[]).is_none());
    }

    async fn store_with_one_session(sid: &str) -> Arc<crate::rpc::session::SessionStore> {
        use zeroclaw_infra::session_queue::SessionActorQueue;
        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let agent = crate::agent::agent::Agent::builder()
            .model_provider(Box::new(DummyModelProvider))
            .tools(vec![])
            .memory(Arc::new(zeroclaw_memory::NoneMemory::new("none")))
            .observer(Arc::new(crate::observability::noop::NoopObserver))
            .tool_dispatcher(Box::new(crate::agent::dispatcher::NativeToolDispatcher))
            .workspace_dir(std::env::temp_dir())
            .build()
            .expect("minimal Agent should build");
        let rpc_session = crate::rpc::session::RpcSession::new(
            agent,
            "test-agent",
            std::env::temp_dir().to_str().unwrap(),
            crate::rpc::types::ChatMode::Chat,
        );
        sessions.insert(sid.to_string(), rpc_session).await.unwrap();
        sessions
    }

    #[tokio::test]
    async fn plan_event_is_stored_before_emitting() {
        use zeroclaw_api::plan::{PlanEntry, PlanPriority, PlanStatus};
        let sid = "persist-plan-sess";
        let store = store_with_one_session(sid).await;

        let entries = vec![PlanEntry {
            content: "A".to_string(),
            status: PlanStatus::InProgress,
            priority: PlanPriority::High,
            active_form: None,
        }];
        let event = TurnEvent::Plan {
            entries: entries.clone(),
        };
        persist_plan_if_any(&store, None, sid, &event).await;
        assert_eq!(store.get_plan(sid).await.unwrap(), entries);
    }

    #[tokio::test]
    async fn non_plan_event_does_not_touch_stored_plan() {
        let sid = "no-touch-sess";
        let store = store_with_one_session(sid).await;
        persist_plan_if_any(&store, None, sid, &TurnEvent::Chunk { delta: "hi".into() }).await;
        assert!(store.get_plan(sid).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn plan_event_persists_to_durable_acp_store() {
        use zeroclaw_api::plan::{PlanEntry, PlanPriority, PlanStatus};
        let sid = "durable-plan-sess";
        let sessions = store_with_one_session(sid).await;

        let tmp = tempfile::TempDir::new().unwrap();
        let acp =
            Arc::new(zeroclaw_infra::acp_session_store::AcpSessionStore::new(tmp.path()).unwrap());
        acp.create_session(sid, "alpha", tmp.path().to_str().unwrap())
            .unwrap();

        let entries = vec![PlanEntry {
            content: "Durable".to_string(),
            status: PlanStatus::Pending,
            priority: PlanPriority::Low,
            active_form: None,
        }];
        let event = TurnEvent::Plan {
            entries: entries.clone(),
        };
        persist_plan_if_any(&sessions, Some(&acp), sid, &event).await;

        // In-memory cache updated…
        assert_eq!(sessions.get_plan(sid).await.unwrap(), entries);
        // …and durable store updated (survives daemon restart / eviction).
        assert_eq!(acp.get_plan(sid).unwrap(), entries);
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
    fn history_trimmed_notification() {
        let event = TurnEvent::HistoryTrimmed {
            dropped_messages: 12,
            kept_turns: 1,
            reason: "context token budget exceeded".into(),
        };
        let json = notification_for_turn_event("s1", &event, None).unwrap();
        let v = parse(&json);
        assert_eq!(v["method"], "session/update");
        assert_eq!(v["params"]["type"], "history_trimmed");
        assert_eq!(v["params"]["session_id"], "s1");
        assert_eq!(v["params"]["dropped_messages"], 12);
        assert_eq!(v["params"]["kept_turns"], 1);
        assert_eq!(v["params"]["reason"], "context token budget exceeded");
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

    /// Regression: Zerocode's context meter must read the runtime-profile
    /// `max_context_tokens` budget, not the provider model-window helper.
    /// The model-window path falls back to 32_000 when `context_window` is
    /// unset, which made the meter ignore a profile set to e.g. 128_000.
    #[test]
    fn context_usage_max_tokens_uses_runtime_profile_budget() {
        use std::collections::HashMap;
        use zeroclaw_config::schema::{AliasedAgentConfig, Config, RuntimeProfileConfig};

        let mut runtime_profiles = HashMap::new();
        runtime_profiles.insert(
            "coding".to_string(),
            RuntimeProfileConfig {
                max_context_tokens: Some(128_000),
                ..RuntimeProfileConfig::default()
            },
        );

        let mut agents = HashMap::new();
        agents.insert(
            "coder".to_string(),
            AliasedAgentConfig {
                enabled: true,
                runtime_profile: "coding".into(),
                // No provider context_window configured — the broken path
                // would fall back to 32_000 here.
                ..AliasedAgentConfig::default()
            },
        );

        let cfg = Config {
            agents,
            runtime_profiles,
            ..Config::default()
        };

        assert_eq!(
            context_usage_max_tokens(&cfg, "coder"),
            128_000,
            "context meter must use runtime_profiles.<name>.max_context_tokens"
        );
        assert_eq!(
            cfg.effective_model_context_window("coder"),
            32_000,
            "sanity: model-window helper still defaults to 32k without provider context_window"
        );
    }

    /// Boundary regression: prove the corrected ceiling survives the *wire*
    /// path, not just the config helper. This threads
    /// `context_usage_max_tokens(&cfg, alias)` through the exact
    /// `notification_for_turn_event` serialization the RPC dispatch emits, and
    /// asserts the on-the-wire `context_usage.max_context_tokens` reads the
    /// runtime-profile budget (128_000) rather than the model-window fallback
    /// (32_000). This closes the "helper is right but does the emitted payload
    /// carry it?" gap without needing a live daemon smoke.
    #[test]
    fn context_usage_notification_wire_reports_runtime_profile_budget() {
        use std::collections::HashMap;
        use zeroclaw_config::schema::{AliasedAgentConfig, Config, RuntimeProfileConfig};

        let mut runtime_profiles = HashMap::new();
        runtime_profiles.insert(
            "coding".to_string(),
            RuntimeProfileConfig {
                max_context_tokens: Some(128_000),
                ..RuntimeProfileConfig::default()
            },
        );

        let mut agents = HashMap::new();
        agents.insert(
            "coder".to_string(),
            AliasedAgentConfig {
                enabled: true,
                runtime_profile: "coding".into(),
                // No provider context_window: the broken path would emit 32_000.
                ..AliasedAgentConfig::default()
            },
        );

        let cfg = Config {
            agents,
            runtime_profiles,
            ..Config::default()
        };

        // Resolve the ceiling exactly as RPC dispatch does, then emit it
        // through the real wire serializer.
        let max_ctx = context_usage_max_tokens(&cfg, "coder");
        let event = TurnEvent::Usage {
            input_tokens: Some(100),
            cached_input_tokens: None,
            output_tokens: Some(50),
            cost_usd: Some(0.01),
        };
        let json = notification_for_turn_event("s1", &event, Some(max_ctx)).unwrap();
        let v = parse(&json);

        assert_eq!(v["params"]["type"], "context_usage");
        assert_eq!(
            v["params"]["max_context_tokens"], 128_000,
            "emitted context_usage must carry the runtime-profile budget, not the 32k model-window fallback"
        );
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

    #[test]
    fn status_runtime_context_reports_config_root_and_local_endpoint() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("data"),
            ..zeroclaw_config::schema::Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();

        let context = status_runtime_context(&config, RuntimeConfigKind::Temporary);

        assert_eq!(context.config_dir, tmp.path().display().to_string());
        assert_eq!(
            context.config_file,
            tmp.path().join("config.toml").display().to_string()
        );
        assert_eq!(context.config_kind, RuntimeConfigKind::Temporary);
        assert_eq!(
            context.local_ipc_endpoint,
            crate::rpc::local::socket_path(&config)
                .display()
                .to_string()
        );

        config.config_path = std::path::PathBuf::from("/opt/zeroclaw/config.toml");
        assert_eq!(
            status_runtime_context(&config, RuntimeConfigKind::Custom).config_kind,
            RuntimeConfigKind::Custom
        );
    }

    #[tokio::test]
    async fn handle_status_includes_runtime_context_fields() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("data"),
            ..zeroclaw_config::schema::Config::default()
        };
        let (dispatcher, _sessions) = make_acp_test_dispatcher(config.clone());

        let value = dispatcher.handle_status().await.expect("status result");
        let status: StatusResult = serde_json::from_value(value).expect("status shape");

        assert_eq!(
            status.config_dir.as_deref(),
            Some(tmp.path().to_str().unwrap())
        );
        assert_eq!(
            status.config_file.as_deref(),
            Some(tmp.path().join("config.toml").to_str().unwrap())
        );
        assert_eq!(status.config_kind, Some(RuntimeConfigKind::Temporary));
        assert_eq!(
            status.local_ipc_endpoint.as_deref(),
            Some(crate::rpc::local::socket_path(&config).to_str().unwrap())
        );
    }

    /// Cover the `initialize` parsing path that caches the TUI's
    /// `clientCapabilities.elicitation` block so the per-session
    /// `RpcApprovalChannel` can route `request_choice` over
    /// `elicitation/create`. Source-of-truth check: the dispatcher
    /// is the canonical owner; the test reads the field directly.
    #[tokio::test]
    async fn handle_initialize_caches_elicitation_form_capability() {
        let (mut dispatcher, _sessions) =
            make_acp_test_dispatcher(zeroclaw_config::schema::Config::default());
        let params = serde_json::json!({
            "protocol_version": RPC_PROTOCOL_VERSION,
            "clientCapabilities": { "elicitation": { "form": {} } }
        });
        let result = dispatcher.handle_initialize(&params).await;
        assert!(result.is_ok(), "initialize should succeed; got {result:?}");
        assert!(dispatcher.client_elicitation_caps.form);
        assert!(!dispatcher.client_elicitation_caps.url);
    }

    #[tokio::test]
    async fn handle_initialize_without_elicitation_leaves_caps_unset() {
        let (mut dispatcher, _sessions) =
            make_acp_test_dispatcher(zeroclaw_config::schema::Config::default());
        let params = serde_json::json!({
            "protocol_version": RPC_PROTOCOL_VERSION,
        });
        let _ = dispatcher.handle_initialize(&params).await.unwrap();
        assert!(!dispatcher.client_elicitation_caps.form);
        assert!(!dispatcher.client_elicitation_caps.url);
    }

    #[tokio::test]
    async fn handle_initialize_empty_elicitation_object_is_form_only() {
        // RFD backward-compat: `"elicitation": {}` advertises form-only.
        let (mut dispatcher, _sessions) =
            make_acp_test_dispatcher(zeroclaw_config::schema::Config::default());
        let params = serde_json::json!({
            "protocol_version": RPC_PROTOCOL_VERSION,
            "clientCapabilities": { "elicitation": {} }
        });
        let _ = dispatcher.handle_initialize(&params).await.unwrap();
        assert!(dispatcher.client_elicitation_caps.form);
        assert!(!dispatcher.client_elicitation_caps.url);
    }

    use zeroclaw_tools::MEMORY_TOOL_NAMES as MEMORY_TOOLS;

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
                risk_profile: "test-profile".into(),
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
        make_acp_test_dispatcher_with_events(config, None)
    }

    fn make_acp_test_dispatcher_with_events(
        config: zeroclaw_config::schema::Config,
        event_tx: Option<tokio::sync::broadcast::Sender<Value>>,
    ) -> (RpcDispatcher, Arc<crate::rpc::session::SessionStore>) {
        use zeroclaw_infra::session_queue::SessionActorQueue;
        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let ctx = RpcContext::minimal(config, Arc::clone(&sessions));
        let mut ctx = Arc::try_unwrap(ctx)
            .ok()
            .expect("minimal test context should be uniquely owned");
        ctx.event_tx = event_tx;
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let dispatcher = RpcDispatcher::new(Arc::new(ctx), tx, "test-peer".into());
        (dispatcher, sessions)
    }

    #[tokio::test]
    async fn cron_trigger_rpc_persists_run_history() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = make_acp_test_config(&tmp);
        config
            .risk_profiles
            .entry("test-profile".into())
            .or_default()
            .allowed_commands = vec!["echo".into()];
        let job = crate::cron::add_shell_job_with_approval(
            &config,
            "test-agent",
            Some("rpc-trigger".into()),
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "echo rpc-trigger-ok",
            None,
            true,
        )
        .expect("test cron job should be created");
        let (dispatcher, _sessions) = make_acp_test_dispatcher(config.clone());

        let value = dispatcher
            .handle_cron_trigger(&json!({ "id": job.id }))
            .await
            .expect("cron/trigger should succeed");

        assert_eq!(value["id"], job.id);
        assert_eq!(value["success"], true);
        assert_eq!(value["status"], "ok");
        assert!(
            value["output"]
                .as_str()
                .unwrap_or("")
                .contains("rpc-trigger-ok")
        );

        let updated = crate::cron::get_job(&config, &job.id).expect("job should still exist");
        assert_eq!(updated.last_status.as_deref(), Some("ok"));
        assert!(
            updated
                .last_output
                .as_deref()
                .is_some_and(|output| output.contains("rpc-trigger-ok"))
        );

        let runs =
            crate::cron::list_runs(&config, &job.id, 10).expect("RPC trigger should persist runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "ok");
        assert!(
            runs[0]
                .output
                .as_deref()
                .unwrap_or("")
                .contains("rpc-trigger-ok")
        );
    }

    #[tokio::test]
    async fn cron_trigger_rpc_reports_degraded_status_and_broadcasts() {
        crate::cron::scheduler::register_delivery_fn(Box::new(
            |_config, channel, _target, _thread_id, _output| {
                Box::pin(async move {
                    if channel == "fail-delivery" {
                        anyhow::bail!("synthetic delivery failure");
                    }
                    Ok(())
                })
            },
        ));

        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = make_acp_test_config(&tmp);
        config
            .risk_profiles
            .entry("test-profile".into())
            .or_default()
            .allowed_commands = vec!["echo".into()];
        let job = crate::cron::add_shell_job_with_approval(
            &config,
            "test-agent",
            Some("rpc-trigger-degraded".into()),
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "echo rpc-trigger-degraded",
            Some(crate::cron::DeliveryConfig {
                mode: "announce".into(),
                channel: Some("fail-delivery".into()),
                to: Some("123456".into()),
                thread_id: None,
                best_effort: true,
            }),
            true,
        )
        .expect("test cron job should be created");
        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(8);
        let (dispatcher, _sessions) =
            make_acp_test_dispatcher_with_events(config.clone(), Some(event_tx));

        let value = dispatcher
            .handle_cron_trigger(&json!({ "id": job.id }))
            .await
            .expect("cron/trigger should succeed");

        assert_eq!(value["id"], job.id);
        assert_eq!(value["success"], true);
        assert_eq!(value["status"], "degraded");
        assert!(
            value["output"]
                .as_str()
                .unwrap_or("")
                .contains("delivery failed:")
        );
        assert!(value["duration_ms"].as_i64().is_some());
        assert!(value["started_at"].as_str().unwrap_or("").contains('T'));
        assert!(value["finished_at"].as_str().unwrap_or("").contains('T'));

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("cron trigger should broadcast")
            .expect("broadcast channel should stay open");
        assert_eq!(event["type"], "cron_result");
        assert_eq!(event["job_id"], job.id);
        assert_eq!(event["success"], true);
        assert_eq!(event["manual"], true);
        assert!(
            event["output"]
                .as_str()
                .unwrap_or("")
                .contains("delivery failed:")
        );

        let runs =
            crate::cron::list_runs(&config, &job.id, 10).expect("RPC trigger should persist runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "degraded");
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
    async fn acp_chat_mode_strips_memory_tools_without_exclude_flag() {
        // The server must derive memory exclusion from `chat_mode: acp`, not
        // trust the wire `exclude_memory` flag. A Code session that omits the
        // flag entirely must still come up with no memory tools.
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let data_dir = config.data_dir.clone();
        let (dispatcher, sessions, _chat_backend, _acp_store) =
            make_persistence_test_dispatcher(config, &data_dir);

        let params = json!({
            "agent_alias": "test-agent",
            "chat_mode": "acp",
            "session_id": "acp-no-flag-session-001"
        });

        let result = dispatcher.handle_session_new_for_test(&params).await;
        assert!(
            result.is_ok(),
            "session/new should succeed; got: {:?}",
            result.err()
        );

        let agent_arc = sessions
            .get_agent("acp-no-flag-session-001")
            .await
            .expect("session must be registered in the store after session/new");

        let agent = agent_arc.lock().await;
        let tool_names = agent.tool_names();

        for &mem_tool in MEMORY_TOOLS {
            assert!(
                !tool_names.contains(&mem_tool),
                "ACP chat_mode must strip `{mem_tool}` even without exclude_memory — \
                 tool list: {tool_names:?}"
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

    #[tokio::test]
    async fn seed_trim_event_is_forwarded_exactly_once() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let (dispatcher, mut rx, _sessions) = make_dispatcher_with_capture(config);
        let event = TurnEvent::HistoryTrimmed {
            dropped_messages: 4,
            kept_turns: 1,
            reason: "message cap".into(),
        };

        dispatcher
            .forward_seed_event("restored-session", Some(event))
            .await;

        let raw = rx
            .try_recv()
            .expect("restored history trim must notify the active client");
        let notification: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(notification["method"], notification::SESSION_UPDATE);
        assert_eq!(notification["params"]["session_id"], "restored-session");
        assert_eq!(notification["params"]["dropped_messages"], 4);
        assert!(
            rx.try_recv().is_err(),
            "one seed trim must emit exactly one notification"
        );
    }

    #[tokio::test]
    async fn acp_persistence_appends_complete_pretrim_delta_at_cap() {
        use zeroclaw_api::model_provider::ConversationMessage;

        let tmp = tempfile::TempDir::new().unwrap();
        let store =
            Arc::new(zeroclaw_infra::acp_session_store::AcpSessionStore::new(tmp.path()).unwrap());
        let sid = "trim-at-cap";
        store.create_session(sid, "agent", "/tmp").unwrap();
        let existing = (0..50)
            .map(|index| ConversationMessage::Chat(ChatMessage::user(format!("old-{index}"))))
            .collect::<Vec<_>>();
        store.append_turn(sid, &existing).unwrap();

        let new_messages = vec![
            ConversationMessage::Chat(ChatMessage::user("new-user")),
            ConversationMessage::Chat(ChatMessage::assistant("new-assistant")),
        ];
        let outcome = Ok(TurnOutcome::Completed {
            text: "new-assistant".into(),
            messages: new_messages.clone(),
        });

        assert_eq!(persist_acp_turn(&store, sid, &outcome).await, None);

        let restored = store.load_session(sid).unwrap().unwrap();
        assert_eq!(restored.messages.len(), 52);
        assert_eq!(
            serde_json::to_value(&restored.messages[50..]).unwrap(),
            serde_json::to_value(&new_messages).unwrap()
        );
    }

    #[tokio::test]
    async fn acp_persistence_skips_empty_and_failed_turns() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store =
            Arc::new(zeroclaw_infra::acp_session_store::AcpSessionStore::new(tmp.path()).unwrap());
        let sid = "no-turn-delta";
        store.create_session(sid, "agent", "/tmp").unwrap();

        let empty = Ok(TurnOutcome::Cancelled {
            partial_text: String::new(),
            messages: Vec::new(),
        });
        assert_eq!(persist_acp_turn(&store, sid, &empty).await, None);

        let failed = Err(crate::rpc::turn::TurnError::AgentError("failed".into()));
        assert_eq!(persist_acp_turn(&store, sid, &failed).await, None);
        assert!(
            store
                .load_session(sid)
                .unwrap()
                .unwrap()
                .messages
                .is_empty()
        );
    }

    fn make_agent_rename_test_config(tmp: &tempfile::TempDir) -> zeroclaw_config::schema::Config {
        use zeroclaw_config::multi_agent::{AccessMode, AgentAlias, PeerGroupConfig};
        use zeroclaw_config::schema::{AliasedAgentConfig, DelegateTargetConfig};

        let mut config = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        config.heartbeat.enabled = true;
        config.heartbeat.agent = "alpha".to_string();
        config.acp.default_agent = Some("alpha".to_string());

        let mut alpha = AliasedAgentConfig {
            delegates: vec![DelegateTargetConfig::bounded("alpha")],
            ..Default::default()
        };
        alpha
            .workspace
            .access
            .insert(AgentAlias::new("alpha"), AccessMode::Read);
        config.agents.insert("alpha".to_string(), alpha);

        let mut reviewer = AliasedAgentConfig {
            delegates: vec![DelegateTargetConfig::bounded("alpha")],
            ..Default::default()
        };
        reviewer
            .workspace
            .read_memory_from
            .push(AgentAlias::new("alpha"));
        config.agents.insert("reviewer".to_string(), reviewer);

        let mut group = PeerGroupConfig::default();
        group.agents.push(AgentAlias::new("alpha"));
        config.peer_groups.insert("crew".to_string(), group);

        config
    }

    #[tokio::test]
    async fn config_map_key_rename_uses_agent_cascade() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_agent_rename_test_config(&tmp);
        let data_dir = config.data_dir.clone();
        let (dispatcher, _sessions, _chat_backend, _acp_store) =
            make_persistence_test_dispatcher(config, &data_dir);

        let result = dispatcher
            .handle_config_map_key_rename(&json!({
                "path": "agents",
                "from": "alpha",
                "to": "beta"
            }))
            .await
            .expect("agent rename must succeed");

        assert_eq!(result["renamed"], true);
        assert_eq!(result["path"], "agents");
        assert_eq!(result["from"], "alpha");
        assert_eq!(result["to"], "beta");
        assert!(
            result.get("warnings").is_none(),
            "test stores should make owned-state cascade warning-free: {result:?}"
        );

        let config = dispatcher.ctx.config.read().clone();
        assert!(!config.agents.contains_key("alpha"));
        assert!(config.agents.contains_key("beta"));
        assert_eq!(config.heartbeat.agent, "beta");
        assert_eq!(config.acp.default_agent.as_deref(), Some("beta"));
        assert_eq!(
            config.agents["beta"].delegates,
            vec![zeroclaw_config::schema::DelegateTargetConfig::bounded(
                "beta"
            )]
        );
        assert!(
            config.agents["beta"]
                .workspace
                .access
                .contains_key(&zeroclaw_config::multi_agent::AgentAlias::new("beta"))
        );
        assert_eq!(
            config.agents["reviewer"].delegates,
            vec![zeroclaw_config::schema::DelegateTargetConfig::bounded(
                "beta"
            )]
        );
        assert_eq!(
            config.agents["reviewer"].workspace.read_memory_from,
            vec![zeroclaw_config::multi_agent::AgentAlias::new("beta")]
        );
        assert_eq!(
            config.peer_groups["crew"].agents,
            vec![zeroclaw_config::multi_agent::AgentAlias::new("beta")]
        );

        let written = std::fs::read_to_string(&config.config_path).unwrap();
        assert!(written.contains("[agents.beta]"), "{written}");
        assert!(!written.contains("[agents.alpha]"), "{written}");
        assert!(written.contains("agent = \"beta\""), "{written}");
        assert!(written.contains("default_agent = \"beta\""), "{written}");
    }

    #[tokio::test]
    async fn config_map_key_rename_resumes_committed_agent_rename_side_effects() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = make_agent_rename_test_config(&tmp);
        let old_workspace = config.agent_workspace_dir("alpha");
        std::fs::create_dir_all(&old_workspace).unwrap();
        std::fs::write(old_workspace.join("marker.txt"), "lagged workspace").unwrap();

        zeroclaw_config::alias_refs::rename_with_cascade(
            &mut config,
            &zeroclaw_config::alias_refs::AliasKind::Agent,
            "alpha",
            "beta",
        )
        .expect("seed config already committed to beta");
        let new_workspace = config.agent_workspace_dir("beta");
        let data_dir = config.data_dir.clone();
        let (dispatcher, _sessions, _chat_backend, _acp_store) =
            make_persistence_test_dispatcher(config, &data_dir);

        let result = dispatcher
            .handle_config_map_key_rename(&json!({
                "path": "agents",
                "from": "alpha",
                "to": "beta"
            }))
            .await
            .expect("re-issued rename must converge lagging side effects");

        assert_eq!(result["renamed"], true);
        assert_eq!(result["from"], "alpha");
        assert_eq!(result["to"], "beta");
        assert!(
            !old_workspace.exists(),
            "old workspace should be moved on resume"
        );
        assert!(
            new_workspace.join("marker.txt").exists(),
            "workspace residue should converge onto the renamed alias"
        );
    }

    #[test]
    fn config_alias_rename_future_is_small_enough_for_rpc_task_stack() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_agent_rename_test_config(&tmp);
        let data_dir = config.data_dir.clone();
        let (dispatcher, _sessions, _chat_backend, _acp_store) =
            make_persistence_test_dispatcher(config, &data_dir);

        let params = json!({
            "path": "agents",
            "from": "alpha",
            "to": "beta"
        });
        let future = dispatcher.handle_config_map_key_rename(&params);
        let future_size = std::mem::size_of_val(&future);
        drop(future);

        assert!(
            future_size < 16 * 1024,
            "agent alias rename future is {future_size} bytes; keep large config snapshots \
             out of the RPC task stack"
        );
    }

    #[tokio::test]
    async fn config_map_key_rename_refuses_active_agent_sessions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let data_dir = config.data_dir.clone();
        let (dispatcher, sessions, _chat_backend, _acp_store) =
            make_persistence_test_dispatcher(config, &data_dir);

        dispatcher
            .handle_session_new_for_test(&json!({
                "agent_alias": "test-agent",
                "session_id": "live-agent-session"
            }))
            .await
            .expect("session/new should succeed");
        assert_eq!(sessions.count_by_agent().await.get("test-agent"), Some(&1));

        let err = dispatcher
            .handle_config_map_key_rename(&json!({
                "path": "agents",
                "from": "test-agent",
                "to": "renamed-agent"
            }))
            .await
            .expect_err("agent rename must refuse active sessions");

        assert_eq!(err.code, INVALID_PARAMS);
        assert!(
            err.message.contains("active RPC session"),
            "unexpected error message: {}",
            err.message
        );
    }

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

    #[tokio::test]
    async fn session_messages_falls_back_to_acp_store_for_acp_sessions() {
        use serde_json::from_value;
        use zeroclaw_api::model_provider::{ChatMessage, ConversationMessage};
        use zeroclaw_providers::{ToolCall, ToolResultMessage};

        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let data_dir = config.data_dir.clone();
        let (dispatcher, _sessions, chat_backend, acp_store) =
            make_persistence_test_dispatcher(config, &data_dir);

        let sid = "acp-resume-7799";
        acp_store
            .create_session(sid, "test-agent", "/tmp/ws")
            .expect("ACP session row");
        acp_store
            .append_turn(
                sid,
                &[
                    ConversationMessage::Chat(ChatMessage {
                        role: "user".into(),
                        content: "hello from prior turn".into(),
                    }),
                    ConversationMessage::AssistantToolCalls {
                        text: Some("let me check the logs".into()),
                        tool_calls: vec![ToolCall {
                            id: "tc-1".into(),
                            name: "shell".into(),
                            arguments: r#"{"command":"tail log"}"#.into(),
                            extra_content: None,
                        }],
                        reasoning_content: None,
                    },
                    ConversationMessage::ToolResults(vec![ToolResultMessage {
                        tool_call_id: "tc-1".into(),
                        content: "log contents".into(),
                        tool_name: String::new(),
                    }]),
                    ConversationMessage::AssistantToolCalls {
                        text: None,
                        tool_calls: vec![ToolCall {
                            id: "tc-2".into(),
                            name: "shell".into(),
                            arguments: r#"{"command":"grep err"}"#.into(),
                            extra_content: None,
                        }],
                        reasoning_content: None,
                    },
                    ConversationMessage::ToolResults(vec![ToolResultMessage {
                        tool_call_id: "tc-2".into(),
                        content: "no errors".into(),
                        tool_name: String::new(),
                    }]),
                    ConversationMessage::Chat(ChatMessage {
                        role: "assistant".into(),
                        content: "ack from prior turn".into(),
                    }),
                ],
            )
            .expect("append turn");

        // Sanity: the unified backend really is empty for this id under any
        // candidate key. If this ever changes the test below stops being a
        // regression for the ACP-store fallback.
        for key in [sid.to_string(), format!("rpc_{sid}"), format!("gw_{sid}")] {
            assert!(
                chat_backend.load(&key).is_empty(),
                "precondition: unified backend has no rows for {key}"
            );
        }

        let result = dispatcher
            .handle_session_messages_for_test(&json!({ "session_id": sid }))
            .await
            .expect("session/messages should succeed");
        let parsed: SessionMessagesResult =
            from_value(result).expect("SessionMessagesResult shape");

        assert_eq!(parsed.session_id, sid);
        assert_eq!(
            parsed.total, 3,
            "ACP-backed sessions must report their full replayable message count"
        );
        assert_eq!(
            parsed.messages.len(),
            3,
            "ACP-backed sessions must replay their persisted messages, not a blank transcript"
        );
        assert_eq!(parsed.messages[0].role, "user");
        assert_eq!(parsed.messages[0].content, "hello from prior turn");
        assert_eq!(parsed.messages[1].role, "assistant");
        assert_eq!(
            parsed.messages[1].content, "let me check the logs",
            "assistant narration on an AssistantToolCalls row must be preserved \
             when flattening for session/messages — the agent stores it ONLY \
             on that row, so dropping it would lose visible turns from the \
             replayed transcript"
        );
        assert_eq!(parsed.messages[2].role, "assistant");
        assert_eq!(parsed.messages[2].content, "ack from prior turn");
    }

    #[tokio::test]
    async fn reaped_acp_session_rehydrates_to_working_instead_of_failing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let data_dir = config.data_dir.clone();
        let (dispatcher, sessions, _chat_backend, acp_store) =
            make_persistence_test_dispatcher(config, &data_dir);

        let sid = "acp-reaped-001";
        dispatcher
            .handle_session_new_for_test(&json!({
                "agent_alias": "test-agent",
                "exclude_memory": true,
                "chat_mode": "acp",
                "session_id": sid,
            }))
            .await
            .expect("session/new should succeed");

        assert!(
            sessions.get_agent(sid).await.is_some(),
            "freshly created session must be live in memory"
        );
        assert!(
            acp_store.load_session(sid).unwrap().is_some(),
            "durable row must exist for the rehydrate source"
        );

        // Simulate the reaper tearing the in-memory session down while the
        // durable row survives.
        assert!(
            sessions.remove(sid).await,
            "reap must remove the in-memory session"
        );
        assert!(
            sessions.get_agent(sid).await.is_none(),
            "post-reap the session must be absent from memory"
        );

        let recovered = dispatcher.rehydrate_reaped_session(sid).await;
        assert!(
            recovered.is_some(),
            "a reaped session with a live durable row must rehydrate to a \
             working agent, not fail; failing here is the irrecoverable hang"
        );
        assert!(
            sessions.get_agent(sid).await.is_some(),
            "after rehydrate the session must be live in memory again so the \
             next prompt lands on a working session"
        );
    }

    #[tokio::test]
    async fn acp_resume_recovers_persisted_cwd() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let data_dir = config.data_dir.clone();
        let (dispatcher, _sessions, _chat_backend, acp_store) =
            make_persistence_test_dispatcher(config, &data_dir);

        let sid = "acp-cwd-resume-001";
        let original_cwd = tmp.path().join("project-dir").to_string_lossy().to_string();

        // First create the session with an explicit cwd.
        let created = dispatcher
            .handle_session_new_for_test(&json!({
                "agent_alias": "test-agent",
                "exclude_memory": true,
                "chat_mode": "acp",
                "session_id": sid,
                "cwd": original_cwd,
            }))
            .await
            .expect("initial session/new should succeed");
        assert_eq!(created["workspace_dir"], original_cwd);
        assert_eq!(
            acp_store.load_session(sid).unwrap().unwrap().workspace_dir,
            original_cwd
        );

        // Resume with NO cwd: the daemon must report the persisted cwd, not the
        // agent workspace dir.
        let resumed = dispatcher
            .handle_session_new_for_test(&json!({
                "agent_alias": "test-agent",
                "exclude_memory": true,
                "chat_mode": "acp",
                "session_id": sid,
            }))
            .await
            .expect("resume session/new should succeed");
        assert_eq!(
            resumed["workspace_dir"], original_cwd,
            "resume must keep the retained session's cwd, not default it"
        );
    }

    #[tokio::test]
    async fn reaped_acp_session_rehydrates_without_memory_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let data_dir = config.data_dir.clone();
        let (dispatcher, sessions, _chat_backend, _acp_store) =
            make_persistence_test_dispatcher(config, &data_dir);

        let sid = "acp-reaped-mem-001";
        dispatcher
            .handle_session_new_for_test(&json!({
                "agent_alias": "test-agent",
                "chat_mode": "acp",
                "session_id": sid,
            }))
            .await
            .expect("session/new should succeed");

        // Reap the in-memory session, leaving the durable row to rehydrate from.
        assert!(sessions.remove(sid).await, "reap must remove the session");

        let recovered = dispatcher
            .rehydrate_reaped_session(sid)
            .await
            .expect("a reaped ACP session must rehydrate to a working agent");

        let agent = recovered.lock().await;
        let tool_names = agent.tool_names();
        for &mem_tool in MEMORY_TOOLS {
            assert!(
                !tool_names.contains(&mem_tool),
                "rehydrated ACP session must NOT expose `{mem_tool}` — found in tool list: {tool_names:?}"
            );
        }
    }

    #[tokio::test]
    async fn killed_acp_session_does_not_rehydrate_from_durable_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let data_dir = config.data_dir.clone();
        let (dispatcher, sessions, _chat_backend, acp_store) =
            make_persistence_test_dispatcher(config, &data_dir);

        let sid = "acp-killed-001";
        dispatcher
            .handle_session_new_for_test(&json!({
                "agent_alias": "test-agent",
                "exclude_memory": true,
                "chat_mode": "acp",
                "session_id": sid,
            }))
            .await
            .expect("session/new should succeed");

        assert!(
            sessions.get_agent(sid).await.is_some(),
            "freshly created session must be live in memory"
        );
        assert!(
            acp_store.load_session(sid).unwrap().is_some(),
            "durable row must exist before kill"
        );

        dispatcher
            .handle_session_kill(&json!({ "session_id": sid }))
            .await
            .expect("session/kill should succeed");

        assert!(
            sessions.get_agent(sid).await.is_none(),
            "session/kill must remove the live in-memory agent"
        );
        assert!(
            acp_store.load_session(sid).unwrap().is_some(),
            "session/kill must preserve durable history"
        );

        let recovered = dispatcher.rehydrate_reaped_session(sid).await;
        assert!(
            recovered.is_none(),
            "admin-killed ACP sessions must stay killed instead of rehydrating \
             from durable history on the next prompt"
        );
        assert!(
            sessions.get_agent(sid).await.is_none(),
            "failed rehydrate must leave the session absent from memory"
        );
    }

    #[tokio::test]
    async fn killed_acp_session_new_resume_is_rejected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let data_dir = config.data_dir.clone();
        let (dispatcher, sessions, _chat_backend, acp_store) =
            make_persistence_test_dispatcher(config, &data_dir);

        let sid = "acp-killed-resume-001";
        dispatcher
            .handle_session_new_for_test(&json!({
                "agent_alias": "test-agent",
                "exclude_memory": true,
                "chat_mode": "acp",
                "session_id": sid,
            }))
            .await
            .expect("session/new should create the original ACP session");
        dispatcher
            .handle_session_kill(&json!({ "session_id": sid }))
            .await
            .expect("session/kill should succeed");

        let resumed = dispatcher
            .handle_session_new_for_test(&json!({
                "agent_alias": "test-agent",
                "exclude_memory": true,
                "chat_mode": "acp",
                "session_id": sid,
            }))
            .await;

        assert!(
            resumed.is_err(),
            "session/new must not revive a killed ACP session"
        );
        assert!(
            sessions.get_agent(sid).await.is_none(),
            "rejected resume must leave the killed session absent from memory"
        );
        assert!(
            acp_store.load_session(sid).unwrap().is_some(),
            "rejected resume must preserve durable history"
        );
    }

    #[tokio::test]
    async fn acp_session_new_resume_rejects_agent_alias_mismatch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let data_dir = config.data_dir.clone();
        let (dispatcher, sessions, _chat_backend, acp_store) =
            make_persistence_test_dispatcher(config, &data_dir);

        let sid = "acp-alias-mismatch-001";
        acp_store
            .create_session(sid, "test-agent", "/tmp/test-agent")
            .expect("test should seed durable ACP session");

        let resumed = dispatcher
            .handle_session_new_for_test(&json!({
                "agent_alias": "test-agent-2",
                "exclude_memory": true,
                "chat_mode": "acp",
                "session_id": sid,
            }))
            .await;

        let err = resumed.expect_err("session/new must reject ACP alias mismatches");
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(
            sessions.get_agent(sid).await.is_none(),
            "rejected mismatched resume must not create a live session"
        );
        assert!(
            acp_store.load_session(sid).unwrap().is_some(),
            "rejected mismatched resume must preserve durable history"
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

    fn make_secret_test_config(tmp: &tempfile::TempDir) -> zeroclaw_config::schema::Config {
        let mut cfg = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        cfg.create_map_key("providers.models.anthropic", "default")
            .expect("create anthropic.default");
        cfg
    }

    #[tokio::test]
    async fn config_set_writes_real_secret_through_set_prop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dispatcher = make_config_set_test_dispatcher(make_secret_test_config(&tmp));
        let params = json!({
            "prop": "providers.models.anthropic.default.api_key",
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
    async fn config_set_refreshes_memory_embedder_on_provider_change() {
        use zeroclaw_infra::session_queue::SessionActorQueue;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut cfg = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        cfg.create_map_key("providers.models.openai", "default")
            .expect("create openai.default");
        // Memory embeddings resolve from openai.default.
        cfg.memory.embedding_provider = "openai.default".into();
        cfg.memory.embedding_model = "text-embedding-3-small".into();
        cfg.memory.embedding_dimensions = 1536;

        // Long-lived handle constructed with the Noop embedder (dims 0), exactly
        // the stale state the bug leaves behind.
        let mem = Arc::new(zeroclaw_memory::SqliteMemory::new("default", tmp.path()).unwrap());
        assert_eq!(mem.embedder_dimensions(), 0, "starts on the Noop embedder");

        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let ctx = RpcContext::minimal_with_memory(
            cfg,
            Arc::clone(&sessions),
            Arc::clone(&mem) as Arc<dyn zeroclaw_api::memory_traits::Memory>,
        );
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let mut dispatcher = RpcDispatcher::new(ctx, tx, "test-peer".into());
        dispatcher.authenticated = true;

        let params = json!({
            "prop": "providers.models.openai.default.api_key",
            "value": "sk-rotated-key"
        });
        let res = dispatcher.handle_config_set(&params).await;
        assert!(res.is_ok(), "config/set must succeed: {res:?}");

        assert_eq!(
            mem.embedder_dimensions(),
            1536,
            "config/set on the memory embedding provider must hot-swap the live \
             handle's embedder to the resolved provider (#8359)"
        );
    }

    #[tokio::test]
    async fn config_set_routes_memory_embeds_to_new_endpoint_and_key() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use zeroclaw_api::memory_traits::{Memory, MemoryCategory};
        use zeroclaw_infra::session_queue::SessionActorQueue;

        let mock_a = MockServer::start().await;
        let mock_b = MockServer::start().await;
        let embed_body = serde_json::json!({ "data": [{ "embedding": [0.1, 0.2, 0.3] }] });
        for server in [&mock_a, &mock_b] {
            Mock::given(method("POST"))
                .and(path("/v1/embeddings"))
                .respond_with(ResponseTemplate::new(200).set_body_json(embed_body.clone()))
                .mount(server)
                .await;
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let mut cfg = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        cfg.create_map_key("providers.models.openai", "default")
            .expect("create openai.default");
        cfg.set_prop_persistent("providers.models.openai.default.uri", &mock_a.uri())
            .expect("set initial uri");
        cfg.set_prop_persistent("providers.models.openai.default.api_key", "key-a")
            .expect("set initial key");
        cfg.memory.embedding_provider = "openai.default".into();
        cfg.memory.embedding_model = "text-embedding-3-small".into();
        cfg.memory.embedding_dimensions = 3;

        // Long-lived handle built via the real factory → embedder points at A.
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory_with_storage_and_routes(
                &cfg.memory,
                &cfg.embedding_routes,
                cfg.resolve_active_storage(),
                &cfg.data_dir,
                None,
                Some(&cfg.providers.models),
            )
            .expect("build memory"),
        );

        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let ctx = RpcContext::minimal_with_memory(cfg, Arc::clone(&sessions), Arc::clone(&mem));
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let mut dispatcher = RpcDispatcher::new(ctx, tx, "test-peer".into());
        dispatcher.authenticated = true;

        // Rotate the provider profile's endpoint + key through config/set.
        for (prop, value) in [
            ("providers.models.openai.default.uri", mock_b.uri()),
            (
                "providers.models.openai.default.api_key",
                "key-b".to_string(),
            ),
        ] {
            let res = dispatcher
                .handle_config_set(&json!({ "prop": prop, "value": value }))
                .await;
            assert!(res.is_ok(), "config/set {prop} must succeed: {res:?}");
        }

        // Next embed must go to the NEW endpoint with the NEW key.
        mem.store("k1", "hello wiremock", MemoryCategory::Core, None)
            .await
            .expect("store");

        let b_reqs = mock_b
            .received_requests()
            .await
            .expect("request recording enabled");
        let hit = b_reqs
            .iter()
            .find(|r| r.url.path() == "/v1/embeddings")
            .expect("new endpoint (mock B) must receive the embed after config/set");
        let auth = hit
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert_eq!(auth, "Bearer key-b", "embed must carry the rotated api key");

        let a_reqs = mock_a.received_requests().await.unwrap_or_default();
        assert!(
            a_reqs.iter().all(|r| r.url.path() != "/v1/embeddings"),
            "stale endpoint (mock A) must not receive embeds after the refresh"
        );
    }

    #[tokio::test]
    async fn config_set_refreshes_live_agent_session_memory() {
        use zeroclaw_api::memory_traits::Memory;
        use zeroclaw_infra::session_queue::SessionActorQueue;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut cfg = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        cfg.create_map_key("providers.models.openai", "default")
            .expect("create openai.default");
        cfg.memory.embedding_provider = "openai.default".into();
        cfg.memory.embedding_model = "text-embedding-3-small".into();
        cfg.memory.embedding_dimensions = 1536;

        // The agent's memory: AgentScopedMemory wrapping a concrete SQLite
        // backend (Noop, dims 0) — the stale state config/set must repair.
        let sqlite = Arc::new(zeroclaw_memory::SqliteMemory::new("agent", tmp.path()).unwrap());
        assert_eq!(sqlite.embedder_dimensions(), 0);
        let scoped: Arc<dyn Memory> = Arc::new(zeroclaw_memory::AgentScopedMemory::new(
            Arc::clone(&sqlite) as Arc<dyn Memory>,
            "agent-uuid",
            Vec::<String>::new(),
        ));

        let agent = crate::agent::agent::Agent::builder()
            .model_provider(Box::new(DummyModelProvider))
            .tools(vec![])
            .memory(scoped)
            .observer(Arc::new(crate::observability::noop::NoopObserver))
            .tool_dispatcher(Box::new(crate::agent::dispatcher::NativeToolDispatcher))
            .workspace_dir(std::env::temp_dir())
            .build()
            .expect("agent builds");

        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        sessions
            .insert(
                "s1".into(),
                crate::rpc::session::RpcSession::new(
                    agent,
                    "agent",
                    ".",
                    crate::rpc::types::ChatMode::Chat,
                ),
            )
            .await
            .unwrap();

        let ctx = RpcContext::minimal(cfg, Arc::clone(&sessions));
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let mut dispatcher = RpcDispatcher::new(ctx, tx, "test-peer".into());
        dispatcher.authenticated = true;

        // Full RPC path: this schedules the live-agent memory refresh.
        let res = dispatcher
            .handle_config_set(&json!({
                "prop": "providers.models.openai.default.api_key",
                "value": "sk-rotated"
            }))
            .await;
        assert!(res.is_ok(), "config/set must succeed: {res:?}");

        // The agent refresh is spawned; wait (bounded) for it to land.
        let mut dims = 0;
        for _ in 0..200 {
            dims = sqlite.embedder_dimensions();
            if dims == 1536 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            dims, 1536,
            "config/set must refresh the live session's per-agent memory embedder \
             through the AgentScopedMemory wrapper (#8359)"
        );
    }

    #[tokio::test]
    async fn config_set_rejects_masked_secret_value() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut cfg = make_secret_test_config(&tmp);
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
                "prop": "providers.models.anthropic.default.api_key",
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
    async fn config_set_handles_dynamic_http_request_secret_paths() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dispatcher = make_config_set_test_dispatcher(zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("data"),
            ..Default::default()
        });

        let params = json!({
            "prop": "http_request.secrets.api_token",
            "value": "Bearer runtime-secret"
        });
        let res = dispatcher.handle_config_set(&params).await;
        assert!(
            res.is_ok(),
            "config/set must accept a real dynamic http_request secret: {res:?}"
        );
        let cfg = dispatcher.ctx.config.read().clone();
        assert_eq!(
            cfg.http_request
                .secrets
                .get("api_token")
                .map(String::as_str),
            Some("Bearer runtime-secret")
        );

        for masked in [zeroclaw_config::traits::MASKED_SECRET, "****", ""] {
            let params = json!({
                "prop": "http_request.secrets.next_token",
                "value": masked
            });
            let res = dispatcher.handle_config_set(&params).await;
            assert!(
                res.is_err(),
                "config/set must reject masked/empty dynamic secret (`{masked}`), got: {res:?}"
            );
        }
        let cfg_after = dispatcher.ctx.config.read().clone();
        assert!(
            !cfg_after.http_request.secrets.contains_key("next_token"),
            "masked dynamic writes must not materialize a secret key"
        );
    }

    #[tokio::test]
    async fn config_set_non_secret_field_still_uses_set_prop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dispatcher = make_config_set_test_dispatcher(make_secret_test_config(&tmp));
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

    #[tokio::test]
    async fn config_set_persists_mcp_server_field_to_disk() {
        use zeroclaw_config::schema::{McpServerConfig, McpTransport};

        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");

        // Seed an on-disk file with an existing `[[mcp.servers]]`
        // entry so `save_dirty` exercises its incremental path
        // (the new-file fallback to full `save` would mask the
        // dirty-path bug because it serializes the whole struct).
        let seed = format!(
            "schema_version = {}\n\n\
             [[mcp.servers]]\n\
             name = \"fs\"\n\
             transport = \"stdio\"\n\
             command = \"/usr/bin/mcp-fs\"\n",
            zeroclaw_config::migration::CURRENT_SCHEMA_VERSION
        );
        std::fs::write(&config_path, &seed).unwrap();

        let mut cfg = zeroclaw_config::schema::Config {
            config_path: config_path.clone(),
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        cfg.mcp.servers.push(McpServerConfig {
            name: "fs".into(),
            transport: McpTransport::Stdio,
            command: "/usr/bin/mcp-fs".into(),
            ..Default::default()
        });
        let dispatcher = make_config_set_test_dispatcher(cfg);

        // The exact wire shape the dashboard / TUI send for a
        // per-field edit on an `[[mcp.servers]]` entry.
        let params = json!({
            "prop": "mcp.servers.fs.command",
            "value": "/usr/local/bin/mcp-fs"
        });
        let res = dispatcher.handle_config_set(&params).await;
        assert!(
            res.is_ok(),
            "config/set on a per-field mcp.servers path must succeed: {res:?}"
        );

        // In-memory landed (this is what the UI sees — and what was
        // working before; the bug was strictly on the save side).
        let in_memory = dispatcher
            .ctx
            .config
            .read()
            .mcp
            .servers
            .iter()
            .find(|s| s.name == "fs")
            .map(|s| s.command.clone());
        assert_eq!(
            in_memory.as_deref(),
            Some("/usr/local/bin/mcp-fs"),
            "in-memory mutation must land — this part already worked"
        );

        // The regression: the same value must reach disk.
        let written = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            written.contains("/usr/local/bin/mcp-fs"),
            "config/set on `mcp.servers.fs.command` must persist to disk; \
             on-disk file still reads:\n{written}"
        );
        assert!(
            !written.contains("/usr/bin/mcp-fs"),
            "stale command must be overwritten on disk; got:\n{written}"
        );
        // The natural-key field itself must stay on disk so the entry
        // remains addressable on the next load.
        assert!(
            written.contains("name = \"fs\""),
            "natural-key `name` must survive the incremental save; got:\n{written}"
        );

        let reparsed: zeroclaw_config::schema::Config = toml::from_str(&written).unwrap();
        let entry = reparsed
            .mcp
            .servers
            .iter()
            .find(|s| s.name == "fs")
            .expect("reparse must surface the entry by natural key");
        assert_eq!(entry.command, "/usr/local/bin/mcp-fs");
    }

    fn make_model_refresh_test_config(tmp: &tempfile::TempDir) -> zeroclaw_config::schema::Config {
        use std::collections::HashMap;
        use zeroclaw_config::schema::{AliasedAgentConfig, Config, RiskProfileConfig};

        let workspace_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();

        let mut config = Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        let provider = config
            .providers
            .models
            .ensure("openai", "test-provider")
            .expect("openai provider slot exists");
        provider.api_key = Some("test-key".into());
        provider.uri = Some("http://127.0.0.1:1".into());
        provider.model = Some("old-model".into());
        provider.temperature = Some(0.2);

        config.agents = HashMap::from([(
            "test-agent".to_string(),
            AliasedAgentConfig {
                enabled: true,
                model_provider: "openai.test-provider".into(),
                risk_profile: "test-profile".into(),
                ..Default::default()
            },
        )]);
        config
            .risk_profiles
            .insert("test-profile".into(), RiskProfileConfig::default());
        config
            .runtime_profiles
            .insert("default".into(), Default::default());
        config
    }

    async fn create_model_refresh_test_session(
        dispatcher: &RpcDispatcher,
        tmp: &tempfile::TempDir,
    ) -> String {
        let session_res = dispatcher
            .handle_session_new_for_test(&json!({
                "agent_alias": "test-agent",
                "cwd": tmp.path().join("workspace"),
            }))
            .await
            .expect("session/new should create the agent");
        session_res
            .get("session_id")
            .and_then(|v| v.as_str())
            .expect("session/new result includes session_id")
            .to_string()
    }

    async fn model_name_for_session(dispatcher: &RpcDispatcher, session_id: &str) -> String {
        let agent = dispatcher
            .ctx
            .sessions
            .get_agent(session_id)
            .await
            .expect("session agent exists");
        agent.lock().await.attribution_fields().2
    }

    async fn temperature_for_session(dispatcher: &RpcDispatcher, session_id: &str) -> Option<f64> {
        let agent = dispatcher
            .ctx
            .sessions
            .get_agent(session_id)
            .await
            .expect("session agent exists");
        agent.lock().await.temperature_for_test()
    }

    async fn wait_for_model_name(dispatcher: &RpcDispatcher, session_id: &str, expected: &str) {
        for _ in 0..50 {
            if model_name_for_session(dispatcher, session_id).await == expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            model_name_for_session(dispatcher, session_id).await,
            expected
        );
    }

    async fn wait_for_temperature(
        dispatcher: &RpcDispatcher,
        session_id: &str,
        expected: Option<f64>,
    ) {
        for _ in 0..50 {
            if temperature_for_session(dispatcher, session_id).await == expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            temperature_for_session(dispatcher, session_id).await,
            expected
        );
    }

    #[tokio::test]
    async fn config_set_agent_model_provider_refreshes_bound_live_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut cfg = make_model_refresh_test_config(&tmp);

        let other = cfg
            .providers
            .models
            .ensure("openai", "other-provider")
            .expect("openai provider slot exists");
        other.api_key = Some("test-key".into());
        other.uri = Some("http://127.0.0.1:1".into());
        other.model = Some("other-model".into());
        other.temperature = Some(0.2);

        let dispatcher = make_config_set_test_dispatcher(cfg);
        let session_id = create_model_refresh_test_session(&dispatcher, &tmp).await;
        assert_eq!(
            model_name_for_session(&dispatcher, &session_id).await,
            "old-model",
            "session must start on the currently-bound provider's model"
        );

        let res = dispatcher
            .handle_config_set(&json!({
                "prop": "agents.test-agent.model_provider",
                "value": "openai.other-provider"
            }))
            .await;
        assert!(
            res.is_ok(),
            "config/set agents.<alias>.model_provider must succeed: {res:?}"
        );

        wait_for_model_name(&dispatcher, &session_id, "other-model").await;
    }

    #[tokio::test]
    async fn existing_session_uses_reloaded_structured_history_cap() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = make_model_refresh_test_config(&tmp);
        config
            .agents
            .get_mut("test-agent")
            .expect("test agent exists")
            .runtime_profile = "reloadable".into();
        config.runtime_profiles.insert(
            "reloadable".into(),
            zeroclaw_config::schema::RuntimeProfileConfig {
                max_history_messages: Some(10),
                ..Default::default()
            },
        );

        let dispatcher = make_config_set_test_dispatcher(config);
        let session_id = create_model_refresh_test_session(&dispatcher, &tmp).await;
        dispatcher
            .ctx
            .config
            .write()
            .runtime_profiles
            .get_mut("reloadable")
            .expect("runtime profile exists")
            .max_history_messages = Some(2);

        let agent = dispatcher
            .ctx
            .sessions
            .get_agent(&session_id)
            .await
            .expect("session agent exists");
        let mut agent = agent.lock().await;
        let event = agent.seed_history_with_event(&[
            ChatMessage::user("old user"),
            ChatMessage::assistant("old assistant"),
            ChatMessage::user("new user"),
            ChatMessage::assistant("new assistant"),
        ]);

        assert!(
            matches!(event, Some(TurnEvent::HistoryTrimmed { .. })),
            "an existing session must observe the reloaded runtime-profile cap"
        );
        assert!(!agent.history().iter().any(|message| matches!(
            message,
            zeroclaw_providers::ConversationMessage::Chat(chat) if chat.content == "old user"
        )));
        assert!(agent.history().iter().any(|message| matches!(
            message,
            zeroclaw_providers::ConversationMessage::Chat(chat)
                if chat.content == "new assistant"
        )));
    }

    #[tokio::test]
    async fn config_set_provider_model_refreshes_matching_live_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dispatcher = make_config_set_test_dispatcher(make_model_refresh_test_config(&tmp));
        let session_id = create_model_refresh_test_session(&dispatcher, &tmp).await;
        assert_eq!(
            model_name_for_session(&dispatcher, &session_id).await,
            "old-model"
        );

        let res = dispatcher
            .handle_config_set(&json!({
                "prop": "providers.models.openai.test-provider.model",
                "value": "new-model"
            }))
            .await;
        assert!(res.is_ok(), "config/set must succeed: {res:?}");

        wait_for_model_name(&dispatcher, &session_id, "new-model").await;
    }

    #[tokio::test]
    async fn config_set_provider_refresh_failure_does_not_fail_saved_write() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dispatcher = make_config_set_test_dispatcher(make_model_refresh_test_config(&tmp));
        let session_id = create_model_refresh_test_session(&dispatcher, &tmp).await;
        assert_eq!(
            model_name_for_session(&dispatcher, &session_id).await,
            "old-model"
        );

        let res = dispatcher
            .handle_config_set(&json!({
                "prop": "providers.models.openai.test-provider.model",
                "value": ""
            }))
            .await;
        assert!(
            res.is_ok(),
            "config/set must report the saved write even if live refresh cannot rebuild: {res:?}"
        );
        let cfg = dispatcher.ctx.config.read().clone();
        let stored = cfg
            .providers
            .models
            .openai
            .get("test-provider")
            .and_then(|e| e.base.model.clone());
        assert_eq!(
            stored, None,
            "config/set must still persist the requested provider-profile clear"
        );
        assert_eq!(
            model_name_for_session(&dispatcher, &session_id).await,
            "old-model",
            "failed live refresh must leave the existing session provider intact"
        );
    }

    #[tokio::test]
    async fn session_configure_invalid_provider_does_not_commit_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dispatcher = make_config_set_test_dispatcher(make_model_refresh_test_config(&tmp));
        let session_id = create_model_refresh_test_session(&dispatcher, &tmp).await;
        assert_eq!(
            model_name_for_session(&dispatcher, &session_id).await,
            "old-model"
        );

        let res = dispatcher
            .handle_session_configure(&json!({
                "session_id": session_id,
                "overrides": {
                    "model_provider": "openai.missing"
                }
            }))
            .await;
        assert!(
            res.is_err(),
            "invalid provider switch must fail before mutating session overrides"
        );

        let overrides = dispatcher
            .ctx
            .sessions
            .get_overrides(&session_id)
            .await
            .expect("session still exists");
        assert_eq!(
            overrides.model_provider, None,
            "failed provider switch must not leave a stale override behind"
        );
        assert_eq!(
            model_name_for_session(&dispatcher, &session_id).await,
            "old-model",
            "failed provider switch must leave the live agent unchanged"
        );
    }

    #[tokio::test]
    async fn session_configure_blank_model_fields_do_not_commit_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dispatcher = make_config_set_test_dispatcher(make_model_refresh_test_config(&tmp));
        let session_id = create_model_refresh_test_session(&dispatcher, &tmp).await;
        assert_eq!(
            model_name_for_session(&dispatcher, &session_id).await,
            "old-model"
        );

        for model in ["", "   "] {
            let res = dispatcher
                .handle_session_configure(&json!({
                    "session_id": session_id,
                    "overrides": {
                        "model": model
                    }
                }))
                .await;
            let err = res.expect_err("blank model must be rejected");
            assert_eq!(err.code, INVALID_PARAMS);

            let overrides = dispatcher
                .ctx
                .sessions
                .get_overrides(&session_id)
                .await
                .expect("session still exists");
            assert_eq!(
                overrides.model, None,
                "failed model switch must not leave a stale override behind"
            );
            assert_eq!(
                model_name_for_session(&dispatcher, &session_id).await,
                "old-model",
                "failed model switch must leave the live agent unchanged"
            );
        }

        for model_provider in ["", "   "] {
            let res = dispatcher
                .handle_session_configure(&json!({
                    "session_id": session_id,
                    "overrides": {
                        "model_provider": model_provider
                    }
                }))
                .await;
            let err = res.expect_err("blank model_provider must be rejected");
            assert_eq!(err.code, INVALID_PARAMS);

            let overrides = dispatcher
                .ctx
                .sessions
                .get_overrides(&session_id)
                .await
                .expect("session still exists");
            assert_eq!(
                overrides.model_provider, None,
                "failed provider switch must not leave a stale override behind"
            );
            assert_eq!(
                model_name_for_session(&dispatcher, &session_id).await,
                "old-model",
                "failed provider switch must leave the live agent unchanged"
            );
        }
    }

    #[tokio::test]
    async fn config_set_provider_temperature_refreshes_matching_live_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dispatcher = make_config_set_test_dispatcher(make_model_refresh_test_config(&tmp));
        let session_id = create_model_refresh_test_session(&dispatcher, &tmp).await;
        assert_eq!(
            temperature_for_session(&dispatcher, &session_id).await,
            Some(0.2)
        );

        let res = dispatcher
            .handle_config_set(&json!({
                "prop": "providers.models.openai.test-provider.temperature",
                "value": 0.4
            }))
            .await;
        assert!(res.is_ok(), "config/set must succeed: {res:?}");

        wait_for_temperature(&dispatcher, &session_id, Some(0.4)).await;
    }

    #[tokio::test]
    async fn config_set_provider_refresh_preserves_session_temperature_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dispatcher = make_config_set_test_dispatcher(make_model_refresh_test_config(&tmp));
        let session_id = create_model_refresh_test_session(&dispatcher, &tmp).await;
        let merged = dispatcher
            .ctx
            .sessions
            .set_overrides(
                &session_id,
                crate::rpc::session::SessionOverrides {
                    temperature: Some(0.6),
                    ..Default::default()
                },
            )
            .await
            .expect("session override applies");
        assert_eq!(merged.temperature, Some(0.6));

        let res = dispatcher
            .handle_config_set(&json!({
                "prop": "providers.models.openai.test-provider.model",
                "value": "new-model"
            }))
            .await;
        assert!(res.is_ok(), "config/set must succeed: {res:?}");

        wait_for_model_name(&dispatcher, &session_id, "new-model").await;
        assert_eq!(
            temperature_for_session(&dispatcher, &session_id).await,
            Some(0.6),
            "session temperature override must win over provider profile temperature"
        );
    }

    #[tokio::test]
    async fn config_delete_provider_temperature_refreshes_matching_live_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dispatcher = make_config_set_test_dispatcher(make_model_refresh_test_config(&tmp));
        let session_id = create_model_refresh_test_session(&dispatcher, &tmp).await;
        assert_eq!(
            temperature_for_session(&dispatcher, &session_id).await,
            Some(0.2)
        );

        let res = dispatcher
            .handle_config_delete(&json!({
                "prop": "providers.models.openai.test-provider.temperature",
            }))
            .await;
        assert!(res.is_ok(), "config/delete must succeed: {res:?}");

        wait_for_temperature(&dispatcher, &session_id, None).await;
    }

    // -----------------------------------------------------------------------
    // session/cancel ownership enforcement — the spurious-cancel bug
    // -----------------------------------------------------------------------

    /// Build two dispatchers sharing one `RpcContext`/`SessionStore`. Mirrors
    /// production where each TUI connection gets its own dispatcher with its
    /// own `tui_id`, all routing to the same shared session map.
    fn make_two_dispatchers_sharing_context(
        config: zeroclaw_config::schema::Config,
    ) -> (
        RpcDispatcher,
        RpcDispatcher,
        Arc<crate::rpc::session::SessionStore>,
    ) {
        use zeroclaw_infra::session_queue::SessionActorQueue;
        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let ctx = RpcContext::minimal(config, Arc::clone(&sessions));
        let (tx_a, _rx_a) = tokio::sync::mpsc::channel(64);
        let (tx_b, _rx_b) = tokio::sync::mpsc::channel(64);
        let dispatcher_a = RpcDispatcher::new(Arc::clone(&ctx), tx_a, "test-peer-a:pid=1".into());
        let dispatcher_b = RpcDispatcher::new(ctx, tx_b, "test-peer-b:pid=2".into());
        (dispatcher_a, dispatcher_b, sessions)
    }

    async fn create_session_with_owner(
        dispatcher: &mut RpcDispatcher,
        sessions: &Arc<crate::rpc::session::SessionStore>,
        session_id: &str,
        owner_tui_id: &str,
    ) -> tokio_util::sync::CancellationToken {
        dispatcher.set_tui_id_for_test(Some(owner_tui_id.to_string()));
        let params = json!({
            "agent_alias": "test-agent",
            "session_id": session_id,
        });
        dispatcher
            .handle_session_new_for_test(&params)
            .await
            .expect("session/new must succeed");

        let stamped_owner = sessions
            .session_owner_tui_id(session_id)
            .await
            .expect("session must exist after session/new");
        assert_eq!(
            stamped_owner.as_deref(),
            Some(owner_tui_id),
            "harness invariant: session/new must stamp owner_tui_id from the \
             caller's tui_id; if this fails, the ownership tests below are \
             measuring nothing"
        );

        let token = tokio_util::sync::CancellationToken::new();
        sessions.register_cancel_token(session_id, token.clone());
        token
    }

    fn make_dispatcher_with_capture(
        config: zeroclaw_config::schema::Config,
    ) -> (
        RpcDispatcher,
        tokio::sync::mpsc::Receiver<String>,
        Arc<crate::rpc::session::SessionStore>,
    ) {
        use zeroclaw_infra::session_queue::SessionActorQueue;
        let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let ctx = RpcContext::minimal(config, Arc::clone(&sessions));
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let dispatcher = RpcDispatcher::new(ctx, tx, "test-peer-cap:pid=1".into());
        (dispatcher, rx, sessions)
    }

    #[tokio::test]
    async fn session_prompt_on_missing_session_emits_turn_complete_failed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let (dispatcher, mut rx, _sessions) = make_dispatcher_with_capture(config);

        let result = dispatcher
            .handle_session_prompt(&json!({
                "session_id": "gone-id",
                "prompt": "anything",
            }))
            .await;
        assert!(
            result.is_err(),
            "missing session must still produce an RPC error for legacy \
             request-form callers; the new behaviour is the additional \
             notification, not replacing the error"
        );

        // The notification must already be queued on the writer channel by
        // the time `handle_session_prompt` returns. `try_recv` rules out
        // any test flakiness from racing with a spawned task.
        let raw = rx.try_recv().expect(
            "handle_session_prompt must emit a session/update TurnComplete \
             notification before returning on missing-session — without it \
             the TUI's `working` state never clears and the next prompt is \
             the production freeze",
        );
        let v: serde_json::Value = serde_json::from_str(&raw).expect("notification must be JSON");
        assert_eq!(v["method"], notification::SESSION_UPDATE);
        assert_eq!(v["params"]["session_id"], "gone-id");
        assert_eq!(
            v["params"]["outcome"], "failed",
            "missing-session is not Completed and not Cancelled — it is a \
             distinct Failed verdict. Folding it into Cancelled would lie \
             about whether the user pressed Esc."
        );
    }

    #[tokio::test]
    async fn session_cancel_from_distinct_non_owner_dispatcher_is_rejected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let (mut dispatcher_a, mut dispatcher_b, sessions) =
            make_two_dispatchers_sharing_context(config);

        let token =
            create_session_with_owner(&mut dispatcher_a, &sessions, "sess-owned-by-tui-A", "tui-A")
                .await;

        dispatcher_b.set_tui_id_for_test(Some("tui-B".to_string()));
        let result = dispatcher_b
            .handle_session_cancel(&json!({
                "session_id": "sess-owned-by-tui-A",
            }))
            .await;

        let err = result.expect_err(
            "a cancel from a dispatcher whose tui_id does not match the \
             session's owner_tui_id must be refused",
        );
        assert_ne!(
            err.code, SESSION_NOT_FOUND,
            "the rejection must NOT be reported as SESSION_NOT_FOUND — the \
             session DOES exist; reporting NOT_FOUND would hide the \
             ownership violation behind a benign-looking error"
        );
        assert!(
            !token.is_cancelled(),
            "the owner's cancel token must remain un-fired — the rightful \
             owner's turn must survive a mis-targeted cancel from another TUI"
        );
    }

    #[tokio::test]
    async fn session_cancel_from_anonymous_dispatcher_is_rejected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let (mut dispatcher_a, mut dispatcher_b, sessions) =
            make_two_dispatchers_sharing_context(config);

        let token =
            create_session_with_owner(&mut dispatcher_a, &sessions, "sess-owned-by-tui-A", "tui-A")
                .await;

        // dispatcher_b never set its tui_id — fresh connection, no
        // initialize handshake yet.
        dispatcher_b.set_tui_id_for_test(None);
        let result = dispatcher_b
            .handle_session_cancel(&json!({
                "session_id": "sess-owned-by-tui-A",
            }))
            .await;

        let err = result.expect_err("anonymous cancel must be refused");
        assert_ne!(err.code, SESSION_NOT_FOUND);
        assert!(
            !token.is_cancelled(),
            "anonymous cancel must not fire the token"
        );
    }

    #[tokio::test]
    async fn session_cancel_from_owner_dispatcher_still_works() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = make_acp_test_config(&tmp);
        let (mut dispatcher_a, _dispatcher_b, sessions) =
            make_two_dispatchers_sharing_context(config);

        let token =
            create_session_with_owner(&mut dispatcher_a, &sessions, "sess-owned-by-tui-A", "tui-A")
                .await;

        // Same dispatcher, same tui_id that created the session.
        let result = dispatcher_a
            .handle_session_cancel(&json!({
                "session_id": "sess-owned-by-tui-A",
            }))
            .await;

        assert!(
            result.is_ok(),
            "owner cancel must succeed; got: {:?}",
            result.err()
        );
        assert!(
            token.is_cancelled(),
            "owner cancel must fire the session's cancel token"
        );
    }

    // ── Missing-session regression: close / delete must not fabricate
    //    session_end for sessions that never existed ──────────────────

    struct EndCountingHook {
        end_count: Arc<std::sync::atomic::AtomicU32>,
    }

    impl EndCountingHook {
        fn new() -> (Self, Arc<std::sync::atomic::AtomicU32>) {
            let count = Arc::new(std::sync::atomic::AtomicU32::new(0));
            (
                Self {
                    end_count: count.clone(),
                },
                count,
            )
        }
    }

    #[async_trait]
    impl crate::hooks::HookHandler for EndCountingHook {
        fn name(&self) -> &str {
            "end-counter"
        }
        fn priority(&self) -> i32 {
            0
        }
        async fn on_session_end(&self, _session_id: &str, _channel: &str) {
            self.end_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn session_close_missing_session_does_not_fire_session_end() {
        let queue = Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
            4, 10, 60,
        ));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let mut runner = crate::hooks::HookRunner::new();
        let (_hook, end_count) = EndCountingHook::new();
        runner.register(Box::new(_hook));
        let ctx = Arc::new(crate::rpc::context::RpcContext {
            config: Arc::new(parking_lot::RwLock::new(
                zeroclaw_config::schema::Config::default(),
            )),
            sessions: Arc::clone(&sessions),
            session_backend: None,
            memory: None,
            cost_tracker: None,
            event_tx: None,
            reload_tx: None,
            gateway_shutdown_tx: None,
            approval_pending: Arc::new(crate::rpc::context::ApprovalPendingMap::default()),
            tui_registry: Arc::new(crate::rpc::tui_identity::TuiRegistry::new_unsigned()),
            acp_session_store: None,
            sop_engine: None,
            sop_audit: None,
            hooks: Some(Arc::new(runner)),
        });
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let dispatcher = RpcDispatcher::new(ctx, tx, "test-peer-close:pid=1".into());

        let result = dispatcher
            .handle_session_close(&serde_json::json!({"session_id": "ghost-close"}))
            .await;
        assert!(result.is_err(), "close on nonexistent session must error");

        assert_eq!(
            end_count.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "session_end must not fire when close targets a missing session"
        );
    }

    #[tokio::test]
    async fn session_delete_missing_session_does_not_fire_session_end() {
        let queue = Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
            4, 10, 60,
        ));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let mut runner = crate::hooks::HookRunner::new();
        let (_hook, end_count) = EndCountingHook::new();
        runner.register(Box::new(_hook));
        let ctx = Arc::new(crate::rpc::context::RpcContext {
            config: Arc::new(parking_lot::RwLock::new(
                zeroclaw_config::schema::Config::default(),
            )),
            sessions: Arc::clone(&sessions),
            session_backend: None,
            memory: None,
            cost_tracker: None,
            event_tx: None,
            reload_tx: None,
            gateway_shutdown_tx: None,
            approval_pending: Arc::new(crate::rpc::context::ApprovalPendingMap::default()),
            tui_registry: Arc::new(crate::rpc::tui_identity::TuiRegistry::new_unsigned()),
            acp_session_store: None,
            sop_engine: None,
            sop_audit: None,
            hooks: Some(Arc::new(runner)),
        });
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let dispatcher = RpcDispatcher::new(ctx, tx, "test-peer-delete:pid=1".into());

        let result = dispatcher
            .handle_session_delete(&serde_json::json!({"session_id": "ghost-delete"}))
            .await;
        assert!(
            result.is_ok(),
            "delete on nonexistent session should succeed"
        );

        assert_eq!(
            end_count.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "session_end must not fire when delete targets a missing session"
        );
    }

    // ── Positive lifecycle regression: close on a real session must fire
    //    session_end so that configured hooks observe RPC lifecycles ──

    struct DummyModelProvider;

    #[async_trait]
    impl zeroclaw_api::model_provider::ModelProvider for DummyModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }
    }

    impl zeroclaw_api::attribution::Attributable for DummyModelProvider {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Provider(
                zeroclaw_api::attribution::ProviderKind::Model(
                    zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "dummy"
        }
    }

    #[tokio::test]
    async fn session_close_real_session_fires_session_end_hook() {
        let queue = Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
            4, 10, 60,
        ));
        let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
        let sid = "real-session-close-hook".to_string();

        // Build a minimal agent and insert it into the store so the
        // dispatcher sees a live session.
        let agent = crate::agent::agent::Agent::builder()
            .model_provider(Box::new(DummyModelProvider))
            .tools(vec![])
            .memory(Arc::new(zeroclaw_memory::NoneMemory::new("none")))
            .observer(Arc::new(crate::observability::noop::NoopObserver))
            .tool_dispatcher(Box::new(crate::agent::dispatcher::NativeToolDispatcher))
            .workspace_dir(std::env::temp_dir())
            .build()
            .expect("minimal Agent should build");
        let rpc_session = crate::rpc::session::RpcSession::new(
            agent,
            "test-agent",
            std::env::temp_dir().to_str().unwrap(),
            crate::rpc::types::ChatMode::Chat,
        );
        sessions.insert(sid.clone(), rpc_session).await.unwrap();

        // Wire a counting hook.
        let mut runner = crate::hooks::HookRunner::new();
        let (_hook, end_count) = EndCountingHook::new();
        runner.register(Box::new(_hook));

        let ctx = Arc::new(crate::rpc::context::RpcContext {
            config: Arc::new(parking_lot::RwLock::new(
                zeroclaw_config::schema::Config::default(),
            )),
            sessions: Arc::clone(&sessions),
            session_backend: None,
            memory: None,
            cost_tracker: None,
            event_tx: None,
            reload_tx: None,
            gateway_shutdown_tx: None,
            approval_pending: Arc::new(crate::rpc::context::ApprovalPendingMap::default()),
            tui_registry: Arc::new(crate::rpc::tui_identity::TuiRegistry::new_unsigned()),
            acp_session_store: None,
            sop_engine: None,
            sop_audit: None,
            hooks: Some(Arc::new(runner)),
        });
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let dispatcher = RpcDispatcher::new(ctx, tx, "test-peer-real-close:pid=1".into());

        // Close the real session.
        let result = dispatcher
            .handle_session_close(&serde_json::json!({"session_id": sid}))
            .await;
        assert!(result.is_ok(), "close on real session must succeed");

        assert_eq!(
            end_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "session_end must fire when a real session is closed"
        );
    }
}
