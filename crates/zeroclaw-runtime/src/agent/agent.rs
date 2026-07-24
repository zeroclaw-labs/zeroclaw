use crate::agent::dispatcher::{NativeToolDispatcher, ToolDispatcher, XmlToolDispatcher};
use crate::agent::eval::AutoClassifyExt;
use crate::agent::prompt::{PromptContext, SystemPromptBuilder};
use crate::approval::ApprovalManager;
use crate::observability::{self, Observer, ObserverEvent};
use crate::platform;
use crate::security::SecurityPolicy;
use crate::sop::{SopAuditLogger, SopEngine};
use crate::tools::{self, Tool};
use anyhow::{Context, Result};
use chrono::{Datelike, Timelike};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use zeroclaw_config::schema::Config;
use zeroclaw_memory::{self, Memory, MemoryCategory};
#[cfg(test)]
use zeroclaw_providers::ChatRequest;
use zeroclaw_providers::{
    self, ChatMessage, ConversationMessage, ModelProvider, ToolResultMessage,
};

// Re-export TurnEvent from zeroclaw-types for backwards compatibility.
pub use zeroclaw_api::agent::TurnEvent;

pub fn build_session_model_provider(
    config: &Config,
    model_provider_ref: &str,
    model_override: Option<&str>,
) -> Result<(Box<dyn ModelProvider>, String, String)> {
    let (model_provider_name, model_provider_alias) = model_provider_ref
        .split_once('.')
        .map(|(t, a)| (t.to_string(), a.to_string()))
        .ok_or_else(|| {
            anyhow::Error::msg(format!(
                "model_provider reference `{model_provider_ref}` must be `<type>.<alias>`"
            ))
        })?;

    let entry = config
        .providers
        .models
        .find(&model_provider_name, &model_provider_alias);
    let model_name = model_override
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .or_else(|| {
            entry
                .and_then(|e| e.model.as_deref())
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(str::to_string)
        })
        .ok_or_else(|| {
            anyhow::Error::msg(format!(
                "model_provider `{model_provider_ref}` has no `model` configured and no model \
                 override was supplied"
            ))
        })?;

    let model_provider_runtime_options = zeroclaw_providers::provider_runtime_options_for_alias(
        config,
        &model_provider_name,
        &model_provider_alias,
    );

    let model_provider = zeroclaw_providers::create_routed_model_provider_with_options(
        config,
        model_provider_ref,
        entry.and_then(|e| e.api_key.as_deref()),
        entry.and_then(|e| e.uri.as_deref()),
        &config.reliability,
        &config.model_routes,
        &model_name,
        &model_provider_runtime_options,
    )?;

    Ok((model_provider, model_provider_name, model_name))
}

/// Resolve the tool dispatcher with the same provider-capability fallback
/// used by fresh agent construction.
#[must_use]
pub fn tool_dispatcher_for_provider(
    agent_cfg: &zeroclaw_config::schema::AliasedAgentConfig,
    model_provider: &dyn ModelProvider,
) -> Box<dyn ToolDispatcher> {
    match agent_cfg.resolved.tool_dispatcher.as_str() {
        "native" => Box::new(NativeToolDispatcher),
        "xml" => Box::new(XmlToolDispatcher),
        _ if model_provider.supports_native_tools() => Box::new(NativeToolDispatcher),
        _ => Box::new(XmlToolDispatcher),
    }
}

pub(crate) enum RoutedApproval {
    /// Use this response. `decider` names the channel that answered, for audit
    /// attribution; `None` for a bridge-synthesized fail-closed deny.
    Decided {
        response: zeroclaw_api::channel::ChannelApprovalResponse,
        decider: Option<String>,
    },
    /// Explicit `InheritOriginator` — defer to the originating-channel fan-out.
    Fallthrough,
}

pub(crate) async fn resolve_routed_approval(
    handles: &tools::PerToolChannelHandle,
    route: &zeroclaw_config::autonomy::ApprovalRoute,
    recipient: &str,
    request: &zeroclaw_api::channel::ChannelApprovalRequest,
) -> RoutedApproval {
    let approver: Option<(String, Arc<dyn zeroclaw_api::channel::Channel>)> = handles
        .read()
        .iter()
        .find(|(name, _)| name.as_str() == route.approver_channel)
        .map(|(name, channel)| (name.clone(), Arc::clone(channel)));

    let reason: &str = if let Some((channel_name, channel)) = approver {
        let dur = std::time::Duration::from_secs(route.timeout_secs.max(1));
        match tokio::time::timeout(dur, channel.request_approval(recipient, request)).await {
            Ok(Ok(Some(response))) => {
                return RoutedApproval::Decided {
                    response,
                    decider: Some(channel_name),
                };
            }
            Ok(Ok(None)) => "approver returned no decision",
            Ok(Err(_)) => "approver channel unreachable",
            Err(_) => "approver timed out",
        }
    } else {
        "approver channel not registered"
    };

    match route.on_no_approver {
        zeroclaw_config::autonomy::OnNoApprover::Deny => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "tool": request.tool_name,
                        "approver_channel": route.approver_channel,
                        "reason": reason,
                        "policy": "deny",
                    })),
                "approval route fail-closed: denying gated tool"
            );
            RoutedApproval::Decided {
                response: zeroclaw_api::channel::ChannelApprovalResponse::Deny,
                decider: None,
            }
        }
        zeroclaw_config::autonomy::OnNoApprover::InheritOriginator => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "tool": request.tool_name,
                        "approver_channel": route.approver_channel,
                        "reason": reason,
                        "policy": "inherit-originator",
                    })),
                "approval route falling back to originating channel"
            );
            RoutedApproval::Fallthrough
        }
    }
}

pub(crate) struct RoutedApprovalChannel {
    handles: tools::PerToolChannelHandle,
    route: zeroclaw_config::autonomy::ApprovalRoute,
}

impl RoutedApprovalChannel {
    pub(crate) fn new(
        handles: tools::PerToolChannelHandle,
        route: zeroclaw_config::autonomy::ApprovalRoute,
    ) -> Self {
        Self { handles, route }
    }
}

impl ::zeroclaw_api::attribution::Attributable for RoutedApprovalChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(::zeroclaw_api::attribution::ChannelKind::Cli)
    }
    fn alias(&self) -> &str {
        "approval-route"
    }
}

#[async_trait::async_trait]
impl zeroclaw_api::channel::Channel for RoutedApprovalChannel {
    fn name(&self) -> &str {
        "approval-route"
    }

    async fn send(&self, _message: &zeroclaw_api::channel::SendMessage) -> anyhow::Result<()> {
        Ok(())
    }

    async fn listen(
        &self,
        _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Non-attributed entry point: delegates to
    /// [`Self::request_approval_attributed`] and drops the attribution so the
    /// routing decision lives in exactly one place.
    async fn request_approval(
        &self,
        recipient: &str,
        request: &zeroclaw_api::channel::ChannelApprovalRequest,
    ) -> anyhow::Result<Option<zeroclaw_api::channel::ChannelApprovalResponse>> {
        Ok(self
            .request_approval_attributed(recipient, request)
            .await?
            .map(|attributed| attributed.response))
    }

    async fn request_approval_attributed(
        &self,
        recipient: &str,
        request: &zeroclaw_api::channel::ChannelApprovalRequest,
    ) -> anyhow::Result<Option<zeroclaw_api::channel::AttributedApprovalResponse>> {
        match resolve_routed_approval(&self.handles, &self.route, recipient, request).await {
            // The deciding approver's name travels on the response itself;
            // `None` for a bridge-synthesized fail-closed deny.
            RoutedApproval::Decided { response, decider } => {
                Ok(Some(zeroclaw_api::channel::AttributedApprovalResponse {
                    response,
                    decided_by: decider,
                }))
            }
            // No originating channel to inherit on this path; let the gate apply
            // the non-interactive default (auto-deny).
            RoutedApproval::Fallthrough => Ok(None),
        }
    }
}

#[derive(Debug)]
struct HistoryTrimNotice {
    dropped_messages: usize,
    kept_turns: usize,
    reason: String,
}

impl HistoryTrimNotice {
    fn into_turn_event(self) -> TurnEvent {
        TurnEvent::HistoryTrimmed {
            dropped_messages: self.dropped_messages,
            kept_turns: self.kept_turns,
            reason: self.reason,
        }
    }
}

async fn forward_history_trim_notice(
    event_tx: &tokio::sync::mpsc::Sender<TurnEvent>,
    notice: Option<HistoryTrimNotice>,
) {
    if let Some(notice) = notice {
        let _ = event_tx.send(notice.into_turn_event()).await;
    }
}

pub struct Agent {
    model_provider: Box<dyn ModelProvider>,
    /// Sealed per-agent tool set. Stored as a [`crate::tools::scoped::ScopedToolRegistry`]
    /// so it can only be handed to the turn engine after passing through
    /// `assemble()` (the seal).
    tools: crate::tools::scoped::ScopedToolRegistry,
    memory: Arc<dyn Memory>,
    observer: Arc<dyn Observer>,
    prompt_builder: SystemPromptBuilder,
    tool_dispatcher: Box<dyn ToolDispatcher>,
    /// Stable half of the engine's memory-context injection policy
    /// (recall limit, relevance floor, budgets). Threaded into `ToolLoop`
    /// as `TurnMemory.cfg` on every turn.
    memory_inject_cfg: crate::agent::memory_inject::MemoryInjectConfig,
    config: zeroclaw_config::schema::AliasedAgentConfig,
    /// Resolves the structured-history cap from canonical config at use time.
    /// Daemon-backed sessions capture the shared live config handle so reloads
    /// affect existing sessions without duplicating config-derived state.
    structured_history_cap_resolver: Option<Arc<dyn Fn() -> usize + Send + Sync>>,
    multimodal_config: zeroclaw_config::schema::MultimodalConfig,
    model_name: String,
    model_provider_name: String,
    temperature: Option<f64>,
    workspace_dir: std::path::PathBuf,
    /// Per-agent persona workspace (`<install>/agents/<alias>/workspace/`).
    /// Holds IDENTITY.md / SOUL.md / USER.md / AGENTS.md. Distinct from
    /// `workspace_dir`, which is the security sandbox root and can be the
    /// session cwd for IDE-driven sessions (ACP, gateway WS).
    agent_workspace_dir: std::path::PathBuf,
    identity_config: zeroclaw_config::schema::IdentityConfig,
    skills: Vec<crate::skills::Skill>,
    skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode,
    auto_save: bool,
    memory_session_id: Option<String>,
    history: Vec<ConversationMessage>,
    /// True only when `history` contains the synthetic trim breadcrumb inserted
    /// by this Agent. User text is never inferred to be synthetic by content.
    history_has_trim_breadcrumb: bool,
    classification_config: zeroclaw_config::schema::QueryClassificationConfig,
    available_hints: Vec<String>,
    route_model_by_hint: HashMap<String, String>,
    response_cache: Option<Arc<zeroclaw_memory::response_cache::ResponseCache>>,
    /// Pre-rendered security policy summary injected into the system prompt
    /// so the LLM knows the concrete constraints before making tool calls.
    security_summary: Option<String>,
    /// Autonomy level from config; controls safety prompt instructions.
    autonomy_level: crate::security::AutonomyLevel,
    /// Cross-channel HITL: resolved from the active risk profile's
    /// `approval_route`. When set, the per-turn approval bridge asks the named
    /// approver channel (bounded + fail-closed) instead of the originating
    /// fan-out. `None` ⇒ today's behavior. See EPIC B.
    approval_route: Option<zeroclaw_config::autonomy::ApprovalRoute>,
    /// Activated MCP tools for deferred loading mode.
    /// When MCP deferred loading is enabled, tools are activated via `tool_search`
    /// and stored here for lookup during tool execution.
    activated_tools: Option<Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    /// Pre-rendered MCP pinned-resource system-prompt section, read once at
    /// construction from each server's `pinned_resources` and provenance-wrapped
    /// (`trust="untrusted-external"`). Empty when no pins are configured or all
    /// were skipped. Appended to the system prompt in `build_system_prompt`.
    mcp_pinned_section: String,
    mcp_deferred_section: String,
    /// Hook runner for tool-call auditing and lifecycle side effects.
    hook_runner: Option<Arc<crate::hooks::HookRunner>>,
    /// Approval manager for direct Agent execution paths such as ACP.
    approval_manager: Option<Arc<ApprovalManager>>,
    /// Agent alias, retained for opening attribution spans at external turn
    /// call sites (ACP, gateway WS) where the alias is otherwise unavailable.
    agent_alias: String,
    channel_handles: AgentChannelHandles,
    /// Per-session cache for resolved local image data URIs, threaded into
    /// the turn loop so each unique local image file is read + base64-encoded
    /// at most once per session even though the multimodal pipeline re-walks
    /// the full conversation history on every turn and tool iteration.
    image_cache: zeroclaw_providers::multimodal::LocalImageCache,
    provider_switch_config: Option<ProviderSwitchConfig>,
    /// Channel name stamped onto observer events to identify the calling surface
    /// (e.g. "agent", "wss", "gateway"). Defaults to "agent" for direct Agent callers.
    channel_name: String,
    #[cfg(test)]
    turn_datetime: Option<Arc<dyn Fn() -> chrono::DateTime<chrono::Local> + Send + Sync>>,
}

impl Drop for Agent {
    fn drop(&mut self) {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_category(::zeroclaw_log::EventCategory::Agent)
                .with_attrs(::serde_json::json!({
                    "model_provider": self.model_provider_name,
                    "model": self.model_name,
                    "history_messages_freed": self.history.len(),
                })),
            "Agent dropped; conversation history and per-session state freed"
        );
    }
}

#[derive(Debug)]
pub struct StreamedTurnSuccess {
    pub response: String,
    pub new_messages: Vec<ConversationMessage>,
}

#[derive(Debug)]
pub struct StreamedTurnError {
    pub error: anyhow::Error,
    pub committed_response: String,
    pub new_messages: Vec<ConversationMessage>,
}

#[derive(Clone, Debug, Default)]
pub struct ProviderSwitchConfig {
    pub config: Option<std::sync::Arc<zeroclaw_config::schema::Config>>,
}

/// Bundle of late-bound channel-map handles owned by an Agent. Cloning is
/// cheap (Arc clones); the underlying maps are shared with the live tools.
#[derive(Clone, Default)]
pub struct AgentChannelHandles {
    pub ask_user: Option<tools::PerToolChannelHandle>,
    pub channel_room: Option<tools::PerToolChannelHandle>,
    pub reaction: tools::PerToolChannelHandle,
    pub poll: Option<tools::PerToolChannelHandle>,
    pub escalate: Option<tools::PerToolChannelHandle>,
}

impl AgentChannelHandles {
    /// Return references to all populated per-tool channel handles.
    fn populated_handles(&self) -> Vec<Option<&tools::PerToolChannelHandle>> {
        vec![
            self.ask_user.as_ref(),
            self.channel_room.as_ref(),
            Some(&self.reaction),
            self.poll.as_ref(),
            self.escalate.as_ref(),
        ]
    }

    /// Register a channel into every populated handle so all channel-driven
    /// tools can resolve it by name.
    pub fn register_channel(
        &self,
        name: impl Into<String>,
        channel: Arc<dyn zeroclaw_api::channel::Channel>,
    ) {
        let name = name.into();
        for handle in self.populated_handles().into_iter().flatten() {
            handle.write().insert(name.clone(), Arc::clone(&channel));
        }
    }

    /// Remove a channel from every populated handle (used on session/stop).
    pub fn unregister_channel(&self, name: &str) {
        for handle in self.populated_handles().into_iter().flatten() {
            handle.write().remove(name);
        }
    }

    /// Look up a registered channel by name from any populated channel map.
    pub fn get_channel(&self, name: &str) -> Option<Arc<dyn zeroclaw_api::channel::Channel>> {
        for handle in self.populated_handles().into_iter().flatten() {
            if let Some(channel) = handle.read().get(name) {
                return Some(Arc::clone(channel));
            }
        }
        None
    }
}

pub struct AgentBuilder {
    model_provider: Option<Box<dyn ModelProvider>>,
    tools: Option<crate::tools::scoped::ScopedToolRegistry>,
    memory: Option<Arc<dyn Memory>>,
    observer: Option<Arc<dyn Observer>>,
    prompt_builder: Option<SystemPromptBuilder>,
    tool_dispatcher: Option<Box<dyn ToolDispatcher>>,
    memory_inject_cfg: Option<crate::agent::memory_inject::MemoryInjectConfig>,
    config: Option<zeroclaw_config::schema::AliasedAgentConfig>,
    structured_history_cap_resolver: Option<Arc<dyn Fn() -> usize + Send + Sync>>,
    multimodal_config: Option<zeroclaw_config::schema::MultimodalConfig>,
    model_name: Option<String>,
    model_provider_name: Option<String>,
    temperature: Option<f64>,
    workspace_dir: Option<std::path::PathBuf>,
    agent_workspace_dir: Option<std::path::PathBuf>,
    identity_config: Option<zeroclaw_config::schema::IdentityConfig>,
    skills: Option<Vec<crate::skills::Skill>>,
    skills_prompt_mode: Option<zeroclaw_config::schema::SkillsPromptInjectionMode>,
    auto_save: Option<bool>,
    memory_session_id: Option<String>,
    classification_config: Option<zeroclaw_config::schema::QueryClassificationConfig>,
    available_hints: Option<Vec<String>>,
    route_model_by_hint: Option<HashMap<String, String>>,
    allowed_tools: Option<Vec<String>>,
    response_cache: Option<Arc<zeroclaw_memory::response_cache::ResponseCache>>,
    security_summary: Option<String>,
    autonomy_level: Option<crate::security::AutonomyLevel>,
    approval_route: Option<zeroclaw_config::autonomy::ApprovalRoute>,
    activated_tools: Option<Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    mcp_pinned_section: Option<String>,
    mcp_deferred_section: Option<String>,
    hook_runner: Option<Arc<crate::hooks::HookRunner>>,
    approval_manager: Option<Arc<ApprovalManager>>,
    agent_alias: Option<String>,
    channel_name: Option<String>,
    exclude_memory: bool,
    provider_switch_config: Option<ProviderSwitchConfig>,
    #[cfg(test)]
    turn_datetime: Option<Arc<dyn Fn() -> chrono::DateTime<chrono::Local> + Send + Sync>>,
}

impl Default for AgentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentBuilder {
    pub fn new() -> Self {
        Self {
            model_provider: None,
            tools: None,
            memory: None,
            observer: None,
            prompt_builder: None,
            tool_dispatcher: None,
            memory_inject_cfg: None,
            config: None,
            structured_history_cap_resolver: None,
            multimodal_config: None,
            model_name: None,
            model_provider_name: None,
            temperature: None,
            workspace_dir: None,
            agent_workspace_dir: None,
            identity_config: None,
            skills: None,
            skills_prompt_mode: None,
            auto_save: None,
            memory_session_id: None,
            classification_config: None,
            available_hints: None,
            route_model_by_hint: None,
            allowed_tools: None,
            response_cache: None,
            security_summary: None,
            autonomy_level: None,
            approval_route: None,
            activated_tools: None,
            mcp_pinned_section: None,
            mcp_deferred_section: None,
            hook_runner: None,
            approval_manager: None,
            agent_alias: None,
            channel_name: None,
            exclude_memory: false,
            provider_switch_config: None,
            #[cfg(test)]
            turn_datetime: None,
        }
    }

    pub fn model_provider(mut self, model_provider: Box<dyn ModelProvider>) -> Self {
        self.model_provider = Some(model_provider);
        self
    }

    pub fn tools(mut self, tools: crate::tools::scoped::ScopedToolRegistry) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn memory(mut self, memory: Arc<dyn Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn observer(mut self, observer: Arc<dyn Observer>) -> Self {
        self.observer = Some(observer);
        self
    }

    pub fn prompt_builder(mut self, prompt_builder: SystemPromptBuilder) -> Self {
        self.prompt_builder = Some(prompt_builder);
        self
    }

    pub fn tool_dispatcher(mut self, tool_dispatcher: Box<dyn ToolDispatcher>) -> Self {
        self.tool_dispatcher = Some(tool_dispatcher);
        self
    }

    /// Stable half of the engine's memory-context injection policy. When
    /// unset, defaults preserve the legacy loader shape (recall limit 5,
    /// the schema-default relevance floor).
    pub fn memory_inject_cfg(
        mut self,
        cfg: crate::agent::memory_inject::MemoryInjectConfig,
    ) -> Self {
        self.memory_inject_cfg = Some(cfg);
        self
    }

    pub fn config(mut self, config: zeroclaw_config::schema::AliasedAgentConfig) -> Self {
        self.config = Some(config);
        self
    }

    fn structured_history_cap_resolver(
        mut self,
        resolver: Arc<dyn Fn() -> usize + Send + Sync>,
    ) -> Self {
        self.structured_history_cap_resolver = Some(resolver);
        self
    }

    #[cfg(test)]
    fn structured_max_history_messages(self, max: usize) -> Self {
        self.structured_history_cap_resolver(Arc::new(move || max))
    }

    pub fn multimodal_config(
        mut self,
        multimodal_config: zeroclaw_config::schema::MultimodalConfig,
    ) -> Self {
        self.multimodal_config = Some(multimodal_config);
        self
    }

    pub fn model_name(mut self, model_name: String) -> Self {
        self.model_name = Some(model_name);
        self
    }

    pub fn model_provider_name(mut self, name: String) -> Self {
        self.model_provider_name = Some(name);
        self
    }

    pub fn temperature(mut self, temperature: Option<f64>) -> Self {
        self.temperature = temperature;
        self
    }

    pub fn workspace_dir(mut self, workspace_dir: std::path::PathBuf) -> Self {
        self.workspace_dir = Some(workspace_dir);
        self
    }

    pub fn agent_workspace_dir(mut self, agent_workspace_dir: std::path::PathBuf) -> Self {
        self.agent_workspace_dir = Some(agent_workspace_dir);
        self
    }

    pub fn identity_config(
        mut self,
        identity_config: zeroclaw_config::schema::IdentityConfig,
    ) -> Self {
        self.identity_config = Some(identity_config);
        self
    }

    pub fn skills(mut self, skills: Vec<crate::skills::Skill>) -> Self {
        self.skills = Some(skills);
        self
    }

    pub fn skills_prompt_mode(
        mut self,
        skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode,
    ) -> Self {
        self.skills_prompt_mode = Some(skills_prompt_mode);
        self
    }

    pub fn auto_save(mut self, auto_save: bool) -> Self {
        self.auto_save = Some(auto_save);
        self
    }

    pub fn memory_session_id(mut self, memory_session_id: Option<String>) -> Self {
        self.memory_session_id = memory_session_id;
        self
    }

    pub fn classification_config(
        mut self,
        classification_config: zeroclaw_config::schema::QueryClassificationConfig,
    ) -> Self {
        self.classification_config = Some(classification_config);
        self
    }

    pub fn available_hints(mut self, available_hints: Vec<String>) -> Self {
        self.available_hints = Some(available_hints);
        self
    }

    pub fn route_model_by_hint(mut self, route_model_by_hint: HashMap<String, String>) -> Self {
        self.route_model_by_hint = Some(route_model_by_hint);
        self
    }

    pub fn allowed_tools(mut self, allowed_tools: Option<Vec<String>>) -> Self {
        self.allowed_tools = allowed_tools;
        self
    }

    pub fn response_cache(
        mut self,
        cache: Option<Arc<zeroclaw_memory::response_cache::ResponseCache>>,
    ) -> Self {
        self.response_cache = cache;
        self
    }

    pub fn security_summary(mut self, summary: Option<String>) -> Self {
        self.security_summary = summary;
        self
    }

    pub fn autonomy_level(mut self, level: crate::security::AutonomyLevel) -> Self {
        self.autonomy_level = Some(level);
        self
    }

    pub fn approval_route(
        mut self,
        route: Option<zeroclaw_config::autonomy::ApprovalRoute>,
    ) -> Self {
        self.approval_route = route;
        self
    }

    pub fn activated_tools(
        mut self,
        activated: Option<Arc<std::sync::Mutex<tools::ActivatedToolSet>>>,
    ) -> Self {
        self.activated_tools = activated;
        self
    }

    pub fn mcp_pinned_section(mut self, section: Option<String>) -> Self {
        self.mcp_pinned_section = section;
        self
    }

    pub fn mcp_deferred_section(mut self, section: Option<String>) -> Self {
        self.mcp_deferred_section = section;
        self
    }

    pub fn hook_runner(mut self, runner: Option<Arc<crate::hooks::HookRunner>>) -> Self {
        self.hook_runner = runner;
        self
    }

    pub fn approval_manager(mut self, manager: Option<Arc<ApprovalManager>>) -> Self {
        self.approval_manager = manager;
        self
    }

    /// Set the agent alias used for turn-span attribution.
    pub fn agent_alias(mut self, alias: String) -> Self {
        self.agent_alias = Some(alias);
        self
    }

    pub fn channel_name(mut self, name: String) -> Self {
        self.channel_name = Some(name);
        self
    }

    #[cfg(test)]
    fn turn_datetime<F>(mut self, provider: F) -> Self
    where
        F: Fn() -> chrono::DateTime<chrono::Local> + Send + Sync + 'static,
    {
        self.turn_datetime = Some(Arc::new(provider));
        self
    }

    pub fn exclude_memory(mut self, exclude: bool) -> Self {
        self.exclude_memory = exclude;
        self
    }

    pub fn provider_switch_config(mut self, cfg: ProviderSwitchConfig) -> Self {
        self.provider_switch_config = Some(cfg);
        self
    }

    pub fn build(self) -> Result<Agent> {
        let mut tools = self.tools.ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"missing_field": "tools"})),
                "AgentBuilder::build missing required field"
            );
            anyhow::Error::msg("tools are required")
        })?;
        let allowed = self.allowed_tools.clone();
        if let Some(ref allow_list) = allowed {
            tools.retain(|t| allow_list.iter().any(|name| name == t.name()));
        }

        // ACP sessions exclude persistent memory: strip memory tools,
        // replace the backend with NoneMemory, and force auto_save off.
        let exclude_memory = self.exclude_memory;
        if exclude_memory {
            tools.retain(|t| !zeroclaw_tools::MEMORY_TOOL_NAMES.contains(&t.name()));
        }

        let memory: Arc<dyn Memory> = if exclude_memory {
            Arc::new(zeroclaw_memory::NoneMemory::new("none"))
        } else {
            self.memory.ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"missing_field": "memory"})),
                    "AgentBuilder::build missing required field"
                );
                anyhow::Error::msg("memory is required")
            })?
        };
        let config = self.config.unwrap_or_default();

        Ok(Agent {
            model_provider: self.model_provider.ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"missing_field": "model_provider"})),
                    "AgentBuilder::build missing required field"
                );
                anyhow::Error::msg("model_provider is required")
            })?,
            tools,
            memory: memory.clone(),
            observer: self.observer.ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"missing_field": "observer"})),
                    "AgentBuilder::build missing required field"
                );
                anyhow::Error::msg("observer is required")
            })?,
            prompt_builder: self
                .prompt_builder
                .unwrap_or_else(SystemPromptBuilder::with_defaults),
            tool_dispatcher: self.tool_dispatcher.ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"missing_field": "tool_dispatcher"})),
                    "AgentBuilder::build missing required field"
                );
                anyhow::Error::msg("tool_dispatcher is required")
            })?,
            memory_inject_cfg: self.memory_inject_cfg.unwrap_or_else(|| {
                crate::agent::memory_inject::MemoryInjectConfig::from_memory_config(
                    &zeroclaw_config::schema::MemoryConfig::default(),
                    crate::agent::memory_inject::DEFAULT_RECALL_LIMIT,
                )
            }),
            config,
            structured_history_cap_resolver: self.structured_history_cap_resolver,
            multimodal_config: self.multimodal_config.unwrap_or_default(),
            model_name: self.model_name.unwrap_or_else(|| "<unconfigured>".into()),
            model_provider_name: self
                .model_provider_name
                .unwrap_or_else(|| "<unconfigured>".into()),
            temperature: self.temperature,
            // Default for test callers that don't call workspace_dir().
            workspace_dir: self
                .workspace_dir
                .clone()
                .unwrap_or_else(|| std::path::PathBuf::from(".")),
            agent_workspace_dir: self.agent_workspace_dir.unwrap_or_else(|| {
                self.workspace_dir
                    .clone()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
            }),
            identity_config: self.identity_config.unwrap_or_default(),
            skills: self.skills.unwrap_or_default(),
            skills_prompt_mode: self.skills_prompt_mode.unwrap_or_default(),
            auto_save: if exclude_memory {
                false
            } else {
                self.auto_save.unwrap_or(false)
            },
            memory_session_id: self.memory_session_id,
            history: Vec::new(),
            history_has_trim_breadcrumb: false,
            classification_config: self.classification_config.unwrap_or_default(),
            available_hints: self.available_hints.unwrap_or_default(),
            route_model_by_hint: self.route_model_by_hint.unwrap_or_default(),
            response_cache: self.response_cache,
            security_summary: self.security_summary,
            approval_route: self.approval_route,
            autonomy_level: self
                .autonomy_level
                .unwrap_or(crate::security::AutonomyLevel::Supervised),
            activated_tools: self.activated_tools,
            mcp_pinned_section: self.mcp_pinned_section.unwrap_or_default(),
            mcp_deferred_section: self.mcp_deferred_section.unwrap_or_default(),
            hook_runner: self.hook_runner,
            approval_manager: self.approval_manager,
            agent_alias: self.agent_alias.unwrap_or_default(),
            channel_handles: AgentChannelHandles::default(),
            image_cache: zeroclaw_providers::multimodal::LocalImageCache::new(),
            provider_switch_config: self.provider_switch_config,
            channel_name: self.channel_name.unwrap_or_else(|| "agent".to_string()),
            #[cfg(test)]
            turn_datetime: self.turn_datetime,
        })
    }
}

impl Agent {
    pub fn builder() -> AgentBuilder {
        AgentBuilder::new()
    }

    /// The full `Config` the agent was constructed from, when available. Sourced
    /// from `provider_switch_config` - the single canonical config snapshot the
    /// agent already carries for provider-alias resolution. `None` only on
    /// configless (test-builder) agents; every production construction path
    /// (`from_config` / `from_config_with_tui_env`) populates it. Used by the
    /// vision route to resolve the configured `vision_model_provider`'s
    /// alias-specific options (the `vision` override, endpoint URI, credentials).
    fn full_config(&self) -> Option<&zeroclaw_config::schema::Config> {
        self.provider_switch_config
            .as_ref()
            .and_then(|cfg| cfg.config.as_deref())
    }

    fn tool_loop_cost_tracking_context(&self) -> crate::agent::loop_::ToolLoopCostTrackingContext {
        if let Ok(Some(context)) =
            crate::agent::loop_::TOOL_LOOP_COST_TRACKING_CONTEXT.try_with(Clone::clone)
        {
            return context;
        }

        crate::agent::loop_::ToolLoopCostTrackingContext::usage_only()
    }

    fn current_turn_datetime(&self) -> chrono::DateTime<chrono::Local> {
        #[cfg(test)]
        if let Some(provider) = &self.turn_datetime {
            return provider();
        }

        chrono::Local::now()
    }

    pub fn set_channel_name(&mut self, name: String) {
        self.channel_name = name;
    }

    fn new_turn_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    fn observer_agent_alias(&self) -> Option<String> {
        if self.agent_alias.is_empty() {
            None
        } else {
            Some(self.agent_alias.clone())
        }
    }

    pub fn history(&self) -> &[ConversationMessage] {
        &self.history
    }

    pub fn channel_handles(&self) -> &AgentChannelHandles {
        &self.channel_handles
    }

    pub fn populate_channels(
        &self,
        channel_map: &std::collections::HashMap<String, Arc<dyn zeroclaw_api::channel::Channel>>,
    ) -> Vec<String> {
        let mut names = Vec::new();
        for (name, ch) in channel_map {
            self.channel_handles.register_channel(name, Arc::clone(ch));
            names.push(name.clone());
        }
        names
    }

    /// Attribution fields for opening a turn span at external call sites
    /// (ACP, gateway WS) so every record inside a streamed turn carries the
    /// same `agent_alias`/`model_provider`/`model` the RPC dispatch path sets.
    /// Returns `(agent_alias, model_provider, model)`.
    pub fn attribution_fields(&self) -> (String, String, String) {
        (
            self.agent_alias.clone(),
            self.model_provider_name.clone(),
            self.model_name.clone(),
        )
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
        self.history_has_trim_breadcrumb = false;
    }

    fn encode_response_cache_transcript(messages: &[ChatMessage]) -> String {
        let mut transcript = String::new();
        for message in messages.iter().filter(|message| message.role != "system") {
            transcript.push_str("role=");
            transcript.push_str(&message.role.len().to_string());
            transcript.push(':');
            transcript.push_str(&message.role);
            transcript.push_str(";content=");
            transcript.push_str(&message.content.len().to_string());
            transcript.push(':');
            transcript.push_str(&message.content);
            transcript.push('\n');
        }
        transcript
    }

    fn memory_injection_active(&self) -> bool {
        if self.memory.name() == "none" {
            return false;
        }
        matches!(
            crate::agent::memory_inject::resolve_inject_policy(
                zeroclaw_api::ingress::TurnOrigin::AgentDirect,
                self.memory_session_id.is_some(),
                false,
            ),
            crate::agent::memory_inject::InjectPolicy::Inject { .. }
        )
    }

    fn response_cache_key_for_messages(
        &self,
        messages: &[ChatMessage],
        effective_model: &str,
    ) -> Option<String> {
        // Bypass the cache when a per-turn memory preamble the key cannot see
        // will be injected downstream (see `memory_injection_active`).
        if self.temperature != Some(0.0)
            || self.response_cache.is_none()
            || self.memory_injection_active()
        {
            return None;
        }

        if messages
            .iter()
            .filter(|message| message.role != "system")
            .any(|message| message.content.contains("[IMAGE:"))
        {
            return None;
        }

        let system = messages
            .iter()
            .find(|message| message.role == "system")
            .map(|message| message.content.as_str());
        let transcript = Self::encode_response_cache_transcript(messages);

        Some(zeroclaw_memory::response_cache::ResponseCache::cache_key(
            effective_model,
            system,
            &transcript,
        ))
    }

    async fn append_streamed_user_message_to_history(
        &mut self,
        user_message: &str,
        new_msgs: &mut Vec<ConversationMessage>,
        turn_id: &str,
    ) {
        // Memory context is injected once in the engine, keyed on the
        // ingress origin (agent::memory_inject).
        if self.auto_save {
            let store_start = std::time::Instant::now();
            let store_result = self
                .memory
                .store(
                    "user_msg",
                    user_message,
                    MemoryCategory::Conversation,
                    self.memory_session_id.as_deref(),
                )
                .await;
            self.observer.record_event(&ObserverEvent::MemoryStore {
                category: MemoryCategory::Conversation.to_string(),
                backend: self.memory.name().to_string(),
                duration: store_start.elapsed(),
                success: store_result.is_ok(),
                channel: Some(self.channel_name.clone()),
                agent_alias: self.observer_agent_alias(),
                turn_id: Some(turn_id.to_string()),
            });
        }

        let now = self.current_turn_datetime().format("%Y-%m-%d %H:%M:%S %Z");
        let enriched = format!("[{now}] {user_message}");

        let user_msg = ConversationMessage::Chat(ChatMessage::user(enriched));
        new_msgs.push(user_msg.clone());
        self.history.push(user_msg);
    }

    pub fn set_memory_session_id(&mut self, session_id: Option<String>) {
        self.memory_session_id = session_id;
    }

    pub fn set_temperature(&mut self, temperature: Option<f64>) {
        self.temperature = temperature;
    }

    pub fn refresh_memory_embedder(
        &self,
        model_provider: &str,
        api_key: Option<&str>,
        model: &str,
        dimensions: usize,
    ) {
        self.memory
            .refresh_embedder(model_provider, api_key, model, dimensions);
    }

    #[cfg(test)]
    pub fn temperature_for_test(&self) -> Option<f64> {
        self.temperature
    }

    pub fn set_model_name(&mut self, model_name: String) {
        self.model_name = model_name;
    }

    pub fn set_model_provider(&mut self, model_provider: Box<dyn ModelProvider>) {
        self.model_provider = model_provider;
    }

    pub fn set_model_provider_name(&mut self, model_provider_name: String) {
        self.model_provider_name = model_provider_name;
    }

    pub fn set_tool_dispatcher(&mut self, tool_dispatcher: Box<dyn ToolDispatcher>) {
        self.tool_dispatcher = tool_dispatcher;
        self.refresh_system_prompt();
    }

    fn refresh_system_prompt(&mut self) {
        let Some(ConversationMessage::Chat(first)) = self.history.first() else {
            return;
        };
        if first.role != "system" {
            return;
        }
        if let Ok(sys) = self.build_system_prompt() {
            self.history[0] = ConversationMessage::Chat(ChatMessage::system(sys));
        }
    }

    #[cfg(test)]
    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name()).collect()
    }

    #[cfg(test)]
    pub fn system_prompt_for_test(&self) -> Result<String> {
        self.build_system_prompt()
    }

    #[cfg(test)]
    pub async fn execute_tool_for_test(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Option<anyhow::Result<zeroclaw_api::tool::ToolResult>> {
        let tool = crate::agent::tool_execution::find_tool(&self.tools, name)?;
        Some(tool.execute(args).await)
    }

    pub fn seed_history(&mut self, messages: &[ChatMessage]) {
        let _ = self.seed_history_with_event(messages);
    }

    /// Hydrate prior chat messages and return a transport event when restoring
    /// the history enforces the structured message cap.
    pub fn seed_history_with_event(&mut self, messages: &[ChatMessage]) -> Option<TurnEvent> {
        if self.history.is_empty()
            && let Ok(sys) = self.build_system_prompt()
        {
            self.history
                .push(ConversationMessage::Chat(ChatMessage::system(sys)));
        }
        for msg in messages {
            if msg.role != "system" {
                self.history.push(ConversationMessage::Chat(msg.clone()));
            }
        }
        self.trim_history(None)
            .map(HistoryTrimNotice::into_turn_event)
    }

    /// Hydrate the agent with a full `ConversationMessage` history (e.g. restored
    /// from an ACP session store). Preserves all variants including `AssistantToolCalls`
    /// and `ToolResults` — use this for ACP restore; use `seed_history` for flat
    /// channel session hydration.
    pub fn seed_conversation_history(&mut self, messages: Vec<ConversationMessage>) {
        let _ = self.seed_conversation_history_with_event(messages);
    }

    /// Hydrate structured conversation history and return a transport event
    /// when restoring the history enforces the structured message cap.
    pub fn seed_conversation_history_with_event(
        &mut self,
        messages: Vec<ConversationMessage>,
    ) -> Option<TurnEvent> {
        if self.history.is_empty()
            && let Ok(sys) = self.build_system_prompt()
        {
            self.history
                .push(ConversationMessage::Chat(ChatMessage::system(sys)));
        }
        for msg in messages {
            // Skip system messages from the seed — the system prompt is already prepended above.
            if matches!(&msg, ConversationMessage::Chat(m) if m.role == "system") {
                continue;
            }
            self.history.push(msg);
        }
        // Trim immediately so pre_len snapshots (taken before the first turn)
        // are always within the configured limit; otherwise a long restored
        // history would cause history[pre_len..] to panic after trim_history
        // shrinks the vec below pre_len during the turn.
        self.trim_history(None)
            .map(HistoryTrimNotice::into_turn_event)
    }

    pub async fn from_config(config: &Config, agent_alias: &str) -> Result<Self> {
        Self::from_config_with_session_cwd(config, agent_alias, None).await
    }

    pub async fn from_config_with_session_cwd(
        config: &Config,
        agent_alias: &str,
        session_cwd: Option<&Path>,
    ) -> Result<Self> {
        Self::from_config_with_session_cwd_and_mcp(config, agent_alias, session_cwd, true).await
    }

    pub async fn from_config_with_session_cwd_and_mcp(
        config: &Config,
        agent_alias: &str,
        session_cwd: Option<&Path>,
        initialize_mcp: bool,
    ) -> Result<Self> {
        Self::from_config_with_session_cwd_and_mcp_approval_mode(
            config,
            agent_alias,
            session_cwd,
            initialize_mcp,
            false,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .await
    }

    pub async fn from_config_with_session_cwd_and_mcp_backchannel(
        config: &Config,
        agent_alias: &str,
        session_cwd: Option<&Path>,
        initialize_mcp: bool,
        exclude_memory: bool,
        sop_engine: Option<Arc<std::sync::Mutex<SopEngine>>>,
        sop_audit: Option<Arc<SopAuditLogger>>,
        canvas_store: Option<tools::CanvasStore>,
    ) -> Result<Self> {
        Self::from_config_with_session_cwd_and_mcp_approval_mode(
            config,
            agent_alias,
            session_cwd,
            initialize_mcp,
            true,
            exclude_memory,
            None,
            sop_engine,
            sop_audit,
            canvas_store,
            None,
        )
        .await
    }

    /// Build a daemon-backed ACP/WS Agent whose structured-history cap follows
    /// the shared config after reloads.
    pub async fn from_live_config_with_session_cwd_and_mcp_backchannel(
        live_config: Arc<parking_lot::RwLock<Config>>,
        agent_alias: &str,
        session_cwd: Option<&Path>,
        initialize_mcp: bool,
        exclude_memory: bool,
        sop_engine: Option<Arc<std::sync::Mutex<SopEngine>>>,
        sop_audit: Option<Arc<SopAuditLogger>>,
        canvas_store: Option<tools::CanvasStore>,
    ) -> Result<Self> {
        let config = live_config.read().clone();
        Self::from_config_with_session_cwd_and_mcp_approval_mode(
            &config,
            agent_alias,
            session_cwd,
            initialize_mcp,
            true,
            exclude_memory,
            None,
            sop_engine,
            sop_audit,
            canvas_store,
            Some(live_config),
        )
        .await
    }

    /// Like [`Self::from_config_with_session_cwd_and_mcp_backchannel`] but also
    /// injects the TUI's captured shell environment so that tools like
    /// `ShellTool` inherit the user's real `PATH`, `SSH_AUTH_SOCK`, etc.
    /// rather than the daemon's stripped-down process environment.
    pub async fn from_config_with_tui_env(
        config: &Config,
        agent_alias: &str,
        session_cwd: Option<&Path>,
        initialize_mcp: bool,
        exclude_memory: bool,
        tui_env: Option<std::collections::HashMap<String, String>>,
        sop_engine: Option<Arc<std::sync::Mutex<SopEngine>>>,
        sop_audit: Option<Arc<SopAuditLogger>>,
    ) -> Result<Self> {
        Self::from_config_with_session_cwd_and_mcp_approval_mode(
            config,
            agent_alias,
            session_cwd,
            initialize_mcp,
            true,
            exclude_memory,
            tui_env,
            sop_engine,
            sop_audit,
            None,
            None,
        )
        .await
    }

    /// Build a daemon-backed TUI Agent whose structured-history cap follows
    /// the shared config after reloads.
    pub async fn from_live_config_with_tui_env(
        live_config: Arc<parking_lot::RwLock<Config>>,
        agent_alias: &str,
        session_cwd: Option<&Path>,
        initialize_mcp: bool,
        exclude_memory: bool,
        tui_env: Option<std::collections::HashMap<String, String>>,
        sop_engine: Option<Arc<std::sync::Mutex<SopEngine>>>,
        sop_audit: Option<Arc<SopAuditLogger>>,
    ) -> Result<Self> {
        let config = live_config.read().clone();
        Self::from_config_with_session_cwd_and_mcp_approval_mode(
            &config,
            agent_alias,
            session_cwd,
            initialize_mcp,
            true,
            exclude_memory,
            tui_env,
            sop_engine,
            sop_audit,
            None,
            Some(live_config),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn from_config_with_session_cwd_and_mcp_approval_mode(
        config: &Config,
        agent_alias: &str,
        session_cwd: Option<&Path>,
        initialize_mcp: bool,
        approval_backchannel: bool,
        exclude_memory: bool,
        tui_env: Option<std::collections::HashMap<String, String>>,
        sop_engine: Option<Arc<std::sync::Mutex<SopEngine>>>,
        sop_audit: Option<Arc<SopAuditLogger>>,
        canvas_store: Option<tools::CanvasStore>,
        live_config: Option<Arc<parking_lot::RwLock<Config>>>,
    ) -> Result<Self> {
        let agent_cfg = config
            .agent(agent_alias)
            .with_context(|| format!("agents.{agent_alias} is not configured"))?;
        let risk_profile = config
            .risk_profile_for_agent(agent_alias)
            .with_context(|| {
                format!(
                    "agents.{agent_alias}.risk_profile does not name a configured risk_profiles entry"
                )
            })?;

        let observer: Arc<dyn Observer> =
            Arc::from(observability::create_observer(&config.observability));
        let runtime: Arc<dyn platform::RuntimeAdapter> =
            Arc::from(platform::create_runtime(&config.runtime)?);
        // Per-agent workspace becomes the SecurityPolicy boundary
        // (file_read/write/edit + shell tool jail to the agent's own
        // dir). The session-cwd override still wins so ACP sessions
        // can pin tool path resolution to an IDE-provided cwd.
        let agent_workspace = config.agent_workspace_dir(agent_alias);
        // Create the per-agent workspace dir on demand so bootstrap
        // file writes (and downstream markdown-memory backends) don't
        // hit ENOENT on a fresh install.
        if let Err(e) = tokio::fs::create_dir_all(&agent_workspace).await {
            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"agent": agent_alias, "workspace": agent_workspace.display().to_string(), "e": e.to_string()})), "Failed to create per-agent workspace dir (continuing): ");
        }
        if let Err(e) = crate::agent::personality::seed_default_personality(
            config,
            agent_alias,
            &agent_workspace,
        )
        .await
        {
            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"agent": agent_alias, "workspace": agent_workspace.display().to_string(), "e": e.to_string()})), "Failed to ensure per-agent bootstrap files (continuing with whatever exists): ");
        }
        let security = Arc::new({
            // Use for_agent so the runtime profile (max_actions_per_hour,
            // shell_timeout_secs, etc.) is applied — from_risk_profile passes
            // None for the runtime profile and silently falls back to the
            // schema default of 20 actions/hour regardless of config.
            let mut policy = SecurityPolicy::for_agent(config, agent_alias).with_context(|| {
                format!("agents.{agent_alias}: failed to build security policy")
            })?;
            if let Some(cwd) = session_cwd {
                policy.workspace_dir = cwd.to_path_buf();
                policy.allowed_roots.push(agent_workspace.clone());
            }
            policy
        });

        let (provider_name, provider_alias, agent_model_provider) =
            match config.resolved_model_provider_for_agent(agent_alias) {
                Some(resolved) => (resolved.0, resolved.1, Some(resolved.2)),
                None => {
                    let agent_ref = agent_cfg.model_provider.as_str();
                    if !agent_ref.is_empty() {
                        anyhow::bail!(
                            "agents.{agent_alias}.model_provider = \"{agent_ref}\" does not \
                             resolve to a configured [providers.models.<type>.<alias>] entry"
                        );
                    }
                    // V3 schema requires every agent to set model_provider.
                    // Empty is a config error rather than a silent fallback.
                    anyhow::bail!(
                        "agents.{agent_alias}.model_provider is empty — set it to a \
                         configured \"<type>.<alias>\" (e.g. \"anthropic.{agent_alias}\")"
                    );
                }
            };
        let memory: Arc<dyn Memory> = zeroclaw_memory::create_memory_for_agent(
            config,
            agent_alias,
            agent_model_provider.and_then(|e| e.api_key.as_deref()),
        )
        .await?;

        let composio_key = if config.composio.enabled {
            config.composio.api_key.as_deref()
        } else {
            None
        };
        let composio_entity_id = if config.composio.enabled {
            Some(config.composio.entity_id.as_str())
        } else {
            None
        };

        // SOP loading is gated on `[sop] sops_dir`: unset disables all SOP
        // runtime behavior, matching the documented rollback path.
        // If caller provided an engine (daemon path), use it; otherwise
        // build our own (CLI/standalone path) only when the gate is set.
        let (sop_engine, sop_audit) = match (sop_engine, sop_audit) {
            (Some(engine), Some(audit)) => (Some(engine), Some(audit)),
            (None, None) if config.sop.sops_dir.is_some() => {
                let mem: Arc<dyn zeroclaw_memory::Memory> =
                    zeroclaw_memory::create_memory_for_agent(config, agent_alias, None).await?;
                // CLI / standalone path: no channel map is wired here, so the route
                // adapter is the no-op (log-only). The daemon path builds the SOP
                // engine with a real channel-delivering adapter instead.
                let (engine, audit) = crate::sop::build_sop_engine(
                    config.sop.clone(),
                    &config.data_dir,
                    mem,
                    Default::default(),
                );
                (Some(engine), Some(audit))
            }
            _ => (None, None),
        };

        let all_tools_result = tools::all_tools_with_runtime(
            Arc::new(config.clone()),
            &security,
            risk_profile,
            agent_alias,
            runtime.clone(),
            memory.clone(),
            composio_key,
            composio_entity_id,
            &config.browser,
            &config.http_request,
            &config.web_fetch,
            &security.workspace_dir,
            &config.agents,
            agent_model_provider.and_then(|e| e.api_key.as_deref()),
            config,
            canvas_store,
            false,
            tui_env,
            sop_engine,
            sop_audit,
            None,
        );
        // Skills are loaded here and handed to `assemble`, which owns skill
        // registration and resolves builtin/MCP elevation against the pre-filter
        // arcs internally. Bundle-aware via `[agents.<alias>].skill_bundles`.
        let skills = crate::skills::load_skills_for_agent_from_config(config, agent_alias);
        let assembled = crate::tools::scoped::ScopedToolRegistry::assemble(
            crate::tools::scoped::ScopedAssembly {
                config,
                agent_alias,
                security: &security,
                built: all_tools_result,
                skills: &skills,
                runtime,
                caller_allowed: None,
                connect_mcp: initialize_mcp,
                connect_peripherals: false,
                exclude_memory,
                list_deferred_mcp_specs: false,
                emit_assembly_logs: true,
                // `from_config` is the Agent (gateway / library) construction
                // path: no cross-turn reuse contract, so the per-call
                // `connect_all` is the correct choice. The daemon heartbeat
                // worker is the only `mcp_registry` supplier.
                mcp_registry: None,
            },
        )
        .await;
        // The Agent injects two distinct MCP prompt slots: `mcp_deferred_section` (the
        // deferred tool-search listing) and `mcp_pinned_section` (pinned resources).
        // `assemble` surfaces the two atomically, so from_config threads each into its
        // own slot below - no duplication, and the deferred advertisement the
        // regression suite asserts is preserved.
        let deferred_section = assembled.deferred_section().to_string();
        let pinned_section = assembled.pinned_section().to_string();
        let crate::tools::scoped::ScopedAssembled {
            registry,
            delegate_handle: _,
            ask_user_handle,
            reaction_handle,
            poll_handle,
            escalate_handle,
            channel_room_handle,
            activated_handle,
            // from_config performs no per-turn tool_filter_groups filtering
            // itself, so mcp_tool_names is dropped here along with `registry`'s
            // already-consumed sibling fields via `..`.
            ..
        } = assembled;
        // Thread the sealed registry straight to the builder - `.tools(...)` now
        // takes a `ScopedToolRegistry`, so no `into_inner()` unwrap here.
        let tools = registry;

        let model_name = match agent_model_provider
            .and_then(|e| e.model.as_deref())
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            Some(m) => m.to_string(),
            None => anyhow::bail!(
                "agents.{agent_alias}.model_provider resolves to a model_provider entry \
                 with no `model` set. Configure [providers.models.{provider_name}.<alias>] \
                 model = \"...\".",
            ),
        };

        let provider_ref = format!("{provider_name}.{provider_alias}");
        let provider_runtime_options = zeroclaw_providers::provider_runtime_options_for_alias(
            config,
            provider_name,
            provider_alias,
        );

        let model_provider: Box<dyn ModelProvider> =
            zeroclaw_providers::create_routed_model_provider_with_options(
                config,
                &provider_ref,
                agent_model_provider.and_then(|e| e.api_key.as_deref()),
                agent_model_provider.and_then(|e| e.uri.as_deref()),
                &config.reliability,
                &config.model_routes,
                &model_name,
                &provider_runtime_options,
            )?;

        let tool_dispatcher = tool_dispatcher_for_provider(agent_cfg, model_provider.as_ref());

        let route_model_by_hint: HashMap<String, String> = config
            .model_routes
            .iter()
            .map(|route| (route.hint.clone(), route.model.clone()))
            .collect();
        let available_hints: Vec<String> = route_model_by_hint.keys().cloned().collect();

        let response_cache = if config.memory.response_cache_enabled {
            zeroclaw_memory::response_cache::ResponseCache::with_hot_cache(
                &config.data_dir,
                config.memory.response_cache_ttl_minutes,
                config.memory.response_cache_max_entries,
                config.memory.response_cache_hot_entries,
            )
            .ok()
            .map(Arc::new)
        } else {
            None
        };

        let approval_manager = if approval_backchannel {
            ApprovalManager::for_non_interactive_backchannel(risk_profile)
        } else {
            ApprovalManager::for_non_interactive(risk_profile)
        };

        let structured_history_cap_resolver: Arc<dyn Fn() -> usize + Send + Sync> =
            if let Some(cap_config) = live_config {
                let cap_agent_alias = agent_alias.to_string();
                Arc::new(move || {
                    cap_config
                        .read()
                        .effective_structured_max_history_messages(&cap_agent_alias)
                })
            } else {
                let max = config.effective_structured_max_history_messages(agent_alias);
                Arc::new(move || max)
            };

        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(tools)
            .memory(memory.clone())
            .observer(observer)
            .response_cache(response_cache)
            .tool_dispatcher(tool_dispatcher)
            .memory_inject_cfg(
                crate::agent::memory_inject::MemoryInjectConfig::from_memory_config(
                    &config.memory,
                    config.effective_memory_recall_limit(agent_alias),
                ),
            )
            .prompt_builder(SystemPromptBuilder::with_defaults())
            .config(
                config
                    .resolved_agent_config(agent_alias)
                    .unwrap_or_else(|| agent_cfg.clone()),
            )
            .structured_history_cap_resolver(structured_history_cap_resolver)
            .multimodal_config(config.multimodal.clone())
            .agent_alias(agent_alias.to_string())
            .model_name(model_name)
            .model_provider_name(provider_name.to_string())
            .temperature(agent_model_provider.and_then(|e| e.temperature))
            .workspace_dir(security.workspace_dir.clone())
            .agent_workspace_dir(agent_workspace.clone())
            .classification_config(config.query_classification.clone())
            .available_hints(available_hints)
            .route_model_by_hint(route_model_by_hint)
            .identity_config(agent_cfg.identity.clone())
            .skills(skills)
            .skills_prompt_mode(config.effective_skills_prompt_mode(agent_alias))
            .auto_save(config.memory.auto_save)
            .exclude_memory(exclude_memory)
            .security_summary(Some(security.prompt_summary()))
            .autonomy_level(risk_profile.level)
            .approval_route(risk_profile.approval_route.clone())
            .activated_tools(activated_handle)
            .mcp_deferred_section(Some(deferred_section))
            .mcp_pinned_section(Some(pinned_section))
            .hook_runner(if config.hooks.enabled {
                Some(Arc::new(crate::hooks::HookRunner::from_config(
                    &config.hooks,
                )))
            } else {
                None
            })
            .approval_manager(Some(Arc::new(approval_manager)))
            .provider_switch_config(ProviderSwitchConfig {
                config: Some(std::sync::Arc::new(config.clone())),
            })
            .build()?;

        // Wire per-tool channel-map handles into the agent so callers (e.g.
        // the ACP server) can register back-channels after construction.
        agent.channel_handles = AgentChannelHandles {
            ask_user: ask_user_handle,
            channel_room: channel_room_handle,
            reaction: reaction_handle,
            poll: poll_handle,
            escalate: escalate_handle,
        };

        Ok(agent)
    }

    fn trim_history(&mut self, turn_id: Option<&str>) -> Option<HistoryTrimNotice> {
        let max = self
            .structured_history_cap_resolver
            .as_ref()
            .map_or(self.config.resolved.max_history_messages, |resolve| {
                resolve()
            });
        if self.history.len() <= max {
            return None;
        }
        let result = crate::agent::history_trim::trim_conversation_to_recent_turns(
            std::mem::take(&mut self.history),
            max,
            self.history_has_trim_breadcrumb,
        );
        self.history = result.history;
        if !result.trimmed {
            return None;
        }

        crate::agent::history_trim::insert_conversation_breadcrumb(&mut self.history);
        self.history_has_trim_breadcrumb = true;
        let reason = crate::i18n::get_required_cli_string("history-trim-reason-message-cap");
        let channel = self.channel_name.clone();
        let agent_alias = self.observer_agent_alias();
        let turn_id = turn_id.map(str::to_owned);

        {
            let scope_span = ::zeroclaw_log::info_span!(
                target: "zeroclaw_log_internal_scope",
                "zeroclaw_scope",
                agent_alias = ::zeroclaw_log::field::Empty,
                channel = %channel,
                trace_id = ::zeroclaw_log::field::Empty,
            );
            if let Some(agent_alias) = agent_alias.as_deref() {
                scope_span.record("agent_alias", agent_alias);
            }
            if let Some(turn_id) = turn_id.as_deref() {
                scope_span.record("trace_id", turn_id);
            }
            let _scope_guard = scope_span.enter();
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_attrs(::serde_json::json!({
                        "max_history_messages": max,
                        "dropped_messages": result.dropped_messages,
                        "dropped_turns": result.dropped_turns,
                        "kept_turns": result.kept_turns,
                        "remaining_messages": self.history.len(),
                    })),
                "trim_history: dropped oldest whole turns"
            );
        }

        self.observer.record_event(&ObserverEvent::HistoryTrimmed {
            dropped_messages: result.dropped_messages,
            kept_turns: result.kept_turns,
            reason: reason.clone(),
            channel: Some(channel),
            agent_alias,
            turn_id,
        });

        Some(HistoryTrimNotice {
            dropped_messages: result.dropped_messages,
            kept_turns: result.kept_turns,
            reason,
        })
    }

    fn append_receipts_block(
        &self,
        response: String,
        scope: Option<&crate::agent::tool_receipts::ReceiptScope>,
    ) -> String {
        if !self.config.resolved.tool_receipts.show_in_response {
            return response;
        }
        let Some(scope) = scope else {
            return response;
        };
        let block = {
            let receipts = scope.collector().lock().unwrap_or_else(|e| e.into_inner());
            crate::agent::tool_receipts::render_receipts_block(&receipts)
        };
        match block {
            Some(block) => {
                if response.is_empty() {
                    block
                } else {
                    format!("{response}\n\n{block}")
                }
            }
            None => response,
        }
    }

    /// Append a user-visible notice when the resilient provider wrapper served
    /// this turn with a different model or provider than requested (silent
    /// model downgrade, e.g. a `fallback_models` entry kicking in). The record
    /// is consumed from the `zeroclaw_providers::reliable` task-local (single
    /// source of truth); nothing is stored.
    ///
    /// The notice is BOTH appended to the returned response (rendered by
    /// consumers of the final text, e.g. the gateway web UI's `done` frame)
    /// and streamed as a trailing [`TurnEvent::Chunk`] (rendered by streaming
    /// consumers that discard the final text on a clean finish, e.g. the
    /// ZeroCode TUI).
    async fn append_model_fallback_notice(
        response: String,
        fallback: Option<&zeroclaw_providers::reliable::ProviderFallbackInfo>,
        event_tx: &tokio::sync::mpsc::Sender<TurnEvent>,
    ) -> String {
        let Some(fallback) = fallback else {
            return response;
        };
        // The wrapper also records plain retries (attempt > 0 on the primary
        // entry); an identical requested/served pair is not a downgrade.
        if fallback.actual_provider == fallback.requested_provider
            && fallback.actual_model == fallback.requested_model
        {
            return response;
        }
        let notice = crate::i18n::get_required_cli_string_with_args(
            "turn-model-fallback-notice",
            &[
                ("requested_model", fallback.requested_model.as_str()),
                ("requested_provider", fallback.requested_provider.as_str()),
                ("actual_model", fallback.actual_model.as_str()),
                ("actual_provider", fallback.actual_provider.as_str()),
            ],
        );
        let delta = format!("\n\n{notice}");
        let _ = event_tx
            .send(TurnEvent::Chunk {
                delta: delta.clone(),
            })
            .await;
        if response.is_empty() {
            notice
        } else {
            format!("{response}{delta}")
        }
    }

    fn build_system_prompt(&self) -> Result<String> {
        self.build_system_prompt_with_dispatcher(self.tool_dispatcher.as_ref())
    }

    fn build_system_prompt_with_dispatcher(
        &self,
        dispatcher: &dyn ToolDispatcher,
    ) -> Result<String> {
        let expose_text_tool_protocol =
            !self.config.resolved.strict_tool_parsing || dispatcher.should_send_tool_specs();
        let no_tools: Vec<Box<dyn Tool>> = Vec::new();
        // Both arms resolve to `&[Box<dyn Tool>]`: the sealed registry derefs to
        // the same slice the raw `Vec` used to expose, so downstream prompt
        // construction is unchanged.
        let prompt_tools: &[Box<dyn Tool>] = if expose_text_tool_protocol {
            &self.tools
        } else {
            &no_tools
        };
        let instructions = dispatcher.prompt_instructions(prompt_tools);
        let ctx = PromptContext {
            workspace_dir: &self.workspace_dir,
            agent_workspace_dir: &self.agent_workspace_dir,
            model_name: &self.model_name,
            tools: prompt_tools,
            skills: &self.skills,
            skills_prompt_mode: self.skills_prompt_mode,
            identity_config: Some(&self.identity_config),
            dispatcher_instructions: &instructions,
            sends_native_tool_specs: dispatcher.should_send_tool_specs()
                && !prompt_tools.is_empty(),
            security_summary: self.security_summary.clone(),
            autonomy_level: self.autonomy_level,
        };
        let mut prompt = self.prompt_builder.build(&ctx)?;
        let receipts = &self.config.resolved.tool_receipts;
        if receipts.enabled && receipts.inject_system_prompt {
            prompt.push_str(crate::agent::tool_receipts::SYSTEM_PROMPT_ADDENDUM);
        }
        if !self.mcp_deferred_section.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&self.mcp_deferred_section);
        }
        if !self.mcp_pinned_section.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&self.mcp_pinned_section);
        }
        Ok(prompt)
    }

    fn rebuild_system_prompt_for_dispatcher(
        &mut self,
        dispatcher: &dyn ToolDispatcher,
    ) -> Result<()> {
        let new_prompt = self.build_system_prompt_with_dispatcher(dispatcher)?;
        let Some(ConversationMessage::Chat(first)) = self.history.first_mut() else {
            return Ok(());
        };
        if first.role != "system" {
            return Ok(());
        }
        first.content = new_prompt;
        Ok(())
    }

    fn try_apply_pending_model_switch(&mut self, current_effective_model: &str) -> Option<String> {
        let pending = crate::agent::loop_::get_model_switch_state()
            .lock()
            .ok()
            .and_then(|guard| guard.clone())?;
        let (new_model_provider, new_model) = pending;

        // Same-provider, same-model: nothing to do. Still clear the
        // request so a stale equal-value entry does not linger.
        if new_model_provider == self.model_provider_name && new_model == current_effective_model {
            crate::agent::loop_::clear_model_switch_request();
            return None;
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "Model switch detected in turn_streamed: {} {} -> {} {}",
                self.model_provider_name, current_effective_model, new_model_provider, new_model
            )
        );

        let switch_outcome: anyhow::Result<Box<dyn ModelProvider>> = match self
            .provider_switch_config
            .as_ref()
            .and_then(|cfg| cfg.config.as_ref())
        {
            Some(full_config) => {
                let agent_entry = full_config
                    .resolved_model_provider_for_agent(&self.agent_alias)
                    .map(|(_ty, _alias, entry)| entry);
                let default_api_key = agent_entry.and_then(|e| e.api_key.as_deref());
                let default_base_url = agent_entry.and_then(|e| e.uri.as_deref());

                // Prefer a route-specific api_key when the switched
                // provider/model matches a configured model_route entry.
                let route_api_key = full_config
                    .model_routes
                    .iter()
                    .find(|r| {
                        r.model_provider.eq_ignore_ascii_case(&new_model_provider)
                            && (r.model.eq_ignore_ascii_case(&new_model)
                                || r.hint.eq_ignore_ascii_case(&new_model))
                    })
                    .and_then(|r| r.api_key.as_deref());
                let api_key = route_api_key.or(default_api_key);

                let runtime_options = new_model_provider
                    .split_once('.')
                    .map(|(family, alias)| {
                        zeroclaw_providers::provider_runtime_options_for_alias(
                            full_config.as_ref(),
                            family,
                            alias,
                        )
                    })
                    .unwrap_or_default();

                zeroclaw_providers::create_routed_model_provider_with_options(
                    full_config.as_ref(),
                    &new_model_provider,
                    api_key,
                    default_base_url,
                    &full_config.reliability,
                    &full_config.model_routes,
                    &new_model,
                    &runtime_options,
                )
            }
            None => Err(anyhow::Error::msg(
                "model_switch requested but agent has no provider_switch_config; \
                 cannot rebuild provider safely",
            )),
        };

        let result = match switch_outcome {
            Ok(new_prov) => {
                // Commit state only after the provider was built
                // successfully.
                self.model_provider = new_prov;
                self.model_provider_name = new_model_provider;
                self.model_name = new_model.clone();
                Some(new_model)
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"err": e.to_string()})),
                    &format!(
                        "Failed to apply model_switch in turn_streamed; staying on {} {}",
                        self.model_provider_name, current_effective_model
                    )
                );
                None
            }
        };
        crate::agent::loop_::clear_model_switch_request();
        result
    }

    fn classify_model(&self, user_message: &str) -> String {
        if let Some(decision) =
            super::classifier::classify_with_decision(&self.classification_config, user_message)
            && self.available_hints.contains(&decision.hint)
        {
            let resolved_model = self
                .route_model_by_hint
                .get(&decision.hint)
                .map(String::as_str)
                .unwrap_or("unknown");
            ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"hint": decision.hint.as_str(), "model": resolved_model, "rule_priority": decision.priority, "message_length": user_message.len()})), "Classified message route");
            return format!("hint:{}", decision.hint);
        }

        // Fallback: auto-classify by complexity when no rule matched.
        if let Some(ref ac) = self.config.resolved.auto_classify {
            let tier = super::eval::estimate_complexity(user_message);
            if let Some(hint) = ac.hint_for(tier)
                && self.available_hints.contains(&hint.to_string())
            {
                ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"hint": hint, "complexity": format!("{:?}", tier), "message_length": user_message.len()})), "Auto-classified by complexity");
                return format!("hint:{hint}");
            }
        }

        self.model_name.clone()
    }

    fn replay_loop_messages(loop_messages: &[ChatMessage]) -> Vec<ConversationMessage> {
        let mut replayed: Vec<ConversationMessage> = Vec::with_capacity(loop_messages.len());
        let push_tool_results = |replayed: &mut Vec<ConversationMessage>,
                                 results: Vec<ToolResultMessage>| {
            if let Some(ConversationMessage::ToolResults(previous)) = replayed.last_mut() {
                previous.extend(results);
            } else {
                replayed.push(ConversationMessage::ToolResults(results));
            }
        };
        for msg in loop_messages {
            if msg.role == "assistant"
                && let Ok(serde_json::Value::Object(obj)) =
                    serde_json::from_str::<serde_json::Value>(&msg.content)
                && let Some(calls) = obj.get("tool_calls").and_then(|c| c.as_array())
                && !calls.is_empty()
                && calls.iter().all(|c| {
                    c.get("id").is_some_and(serde_json::Value::is_string)
                        && c.get("name").is_some_and(serde_json::Value::is_string)
                })
            {
                let tool_calls = calls
                    .iter()
                    .map(|c| zeroclaw_providers::ToolCall {
                        id: c
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        name: c
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        arguments: c
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        extra_content: None,
                    })
                    .collect();
                replayed.push(ConversationMessage::AssistantToolCalls {
                    text: obj
                        .get("content")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    tool_calls,
                    reasoning_content: obj
                        .get("reasoning_content")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                });
                continue;
            }
            if msg.role == "tool" {
                if let Ok(vals) = serde_json::from_str::<Vec<serde_json::Value>>(&msg.content) {
                    let results: Vec<ToolResultMessage> = vals
                        .into_iter()
                        .filter_map(|v| {
                            Some(ToolResultMessage {
                                tool_call_id: v.get("tool_call_id")?.as_str()?.to_string(),
                                content: v
                                    .get("content")
                                    .and_then(|c| c.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                // Provider-wire tool messages do not carry the
                                // producing tool name; replayed results fall back
                                // to blind canonicalization
                                tool_name: String::new(),
                            })
                        })
                        .collect();
                    if !results.is_empty() {
                        push_tool_results(&mut replayed, results);
                        continue;
                    }
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&msg.content) {
                    let result = ToolResultMessage {
                        tool_call_id: v
                            .get("tool_call_id")
                            .and_then(|id| id.as_str())
                            .unwrap_or("unknown")
                            .to_string(),
                        content: v
                            .get("content")
                            .and_then(|c| c.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        // No provenance on the provider-wire shape; blind canon
                        // applies as before
                        tool_name: String::new(),
                    };
                    push_tool_results(&mut replayed, vec![result]);
                    continue;
                }
            }
            replayed.push(ConversationMessage::Chat(msg.clone()));
        }
        replayed
    }

    pub async fn turn(&mut self, user_message: &str) -> Result<String> {
        if user_message.trim().is_empty() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "reason": "empty_user_message",
                        "entry_point": "Agent::turn",
                        "raw_len": user_message.len(),
                    })),
                "Refusing blank user turn (would emit timestamp-only message and risk prompt-template bleed-through)"
            );
            return Err(anyhow::Error::msg(
                "empty user message: refusing to dispatch a blank turn",
            ));
        }

        if self.history.is_empty() {
            let system_prompt = self.build_system_prompt()?;
            self.history
                .push(ConversationMessage::Chat(ChatMessage::system(
                    system_prompt,
                )));
        }

        let effective_model = self.classify_model(user_message);

        let turn_id = Self::new_turn_id();
        let turn_observer = Arc::clone(&self.observer);
        let mut guard = crate::observability::AgentTurnGuard::start(
            turn_observer.as_ref(),
            self.model_provider_name.clone(),
            effective_model.clone(),
            Some(self.channel_name.clone()),
            self.observer_agent_alias(),
            Some(turn_id.clone()),
        );

        // Memory context is injected once in the engine, keyed on the
        // ingress origin (agent::memory_inject).
        if self.auto_save {
            let store_start = std::time::Instant::now();
            let store_result = self
                .memory
                .store(
                    "user_msg",
                    user_message,
                    MemoryCategory::Conversation,
                    self.memory_session_id.as_deref(),
                )
                .await;
            self.observer.record_event(&ObserverEvent::MemoryStore {
                category: MemoryCategory::Conversation.to_string(),
                backend: self.memory.name().to_string(),
                duration: store_start.elapsed(),
                success: store_result.is_ok(),
                channel: Some(self.channel_name.clone()),
                agent_alias: self.observer_agent_alias(),
                turn_id: Some(turn_id.clone()),
            });
        }

        let now = self.current_turn_datetime();
        let (year, month, day) = (now.year(), now.month(), now.day());
        let (hour, minute, second) = (now.hour(), now.minute(), now.second());
        let tz = now.format("%Z");
        let date_str =
            format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} {tz}");

        let enriched = format!("[CURRENT DATE & TIME: {date_str}]\n\n{user_message}");

        self.history
            .push(ConversationMessage::Chat(ChatMessage::user(enriched)));

        let active_dispatcher = {
            let base_provider_messages = self.tool_dispatcher.to_provider_messages(&self.history);
            let (vision_provider_box, _degrade_strip_images) =
                match crate::agent::turn::resolve_vision_provider(
                    self.full_config(),
                    self.model_provider.as_ref(),
                    &base_provider_messages,
                    &self.multimodal_config,
                    &self.model_provider_name,
                    &effective_model,
                ) {
                    Ok(resolved) => resolved,
                    Err(error) => {
                        let _ = self.trim_history(Some(&turn_id));
                        return Err(error);
                    }
                };
            let active_provider: &dyn ModelProvider = vision_provider_box
                .as_ref()
                .map(|resolved| resolved.provider.as_ref())
                .unwrap_or(self.model_provider.as_ref());
            tool_dispatcher_for_provider(&self.config, active_provider)
        };

        if let Err(error) = self.rebuild_system_prompt_for_dispatcher(active_dispatcher.as_ref()) {
            let _ = self.trim_history(Some(&turn_id));
            return Err(error);
        }

        let provider_messages = active_dispatcher.to_provider_messages(&self.history);
        let cache_key = self.response_cache_key_for_messages(&provider_messages, &effective_model);

        if let (Some(cache), Some(key)) = (&self.response_cache, &cache_key) {
            if let Ok(Some(cached)) = cache.get(key) {
                self.observer.record_event(&ObserverEvent::CacheHit {
                    cache_type: "response".into(),
                    tokens_saved: 0,
                });
                self.history
                    .push(ConversationMessage::Chat(ChatMessage::assistant(
                        cached.clone(),
                    )));
                let _ = self.trim_history(Some(&turn_id));
                return Ok(cached);
            }
            self.observer.record_event(&ObserverEvent::CacheMiss {
                cache_type: "response".into(),
            });
        }

        let mut loop_history = provider_messages;
        let mut loop_new_messages: Vec<ChatMessage> = Vec::new();

        let knobs = crate::agent::loop_::LoopKnobs {
            dedup_enabled: false,
            max_iteration_behavior: crate::agent::loop_::MaxIterationBehavior::ErrorAtCap,
            detect_protocol_without_tools: false,
        };
        // E3 never had pattern-based loop detection; default pacing turns it
        // on. Keep the embedder contract (an N-step identical-args tool chain
        // completes) until the Agent surface grows a pacing config of its own.
        let pacing = zeroclaw_config::schema::PacingConfig {
            loop_detection_enabled: false,
            ..zeroclaw_config::schema::PacingConfig::default()
        };

        // Keep the loop call as a plain `.await` on this task. Caller-scoped
        // task-locals (session key, cost tracking, tool choice / thinking
        // overrides) silently vanish across a spawn.
        let cost_context = self.tool_loop_cost_tracking_context();
        let receipt_scope = crate::agent::tool_receipts::ReceiptScope::from_config(
            &self.config.resolved.tool_receipts,
        );
        let agent_alias_for_loop = self.observer_agent_alias();
        let loop_result = crate::agent::loop_::TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(
                Some(cost_context.clone()),
                crate::agent::tool_receipts::scope_receipts(
                    receipt_scope.clone(),
                    crate::agent::loop_::run_tool_call_loop(crate::agent::loop_::ToolLoop {
                        exec: crate::agent::loop_::ResolvedAgentExecution::resolve(
                            crate::agent::loop_::ResolvedModelAccess {
                                model_provider: self.model_provider.as_ref(),
                                provider_name: &self.model_provider_name,
                                model: &effective_model,
                                temperature: self.temperature,
                            },
                            crate::agent::loop_::ResolvedIo {
                                tools_registry: &self.tools,
                                observer: self.observer.as_ref(),
                                silent: false,
                                approval: self.approval_manager.as_deref(),
                                multimodal_config: &self.multimodal_config,
                                // Inlined `full_config()` (per-field borrow) so it coexists with
                                // the `&mut self.image_cache` in this same ToolLoop expression.
                                config: self
                                    .provider_switch_config
                                    .as_ref()
                                    .and_then(|c| c.config.as_deref()),
                                hooks: self.hook_runner.as_deref(),
                                activated_tools: self.activated_tools.as_ref(),
                                model_switch_callback: None,
                                receipt_generator: receipt_scope
                                    .as_ref()
                                    .map(crate::agent::tool_receipts::ReceiptScope::generator),
                            },
                            crate::agent::loop_::ResolvedRuntimeKnobs {
                                max_tool_iterations: self.config.resolved.max_tool_iterations,
                                excluded_tools: &[],
                                dedup_exempt_tools: &self.config.resolved.tool_call_dedup_exempt,
                                pacing: &pacing,
                                strict_tool_parsing: self.config.resolved.strict_tool_parsing,
                                parallel_tools: self.config.resolved.parallel_tools,
                                max_tool_result_chars: self.config.resolved.max_tool_result_chars,
                                context_token_budget: self
                                    .config
                                    .resolved
                                    .effective_context_budget(),
                                knobs: &knobs,
                            },
                        ),
                        history: &mut loop_history,
                        channel_name: &self.channel_name,
                        channel_reply_target: None,
                        cancellation_token: None,
                        on_delta: None,
                        shared_budget: None,
                        channel: None,
                        collected_receipts: receipt_scope
                            .as_ref()
                            .map(crate::agent::tool_receipts::ReceiptScope::collector),
                        event_tx: None,
                        steering: None,
                        new_messages_out: Some(&mut loop_new_messages),
                        image_cache: Some(&mut self.image_cache),
                        // Direct embedded Agent::turn call; source/transport/
                        // trust stay placeholders, not yet stamped at the edge.
                        memory: Some(crate::agent::memory_inject::TurnMemory {
                            handle: self.memory.as_ref(),
                            query: user_message.to_string(),
                            sessions: vec![self.memory_session_id.clone()],
                            suppress: false,
                            cfg: self.memory_inject_cfg,
                        }),
                        ingress: zeroclaw_api::ingress::IngressContext::agent_direct(),
                        agent_alias: agent_alias_for_loop.as_deref(),
                        parent_agent_alias: None,
                        turn_id: &turn_id,
                        // Live-daemon SOP path: re-assemble a nested step's agent
                        // when it delegates elsewhere. Config survives only via
                        // `provider_switch_config`; with `None` (test builder) a
                        // cross-agent step FAILS CLOSED rather than inheriting
                        // this turn's context.
                        sop_reassembly: self
                            .provider_switch_config
                            .as_ref()
                            .and_then(|c| c.config.as_deref())
                            .map(|config| crate::agent::turn::SopStepReassembly { config }),
                    }),
                ),
            )
            .await;

        // Feed the accumulated per-call usage into the AgentEnd guard before
        // any return below drops it — including the error path, which must
        // still report usage from calls that succeeded earlier in the turn.
        let usage = cost_context.snapshot_turn_usage();
        if usage.input_tokens > 0 || usage.output_tokens > 0 {
            guard.set_usage(
                Some(zeroclaw_api::observability_traits::TurnTokenUsage {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                }),
                None,
            );
        }
        for replayed in Self::replay_loop_messages(&loop_new_messages) {
            self.history.push(replayed);
        }
        let response = match loop_result {
            Ok(response) => response,
            Err(error) => {
                let _ = self.trim_history(Some(&turn_id));
                return Err(error);
            }
        };

        let response = self.append_receipts_block(response, receipt_scope.as_ref());

        // Store in the response cache only when the turn was a single
        // tool-free exchange (exactly one assistant message), mirroring the
        // old "no tool calls" put condition.
        if let (Some(cache), Some(key)) = (&self.response_cache, &cache_key)
            && loop_new_messages.len() == 1
            && loop_new_messages[0].role == "assistant"
        {
            #[allow(clippy::cast_possible_truncation)]
            let _ = cache.put(key, &effective_model, &response, usage.output_tokens as u32);
        }

        let _ = self.trim_history(Some(&turn_id));

        Ok(response)
    }

    pub async fn turn_streamed(
        &mut self,
        user_message: &str,
        event_tx: tokio::sync::mpsc::Sender<TurnEvent>,
        cancel_token: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<(String, Vec<ConversationMessage>)> {
        // See `Agent::turn` for the rationale. Same guard: blank input would
        // push a timestamp-only user message into history and the model would
        // narrate the trailing prompt-template sentinel instead of replying.
        if user_message.trim().is_empty() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "reason": "empty_user_message",
                        "entry_point": "Agent::turn_streamed",
                        "raw_len": user_message.len(),
                    })),
                "Refusing blank user turn (would emit timestamp-only message and risk prompt-template bleed-through)"
            );
            return Err(anyhow::Error::msg(
                "empty user message: refusing to dispatch a blank turn",
            ));
        }

        self.turn_streamed_with_steering_state(user_message, event_tx, cancel_token, None)
            .await
            .map(|outcome| (outcome.response, outcome.new_messages))
            .map_err(|err| err.error)
    }

    pub async fn turn_streamed_with_steering_state(
        &mut self,
        user_message: &str,
        event_tx: tokio::sync::mpsc::Sender<TurnEvent>,
        cancel_token: Option<tokio_util::sync::CancellationToken>,
        mut steering_rx: Option<&mut tokio::sync::mpsc::Receiver<String>>,
    ) -> std::result::Result<StreamedTurnSuccess, StreamedTurnError> {
        // See `Agent::turn` for the rationale. Same guard: blank input would
        // push a timestamp-only user message into history and the model would
        // narrate the trailing prompt-template sentinel instead of replying.
        if user_message.trim().is_empty() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "reason": "empty_user_message",
                        "entry_point": "Agent::turn_streamed_with_steering_state",
                        "raw_len": user_message.len(),
                    })),
                "Refusing blank user turn (would emit timestamp-only message and risk prompt-template bleed-through)"
            );
            return Err(StreamedTurnError {
                error: anyhow::Error::msg("empty user message: refusing to dispatch a blank turn"),
                committed_response: String::new(),
                new_messages: Vec::new(),
            });
        }

        // ── Preamble (identical to turn) ───────────────────────────────
        if self.history.is_empty() {
            let system_prompt = self
                .build_system_prompt()
                .map_err(|error| StreamedTurnError {
                    error,
                    committed_response: String::new(),
                    new_messages: Vec::new(),
                })?;
            self.history
                .push(ConversationMessage::Chat(ChatMessage::system(
                    system_prompt,
                )));
        }

        let mut new_msgs: Vec<ConversationMessage> = Vec::new();
        // `effective_model` is `mut` so a `model_switch` requested mid-turn
        // (handled in the round loop's `ModelSwitchRequested` arm via
        // `try_apply_pending_model_switch`) can rebind it for later rounds
        let mut effective_model = self.classify_model(user_message);
        let turn_id = Self::new_turn_id();
        let mut committed_response = String::new();
        // Requested-vs-served divergence for THIS turn. Source of truth is the
        // task-local record inside `zeroclaw_providers::reliable`, consumed
        // once per round below; this is a per-turn transient resolved at
        // use-time, never stored on the agent.
        let mut turn_model_fallback: Option<zeroclaw_providers::reliable::ProviderFallbackInfo> =
            None;
        let turn_observer = Arc::clone(&self.observer);
        let mut guard = crate::observability::AgentTurnGuard::start(
            turn_observer.as_ref(),
            self.model_provider_name.clone(),
            effective_model.clone(),
            Some(self.channel_name.clone()),
            self.observer_agent_alias(),
            Some(turn_id.clone()),
        );
        self.append_streamed_user_message_to_history(user_message, &mut new_msgs, &turn_id)
            .await;

        let active_dispatcher = {
            let base_provider_messages = self.tool_dispatcher.to_provider_messages(&self.history);
            let (vision_provider_box, _degrade_strip_images) =
                match crate::agent::turn::resolve_vision_provider(
                    self.full_config(),
                    self.model_provider.as_ref(),
                    &base_provider_messages,
                    &self.multimodal_config,
                    &self.model_provider_name,
                    &effective_model,
                ) {
                    Ok(resolved) => resolved,
                    Err(error) => {
                        let notice = self.trim_history(Some(&turn_id));
                        forward_history_trim_notice(&event_tx, notice).await;
                        return Err(StreamedTurnError {
                            error,
                            committed_response: String::new(),
                            new_messages: new_msgs,
                        });
                    }
                };
            let active_provider: &dyn ModelProvider = vision_provider_box
                .as_ref()
                .map(|resolved| resolved.provider.as_ref())
                .unwrap_or(self.model_provider.as_ref());
            tool_dispatcher_for_provider(&self.config, active_provider)
        };

        if let Err(error) = self.rebuild_system_prompt_for_dispatcher(active_dispatcher.as_ref()) {
            let notice = self.trim_history(Some(&turn_id));
            forward_history_trim_notice(&event_tx, notice).await;
            return Err(StreamedTurnError {
                error,
                committed_response: String::new(),
                new_messages: new_msgs,
            });
        }

        let provider_messages = active_dispatcher.to_provider_messages(&self.history);
        let cache_key = self.response_cache_key_for_messages(&provider_messages, &effective_model);

        if let (Some(cache), Some(key)) = (&self.response_cache, &cache_key) {
            if let Ok(Some(cached)) = cache.get(key) {
                self.observer.record_event(&ObserverEvent::CacheHit {
                    cache_type: "response".into(),
                    tokens_saved: 0,
                });
                let cached_msg = ConversationMessage::Chat(ChatMessage::assistant(cached.clone()));
                new_msgs.push(cached_msg.clone());
                self.history.push(cached_msg);
                let notice = self.trim_history(Some(&turn_id));
                forward_history_trim_notice(&event_tx, notice).await;
                self.observer.record_event(&ObserverEvent::TurnComplete);
                committed_response.push_str(&cached);
                return Ok(StreamedTurnSuccess {
                    response: committed_response,
                    new_messages: new_msgs,
                });
            }
            self.observer.record_event(&ObserverEvent::CacheMiss {
                cache_type: "response".into(),
            });
        }

        let mut loop_history = provider_messages;

        let approval_bridge: Option<Box<dyn zeroclaw_api::channel::Channel>> =
            self.channel_handles.ask_user.as_ref().map(|handles| {
                Box::new(crate::agent::approval_bridge::AskUserApprovalBridge::new(
                    Arc::clone(handles),
                    self.approval_route.clone(),
                )) as Box<dyn zeroclaw_api::channel::Channel>
            });

        let knobs = crate::agent::loop_::LoopKnobs {
            dedup_enabled: false,
            max_iteration_behavior: crate::agent::loop_::MaxIterationBehavior::GracefulSummary,
            detect_protocol_without_tools: false,
        };
        // The streaming engine never had pattern-based loop detection; default
        // pacing turns it on. Keep the embedder contract until this surface
        // grows a pacing config of its own (matches `Agent::turn`).
        let pacing = zeroclaw_config::schema::PacingConfig {
            loop_detection_enabled: false,
            ..zeroclaw_config::schema::PacingConfig::default()
        };

        let cost_context = self.tool_loop_cost_tracking_context();
        let agent_alias_for_loop = self.observer_agent_alias();

        // Built once per turn so the HMAC key is stable across steering rounds
        // and the same collector accumulates every round's receipts. `None`
        // when receipts are disabled, gated by the one shared seam.
        let receipt_scope = crate::agent::tool_receipts::ReceiptScope::from_config(
            &self.config.resolved.tool_receipts,
        );

        // ── Round loop: one tool-call-loop run per steering round ──────────
        for round in 0..self.config.resolved.max_tool_iterations {
            // Early exit if the caller cancelled this turn (e.g. user abort)
            if cancel_token
                .as_ref()
                .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
            {
                let marker = crate::i18n::get_required_cli_string("turn-interrupted-by-user");
                let interruption =
                    ConversationMessage::Chat(ChatMessage::assistant(marker.clone()));
                new_msgs.push(interruption.clone());
                self.history.push(interruption);
                committed_response.push_str(&marker);
                let notice = self.trim_history(Some(&turn_id));
                forward_history_trim_notice(&event_tx, notice).await;
                return Err(StreamedTurnError {
                    error: crate::agent::loop_::ToolLoopCancelled.into(),
                    committed_response,
                    new_messages: new_msgs,
                });
            }

            // Steering drain: each accepted mid-turn message becomes its own
            // enriched user turn in both transcripts before the next round.
            for steering_message in crate::agent::loop_::drain_steering_messages(&mut steering_rx) {
                self.append_streamed_user_message_to_history(
                    &steering_message,
                    &mut new_msgs,
                    &turn_id,
                )
                .await;
                if let Some(ConversationMessage::Chat(user_msg)) = new_msgs.last() {
                    loop_history.push(user_msg.clone());
                }
            }

            // Per-round append-log: the loop mirrors every message it adds to
            // `loop_history` into this capture at push time, on success AND
            // error exits — never derived from history indices, which the
            // loop's own preflight pruning can invalidate.
            let mut round_added: Vec<ChatMessage> = Vec::new();
            let round_loop = crate::agent::loop_::TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                Some(cost_context.clone()),
                crate::agent::tool_receipts::scope_receipts(
                    receipt_scope.clone(),
                    crate::agent::loop_::run_tool_call_loop(crate::agent::loop_::ToolLoop {
                        exec: crate::agent::loop_::ResolvedAgentExecution::resolve(
                            crate::agent::loop_::ResolvedModelAccess {
                                model_provider: self.model_provider.as_ref(),
                                provider_name: &self.model_provider_name,
                                model: &effective_model,
                                temperature: self.temperature,
                            },
                            crate::agent::loop_::ResolvedIo {
                                tools_registry: &self.tools,
                                observer: self.observer.as_ref(),
                                silent: true,
                                approval: self.approval_manager.as_deref(),
                                multimodal_config: &self.multimodal_config,
                                // Inlined `full_config()` (per-field borrow) so it coexists with
                                // the `&mut self.image_cache` in this same ToolLoop expression.
                                config: self
                                    .provider_switch_config
                                    .as_ref()
                                    .and_then(|c| c.config.as_deref()),
                                hooks: self.hook_runner.as_deref(),
                                activated_tools: self.activated_tools.as_ref(),
                                model_switch_callback: Some(
                                    crate::agent::loop_::get_model_switch_state(),
                                ),
                                receipt_generator: receipt_scope
                                    .as_ref()
                                    .map(crate::agent::tool_receipts::ReceiptScope::generator),
                            },
                            crate::agent::loop_::ResolvedRuntimeKnobs {
                                max_tool_iterations: self.config.resolved.max_tool_iterations,
                                excluded_tools: &[],
                                dedup_exempt_tools: &self.config.resolved.tool_call_dedup_exempt,
                                pacing: &pacing,
                                strict_tool_parsing: self.config.resolved.strict_tool_parsing,
                                parallel_tools: self.config.resolved.parallel_tools,
                                max_tool_result_chars: self.config.resolved.max_tool_result_chars,
                                context_token_budget: self
                                    .config
                                    .resolved
                                    .effective_context_budget(),
                                knobs: &knobs,
                            },
                        ),
                        history: &mut loop_history,
                        channel_name: &self.channel_name,
                        channel_reply_target: None,
                        cancellation_token: cancel_token.clone(),
                        on_delta: None,
                        shared_budget: None,
                        channel: approval_bridge.as_deref(),
                        collected_receipts: receipt_scope
                            .as_ref()
                            .map(crate::agent::tool_receipts::ReceiptScope::collector),
                        event_tx: Some(event_tx.clone()),
                        steering: None,
                        new_messages_out: Some(&mut round_added),
                        image_cache: Some(&mut self.image_cache),
                        // Direct embedded Agent::turn call; source/transport/
                        // trust stay placeholders, not yet stamped at the edge.
                        memory: Some(crate::agent::memory_inject::TurnMemory {
                            handle: self.memory.as_ref(),
                            query: user_message.to_string(),
                            sessions: vec![self.memory_session_id.clone()],
                            suppress: false,
                            cfg: self.memory_inject_cfg,
                        }),
                        ingress: zeroclaw_api::ingress::IngressContext::agent_direct(),
                        agent_alias: agent_alias_for_loop.as_deref(),
                        parent_agent_alias: None,
                        turn_id: &turn_id,
                        // Live-daemon SOP path: re-assemble a nested step's
                        // agent when it delegates elsewhere. Config survives
                        // only via `provider_switch_config`; with `None`
                        // (test builder) a cross-agent step FAILS CLOSED
                        // rather than inheriting this turn's context.
                        sop_reassembly: self
                            .provider_switch_config
                            .as_ref()
                            .and_then(|c| c.config.as_deref())
                            .map(|config| crate::agent::turn::SopStepReassembly { config }),
                    }),
                ),
            );
            // Scope the provider-fallback task-local around the round so the
            // resilient wrapper's requested-vs-served record is visible here,
            // then read it immediately (same pattern as the channels
            // orchestrator's `scope_provider_fallback` wrapping). Box::pin
            // moves the round future to the heap: nesting it inside another
            // async block otherwise grows the turn future past the tokio
            // worker stack in debug builds (observed live as a worker-thread
            // stack overflow aborting the gateway).
            let (loop_result, round_fallback) =
                zeroclaw_providers::reliable::scope_provider_fallback(async {
                    let result = Box::pin(round_loop).await;
                    (
                        result,
                        zeroclaw_providers::reliable::take_last_provider_fallback(),
                    )
                })
                .await;
            if round_fallback.is_some() {
                turn_model_fallback = round_fallback;
            }

            // Feed cumulative usage into the AgentEnd guard before any return
            // below drops it — the error paths must still report usage from
            // calls that succeeded earlier in the turn.
            let usage = cost_context.snapshot_turn_usage();
            if usage.input_tokens > 0 || usage.output_tokens > 0 {
                guard.set_usage(
                    Some(zeroclaw_api::observability_traits::TurnTokenUsage {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                    }),
                    None,
                );
            }

            // Replay everything the loop appended this round into the
            // conversation history and the persistence capture.
            let single_text_exchange =
                round == 0 && round_added.len() == 1 && round_added[0].role == "assistant";
            for replayed in Self::replay_loop_messages(&round_added) {
                new_msgs.push(replayed.clone());
                self.history.push(replayed);
            }

            match loop_result {
                Ok(response) => {
                    // Commit-before-drain: this round's assistant output is in
                    // history/new_msgs (replay above) and committed_response
                    // before any steering continuation is folded in.
                    committed_response.push_str(&response);
                    let notice = self.trim_history(Some(&turn_id));
                    forward_history_trim_notice(&event_tx, notice).await;

                    let has_more_steering =
                        steering_rx.as_deref_mut().is_some_and(|rx| !rx.is_empty());
                    if has_more_steering {
                        continue;
                    }

                    // Cache put only when the turn was a single tool-free
                    // exchange, mirroring the old "no tool calls" condition.
                    if single_text_exchange
                        && let (Some(cache), Some(key)) = (&self.response_cache, &cache_key)
                    {
                        #[allow(clippy::cast_possible_truncation)]
                        let _ =
                            cache.put(key, &effective_model, &response, usage.output_tokens as u32);
                    }

                    self.observer.record_event(&ObserverEvent::TurnComplete);
                    let committed_response =
                        self.append_receipts_block(committed_response, receipt_scope.as_ref());
                    let committed_response = Self::append_model_fallback_notice(
                        committed_response,
                        turn_model_fallback.as_ref(),
                        &event_tx,
                    )
                    .await;
                    return Ok(StreamedTurnSuccess {
                        response: committed_response,
                        new_messages: new_msgs,
                    });
                }
                Err(error) => {
                    // Model switch requested mid-turn: the unified loop
                    // signals a pending `model_switch` by returning
                    // `ModelSwitchRequested` without clearing the request. The
                    // round's tool call + result are already replayed into
                    // history/new_msgs above; rebuild the provider from the
                    // captured `ProviderSwitchConfig` and continue the round
                    // loop so the next provider call uses the switched
                    // provider/model. A failed rebuild (no switch config / build
                    // error) falls through to the normal error handling below.
                    if crate::agent::loop_::is_model_switch_requested(&error).is_some()
                        && let Some(new_effective_model) =
                            self.try_apply_pending_model_switch(&effective_model)
                    {
                        let notice = self.trim_history(Some(&turn_id));
                        forward_history_trim_notice(&event_tx, notice).await;
                        effective_model = new_effective_model;
                        continue;
                    }
                    // Rebuild the committed text from the failed round's plain
                    // assistant output (e.g. a persisted stream partial) when
                    // no prior round committed anything.
                    if committed_response.is_empty() {
                        for replayed in Self::replay_loop_messages(&round_added) {
                            if let ConversationMessage::Chat(message) = &replayed
                                && message.role == "assistant"
                            {
                                committed_response.push_str(&message.content);
                            }
                        }
                    }
                    let error = if crate::agent::loop_::is_tool_loop_cancelled(&error) {
                        // When the cancel arrived after event-visible
                        // streamed text, the error itself carries the
                        // partial the loop persisted (replayed into
                        // history/new_msgs above, and into
                        // committed_response by the empty-committed
                        // rebuild). Provenance, not content sniffing:
                        // model-authored text can end with the marker
                        // literal, so suffix-matching round_added would
                        // misfire. Synthesize the bare marker only when no
                        // interruption text was persisted this round.
                        let marker =
                            crate::i18n::get_required_cli_string("turn-interrupted-by-user");
                        let persisted_interruption = error
                            .downcast_ref::<crate::agent::loop_::StreamCancelledAfterOutput>()
                            .map(|cancelled| format!("{}\n\n{marker}", cancelled.partial_text));
                        match persisted_interruption {
                            Some(text) => {
                                if !committed_response.ends_with(&marker) {
                                    if !committed_response.is_empty() {
                                        committed_response.push_str("\n\n");
                                    }
                                    committed_response.push_str(&text);
                                }
                            }
                            None => {
                                committed_response.push_str(&marker);
                                let interruption = ConversationMessage::Chat(
                                    ChatMessage::assistant(marker.clone()),
                                );
                                new_msgs.push(interruption.clone());
                                self.history.push(interruption);
                            }
                        }
                        crate::agent::loop_::ToolLoopCancelled.into()
                    } else {
                        // Mark the interruption only when nothing was committed —
                        // prior-round text must round-trip unmodified.
                        if committed_response.is_empty() {
                            committed_response.push_str(&crate::i18n::get_required_cli_string(
                                "turn-stream-interrupted",
                            ));
                        }
                        error
                    };
                    let notice = self.trim_history(Some(&turn_id));
                    forward_history_trim_notice(&event_tx, notice).await;
                    return Err(StreamedTurnError {
                        error,
                        committed_response,
                        new_messages: new_msgs,
                    });
                }
            }
        }

        let notice = self.trim_history(Some(&turn_id));
        forward_history_trim_notice(&event_tx, notice).await;
        Err(StreamedTurnError {
            error: anyhow::Error::msg(format!(
                "Agent exceeded maximum tool iterations ({})",
                self.config.resolved.max_tool_iterations
            )),
            committed_response,
            new_messages: new_msgs,
        })
    }

    pub async fn run_single(&mut self, message: &str) -> Result<String> {
        self.turn(message).await
    }

    pub async fn run_interactive(&mut self) -> Result<()> {
        println!("🦀 ZeroClaw Interactive Mode");
        println!("Type /quit to exit.\n");

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let cli = crate::agent::loop_::CLI_CHANNEL_FN
            .get()
            .expect("CLI channel factory not registered — call register_cli_channel_fn at startup")(
        );

        let listen_handle = zeroclaw_spawn::spawn!(async move {
            let _ = zeroclaw_api::channel::Channel::listen(&*cli, tx).await;
        });

        while let Some(msg) = rx.recv().await {
            let response = match self.turn(&msg.content).await {
                Ok(resp) => resp,
                Err(e) => {
                    eprintln!("\nError: {e}\n");
                    continue;
                }
            };
            println!("\n{response}\n");
        }

        listen_handle.abort();
        Ok(())
    }
}

pub async fn run(
    config: Config,
    agent_alias: &str,
    message: Option<String>,
    provider_override: Option<String>,
    model_override: Option<String>,
    temperature: Option<f64>,
) -> Result<()> {
    let mut effective_config = config;
    if let Some(ref p) = provider_override {
        // When a model_provider override is specified, ensure that model_provider type exists
        // in models and update the agent's model_provider to reference it.
        let (type_key, alias_key) = p.split_once('.').unwrap_or((p.as_str(), agent_alias));
        effective_config
            .providers
            .models
            .ensure(type_key, alias_key);
        if let Some(agent_cfg) = effective_config.agents.get_mut(agent_alias) {
            agent_cfg.model_provider = format!("{type_key}.{alias_key}").into();
        }
    }
    // Apply model/temperature overrides to the agent's resolved provider entry.
    if let Some(agent_cfg) = effective_config.agents.get(agent_alias)
        && let Some((fam, ali)) = agent_cfg.model_provider.split_once('.')
        && let Some(entry) = effective_config.providers.models.ensure(fam, ali)
    {
        if let Some(m) = model_override {
            entry.model = Some(m);
        }
        entry.temperature = temperature;
    }

    let mut agent = Agent::from_config(&effective_config, agent_alias).await?;

    if let Some(msg) = message {
        let response = agent.run_single(&msg).await?;
        println!("{response}");
    } else {
        agent.run_interactive().await?;
    }

    Ok(())
}

// safety net (child module so fixtures can reach Agent internals the
// same way `mod tests` does).
#[cfg(test)]
#[path = "safety_net.rs"]
mod safety_net;

#[cfg(test)]
#[path = "parity.rs"]
mod parity;

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::TimeZone;
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_api::observability_traits::ObserverMetric;

    #[test]
    fn build_session_model_provider_rejects_undotted_ref() {
        let config = Config::default();
        let err = match build_session_model_provider(&config, "anthropic", Some("m")) {
            Ok(_) => panic!("undotted ref must error"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("<type>.<alias>"), "got: {err}");
    }

    #[test]
    fn build_session_model_provider_requires_a_model() {
        // No configured entry and no override → cannot resolve a model name.
        let config = Config::default();
        let err = match build_session_model_provider(&config, "anthropic.default", None) {
            Ok(_) => panic!("missing model must error"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("no `model` configured"),
            "got: {err}"
        );
    }

    zeroclaw_api::mock_tool_attribution!(
        CountingTool,
        NamedMockTool,
        MockTool,
        SlowTool,
        ModelSwitchTriggerTool,
    );

    struct MockModelProvider {
        responses: Mutex<Vec<zeroclaw_providers::ChatResponse>>,
    }

    #[async_trait]
    impl ModelProvider for MockModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            Ok("ok".into())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<zeroclaw_providers::ChatResponse> {
            let mut guard = self.responses.lock();
            if guard.is_empty() {
                return Ok(zeroclaw_providers::ChatResponse {
                    text: Some("done".into()),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                });
            }
            Ok(guard.remove(0))
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for MockModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "MockModelProvider"
        }
    }

    const BLANK_TURN_ERROR: &str = "empty user message: refusing to dispatch a blank turn";

    fn blank_input_agent(model_provider: Box<dyn ModelProvider>) -> Agent {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                Vec::new(),
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config")
    }

    #[tokio::test]
    async fn turn_rejects_blank_input() {
        let model_provider = Box::new(MockModelProvider {
            responses: Mutex::new(Vec::new()),
        });
        let mut agent = blank_input_agent(model_provider);
        let err = agent.turn("").await.expect_err("blank turn must fail");
        assert_eq!(err.to_string(), BLANK_TURN_ERROR);
    }

    #[tokio::test]
    async fn turn_rejects_whitespace_only_input() {
        let model_provider = Box::new(MockModelProvider {
            responses: Mutex::new(Vec::new()),
        });
        let mut agent = blank_input_agent(model_provider);
        let err = agent
            .turn("   \n\t")
            .await
            .expect_err("whitespace-only turn must fail");
        assert_eq!(err.to_string(), BLANK_TURN_ERROR);
    }

    // ── model-fallback notice (silent downgrade surfacing) ──────────────

    fn fallback_info(
        requested_provider: &str,
        requested_model: &str,
        actual_provider: &str,
        actual_model: &str,
    ) -> zeroclaw_providers::reliable::ProviderFallbackInfo {
        zeroclaw_providers::reliable::ProviderFallbackInfo {
            requested_provider: requested_provider.into(),
            requested_model: requested_model.into(),
            actual_provider: actual_provider.into(),
            actual_model: actual_model.into(),
        }
    }

    #[tokio::test]
    async fn model_fallback_notice_appended_and_streamed_on_model_downgrade() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        // Same provider family, different model — the case the channels
        // orchestrator's family check suppresses; direct-turn surfaces must
        // still see it.
        let info = fallback_info("anthropic", "model-requested", "anthropic", "model-served");
        let out = Agent::append_model_fallback_notice("hello".to_string(), Some(&info), &tx).await;
        assert!(
            out.starts_with("hello\n\n"),
            "reply text must be preserved ahead of the notice: {out}"
        );
        assert!(
            out.contains("model-requested") && out.contains("model-served"),
            "notice must name both models: {out}"
        );
        match rx.try_recv() {
            Ok(TurnEvent::Chunk { delta }) => {
                assert!(
                    delta.contains("model-served"),
                    "streamed chunk must carry the notice for delta-only consumers: {delta}"
                );
            }
            other => panic!("expected a trailing Chunk carrying the notice, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn model_fallback_notice_skipped_for_pure_retry() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        // The resilient wrapper records retries too (attempt > 0 on the
        // primary entry); an identical requested/served pair is not a
        // downgrade and must stay silent.
        let info = fallback_info("anthropic", "same-model", "anthropic", "same-model");
        let out = Agent::append_model_fallback_notice("hello".to_string(), Some(&info), &tx).await;
        assert_eq!(out, "hello");
        assert!(rx.try_recv().is_err(), "no chunk for a retry");
    }

    #[tokio::test]
    async fn model_fallback_notice_absent_without_fallback_info() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let out = Agent::append_model_fallback_notice("hello".to_string(), None, &tx).await;
        assert_eq!(out, "hello");
        assert!(rx.try_recv().is_err());
    }

    #[derive(Clone, Copy)]
    enum RuntimeStreamPlan {
        Unsupported,
        Text(&'static str),
        Error,
    }

    struct RuntimeStreamingProbeProvider {
        stream: RuntimeStreamPlan,
        chat_text: Option<&'static str>,
    }

    #[async_trait]
    impl ModelProvider for RuntimeStreamingProbeProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            Ok(self.chat_text.unwrap_or("ok").to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<zeroclaw_providers::ChatResponse> {
            let Some(text) = self.chat_text else {
                anyhow::bail!("chat path must not be used for this probe");
            };
            Ok(zeroclaw_providers::ChatResponse {
                text: Some(text.to_string()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }

        fn supports_streaming(&self) -> bool {
            !matches!(self.stream, RuntimeStreamPlan::Unsupported)
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: zeroclaw_providers::traits::StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<zeroclaw_providers::traits::StreamEvent>,
        > {
            use futures_util::StreamExt as _;

            match self.stream {
                RuntimeStreamPlan::Unsupported => futures_util::stream::empty().boxed(),
                RuntimeStreamPlan::Text(text) => futures_util::stream::iter(vec![
                    Ok(zeroclaw_providers::traits::StreamEvent::TextDelta(
                        zeroclaw_providers::traits::StreamChunk::delta(text),
                    )),
                    Ok(zeroclaw_providers::traits::StreamEvent::Final),
                ])
                .boxed(),
                RuntimeStreamPlan::Error => futures_util::stream::iter(vec![Err(
                    zeroclaw_providers::traits::StreamError::ModelProvider(
                        "stream failed before output".into(),
                    ),
                )])
                .boxed(),
            }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for RuntimeStreamingProbeProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "RuntimeStreamingProbeProvider"
        }
    }

    fn streaming_probe_reliable_provider(
        primary: RuntimeStreamingProbeProvider,
        fallback: RuntimeStreamingProbeProvider,
    ) -> zeroclaw_providers::reliable::ReliableModelProvider {
        zeroclaw_providers::reliable::ReliableModelProvider::new(
            "test",
            vec![
                (
                    "provider-requested".to_string(),
                    Box::new(primary) as Box<dyn ModelProvider>,
                ),
                (
                    "provider-served".to_string(),
                    Box::new(fallback) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            1,
        )
    }

    /// End-to-end: a resilient wrapper that fails over to a second entry
    /// mid-turn must surface the downgrade in BOTH the returned response and
    /// the event stream.
    #[tokio::test]
    async fn streamed_turn_surfaces_provider_fallback_notice() {
        struct FailingModelProvider;
        #[async_trait]
        impl ModelProvider for FailingModelProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> Result<String> {
                anyhow::bail!("primary provider is down")
            }
            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
            ) -> Result<zeroclaw_providers::ChatResponse> {
                anyhow::bail!("primary provider is down")
            }
        }
        impl ::zeroclaw_api::attribution::Attributable for FailingModelProvider {
            fn role(&self) -> ::zeroclaw_api::attribution::Role {
                ::zeroclaw_api::attribution::Role::Provider(
                    ::zeroclaw_api::attribution::ProviderKind::Model(
                        ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "FailingModelProvider"
            }
        }

        let reliable = zeroclaw_providers::reliable::ReliableModelProvider::new(
            "test",
            vec![
                (
                    "provider-requested".to_string(),
                    Box::new(FailingModelProvider) as Box<dyn ModelProvider>,
                ),
                (
                    "provider-served".to_string(),
                    Box::new(MockModelProvider {
                        responses: Mutex::new(Vec::new()),
                    }) as Box<dyn ModelProvider>,
                ),
            ],
            0,
            50,
        );

        let mut agent = blank_input_agent(Box::new(reliable));
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let outcome = agent
            .turn_streamed_with_steering_state("hello", tx, None, None)
            .await
            .expect("turn must succeed via the fallback entry");

        assert!(
            outcome.response.contains("provider-served")
                && outcome.response.contains("provider-requested"),
            "final response must carry the fallback notice: {}",
            outcome.response
        );

        let mut chunk_carried_notice = false;
        while let Ok(event) = rx.try_recv() {
            if let TurnEvent::Chunk { delta } = event
                && delta.contains("provider-served")
            {
                chunk_carried_notice = true;
            }
        }
        assert!(
            chunk_carried_notice,
            "the notice must also be streamed for delta-only consumers (ZeroCode)"
        );
    }

    #[tokio::test]
    async fn streamed_turn_surfaces_streaming_provider_fallback_notice() {
        let reliable = streaming_probe_reliable_provider(
            RuntimeStreamingProbeProvider {
                stream: RuntimeStreamPlan::Unsupported,
                chat_text: None,
            },
            RuntimeStreamingProbeProvider {
                stream: RuntimeStreamPlan::Text("streamed fallback"),
                chat_text: None,
            },
        );

        let mut agent = blank_input_agent(Box::new(reliable));
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let outcome = agent
            .turn_streamed_with_steering_state("hello", tx, None, None)
            .await
            .expect("turn must succeed via the streaming fallback entry");

        assert!(
            outcome.response.contains("streamed fallback")
                && outcome.response.contains("provider-served"),
            "final response must include streamed text and fallback notice: {}",
            outcome.response
        );

        let mut streamed = String::new();
        while let Ok(event) = rx.try_recv() {
            if let TurnEvent::Chunk { delta } = event {
                streamed.push_str(&delta);
            }
        }
        assert!(
            streamed.contains("streamed fallback") && streamed.contains("provider-served"),
            "streamed chunks must include the live fallback output and notice: {streamed}"
        );
    }

    #[tokio::test]
    async fn streamed_turn_does_not_surface_stale_record_after_stream_error() {
        let reliable = streaming_probe_reliable_provider(
            RuntimeStreamingProbeProvider {
                stream: RuntimeStreamPlan::Unsupported,
                chat_text: Some("primary final"),
            },
            RuntimeStreamingProbeProvider {
                stream: RuntimeStreamPlan::Error,
                chat_text: None,
            },
        );

        let mut agent = blank_input_agent(Box::new(reliable));
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let outcome = agent
            .turn_streamed_with_steering_state("hello", tx, None, None)
            .await
            .expect("pre-output stream error must fall back to primary chat");

        assert_eq!(
            outcome.response, "primary final",
            "failed fallback streams must not leave stale fallback notice state"
        );
    }

    #[tokio::test]
    async fn turn_streamed_rejects_blank_input() {
        let model_provider = Box::new(MockModelProvider {
            responses: Mutex::new(Vec::new()),
        });
        let mut agent = blank_input_agent(model_provider);
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(8);
        let err = agent
            .turn_streamed("", event_tx, None)
            .await
            .expect_err("blank streamed turn must fail");
        assert_eq!(err.to_string(), BLANK_TURN_ERROR);
    }

    #[tokio::test]
    async fn turn_streamed_rejects_whitespace_only_input() {
        let model_provider = Box::new(MockModelProvider {
            responses: Mutex::new(Vec::new()),
        });
        let mut agent = blank_input_agent(model_provider);
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(8);
        let err = agent
            .turn_streamed("  \n", event_tx, None)
            .await
            .expect_err("whitespace-only streamed turn must fail");
        assert_eq!(err.to_string(), BLANK_TURN_ERROR);
    }

    struct ModelCaptureModelProvider {
        responses: Mutex<Vec<zeroclaw_providers::ChatResponse>>,
        seen_models: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl ModelProvider for ModelCaptureModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            Ok("ok".into())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            model: &str,
            _temperature: Option<f64>,
        ) -> Result<zeroclaw_providers::ChatResponse> {
            self.seen_models.lock().push(model.to_string());
            let mut guard = self.responses.lock();
            if guard.is_empty() {
                return Ok(zeroclaw_providers::ChatResponse {
                    text: Some("done".into()),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                });
            }
            Ok(guard.remove(0))
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for ModelCaptureModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ModelCaptureModelProvider"
        }
    }

    struct TranscriptCaptureModelProvider {
        responses: Mutex<Vec<zeroclaw_providers::ChatResponse>>,
        seen_messages: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
    }

    #[async_trait]
    impl ModelProvider for TranscriptCaptureModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            Ok("ok".into())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<zeroclaw_providers::ChatResponse> {
            self.seen_messages.lock().push(request.messages.to_vec());
            let mut responses = self.responses.lock();
            if responses.is_empty() {
                return Ok(zeroclaw_providers::ChatResponse {
                    text: Some("done".into()),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                });
            }
            Ok(responses.remove(0))
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for TranscriptCaptureModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "TranscriptCaptureModelProvider"
        }
    }

    struct StreamingSteeringModelProvider {
        seen_messages: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
        call_count: AtomicUsize,
        fail_on_call: Option<usize>,
        fail_chat_on_call: Option<usize>,
        fail_after_delta_on_call: Option<usize>,
        delay_chat_on_call: Option<usize>,
    }

    #[async_trait]
    impl ModelProvider for StreamingSteeringModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            Ok("ok".into())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<zeroclaw_providers::ChatResponse> {
            let call = self.call_count.fetch_add(1, Ordering::SeqCst) + 1;
            self.seen_messages.lock().push(request.messages.to_vec());
            if self.delay_chat_on_call == Some(call) {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
            if self.fail_on_call == Some(call) {
                anyhow::bail!("synthetic provider failure on call {call}");
            }
            if self.fail_chat_on_call == Some(call) {
                anyhow::bail!("synthetic chat failure on call {call}");
            }
            if self.fail_after_delta_on_call == Some(call) {
                anyhow::bail!("synthetic provider failure after delta on call {call}");
            }
            Ok(zeroclaw_providers::ChatResponse {
                text: Some(if call == 1 { "draft" } else { "final" }.into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: zeroclaw_providers::traits::StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<zeroclaw_providers::traits::StreamEvent>,
        > {
            use futures_util::StreamExt as _;

            let call = self.call_count.fetch_add(1, Ordering::SeqCst) + 1;
            self.seen_messages.lock().push(request.messages.to_vec());
            let should_fail = self.fail_on_call == Some(call);
            let should_fail_after_delta = self.fail_after_delta_on_call == Some(call);
            let delta = if call == 1 { "draft" } else { "final" }.to_string();
            futures_util::stream::unfold(0, move |step| {
                let delta = delta.clone();
                async move {
                    match step {
                        0 if should_fail => Some((
                            Err(zeroclaw_providers::traits::StreamError::ModelProvider(
                                "synthetic provider failure".into(),
                            )),
                            1,
                        )),
                        0 => Some((
                            Ok(zeroclaw_providers::traits::StreamEvent::TextDelta(
                                zeroclaw_providers::traits::StreamChunk {
                                    delta,
                                    is_final: false,
                                    reasoning: None,
                                    token_count: 0,
                                },
                            )),
                            1,
                        )),
                        1 if should_fail_after_delta => Some((
                            Err(zeroclaw_providers::traits::StreamError::ModelProvider(
                                "synthetic provider failure after delta".into(),
                            )),
                            2,
                        )),
                        1 => {
                            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                            Some((Ok(zeroclaw_providers::traits::StreamEvent::Final), 2))
                        }
                        _ => None,
                    }
                }
            })
            .boxed()
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for StreamingSteeringModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "StreamingSteeringModelProvider"
        }
    }

    #[derive(Default)]
    struct CapturingObserver {
        events: parking_lot::Mutex<Vec<ObserverEvent>>,
    }

    fn fixed_response_cache_turn_datetime() -> chrono::DateTime<chrono::Local> {
        chrono::Local
            .with_ymd_and_hms(2026, 6, 25, 12, 0, 0)
            .single()
            .expect("fixed local test timestamp")
    }

    impl Observer for CapturingObserver {
        fn record_event(&self, event: &ObserverEvent) {
            self.events.lock().push(event.clone());
        }
        fn record_metric(&self, _metric: &ObserverMetric) {}
        fn name(&self) -> &str {
            "capturing"
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn flush(&self) {}
    }

    struct MultimodalCaptureProvider {
        seen_user_messages: Arc<Mutex<Vec<String>>>,
        streamed: bool,
    }

    #[async_trait]
    impl ModelProvider for MultimodalCaptureProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            Ok("ok".into())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<zeroclaw_providers::ChatResponse> {
            if let Some(message) = request.messages.iter().rfind(|msg| msg.role == "user") {
                self.seen_user_messages.lock().push(message.content.clone());
            }
            Ok(zeroclaw_providers::ChatResponse {
                text: Some("done".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }

        fn stream_chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: zeroclaw_providers::traits::StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<zeroclaw_providers::traits::StreamEvent>,
        > {
            use futures_util::stream::{self, StreamExt};

            if let Some(message) = request.messages.iter().rfind(|msg| msg.role == "user") {
                self.seen_user_messages.lock().push(message.content.clone());
            }

            if self.streamed {
                let chunk = zeroclaw_providers::traits::StreamEvent::TextDelta(
                    zeroclaw_providers::traits::StreamChunk {
                        delta: "stream-done".into(),
                        is_final: false,
                        reasoning: None,
                        token_count: 0,
                    },
                );
                stream::iter(vec![
                    Ok(chunk),
                    Ok(zeroclaw_providers::traits::StreamEvent::Final),
                ])
                .boxed()
            } else {
                stream::iter(vec![Ok(zeroclaw_providers::traits::StreamEvent::Final)]).boxed()
            }
        }

        fn supports_vision(&self) -> bool {
            true
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for MultimodalCaptureProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "MultimodalCaptureProvider"
        }
    }

    struct MockTool;

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            "echo"
        }

        fn description(&self) -> &str {
            "echo"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(&self, _args: serde_json::Value) -> Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: true,
                output: "tool-out".into(),
                error: None,
            })
        }
    }

    #[test]
    fn direct_agent_turn_does_not_write_intermediate_native_text_to_stdout() {
        let current_exe = std::env::current_exe().expect("current test binary path");
        let output = std::process::Command::new(current_exe)
            .args([
                "direct_agent_turn_stdout_boundary_helper_4721",
                "--ignored",
                "--nocapture",
            ])
            .output()
            .expect("helper test process should run");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            output.status.success(),
            "helper failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            stdout,
            stderr
        );
        assert!(
            !stdout.contains("intermediate native narration"),
            "intermediate native narration leaked to stdout:\n{stdout}"
        );
        assert!(
            stderr.contains("intermediate native narration"),
            "intermediate native narration was not routed to stderr:\n{stderr}"
        );
    }

    #[tokio::test]
    #[ignore = "subprocess helper for stdout/stderr boundary regression"]
    async fn direct_agent_turn_stdout_boundary_helper_4721() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let model_provider = Box::new(MockModelProvider {
            responses: Mutex::new(vec![
                zeroclaw_providers::ChatResponse {
                    text: Some("intermediate native narration".into()),
                    tool_calls: vec![zeroclaw_providers::ToolCall {
                        id: "tc1".into(),
                        name: "echo".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                },
                zeroclaw_providers::ChatResponse {
                    text: Some("final answer".into()),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                },
            ]),
        });

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let answer = agent
            .turn("run the tool")
            .await
            .expect("turn should finish");
        assert_eq!(answer, "final answer");
    }

    struct FailingModelProvider;

    #[async_trait]
    impl ModelProvider for FailingModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            Err(anyhow::Error::msg("provider unavailable"))
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<zeroclaw_providers::ChatResponse> {
            Err(anyhow::Error::msg("provider unavailable"))
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for FailingModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "FailingModelProvider"
        }
    }

    struct FailingPromptSection;

    impl crate::agent::prompt::PromptSection for FailingPromptSection {
        fn name(&self) -> &str {
            "failing-test-section"
        }

        fn build(&self, _ctx: &PromptContext<'_>) -> Result<String> {
            Err(anyhow::Error::msg("synthetic prompt rebuild failure"))
        }
    }

    struct ToolThenFailingModelProvider {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl ModelProvider for ToolThenFailingModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            Err(anyhow::Error::msg("provider unavailable after tool"))
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<zeroclaw_providers::ChatResponse> {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                return Ok(zeroclaw_providers::ChatResponse {
                    text: Some("running tool".into()),
                    tool_calls: vec![zeroclaw_providers::ToolCall {
                        id: "error-path-call".into(),
                        name: "echo".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                });
            }
            Err(anyhow::Error::msg("provider unavailable after tool"))
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ToolThenFailingModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "ToolThenFailingModelProvider"
        }
    }

    struct SlowTool;

    #[async_trait]
    impl Tool for SlowTool {
        fn name(&self) -> &str {
            "echo"
        }

        fn description(&self) -> &str {
            "echo"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(&self, _args: serde_json::Value) -> Result<crate::tools::ToolResult> {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            Ok(crate::tools::ToolResult {
                success: true,
                output: "tool-out".into(),
                error: None,
            })
        }
    }

    struct CountingTool {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for CountingTool {
        fn name(&self) -> &str {
            "echo"
        }

        fn description(&self) -> &str {
            "echo"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(&self, _args: serde_json::Value) -> Result<crate::tools::ToolResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(crate::tools::ToolResult {
                success: true,
                output: "tool-out".into(),
                error: None,
            })
        }
    }

    #[tokio::test]
    async fn turn_without_tools_returns_text() {
        let model_provider = Box::new(MockModelProvider {
            responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                text: Some("hello".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            }]),
        });

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(XmlToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let response = agent.turn("hi").await.unwrap();
        assert_eq!(response, "hello");
    }

    #[tokio::test]
    async fn direct_agent_strict_tool_parsing_ignores_xml_dispatcher_calls() {
        let provider = Box::new(MockModelProvider {
            responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                text: Some(
                    r#"<tool_call>{"name":"echo","arguments":{"value":"ignored"}}</tool_call>"#
                        .into(),
                ),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            }]),
        });

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let calls = Arc::new(AtomicUsize::new(0));
        let agent_config = zeroclaw_config::schema::AliasedAgentConfig {
            resolved: zeroclaw_config::schema::ResolvedRuntime {
                strict_tool_parsing: true,
                ..Default::default()
            },
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        };
        let mut agent = Agent::builder()
            .model_provider(provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(CountingTool {
                    calls: Arc::clone(&calls),
                })],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(XmlToolDispatcher))
            .config(agent_config)
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let system_prompt = agent
            .build_system_prompt()
            .expect("system prompt should render");
        assert!(
            !system_prompt.contains("## Tools"),
            "strict parsing should not advertise text tool instructions"
        );
        assert!(
            !system_prompt.contains("<tool_call"),
            "strict parsing should not advertise XML tool calls"
        );

        let response = agent.turn("hi").await.unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(response.contains("<tool_call>"));
    }

    #[test]
    fn native_agent_prompt_omits_duplicate_tools_section() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let workspace = tempfile::TempDir::new().expect("temp dir");
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, workspace.path(), None)
                .expect("memory creation should succeed with valid config"),
        );
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});

        let native_agent = Agent::builder()
            .model_provider(Box::new(MockModelProvider {
                responses: Mutex::new(vec![]),
            }))
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(Arc::clone(&mem))
            .observer(Arc::clone(&observer))
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(workspace.path().to_path_buf())
            .build()
            .expect("agent builder should succeed with valid config");
        let native_prompt = native_agent.build_system_prompt().unwrap();
        assert!(!native_prompt.contains("## Tools"));
        assert!(!native_prompt.contains("echo"));

        let xml_agent = Agent::builder()
            .model_provider(Box::new(MockModelProvider {
                responses: Mutex::new(vec![]),
            }))
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(XmlToolDispatcher))
            .workspace_dir(workspace.path().to_path_buf())
            .build()
            .expect("agent builder should succeed with valid config");
        let xml_prompt = xml_agent.build_system_prompt().unwrap();
        assert!(xml_prompt.contains("## Tools"));
        assert!(xml_prompt.contains("echo"));
        assert!(xml_prompt.contains("## Tool Use Protocol"));
    }

    mod surface2_tests {
        use super::*;
        use crate::agent::dispatcher::{NativeToolDispatcher, XmlToolDispatcher};

        /// Marker text produced by the section-based prompt builder when tools
        /// are advertised as XML/text instructions rather than native tool specs.
        const XML_TOOLS_MARKER: &str = "## Tools";
        type CapturedTranscripts = Arc<Mutex<Vec<Vec<ChatMessage>>>>;

        /// Test provider that captures the provider-visible transcript and
        /// reports a configurable native-tool capability.
        struct CapturingModelProvider {
            responses: Mutex<Vec<zeroclaw_providers::ChatResponse>>,
            supports_native: bool,
            captured_messages: CapturedTranscripts,
        }

        #[async_trait]
        impl ModelProvider for CapturingModelProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> Result<String> {
                Ok("ok".into())
            }

            async fn chat(
                &self,
                request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
            ) -> Result<zeroclaw_providers::ChatResponse> {
                self.captured_messages
                    .lock()
                    .push(request.messages.to_vec());
                let mut guard = self.responses.lock();
                if guard.is_empty() {
                    return Ok(zeroclaw_providers::ChatResponse {
                        text: Some("done".into()),
                        tool_calls: vec![],
                        usage: None,
                        reasoning_content: None,
                    });
                }
                Ok(guard.remove(0))
            }

            fn supports_native_tools(&self) -> bool {
                self.supports_native
            }
        }

        impl ::zeroclaw_api::attribution::Attributable for CapturingModelProvider {
            fn role(&self) -> ::zeroclaw_api::attribution::Role {
                ::zeroclaw_api::attribution::Role::Provider(
                    ::zeroclaw_api::attribution::ProviderKind::Model(
                        ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "CapturingModelProvider"
            }
        }

        fn capturing_provider(
            supports_native: bool,
        ) -> (Box<dyn ModelProvider>, CapturedTranscripts) {
            let captured: CapturedTranscripts = Arc::new(Mutex::new(Vec::new()));
            (
                Box::new(CapturingModelProvider {
                    responses: Mutex::new(vec![]),
                    supports_native,
                    captured_messages: Arc::clone(&captured),
                }),
                captured,
            )
        }

        fn test_agent_with_provider(
            provider: Box<dyn ModelProvider>,
            tools: Vec<Box<dyn Tool>>,
        ) -> Agent {
            test_agent_with_provider_and_multimodal(provider, tools, None, None)
        }

        fn test_agent_with_provider_and_multimodal(
            provider: Box<dyn ModelProvider>,
            tools: Vec<Box<dyn Tool>>,
            tool_dispatcher: Option<Box<dyn ToolDispatcher>>,
            multimodal_config: Option<zeroclaw_config::schema::MultimodalConfig>,
        ) -> Agent {
            let memory_cfg = zeroclaw_config::schema::MemoryConfig {
                backend: "none".into(),
                ..zeroclaw_config::schema::MemoryConfig::default()
            };
            let workspace = tempfile::TempDir::new().expect("temp dir");
            let mem: Arc<dyn Memory> = Arc::from(
                zeroclaw_memory::create_memory(&memory_cfg, workspace.path(), None)
                    .expect("memory creation should succeed"),
            );
            let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
            let mut builder = Agent::builder()
                .model_provider(provider)
                .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                    tools,
                ))
                .memory(mem)
                .observer(observer)
                .workspace_dir(workspace.path().to_path_buf());
            if let Some(dispatcher) = tool_dispatcher {
                builder = builder.tool_dispatcher(dispatcher);
            } else {
                builder = builder.tool_dispatcher(Box::new(NativeToolDispatcher));
            }
            if let Some(mm) = multimodal_config {
                builder = builder.multimodal_config(mm);
            }
            builder.build().expect("agent builder should succeed")
        }

        #[test]
        fn build_system_prompt_with_dispatcher_reflects_dispatcher_mode() {
            let workspace = tempfile::TempDir::new().expect("temp dir");
            let memory_cfg = zeroclaw_config::schema::MemoryConfig {
                backend: "none".into(),
                ..zeroclaw_config::schema::MemoryConfig::default()
            };
            let mem: Arc<dyn Memory> = Arc::from(
                zeroclaw_memory::create_memory(&memory_cfg, workspace.path(), None)
                    .expect("memory creation should succeed"),
            );
            let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
            let agent = Agent::builder()
                .model_provider(Box::new(MockModelProvider {
                    responses: Mutex::new(vec![]),
                }))
                .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                    vec![Box::new(MockTool)],
                ))
                .memory(mem)
                .observer(observer)
                .tool_dispatcher(Box::new(NativeToolDispatcher))
                .workspace_dir(workspace.path().to_path_buf())
                .build()
                .expect("agent builder should succeed");

            let native_prompt = agent
                .build_system_prompt_with_dispatcher(&NativeToolDispatcher as &dyn ToolDispatcher)
                .unwrap();
            assert!(
                !native_prompt.contains(XML_TOOLS_MARKER),
                "native dispatcher must not emit XML tool listing"
            );

            let xml_prompt = agent
                .build_system_prompt_with_dispatcher(&XmlToolDispatcher as &dyn ToolDispatcher)
                .unwrap();
            assert!(
                xml_prompt.contains(XML_TOOLS_MARKER),
                "xml dispatcher must emit XML tool listing"
            );
        }

        #[test]
        fn rebuild_system_prompt_switches_to_xml_when_active_provider_non_native() {
            let (provider, _) = capturing_provider(true);
            let mut agent = test_agent_with_provider(provider, vec![Box::new(MockTool)]);

            // Seed a native-style system prompt as if the agent was built
            // against a native-capable base provider.
            let native_prompt = agent
                .build_system_prompt_with_dispatcher(&NativeToolDispatcher as &dyn ToolDispatcher)
                .unwrap();
            agent.history = vec![ConversationMessage::Chat(ChatMessage::system(
                native_prompt,
            ))];

            // Active provider for this turn does not support native tools.
            agent
                .rebuild_system_prompt_for_dispatcher(&XmlToolDispatcher)
                .expect("rebuild should succeed");

            let prompt = match &agent.history[0] {
                ConversationMessage::Chat(msg) => msg.content.clone(),
                _ => panic!("history[0] should be a chat message"),
            };
            assert!(
                prompt.contains(XML_TOOLS_MARKER),
                "prompt must be rebuilt with XML tool listing"
            );
        }

        #[test]
        fn rebuild_system_prompt_switches_to_native_when_active_provider_native() {
            let (provider, _) = capturing_provider(false);
            let mut agent = test_agent_with_provider(provider, vec![Box::new(MockTool)]);

            let xml_prompt = agent
                .build_system_prompt_with_dispatcher(&XmlToolDispatcher as &dyn ToolDispatcher)
                .unwrap();
            agent.history = vec![ConversationMessage::Chat(ChatMessage::system(xml_prompt))];

            // Active provider for this turn supports native tools.
            agent
                .rebuild_system_prompt_for_dispatcher(&NativeToolDispatcher)
                .expect("rebuild should succeed");

            let prompt = match &agent.history[0] {
                ConversationMessage::Chat(msg) => msg.content.clone(),
                _ => panic!("history[0] should be a chat message"),
            };
            assert!(
                !prompt.contains(XML_TOOLS_MARKER),
                "prompt must be rebuilt without XML tool listing"
            );
        }

        #[tokio::test]
        async fn turn_uses_active_provider_tool_mode_for_transcript() {
            let (provider, captured) = capturing_provider(false);
            let mut agent = test_agent_with_provider(provider, vec![Box::new(MockTool)]);

            // The base provider does not support native tools, so the active
            // provider resolved by the turn path must be non-native. The
            // provider-visible transcript should reflect that.
            agent.turn("hello").await.expect("turn should succeed");

            let messages = captured.lock();
            let first_call = messages
                .first()
                .expect("provider should have received a request");
            let system = first_call
                .iter()
                .find(|m| m.role == "system")
                .expect("transcript must contain a system message");
            assert!(
                system.content.contains(XML_TOOLS_MARKER),
                "system prompt must advertise XML tools when active provider is non-native"
            );
        }

        #[tokio::test]
        async fn turn_streamed_uses_active_provider_tool_mode_for_transcript() {
            let (provider, captured) = capturing_provider(false);
            let mut agent = test_agent_with_provider(provider, vec![Box::new(MockTool)]);
            let (event_tx, _event_rx) = tokio::sync::mpsc::channel(16);

            agent
                .turn_streamed("hello", event_tx, None)
                .await
                .expect("streamed turn should succeed");

            let messages = captured.lock();
            let first_call = messages
                .first()
                .expect("provider should have received a request");
            let system = first_call
                .iter()
                .find(|m| m.role == "system")
                .expect("transcript must contain a system message");
            assert!(
                system.content.contains(XML_TOOLS_MARKER),
                "streamed system prompt must advertise XML tools when active provider is non-native"
            );
        }

        #[tokio::test]
        async fn turn_rebuilds_prompt_for_vision_routed_xml_provider() {
            // Base provider supports native tools but not vision. The configured
            // vision provider is a custom OpenAI-compatible endpoint: it supports
            // vision but not native tools.
            let (base_provider, _captured) = capturing_provider(true);
            let mm_config = zeroclaw_config::schema::MultimodalConfig {
                vision_model_provider: Some("custom:http://127.0.0.1:9".into()),
                ..Default::default()
            };
            let mut agent = test_agent_with_provider_and_multimodal(
                base_provider,
                vec![Box::new(MockTool)],
                Some(Box::new(NativeToolDispatcher)),
                Some(mm_config),
            );

            let msg = "describe this image [IMAGE:data:image/png;base64,iVBORw0KGgo=]";

            // The vision provider will fail to connect to localhost:9, but the
            // prompt rebuild and provider-visible transcript happen before the
            // network call.
            let result = agent.turn(msg).await;
            assert!(
                result.is_err(),
                "vision provider chat should fail to connect"
            );

            let system_content = match &agent.history[0] {
                ConversationMessage::Chat(m) => m.content.clone(),
                _ => panic!("history[0] should be a chat message"),
            };
            assert!(
                system_content.contains(XML_TOOLS_MARKER),
                "stored system prompt must be rebuilt for XML vision provider"
            );

            let provider_messages = XmlToolDispatcher.to_provider_messages(&agent.history);
            let system = provider_messages
                .iter()
                .find(|m| m.role == "system")
                .expect("transcript must contain a system message");
            assert!(
                system.content.contains(XML_TOOLS_MARKER),
                "provider-visible transcript must advertise XML tools for vision provider"
            );
        }

        #[tokio::test]
        async fn turn_streamed_rebuilds_prompt_for_vision_routed_native_provider() {
            // Base provider does not support native tools or vision. The
            // configured vision provider is an Anthropic-compatible endpoint:
            // it supports both vision and native tools.
            let (base_provider, _captured) = capturing_provider(false);
            let mm_config = zeroclaw_config::schema::MultimodalConfig {
                vision_model_provider: Some("anthropic-custom:http://127.0.0.1:9".into()),
                ..Default::default()
            };
            let mut agent = test_agent_with_provider_and_multimodal(
                base_provider,
                vec![Box::new(MockTool)],
                Some(Box::new(XmlToolDispatcher)),
                Some(mm_config),
            );

            let msg = "describe this image [IMAGE:data:image/png;base64,iVBORw0KGgo=]";
            let (event_tx, _event_rx) = tokio::sync::mpsc::channel(16);

            let result = agent.turn_streamed(msg, event_tx, None).await;
            assert!(
                result.is_err(),
                "vision provider chat should fail to connect"
            );

            let system_content = match &agent.history[0] {
                ConversationMessage::Chat(m) => m.content.clone(),
                _ => panic!("history[0] should be a chat message"),
            };
            assert!(
                !system_content.contains(XML_TOOLS_MARKER),
                "stored system prompt must be rebuilt for native vision provider"
            );

            let provider_messages = NativeToolDispatcher.to_provider_messages(&agent.history);
            let system = provider_messages
                .iter()
                .find(|m| m.role == "system")
                .expect("transcript must contain a system message");
            assert!(
                !system.content.contains(XML_TOOLS_MARKER),
                "provider-visible transcript must advertise native tools for vision provider"
            );
        }
    }

    #[tokio::test]
    async fn turn_with_native_dispatcher_handles_tool_results_variant() {
        let model_provider = Box::new(MockModelProvider {
            responses: Mutex::new(vec![
                zeroclaw_providers::ChatResponse {
                    text: Some(String::new()),
                    tool_calls: vec![zeroclaw_providers::ToolCall {
                        id: "tc1".into(),
                        name: "echo".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                },
                zeroclaw_providers::ChatResponse {
                    text: Some("done".into()),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                },
            ]),
        });

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let response = agent.turn("hi").await.unwrap();
        assert_eq!(response, "done");
        assert!(
            agent
                .history()
                .iter()
                .any(|msg| matches!(msg, ConversationMessage::ToolResults(_)))
        );
    }

    #[tokio::test]
    async fn turn_routes_with_hint_when_query_classification_matches() {
        let seen_models = Arc::new(Mutex::new(Vec::new()));
        let model_provider = Box::new(ModelCaptureModelProvider {
            responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                text: Some("classified".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            }]),
            seen_models: seen_models.clone(),
        });

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut route_model_by_hint = HashMap::new();
        route_model_by_hint.insert("fast".to_string(), "anthropic/claude-haiku-4-5".to_string());
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .classification_config(zeroclaw_config::schema::QueryClassificationConfig {
                enabled: true,
                rules: vec![zeroclaw_config::schema::ClassificationRule {
                    hint: "fast".to_string(),
                    keywords: vec!["quick".to_string()],
                    patterns: vec![],
                    min_length: None,
                    max_length: None,
                    priority: 10,
                }],
            })
            .available_hints(vec!["fast".to_string()])
            .route_model_by_hint(route_model_by_hint)
            .build()
            .expect("agent builder should succeed with valid config");

        let response = agent.turn("quick summary please").await.unwrap();
        assert_eq!(response, "classified");
        let seen = seen_models.lock();
        assert_eq!(seen.as_slice(), &["hint:fast".to_string()]);
    }

    #[tokio::test]
    async fn from_config_passes_extra_headers_to_custom_provider() {
        use axum::{Json, Router, http::HeaderMap, routing::post};
        use tempfile::TempDir;
        use tokio::net::TcpListener;

        let captured_headers: Arc<std::sync::Mutex<Option<HashMap<String, String>>>> =
            Arc::new(std::sync::Mutex::new(None));
        let captured_headers_clone = captured_headers.clone();

        let app = Router::new().route(
            "/chat/completions",
            post(
                move |headers: HeaderMap, Json(_body): Json<serde_json::Value>| {
                    let captured_headers = captured_headers_clone.clone();
                    async move {
                        let collected = headers
                            .iter()
                            .filter_map(|(name, value)| {
                                value
                                    .to_str()
                                    .ok()
                                    .map(|value| (name.as_str().to_string(), value.to_string()))
                            })
                            .collect();
                        *captured_headers.lock().unwrap() = Some(collected);
                        Json(serde_json::json!({
                            "choices": [{
                                "message": {
                                    "content": "hello from mock"
                                }
                            }]
                        }))
                    }
                },
            ),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mock_addr = listener.local_addr().unwrap();
        let server_handle = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let tmp = TempDir::new().expect("temp dir");
        let workspace_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();

        let mut config = zeroclaw_config::schema::Config {
            data_dir: workspace_dir,
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        {
            let entry = config
                .providers
                .models
                .ensure("custom", "default")
                .expect("custom model_provider type slot");
            entry.api_key = Some("test-key".to_string());
            entry.model = Some("test-model".to_string());
            entry.uri = Some(format!("http://{mock_addr}"));
            entry.extra_headers.insert(
                "User-Agent".to_string(),
                "zeroclaw-web-test/1.0".to_string(),
            );
            entry
                .extra_headers
                .insert("X-Title".to_string(), "zeroclaw-web".to_string());
        }
        config.memory.backend = "none".to_string();
        config.memory.auto_save = false;

        // An explicit agent is required. Wire up a minimal agent that
        // points at the synthesized model_provider entry, then construct
        // Agent::from_config against it.
        config.risk_profiles.insert(
            "test-profile".to_string(),
            zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        let agent_cfg = zeroclaw_config::schema::AliasedAgentConfig {
            model_provider: "custom.default".into(),
            risk_profile: "test-profile".into(),
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        };
        config.agents.insert("test-agent".to_string(), agent_cfg);

        let mut agent = Agent::from_config(&config, "test-agent")
            .await
            .expect("agent from config");
        let response = agent.turn("hello").await.expect("agent turn");

        assert_eq!(response, "hello from mock");

        let headers = captured_headers
            .lock()
            .unwrap()
            .clone()
            .expect("captured headers");
        assert_eq!(
            headers.get("user-agent").map(String::as_str),
            Some("zeroclaw-web-test/1.0")
        );
        assert_eq!(
            headers.get("x-title").map(String::as_str),
            Some("zeroclaw-web")
        );

        server_handle.abort();
    }

    #[tokio::test]
    async fn from_config_accepts_openai_alias_with_requires_openai_auth() {
        use tempfile::TempDir;
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, ModelProviderConfig, OpenAIModelProviderConfig,
            RiskProfileConfig, WireApi,
        };

        let tmp = TempDir::new().expect("temp dir");
        let workspace_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");

        let mut config = Config {
            data_dir: workspace_dir,
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        config.memory.backend = "none".to_string();
        config.memory.auto_save = false;
        config
            .risk_profiles
            .insert("test-profile".to_string(), RiskProfileConfig::default());
        config.providers.models.openai.insert(
            "codex".to_string(),
            OpenAIModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("gpt-5.4".to_string()),
                    requires_openai_auth: true,
                    wire_api: Some(WireApi::Responses),
                    ..ModelProviderConfig::default()
                },
            },
        );
        config.agents.insert(
            "test-agent".to_string(),
            AliasedAgentConfig {
                model_provider: "openai.codex".into(),
                risk_profile: "test-profile".into(),
                ..AliasedAgentConfig::default()
            },
        );

        let result = Agent::from_config(&config, "test-agent").await;

        assert!(
            result.is_ok(),
            "openai alias with requires_openai_auth should construct via Codex OAuth path: {}",
            result.err().unwrap()
        );
    }

    #[test]
    fn builder_allowed_tools_none_keeps_all_tools() {
        let model_provider = Box::new(MockModelProvider {
            responses: Mutex::new(vec![]),
        });

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .allowed_tools(None)
            .build()
            .expect("agent builder should succeed with valid config");

        assert_eq!(agent.tools.len(), 1);
        assert_eq!(agent.tools[0].name(), "echo");
    }

    #[test]
    fn builder_allowed_tools_some_filters_tools() {
        let model_provider = Box::new(MockModelProvider {
            responses: Mutex::new(vec![]),
        });

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .allowed_tools(Some(vec!["nonexistent".to_string()]))
            .build()
            .expect("agent builder should succeed with valid config");

        assert!(
            agent.tools.is_empty(),
            "No tools should match a non-existent allowlist entry"
        );
    }

    #[test]
    fn session_cwd_keeps_workspace_in_allowed_roots() {
        let workspace = std::env::temp_dir().join("zeroclaw_test_session_cwd_workspace");
        let session = std::env::temp_dir().join("zeroclaw_test_session_cwd_session");
        let _ = std::fs::create_dir_all(&workspace);
        let _ = std::fs::create_dir_all(&session);

        let skill_file = workspace.join("SKILL.md");
        let _ = std::fs::write(&skill_file, "body");
        // is_resolved_path_allowed expects a canonicalized path (symlinks resolved).
        let skill_resolved = std::fs::canonicalize(&skill_file).unwrap_or(skill_file);

        let risk_profile = zeroclaw_config::schema::RiskProfileConfig::default();

        // Policy WITH the fix: workspace pushed into allowed_roots.
        let mut policy = SecurityPolicy::from_risk_profile(&risk_profile, &session);
        policy.allowed_roots.push(workspace.clone());
        assert!(
            policy.is_resolved_path_allowed(&skill_resolved),
            "workspace skills must remain readable when session_cwd differs"
        );

        // Without the push the same path must be denied, confirming the push
        // is the load-bearing fix rather than an incidental side-effect.
        let policy_no_push = SecurityPolicy::from_risk_profile(&risk_profile, &session);
        assert!(
            !policy_no_push.is_resolved_path_allowed(&skill_resolved),
            "without allowed_roots.push, workspace files must be outside the sandbox"
        );
    }

    #[test]
    fn seed_history_prepends_system_and_skips_system_from_seed() {
        let model_provider = Box::new(MockModelProvider {
            responses: Mutex::new(vec![]),
        });

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let seed = vec![
            ChatMessage::system("old system prompt"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi there"),
        ];
        agent.seed_history(&seed);

        let history = agent.history();
        // First message should be a freshly built system prompt (not the seed one)
        assert!(matches!(&history[0], ConversationMessage::Chat(m) if m.role == "system"));
        // System message from seed should be skipped, so next is user
        assert!(
            matches!(&history[1], ConversationMessage::Chat(m) if m.role == "user" && m.content == "hello")
        );
        assert!(
            matches!(&history[2], ConversationMessage::Chat(m) if m.role == "assistant" && m.content == "hi there")
        );
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn set_tool_dispatcher_refreshes_existing_system_prompt() {
        use zeroclaw_api::model_provider::{ChatMessage, ConversationMessage};

        let model_provider = Box::new(MockModelProvider {
            responses: Mutex::new(vec![]),
        });
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(XmlToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        agent.seed_history(&[ChatMessage::user("hello")]);
        let before = match agent.history().first() {
            Some(ConversationMessage::Chat(m)) if m.role == "system" => m.content.clone(),
            other => panic!("expected a system prompt first, got {other:?}"),
        };
        assert!(
            before.contains("Tool Use Protocol"),
            "xml dispatcher system prompt should carry the xml tool protocol"
        );

        agent.set_tool_dispatcher(Box::new(NativeToolDispatcher));
        let after = match agent.history().first() {
            Some(ConversationMessage::Chat(m)) if m.role == "system" => m.content.clone(),
            other => panic!("expected a system prompt first, got {other:?}"),
        };
        assert!(
            !after.contains("Tool Use Protocol"),
            "native dispatcher system prompt must not carry the xml tool protocol after swap"
        );
    }

    #[test]
    fn seed_conversation_history_preserves_tool_call_variants() {
        use zeroclaw_api::model_provider::{
            ChatMessage, ConversationMessage, ToolCall, ToolResultMessage,
        };

        let provider = Box::new(MockModelProvider {
            responses: Mutex::new(vec![]),
        });

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let messages = vec![
            ConversationMessage::Chat(ChatMessage::user("run it")),
            ConversationMessage::AssistantToolCalls {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "tc-1".into(),
                    name: "shell".into(),
                    arguments: r#"{"command":"ls"}"#.into(),
                    extra_content: None,
                }],
                reasoning_content: None,
            },
            ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: "tc-1".into(),
                content: "ok".into(),
                tool_name: String::new(),
            }]),
            ConversationMessage::Chat(ChatMessage::assistant("done")),
        ];

        agent.seed_conversation_history(messages);

        // System prompt may have been prepended; find non-system messages
        let non_system: Vec<_> = agent
            .history()
            .iter()
            .filter(|m| !matches!(m, ConversationMessage::Chat(c) if c.role == "system"))
            .collect();

        assert_eq!(non_system.len(), 4);
        assert!(
            matches!(non_system[1], ConversationMessage::AssistantToolCalls { tool_calls, .. } if tool_calls[0].id == "tc-1")
        );
        assert!(
            matches!(non_system[2], ConversationMessage::ToolResults(r) if r[0].tool_call_id == "tc-1")
        );
    }

    #[test]
    fn seed_history_trims_over_cap_restore_and_returns_transport_event() {
        let capturing = Arc::new(CapturingObserver::default());
        let observer: Arc<dyn Observer> = capturing.clone();
        let mut agent = trim_history_test_agent(2, observer);

        let event = agent.seed_history_with_event(&[
            ChatMessage::user("old request"),
            ChatMessage::assistant("old answer"),
            ChatMessage::user("new request"),
            ChatMessage::assistant("new answer"),
        ]);

        assert!(matches!(
            event,
            Some(TurnEvent::HistoryTrimmed {
                dropped_messages: 2,
                kept_turns: 1,
                ..
            })
        ));
        assert!(agent.history_has_trim_breadcrumb);
        assert!(matches!(
            agent.history.get(2),
            Some(ConversationMessage::Chat(message))
                if message.role == "user" && message.content == "new request"
        ));
        assert_eq!(
            capturing
                .events
                .lock()
                .iter()
                .filter(|event| matches!(event, ObserverEvent::HistoryTrimmed { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn seed_conversation_history_trims_over_cap_restore_without_splitting_tools() {
        use zeroclaw_providers::{ToolCall, ToolResultMessage};

        let capturing = Arc::new(CapturingObserver::default());
        let observer: Arc<dyn Observer> = capturing.clone();
        let mut agent = trim_history_test_agent(4, observer);
        let event = agent.seed_conversation_history_with_event(vec![
            ConversationMessage::Chat(ChatMessage::user("old request")),
            ConversationMessage::Chat(ChatMessage::assistant("old answer")),
            ConversationMessage::Chat(ChatMessage::user("new request")),
            ConversationMessage::AssistantToolCalls {
                text: Some("running".into()),
                tool_calls: vec![ToolCall {
                    id: "seed-call".into(),
                    name: "echo".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                reasoning_content: None,
            },
            ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: "seed-call".into(),
                content: "result".into(),
                tool_name: "echo".into(),
            }]),
            ConversationMessage::Chat(ChatMessage::assistant("new answer")),
        ]);

        assert!(matches!(
            event,
            Some(TurnEvent::HistoryTrimmed {
                dropped_messages: 2,
                kept_turns: 1,
                ..
            })
        ));
        assert!(matches!(
            (&agent.history[3], &agent.history[4]),
            (
                ConversationMessage::AssistantToolCalls { tool_calls, .. },
                ConversationMessage::ToolResults(results),
            ) if tool_calls[0].id == "seed-call" && results[0].tool_call_id == "seed-call"
        ));
        assert_eq!(
            capturing
                .events
                .lock()
                .iter()
                .filter(|event| matches!(event, ObserverEvent::HistoryTrimmed { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn clear_history_resets_trim_breadcrumb_provenance_before_reuse() {
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = trim_history_test_agent(2, observer);
        agent.history = vec![
            ConversationMessage::Chat(ChatMessage::system("system")),
            ConversationMessage::Chat(ChatMessage::user("old user")),
            ConversationMessage::Chat(ChatMessage::assistant("old assistant")),
            ConversationMessage::Chat(ChatMessage::user("new user")),
            ConversationMessage::Chat(ChatMessage::assistant("new assistant")),
        ];
        let _ = agent.trim_history(None);
        assert!(agent.history_has_trim_breadcrumb);

        agent.clear_history();
        assert!(!agent.history_has_trim_breadcrumb);

        let breadcrumb = crate::i18n::get_required_cli_string("history-trim-breadcrumb");
        agent.seed_history(&[
            ChatMessage::user(breadcrumb.clone()),
            ChatMessage::assistant("user-authored marker reply"),
        ]);
        assert!(!agent.history_has_trim_breadcrumb);

        agent.seed_history(&[
            ChatMessage::user("later user"),
            ChatMessage::assistant("later assistant"),
        ]);
        assert!(agent.history_has_trim_breadcrumb);
        assert_eq!(
            agent
                .history
                .iter()
                .filter(|message| matches!(
                    message,
                    ConversationMessage::Chat(chat) if chat.content == breadcrumb
                ))
                .count(),
            1,
            "the user-authored marker must be dropped as an ordinary old turn before one synthetic breadcrumb is inserted"
        );
        assert!(agent.history.iter().any(|message| matches!(
            message,
            ConversationMessage::Chat(chat)
                if chat.role == "user" && chat.content == "later user"
        )));
    }

    #[test]
    fn append_seed_history_preserves_existing_trim_breadcrumb_provenance() {
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = trim_history_test_agent(2, observer);
        agent.seed_history(&[
            ChatMessage::user("old user"),
            ChatMessage::assistant("old assistant"),
            ChatMessage::user("kept user"),
            ChatMessage::assistant("kept assistant"),
        ]);
        assert!(agent.history_has_trim_breadcrumb);

        agent.seed_history(&[
            ChatMessage::user("appended user"),
            ChatMessage::assistant("appended assistant"),
        ]);

        let breadcrumb = crate::i18n::get_required_cli_string("history-trim-breadcrumb");
        assert!(agent.history_has_trim_breadcrumb);
        assert_eq!(
            agent
                .history
                .iter()
                .filter(|message| matches!(
                    message,
                    ConversationMessage::Chat(chat) if chat.content == breadcrumb
                ))
                .count(),
            1
        );
        assert!(agent.history.iter().any(|message| matches!(
            message,
            ConversationMessage::Chat(chat)
                if chat.role == "user" && chat.content == "appended user"
        )));
    }

    #[test]
    fn append_conversation_seed_preserves_existing_trim_breadcrumb_provenance() {
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = trim_history_test_agent(2, observer);
        agent.seed_conversation_history(vec![
            ConversationMessage::Chat(ChatMessage::user("old user")),
            ConversationMessage::Chat(ChatMessage::assistant("old assistant")),
            ConversationMessage::Chat(ChatMessage::user("kept user")),
            ConversationMessage::Chat(ChatMessage::assistant("kept assistant")),
        ]);
        assert!(agent.history_has_trim_breadcrumb);

        agent.seed_conversation_history(vec![
            ConversationMessage::Chat(ChatMessage::user("appended user")),
            ConversationMessage::Chat(ChatMessage::assistant("appended assistant")),
        ]);

        let breadcrumb = crate::i18n::get_required_cli_string("history-trim-breadcrumb");
        assert!(agent.history_has_trim_breadcrumb);
        assert_eq!(
            agent
                .history
                .iter()
                .filter(|message| matches!(
                    message,
                    ConversationMessage::Chat(chat) if chat.content == breadcrumb
                ))
                .count(),
            1
        );
        assert!(agent.history.iter().any(|message| matches!(
            message,
            ConversationMessage::Chat(chat)
                if chat.role == "user" && chat.content == "appended user"
        )));
    }

    /// Mock provider that captures whether tool specs were passed to `stream_chat`
    /// and returns a tool call followed by a text response through the stream.
    struct StreamToolCaptureModelProvider {
        tools_received: Arc<Mutex<Vec<bool>>>,
        call_count: Arc<Mutex<usize>>,
    }

    #[async_trait]
    impl ModelProvider for StreamToolCaptureModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            Ok("ok".into())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<zeroclaw_providers::ChatResponse> {
            self.tools_received.lock().push(request.tools.is_some());
            let mut count = self.call_count.lock();
            *count += 1;
            if *count == 1 {
                Ok(zeroclaw_providers::ChatResponse {
                    text: Some(String::new()),
                    tool_calls: vec![zeroclaw_providers::ToolCall {
                        id: "00000000-0000-0000-0000-000000000001".into(),
                        name: "echo".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                })
            } else {
                Ok(zeroclaw_providers::ChatResponse {
                    text: Some("stream-done".into()),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                })
            }
        }

        fn supports_native_tools(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: zeroclaw_providers::traits::StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<zeroclaw_providers::traits::StreamEvent>,
        > {
            use futures_util::stream::{self, StreamExt};
            self.tools_received.lock().push(request.tools.is_some());
            let mut count = self.call_count.lock();
            *count += 1;
            if *count == 1 {
                let tc = zeroclaw_providers::traits::StreamEvent::ToolCall(
                    zeroclaw_providers::ToolCall {
                        id: "00000000-0000-0000-0000-000000000001".into(),
                        name: "echo".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    },
                );
                stream::iter(vec![
                    Ok(tc),
                    Ok(zeroclaw_providers::traits::StreamEvent::Final),
                ])
                .boxed()
            } else {
                let chunk = zeroclaw_providers::traits::StreamEvent::TextDelta(
                    zeroclaw_providers::traits::StreamChunk {
                        delta: "stream-done".into(),
                        is_final: false,
                        reasoning: None,
                        token_count: 0,
                    },
                );
                stream::iter(vec![
                    Ok(chunk),
                    Ok(zeroclaw_providers::traits::StreamEvent::Final),
                ])
                .boxed()
            }
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for StreamToolCaptureModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "StreamToolCaptureModelProvider"
        }
    }

    #[tokio::test]
    async fn turn_streamed_passes_tool_specs_to_provider() {
        let tools_received = Arc::new(Mutex::new(Vec::new()));
        let model_provider = Box::new(StreamToolCaptureModelProvider {
            tools_received: tools_received.clone(),
            call_count: Arc::new(Mutex::new(0)),
        });

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let (response, _) = agent
            .turn_streamed("use the echo tool", event_tx, None)
            .await
            .unwrap();
        assert_eq!(response, "stream-done");

        // Verify tools were passed in both stream_chat calls
        let received = tools_received.lock();
        assert!(
            received.len() >= 2,
            "Expected at least 2 stream_chat calls, got {}",
            received.len()
        );
        assert!(
            received[0],
            "First stream_chat call should have received tool specs"
        );
        assert!(
            received[1],
            "Second stream_chat call should have received tool specs"
        );

        // Collect events and verify tool call + tool result were emitted
        let mut events = Vec::new();
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }
        let has_tool_call = events
            .iter()
            .any(|e| matches!(e, TurnEvent::ToolCall { name, .. } if name == "echo"));
        let has_tool_result = events
            .iter()
            .any(|e| matches!(e, TurnEvent::ToolResult { name, .. } if name == "echo"));
        assert!(
            has_tool_call,
            "Should have emitted a ToolCall event for 'echo'"
        );
        assert!(
            has_tool_result,
            "Should have emitted a ToolResult event for 'echo'"
        );

        // Verify ID correlation
        let call_id = events
            .iter()
            .find_map(|e| {
                if let TurnEvent::ToolCall { id, .. } = e {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .expect("ToolCall should have an ID");

        let result_id = events
            .iter()
            .find_map(|e| {
                if let TurnEvent::ToolResult { id, .. } = e {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .expect("ToolResult should have an ID");

        assert_eq!(
            call_id, result_id,
            "ToolCall and ToolResult should share the same ID for correlation"
        );

        // Verify it's a valid UUID
        assert!(
            uuid::Uuid::parse_str(&call_id).is_ok(),
            "Generated ID should be a valid UUID: got '{}'",
            call_id
        );
    }

    fn tool_receipts_enabled_config(enabled: bool) -> zeroclaw_config::schema::AliasedAgentConfig {
        zeroclaw_config::schema::AliasedAgentConfig {
            resolved: zeroclaw_config::schema::ResolvedRuntime {
                tool_receipts: zeroclaw_config::schema::ToolReceiptsConfig {
                    enabled,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        }
    }

    fn streamed_agent_with_receipts(enabled: bool) -> Agent {
        let model_provider = Box::new(StreamToolCaptureModelProvider {
            tools_received: Arc::new(Mutex::new(Vec::new())),
            call_count: Arc::new(Mutex::new(0)),
        });
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .config(tool_receipts_enabled_config(enabled))
            .build()
            .expect("agent builder should succeed with valid config")
    }

    fn history_has_receipt(agent: &Agent) -> bool {
        agent.history().iter().any(|m| match m {
            ConversationMessage::ToolResults(results) => results
                .iter()
                .any(|r| r.content.contains("[receipt: zc-receipt-")),
            _ => false,
        })
    }

    // RED on upstream/master: the streamed turn path (ACP, gateway WS) hardcoded
    // `receipt_generator: None`, so an enabled config produced zero receipts.
    // GREEN once `turn_streamed` derives the scope from its own config through
    // the shared `ReceiptScope::from_config` seam.
    #[tokio::test]
    async fn turn_streamed_signs_tool_results_when_receipts_enabled() {
        let mut agent = streamed_agent_with_receipts(true);
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        agent
            .turn_streamed("use the echo tool", event_tx, None)
            .await
            .expect("streamed turn should succeed");
        assert!(
            history_has_receipt(&agent),
            "enabled receipts must sign tool results on the streamed path"
        );
    }

    // GREEN control: disabled config produces no receipts on the same path.
    #[tokio::test]
    async fn turn_streamed_omits_receipts_when_disabled() {
        let mut agent = streamed_agent_with_receipts(false);
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        agent
            .turn_streamed("use the echo tool", event_tx, None)
            .await
            .expect("streamed turn should succeed");
        assert!(
            !history_has_receipt(&agent),
            "disabled receipts must not sign tool results"
        );
    }

    fn show_in_response_config(show: bool) -> zeroclaw_config::schema::AliasedAgentConfig {
        zeroclaw_config::schema::AliasedAgentConfig {
            resolved: zeroclaw_config::schema::ResolvedRuntime {
                tool_receipts: zeroclaw_config::schema::ToolReceiptsConfig {
                    enabled: true,
                    show_in_response: show,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        }
    }

    fn streamed_agent_with_config(config: zeroclaw_config::schema::AliasedAgentConfig) -> Agent {
        let model_provider = Box::new(StreamToolCaptureModelProvider {
            tools_received: Arc::new(Mutex::new(Vec::new())),
            call_count: Arc::new(Mutex::new(0)),
        });
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .config(config)
            .build()
            .expect("agent builder should succeed with valid config")
    }

    // RED on the pre-fix branch: `show_in_response` was read only in the channel
    // orchestrator, so ACP/WS/CLI turns never appended the auditable block.
    // GREEN once the turn paths route the collector through
    // `append_receipts_block`.
    #[tokio::test]
    async fn turn_streamed_appends_receipts_block_when_show_in_response() {
        let mut agent = streamed_agent_with_config(show_in_response_config(true));
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let (response, _msgs) = agent
            .turn_streamed("use the echo tool", event_tx, None)
            .await
            .expect("streamed turn should succeed");
        assert!(
            response.contains("---\nTool receipts:") && response.contains("zc-receipt-"),
            "show_in_response must append the Tool receipts block to the reply, got: {response}"
        );
    }

    // Control: with show_in_response off the reply carries no receipts block,
    // even though receipts are still signed into history.
    #[tokio::test]
    async fn turn_streamed_omits_receipts_block_when_show_in_response_off() {
        let mut agent = streamed_agent_with_config(show_in_response_config(false));
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let (response, _msgs) = agent
            .turn_streamed("use the echo tool", event_tx, None)
            .await
            .expect("streamed turn should succeed");
        assert!(
            !response.contains("Tool receipts:"),
            "no receipts block when show_in_response is off, got: {response}"
        );
        assert!(
            history_has_receipt(&agent),
            "receipts are still signed into history when only the reply block is off"
        );
    }

    // The receipt-echo system-prompt addendum is added on the turn path when
    // inject_system_prompt is on (default), matching the channel orchestrator.
    #[test]
    fn build_system_prompt_injects_receipt_addendum_when_enabled() {
        let agent = streamed_agent_with_config(show_in_response_config(true));
        let prompt = agent
            .build_system_prompt()
            .expect("system prompt should build");
        assert!(
            prompt.contains("## Tool Execution Receipts"),
            "enabled receipts with inject_system_prompt must add the addendum"
        );
    }

    /// then finishes. Used to verify serial dispatch ordering.
    struct TwoToolCallStreamModelProvider {
        call_count: Arc<Mutex<usize>>,
    }

    #[async_trait]
    impl ModelProvider for TwoToolCallStreamModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            Ok("ok".into())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<zeroclaw_providers::ChatResponse> {
            Ok(zeroclaw_providers::ChatResponse {
                text: Some("done".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }

        fn supports_native_tools(&self) -> bool {
            true
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn supports_streaming_tool_events(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: zeroclaw_providers::traits::StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<zeroclaw_providers::traits::StreamEvent>,
        > {
            use futures_util::stream::{self, StreamExt};
            let mut count = self.call_count.lock();
            *count += 1;
            if *count == 1 {
                stream::iter(vec![
                    Ok(zeroclaw_providers::traits::StreamEvent::ToolCall(
                        zeroclaw_providers::ToolCall {
                            id: "00000000-0000-0000-0000-000000000001".into(),
                            name: "echo".into(),
                            arguments: "{}".into(),
                            extra_content: None,
                        },
                    )),
                    Ok(zeroclaw_providers::traits::StreamEvent::ToolCall(
                        zeroclaw_providers::ToolCall {
                            id: "00000000-0000-0000-0000-000000000002".into(),
                            name: "echo".into(),
                            arguments: "{}".into(),
                            extra_content: None,
                        },
                    )),
                    Ok(zeroclaw_providers::traits::StreamEvent::Final),
                ])
                .boxed()
            } else {
                stream::iter(vec![
                    Ok(zeroclaw_providers::traits::StreamEvent::TextDelta(
                        zeroclaw_providers::traits::StreamChunk {
                            delta: "stream-done".into(),
                            is_final: false,
                            reasoning: None,
                            token_count: 0,
                        },
                    )),
                    Ok(zeroclaw_providers::traits::StreamEvent::Final),
                ])
                .boxed()
            }
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for TwoToolCallStreamModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "TwoToolCallStreamModelProvider"
        }
    }

    #[tokio::test]
    async fn turn_streamed_dispatches_multiple_tools_serially_when_parallel_disabled() {
        let model_provider = Box::new(TwoToolCallStreamModelProvider {
            call_count: Arc::new(Mutex::new(0)),
        });

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        // Default resolved config has parallel_tools = false; this is the
        // serial path under test.
        assert!(
            !agent.config.resolved.parallel_tools,
            "test precondition: parallel_tools must be disabled"
        );

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let (response, _) = agent
            .turn_streamed("use echo twice", event_tx, None)
            .await
            .unwrap();
        assert_eq!(response, "stream-done");

        // Reduce events to the call/result sequence, tagged by id.
        let mut seq: Vec<(&'static str, String)> = Vec::new();
        while let Ok(ev) = event_rx.try_recv() {
            match ev {
                TurnEvent::ToolCall { id, .. } => seq.push(("call", id)),
                TurnEvent::ToolResult { id, .. } => seq.push(("result", id)),
                _ => {}
            }
        }

        let id1 = "00000000-0000-0000-0000-000000000001";
        let id2 = "00000000-0000-0000-0000-000000000002";
        assert_eq!(
            seq,
            vec![
                ("call", id1.to_string()),
                ("result", id1.to_string()),
                ("call", id2.to_string()),
                ("result", id2.to_string()),
            ],
            "serial dispatch must interleave call->result per tool, not batch all \
             starts then all results; got {seq:?}"
        );
    }

    struct PreExecutedToolModelProvider;

    #[async_trait]
    impl ModelProvider for PreExecutedToolModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            Ok(String::new())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<zeroclaw_providers::ChatResponse> {
            Ok(zeroclaw_providers::ChatResponse {
                text: Some(String::new()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: zeroclaw_providers::traits::StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<zeroclaw_providers::traits::StreamEvent>,
        > {
            use futures_util::stream::{self, StreamExt};

            stream::iter(vec![
                Ok(
                    zeroclaw_providers::traits::StreamEvent::PreExecutedToolCall {
                        name: "file_read".into(),
                        args: "{\"path\":\"a.txt\"}".into(),
                    },
                ),
                Ok(
                    zeroclaw_providers::traits::StreamEvent::PreExecutedToolCall {
                        name: "shell".into(),
                        args: "{\"command\":\"pwd\"}".into(),
                    },
                ),
                Ok(
                    zeroclaw_providers::traits::StreamEvent::PreExecutedToolResult {
                        name: "file_read".into(),
                        output: "a".into(),
                    },
                ),
                Ok(
                    zeroclaw_providers::traits::StreamEvent::PreExecutedToolResult {
                        name: "shell".into(),
                        output: "b".into(),
                    },
                ),
                Ok(zeroclaw_providers::traits::StreamEvent::Final),
            ])
            .boxed()
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for PreExecutedToolModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "PreExecutedToolModelProvider"
        }
    }

    #[tokio::test]
    async fn pre_executed_tool_results_keep_ids_when_calls_overlap() {
        let model_provider = Box::new(PreExecutedToolModelProvider);

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let _ = agent
            .turn_streamed("use pre-executed tools", event_tx, None)
            .await
            .unwrap();

        let mut call_ids = HashMap::new();
        let mut result_ids = HashMap::new();
        while let Ok(event) = event_rx.try_recv() {
            match event {
                TurnEvent::ToolCall { id, name, .. } => {
                    call_ids.insert(name, id);
                }
                TurnEvent::ToolResult { id, name, .. } => {
                    result_ids.insert(name, id);
                }
                _ => {}
            }
        }

        assert_eq!(call_ids.len(), 2, "expected two pre-executed tool calls");
        assert_eq!(
            result_ids.len(),
            2,
            "expected two pre-executed tool results"
        );
        assert_eq!(call_ids.get("file_read"), result_ids.get("file_read"));
        assert_eq!(call_ids.get("shell"), result_ids.get("shell"));
    }

    #[tokio::test]
    async fn turn_normalizes_user_image_markers_before_provider_call() {
        let seen_user_messages = Arc::new(Mutex::new(Vec::new()));
        let provider = Box::new(MultimodalCaptureProvider {
            seen_user_messages: seen_user_messages.clone(),
            streamed: false,
        });

        let temp = tempfile::tempdir().expect("tempdir");
        let image_path = temp.path().join("agent-turn.png");
        std::fs::write(
            &image_path,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
        )
        .expect("write fixture");

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .multimodal_config(zeroclaw_config::schema::MultimodalConfig::default())
            .build()
            .expect("agent builder should succeed with valid config");

        agent
            .turn(&format!(
                "inspect [IMAGE:{}]",
                image_path.display().to_string()
            ))
            .await
            .expect("turn should succeed");

        let seen = seen_user_messages.lock();
        let last = seen.last().expect("provider should receive a user message");
        assert!(
            last.contains("data:image/png;base64,"),
            "expected normalized data URI in provider request, got: {last}"
        );
    }

    #[tokio::test]
    async fn turn_streamed_normalizes_user_image_markers_before_provider_call() {
        let seen_user_messages = Arc::new(Mutex::new(Vec::new()));
        let provider = Box::new(MultimodalCaptureProvider {
            seen_user_messages: seen_user_messages.clone(),
            streamed: true,
        });

        let temp = tempfile::tempdir().expect("tempdir");
        let image_path = temp.path().join("agent-stream.png");
        std::fs::write(
            &image_path,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
        )
        .expect("write fixture");

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .multimodal_config(zeroclaw_config::schema::MultimodalConfig::default())
            .build()
            .expect("agent builder should succeed with valid config");

        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(8);
        agent
            .turn_streamed(
                &format!("inspect [IMAGE:{}]", image_path.display().to_string()),
                event_tx,
                None,
            )
            .await
            .expect("turn_streamed should succeed");

        let seen = seen_user_messages.lock();
        let last = seen.last().expect("provider should receive a user message");
        assert!(
            last.contains("data:image/png;base64,"),
            "expected normalized data URI in provider request, got: {last}"
        );
    }

    fn trim_history_test_agent(max_history_messages: usize, observer: Arc<dyn Observer>) -> Agent {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );
        let agent_config = zeroclaw_config::schema::AliasedAgentConfig {
            resolved: zeroclaw_config::schema::ResolvedRuntime::default(),
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        };

        Agent::builder()
            .model_provider(Box::new(MockModelProvider {
                responses: Mutex::new(vec![]),
            }))
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .config(agent_config)
            .structured_max_history_messages(max_history_messages)
            .build()
            .expect("agent builder should succeed with valid config")
    }

    fn seed_old_trim_test_turn(agent: &mut Agent) {
        agent.history = vec![
            ConversationMessage::Chat(ChatMessage::system("system")),
            ConversationMessage::Chat(ChatMessage::user("old user")),
            ConversationMessage::Chat(ChatMessage::assistant("old assistant")),
        ];
    }

    fn assert_old_trim_test_turn_was_removed(agent: &Agent) {
        assert!(agent.history_has_trim_breadcrumb);
        assert!(!agent.history.iter().any(|message| matches!(
            message,
            ConversationMessage::Chat(chat)
                if chat.content == "old user" || chat.content == "old assistant"
        )));
    }

    fn drain_history_trim_events(event_rx: &mut tokio::sync::mpsc::Receiver<TurnEvent>) -> usize {
        let mut count = 0;
        while let Ok(event) = event_rx.try_recv() {
            if matches!(event, TurnEvent::HistoryTrimmed { .. }) {
                count += 1;
            }
        }
        count
    }

    fn push_trim_history_tool_exchange(agent: &mut Agent, index: usize) {
        use zeroclaw_providers::{ToolCall, ToolResultMessage};

        let tool_call_id = format!("trim-history-call-{index}");
        agent.history.push(ConversationMessage::AssistantToolCalls {
            text: Some(format!("Calling tool {index}")),
            tool_calls: vec![ToolCall {
                id: tool_call_id.clone(),
                name: "mock".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            reasoning_content: None,
        });
        agent
            .history
            .push(ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id,
                content: format!("result {index}"),
                tool_name: "mock".into(),
            }]));
    }

    #[test]
    fn trim_history_preserves_single_tool_heavy_turn_over_message_cap() {
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = trim_history_test_agent(50, observer);
        agent
            .history
            .push(ConversationMessage::Chat(ChatMessage::user("start")));
        for index in 1..=31 {
            push_trim_history_tool_exchange(&mut agent, index);
        }
        agent
            .history
            .push(ConversationMessage::Chat(ChatMessage::assistant("done")));

        let _ = agent.trim_history(None);

        assert_eq!(
            agent.history.len(),
            64,
            "the newest complete turn must survive even when it exceeds the message cap"
        );
        assert!(matches!(
            agent.history.first(),
            Some(ConversationMessage::Chat(message))
                if message.role == "user" && message.content == "start"
        ));
        assert!(matches!(
            agent.history.last(),
            Some(ConversationMessage::Chat(message))
                if message.role == "assistant" && message.content == "done"
        ));
        for (index, pair) in agent.history[1..63].chunks_exact(2).enumerate() {
            let expected_id = format!("trim-history-call-{}", index + 1);
            match pair {
                [
                    ConversationMessage::AssistantToolCalls { tool_calls, .. },
                    ConversationMessage::ToolResults(results),
                ] => {
                    assert_eq!(tool_calls.len(), 1);
                    assert_eq!(results.len(), 1);
                    assert_eq!(tool_calls[0].id, expected_id);
                    assert_eq!(results[0].tool_call_id, expected_id);
                }
                _ => panic!("tool exchange {} was split or reordered", index + 1),
            }
        }
    }

    #[test]
    fn trim_history_drops_old_turn_with_breadcrumb_and_observer_event() {
        let capturing = Arc::new(CapturingObserver::default());
        let observer: Arc<dyn Observer> = capturing.clone();
        let mut agent = trim_history_test_agent(2, observer);
        agent.history = vec![
            ConversationMessage::Chat(ChatMessage::system("system")),
            ConversationMessage::Chat(ChatMessage::user("old user")),
            ConversationMessage::Chat(ChatMessage::assistant("old assistant")),
            ConversationMessage::Chat(ChatMessage::user("new user")),
            ConversationMessage::Chat(ChatMessage::assistant("new assistant")),
        ];

        let _ = agent.trim_history(None);

        let breadcrumb = crate::i18n::get_required_cli_string("history-trim-breadcrumb");
        assert!(matches!(
            agent.history.first(),
            Some(ConversationMessage::Chat(message))
                if message.role == "system"
        ));
        assert!(matches!(
            agent.history.get(1),
            Some(ConversationMessage::Chat(message))
                if message.role == "user" && message.content == breadcrumb
        ));
        assert_eq!(
            agent
                .history
                .iter()
                .filter(|message| matches!(
                    message,
                    ConversationMessage::Chat(chat) if chat.content == breadcrumb
                ))
                .count(),
            1,
            "trim breadcrumb must be inserted exactly once"
        );
        assert!(matches!(
            agent.history.get(2),
            Some(ConversationMessage::Chat(message))
                if message.role == "user" && message.content == "new user"
        ));
        assert!(matches!(
            agent.history.get(3),
            Some(ConversationMessage::Chat(message))
                if message.role == "assistant" && message.content == "new assistant"
        ));
        assert_eq!(
            agent.history.len(),
            4,
            "only the complete newest turn remains"
        );

        let trim_events: Vec<_> = capturing
            .events
            .lock()
            .iter()
            .filter_map(|event| match event {
                ObserverEvent::HistoryTrimmed {
                    dropped_messages,
                    kept_turns,
                    reason,
                    ..
                } => Some((*dropped_messages, *kept_turns, reason.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(trim_events.len(), 1, "one observer trim event is required");
        assert_eq!(trim_events[0].0, 2);
        assert_eq!(trim_events[0].1, 1);
        assert_eq!(
            trim_events[0].2,
            crate::i18n::get_required_cli_string("history-trim-reason-message-cap")
        );
    }

    #[tokio::test]
    async fn trim_history_runs_after_direct_tool_loop_provider_error() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );
        let capturing = Arc::new(CapturingObserver::default());
        let observer: Arc<dyn Observer> = capturing.clone();
        let config = zeroclaw_config::schema::AliasedAgentConfig {
            resolved: zeroclaw_config::schema::ResolvedRuntime::default(),
            ..Default::default()
        };
        let mut agent = Agent::builder()
            .model_provider(Box::new(ToolThenFailingModelProvider {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }))
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .model_name("test-model".into())
            .config(config)
            .structured_max_history_messages(2)
            .build()
            .expect("agent builder should succeed with valid config");
        agent.history = vec![
            ConversationMessage::Chat(ChatMessage::system("system")),
            ConversationMessage::Chat(ChatMessage::user("old request")),
            ConversationMessage::Chat(ChatMessage::assistant("old answer")),
        ];

        let error = agent
            .turn("new request")
            .await
            .expect_err("second provider call should fail");

        assert!(
            error
                .to_string()
                .contains("provider unavailable after tool")
        );
        assert!(agent.history_has_trim_breadcrumb);
        assert!(!agent.history.iter().any(|message| matches!(
            message,
            ConversationMessage::Chat(chat)
                if chat.content == "old request" || chat.content == "old answer"
        )));
        assert!(agent.history.iter().any(|message| matches!(
            message,
            ConversationMessage::Chat(chat)
                if chat.role == "user" && chat.content.contains("new request")
        )));
        assert!(agent.history.windows(2).any(|pair| matches!(
            pair,
            [
                ConversationMessage::AssistantToolCalls { tool_calls, .. },
                ConversationMessage::ToolResults(results),
            ] if tool_calls[0].id == "error-path-call"
                && results[0].tool_call_id == "error-path-call"
        )));
        assert_eq!(
            capturing
                .events
                .lock()
                .iter()
                .filter(|event| matches!(event, ObserverEvent::HistoryTrimmed { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn trim_history_runs_after_direct_vision_resolution_error() {
        let capturing = Arc::new(CapturingObserver::default());
        let observer: Arc<dyn Observer> = capturing.clone();
        let mut agent = trim_history_test_agent(2, observer);
        seed_old_trim_test_turn(&mut agent);

        let error = agent
            .turn("inspect [IMAGE:data:image/png;base64,iVBORw0KGgo=]")
            .await
            .expect_err("missing vision support should fail before provider dispatch");

        assert!(error.to_string().contains("does not support vision input"));
        assert_old_trim_test_turn_was_removed(&agent);
        assert_eq!(
            capturing
                .events
                .lock()
                .iter()
                .filter(|event| matches!(event, ObserverEvent::HistoryTrimmed { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn trim_history_runs_after_streamed_vision_resolution_error() {
        let capturing = Arc::new(CapturingObserver::default());
        let observer: Arc<dyn Observer> = capturing.clone();
        let mut agent = trim_history_test_agent(2, observer);
        seed_old_trim_test_turn(&mut agent);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(8);

        let error = agent
            .turn_streamed(
                "inspect [IMAGE:data:image/png;base64,iVBORw0KGgo=]",
                event_tx,
                None,
            )
            .await
            .expect_err("missing vision support should fail before provider dispatch");

        assert!(error.to_string().contains("does not support vision input"));
        assert_old_trim_test_turn_was_removed(&agent);
        assert_eq!(drain_history_trim_events(&mut event_rx), 1);
    }

    #[tokio::test]
    async fn trim_history_runs_after_direct_system_prompt_rebuild_error() {
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = trim_history_test_agent(2, observer);
        seed_old_trim_test_turn(&mut agent);
        agent.prompt_builder =
            SystemPromptBuilder::default().add_section(Box::new(FailingPromptSection));

        let error = agent
            .turn("new user")
            .await
            .expect_err("synthetic prompt rebuild should fail");

        assert!(
            error
                .to_string()
                .contains("synthetic prompt rebuild failure")
        );
        assert_old_trim_test_turn_was_removed(&agent);
    }

    #[tokio::test]
    async fn trim_history_runs_after_streamed_system_prompt_rebuild_error() {
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = trim_history_test_agent(2, observer);
        seed_old_trim_test_turn(&mut agent);
        agent.prompt_builder =
            SystemPromptBuilder::default().add_section(Box::new(FailingPromptSection));
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(8);

        let error = agent
            .turn_streamed("new user", event_tx, None)
            .await
            .expect_err("synthetic prompt rebuild should fail");

        assert!(
            error
                .to_string()
                .contains("synthetic prompt rebuild failure")
        );
        assert_old_trim_test_turn_was_removed(&agent);
        assert_eq!(drain_history_trim_events(&mut event_rx), 1);
    }

    #[tokio::test]
    async fn trim_history_runs_before_streamed_round_loop_exhaustion_error() {
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = trim_history_test_agent(2, observer);
        agent.config.resolved.max_tool_iterations = 0;
        seed_old_trim_test_turn(&mut agent);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(8);

        let error = agent
            .turn_streamed("new user", event_tx, None)
            .await
            .expect_err("zero rounds should return the exhaustion error");

        assert!(
            error
                .to_string()
                .contains("exceeded maximum tool iterations (0)")
        );
        assert_old_trim_test_turn_was_removed(&agent);
        assert_eq!(drain_history_trim_events(&mut event_rx), 1);
    }

    #[test]
    fn trim_history_log_uses_canonical_attribution() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut log_rx = zeroclaw_log::subscribe_or_install();
        while log_rx.try_recv().is_ok() {}

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = trim_history_test_agent(2, observer);
        agent.agent_alias = "trim-test-agent".into();
        agent.channel_name = "trim-test-channel".into();
        agent.history = vec![
            ConversationMessage::Chat(ChatMessage::system("system")),
            ConversationMessage::Chat(ChatMessage::user("old user")),
            ConversationMessage::Chat(ChatMessage::assistant("old assistant")),
            ConversationMessage::Chat(ChatMessage::user("new user")),
            ConversationMessage::Chat(ChatMessage::assistant("new assistant")),
        ];

        let _ = agent.trim_history(Some("trim-test-turn"));

        let mut selected = None;
        let mut candidates = Vec::new();
        loop {
            match log_rx.try_recv() {
                Ok(value)
                    if value.get("message").and_then(serde_json::Value::as_str)
                        == Some("trim_history: dropped oldest whole turns") =>
                {
                    if value.get("trace_id").and_then(serde_json::Value::as_str)
                        == Some("trim-test-turn")
                    {
                        selected = Some(value.clone());
                    }
                    candidates.push(value);
                }
                Ok(_) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            }
        }
        let value = selected.unwrap_or_else(|| {
            panic!(
                "trim LogEvent with trace_id=trim-test-turn was not captured; candidates: {candidates:#?}"
            )
        });
        let event: zeroclaw_log::LogEvent =
            serde_json::from_value(value).expect("captured trim event should deserialize");

        assert_eq!(event.zeroclaw.get("agent_alias"), Some("trim-test-agent"));
        assert_eq!(
            event.zeroclaw.get("channel_type"),
            Some("trim-test-channel")
        );
        assert_eq!(event.zeroclaw.get("channel"), None);
        assert_eq!(event.trace_id.as_deref(), Some("trim-test-turn"));
        assert!(event.attributes.get("agent_alias").is_none());
        assert!(event.attributes.get("channel").is_none());
        assert!(event.attributes.get("turn_id").is_none());

        zeroclaw_log::clear_broadcast_hook();
    }

    #[tokio::test]
    async fn trim_history_streamed_turn_forwards_single_hard_cap_event() {
        let capturing = Arc::new(CapturingObserver::default());
        let observer: Arc<dyn Observer> = capturing.clone();
        let mut agent = trim_history_test_agent(2, observer);
        agent.history = vec![
            ConversationMessage::Chat(ChatMessage::system("system")),
            ConversationMessage::Chat(ChatMessage::user("old user")),
            ConversationMessage::Chat(ChatMessage::assistant("old assistant")),
        ];
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(16);

        agent
            .turn_streamed("new user", event_tx, None)
            .await
            .expect("streamed turn should succeed");

        let mut trim_events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            if let TurnEvent::HistoryTrimmed {
                dropped_messages,
                kept_turns,
                reason,
            } = event
            {
                trim_events.push((dropped_messages, kept_turns, reason));
            }
        }
        assert_eq!(trim_events.len(), 1, "one streamed trim event is required");
        assert_eq!(trim_events[0].0, 2);
        assert_eq!(trim_events[0].1, 1);
        assert_eq!(
            trim_events[0].2,
            crate::i18n::get_required_cli_string("history-trim-reason-message-cap")
        );
        assert!(capturing.events.lock().iter().any(|event| matches!(
            event,
            ObserverEvent::HistoryTrimmed {
                turn_id: Some(_),
                ..
            }
        )));
    }

    #[tokio::test]
    async fn trim_history_cancel_before_output_retains_synthesized_newest_turn() {
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = trim_history_test_agent(2, observer);
        agent.history = vec![
            ConversationMessage::Chat(ChatMessage::system("system")),
            ConversationMessage::Chat(ChatMessage::user("old user")),
            ConversationMessage::Chat(ChatMessage::assistant("old assistant")),
        ];
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(16);
        let cancel_token = tokio_util::sync::CancellationToken::new();
        cancel_token.cancel();

        let error = agent
            .turn_streamed_with_steering_state("new user", event_tx, Some(cancel_token), None)
            .await
            .expect_err("pre-cancelled streamed turn should return cancellation");

        let breadcrumb = crate::i18n::get_required_cli_string("history-trim-breadcrumb");
        let interruption = crate::i18n::get_required_cli_string("turn-interrupted-by-user");
        assert!(crate::agent::loop_::is_tool_loop_cancelled(&error.error));
        assert_eq!(error.committed_response, interruption);
        assert_eq!(agent.history.len(), 4);
        assert!(matches!(
            agent.history.first(),
            Some(ConversationMessage::Chat(message))
                if message.role == "system"
        ));
        assert!(matches!(
            agent.history.get(1),
            Some(ConversationMessage::Chat(message))
                if message.role == "user" && message.content == breadcrumb
        ));
        assert!(matches!(
            agent.history.get(2),
            Some(ConversationMessage::Chat(message))
                if message.role == "user" && message.content.contains("new user")
        ));
        assert!(matches!(
            agent.history.last(),
            Some(ConversationMessage::Chat(message))
                if message.role == "assistant" && message.content == interruption
        ));
        assert!(!agent.history.iter().any(|message| matches!(
            message,
            ConversationMessage::Chat(chat)
                if chat.content == "old user" || chat.content == "old assistant"
        )));

        let mut trim_events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            if let TurnEvent::HistoryTrimmed {
                dropped_messages,
                kept_turns,
                ..
            } = event
            {
                trim_events.push((dropped_messages, kept_turns));
            }
        }
        assert_eq!(trim_events, vec![(2, 1)]);
    }

    // ── Duplicate narration guard ────────────────────────────────────

    #[tokio::test]
    async fn narration_with_tool_calls_produces_no_consecutive_assistant_entries() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let model_provider = Box::new(MockModelProvider {
            responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                text: Some("I will echo the message.".into()),
                tool_calls: vec![zeroclaw_providers::ToolCall {
                    id: "tc1".into(),
                    name: "echo".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                usage: None,
                reasoning_content: None,
            }]),
        });

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        agent.turn("hi").await.unwrap();

        let history = agent.history();
        for window in history.windows(2) {
            let prev_is_assistant_chat = matches!(
                &window[0],
                ConversationMessage::Chat(m) if m.role == "assistant"
            );
            let next_is_tool_calls =
                matches!(&window[1], ConversationMessage::AssistantToolCalls { .. });
            assert!(
                !(prev_is_assistant_chat && next_is_tool_calls),
                "history contains Chat(assistant) immediately before AssistantToolCalls — \
                 duplicate narration push was not removed"
            );
        }
    }

    /// Streaming mock that emits narration text + tool call on the first turn,
    /// then a plain text response on the second. Used to verify the streaming
    /// path has the same duplicate-narration guard as the blocking path.
    struct NarrationStreamModelProvider {
        call_count: Arc<Mutex<usize>>,
    }

    #[async_trait]
    impl ModelProvider for NarrationStreamModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            Ok("ok".into())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<zeroclaw_providers::ChatResponse> {
            Ok(zeroclaw_providers::ChatResponse {
                text: Some("done".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }

        fn supports_native_tools(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: zeroclaw_providers::traits::StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<zeroclaw_providers::traits::StreamEvent>,
        > {
            use futures_util::stream::{self, StreamExt};
            let mut count = self.call_count.lock();
            *count += 1;
            if *count == 1 {
                stream::iter(vec![
                    Ok(zeroclaw_providers::traits::StreamEvent::TextDelta(
                        zeroclaw_providers::traits::StreamChunk {
                            delta: "I will echo the message.".into(),
                            is_final: false,
                            reasoning: None,
                            token_count: 0,
                        },
                    )),
                    Ok(zeroclaw_providers::traits::StreamEvent::ToolCall(
                        zeroclaw_providers::ToolCall {
                            id: "tc1".into(),
                            name: "echo".into(),
                            arguments: "{}".into(),
                            extra_content: None,
                        },
                    )),
                    Ok(zeroclaw_providers::traits::StreamEvent::Final),
                ])
                .boxed()
            } else {
                stream::iter(vec![
                    Ok(zeroclaw_providers::traits::StreamEvent::TextDelta(
                        zeroclaw_providers::traits::StreamChunk {
                            delta: "done".into(),
                            is_final: false,
                            reasoning: None,
                            token_count: 0,
                        },
                    )),
                    Ok(zeroclaw_providers::traits::StreamEvent::Final),
                ])
                .boxed()
            }
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for NarrationStreamModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "NarrationStreamModelProvider"
        }
    }

    #[tokio::test]
    async fn streaming_narration_with_tool_calls_produces_no_consecutive_assistant_entries() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let model_provider = Box::new(NarrationStreamModelProvider {
            call_count: Arc::new(Mutex::new(0)),
        });

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        agent.turn_streamed("hi", event_tx, None).await.unwrap();

        let history = agent.history();
        for window in history.windows(2) {
            let prev_is_assistant_chat = matches!(
                &window[0],
                ConversationMessage::Chat(m) if m.role == "assistant"
            );
            let next_is_tool_calls =
                matches!(&window[1], ConversationMessage::AssistantToolCalls { .. });
            assert!(
                !(prev_is_assistant_chat && next_is_tool_calls),
                "streaming path: history contains Chat(assistant) immediately before \
                 AssistantToolCalls — duplicate narration push was not removed"
            );
        }
    }

    #[tokio::test]
    async fn response_cache_key_uses_full_provider_visible_transcript() {
        let tmp = tempfile::tempdir().expect("temp response cache dir");
        let cache = Arc::new(
            zeroclaw_memory::response_cache::ResponseCache::new(tmp.path(), 60, 100)
                .expect("response cache should initialize"),
        );

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem_a: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );
        let mem_b: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let seen_a = Arc::new(Mutex::new(Vec::new()));
        let seen_b = Arc::new(Mutex::new(Vec::new()));
        let provider_a = Box::new(TranscriptCaptureModelProvider {
            responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                text: Some("from prior transcript".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            }]),
            seen_messages: seen_a.clone(),
        });
        let provider_b = Box::new(TranscriptCaptureModelProvider {
            responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                text: Some("from fresh transcript".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            }]),
            seen_messages: seen_b.clone(),
        });

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent_a = Agent::builder()
            .model_provider(provider_a)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem_a)
            .observer(observer.clone())
            .response_cache(Some(cache.clone()))
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .model_name("test-model".into())
            .temperature(Some(0.0))
            .build()
            .expect("agent builder should succeed with valid config");
        agent_a.seed_history(&[
            ChatMessage::user("earlier turn"),
            ChatMessage::assistant("earlier answer"),
        ]);

        let mut agent_b = Agent::builder()
            .model_provider(provider_b)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem_b)
            .observer(observer)
            .response_cache(Some(cache))
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .model_name("test-model".into())
            .temperature(Some(0.0))
            .build()
            .expect("agent builder should succeed with valid config");

        assert_eq!(
            agent_a.turn("same final prompt").await.unwrap(),
            "from prior transcript"
        );
        assert_eq!(
            agent_b.turn("same final prompt").await.unwrap(),
            "from fresh transcript"
        );
        assert_eq!(seen_a.lock().len(), 1);
        assert_eq!(
            seen_b.lock().len(),
            1,
            "fresh transcript must not reuse a cache entry written for a different prior transcript"
        );
    }

    #[tokio::test]
    async fn response_cache_does_not_cross_serve_memory_conditioned_answers() {
        // A backend whose recall always returns one Core entry with the given
        // content, so injection yields a deterministic, agent-specific preamble.
        // name() != "none" marks it a real, injecting backend for the gate.
        struct FixtureRecallMemory {
            content: String,
        }
        #[async_trait]
        impl Memory for FixtureRecallMemory {
            fn name(&self) -> &str {
                "fixture"
            }
            async fn store(
                &self,
                _: &str,
                _: &str,
                _: MemoryCategory,
                _: Option<&str>,
            ) -> anyhow::Result<()> {
                Ok(())
            }
            async fn recall(
                &self,
                _: &str,
                _: usize,
                _: Option<&str>,
                _: Option<&str>,
                _: Option<&str>,
            ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
                Ok(vec![zeroclaw_memory::MemoryEntry {
                    id: "deploy".into(),
                    key: "deploy".into(),
                    content: self.content.clone(),
                    category: MemoryCategory::Core,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    session_id: None,
                    score: None,
                    namespace: "default".into(),
                    importance: None,
                    superseded_by: None,
                    kind: None,
                    pinned: false,
                    tenant_id: None,
                    agent_alias: None,
                    agent_id: None,
                }])
            }
            async fn get(&self, _: &str) -> anyhow::Result<Option<zeroclaw_memory::MemoryEntry>> {
                Ok(None)
            }
            async fn list(
                &self,
                _: Option<&MemoryCategory>,
                _: Option<&str>,
            ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
                Ok(vec![])
            }
            async fn forget(&self, _: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
            async fn forget_for_agent(&self, _: &str, _: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
            async fn count(&self) -> anyhow::Result<usize> {
                Ok(1)
            }
            async fn health_check(&self) -> bool {
                true
            }
            async fn store_with_agent(
                &self,
                _: &str,
                _: &str,
                _: MemoryCategory,
                _: Option<&str>,
                _: Option<&str>,
                _: Option<f64>,
                _: Option<&str>,
            ) -> anyhow::Result<()> {
                Ok(())
            }
            async fn recall_for_agents(
                &self,
                _: &[&str],
                query: &str,
                limit: usize,
                session_id: Option<&str>,
                since: Option<&str>,
                until: Option<&str>,
            ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
                self.recall(query, limit, session_id, since, until).await
            }
        }
        impl ::zeroclaw_api::attribution::Attributable for FixtureRecallMemory {
            fn role(&self) -> ::zeroclaw_api::attribution::Role {
                ::zeroclaw_api::attribution::Role::Memory(
                    ::zeroclaw_api::attribution::MemoryKind::InMemory,
                )
            }
            fn alias(&self) -> &str {
                "FixtureRecallMemory"
            }
        }

        // Frozen clock so both turns share a byte-identical bare transcript (the
        // per-turn `[CURRENT DATE & TIME]` prefix is otherwise second-precision),
        // which is what makes the two pre-injection cache keys collide.
        let fixed = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Local);
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});

        let last_user = |seen: &Arc<Mutex<Vec<Vec<ChatMessage>>>>| -> String {
            seen.lock()
                .last()
                .expect("a model call was captured")
                .iter()
                .rev()
                .find(|m| m.role == "user")
                .expect("a user message")
                .content
                .clone()
        };

        let build =
            |mem: Arc<dyn Memory>,
             seen: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
             cache: Arc<zeroclaw_memory::response_cache::ResponseCache>| {
                let provider = Box::new(TranscriptCaptureModelProvider {
                    responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                        text: Some("answer".into()),
                        tool_calls: vec![],
                        usage: None,
                        reasoning_content: None,
                    }]),
                    seen_messages: seen,
                });
                Agent::builder()
                    .model_provider(provider)
                    .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                        vec![],
                    ))
                    .memory(mem)
                    .observer(observer.clone())
                    .response_cache(Some(cache))
                    .tool_dispatcher(Box::new(NativeToolDispatcher))
                    .workspace_dir(std::path::PathBuf::from("/tmp"))
                    .model_name("test-model".into())
                    .temperature(Some(0.0))
                    .turn_datetime(move || fixed)
                    .build()
                    .expect("agent builder should succeed")
            };

        const PROMPT: &str = "what is the deploy target";

        // Harm case: same prompt, DIFFERENT recalled memory, one shared cache.
        let harm_dir = tempfile::tempdir().expect("cache dir");
        let harm_cache = Arc::new(
            zeroclaw_memory::response_cache::ResponseCache::new(harm_dir.path(), 60, 100)
                .expect("response cache"),
        );
        let seen_a = Arc::new(Mutex::new(Vec::new()));
        let seen_b = Arc::new(Mutex::new(Vec::new()));
        let mut agent_a = build(
            Arc::new(FixtureRecallMemory {
                content: "the deploy target is prod-3-alpha".into(),
            }),
            seen_a.clone(),
            harm_cache.clone(),
        );
        let mut agent_b = build(
            Arc::new(FixtureRecallMemory {
                content: "the deploy target is prod-9-beta".into(),
            }),
            seen_b.clone(),
            harm_cache.clone(),
        );
        agent_a.turn(PROMPT).await.expect("turn a");
        agent_b.turn(PROMPT).await.expect("turn b");

        assert_eq!(seen_a.lock().len(), 1, "agent A always runs the model");
        assert!(
            last_user(&seen_a).contains("prod-3-alpha"),
            "agent A's model call must see A's injected memory"
        );
        // Pre-fix, B's key equals A's (both pre-injection) so B is served A's
        // prod-3 answer and never runs against its own prod-9 memory.
        assert_eq!(
            seen_b.lock().len(),
            1,
            "agent B must run the model, not reuse A's cache entry keyed on the shared pre-injection transcript"
        );
        assert!(
            last_user(&seen_b).contains("prod-9-beta"),
            "agent B's model call must see B's OWN injected memory, not A's"
        );

        // Control: `none` backend injects nothing, so the two transcripts really
        // are identical and the shared cache DOES hit: the second agent is
        // served from cache and never reaches the model. This proves the harm
        // case is not passing merely because the cache never works.
        let ctrl_dir = tempfile::tempdir().expect("cache dir");
        let ctrl_cache = Arc::new(
            zeroclaw_memory::response_cache::ResponseCache::new(ctrl_dir.path(), 60, 100)
                .expect("response cache"),
        );
        let none_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let none_mem = || -> Arc<dyn Memory> {
            Arc::from(
                zeroclaw_memory::create_memory(&none_cfg, std::path::Path::new("/tmp"), None)
                    .expect("none memory"),
            )
        };
        let seen_c = Arc::new(Mutex::new(Vec::new()));
        let seen_d = Arc::new(Mutex::new(Vec::new()));
        let mut agent_c = build(none_mem(), seen_c.clone(), ctrl_cache.clone());
        let mut agent_d = build(none_mem(), seen_d.clone(), ctrl_cache.clone());
        agent_c.turn(PROMPT).await.expect("turn c");
        agent_d.turn(PROMPT).await.expect("turn d");
        assert_eq!(seen_c.lock().len(), 1, "agent C always runs the model");
        assert_eq!(
            seen_d.lock().len(),
            0,
            "control: with no injection the identical prompt is served from the shared response cache"
        );
    }

    #[test]
    fn response_cache_key_skips_multimodal_image_markers() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let cache = Arc::new(
            zeroclaw_memory::response_cache::ResponseCache::new(tmp.path(), 60, 100)
                .expect("response cache init"),
        );

        let agent = Agent::builder()
            .model_provider(Box::new(MockModelProvider {
                responses: Mutex::new(vec![]),
            }))
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(Arc::from(
                zeroclaw_memory::create_memory(
                    &zeroclaw_config::schema::MemoryConfig {
                        backend: "none".into(),
                        ..zeroclaw_config::schema::MemoryConfig::default()
                    },
                    std::path::Path::new("/tmp"),
                    None,
                )
                .expect("memory"),
            ))
            .observer(Arc::from(crate::observability::NoopObserver {}))
            .response_cache(Some(cache))
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .model_name("test-model".into())
            .temperature(Some(0.0))
            .build()
            .expect("agent builder");

        // Plain text messages should produce a cache key.
        let plain_messages = vec![
            ChatMessage::system("system prompt"),
            ChatMessage::user("hello"),
        ];
        let key = agent.response_cache_key_for_messages(&plain_messages, "test-model");
        assert!(key.is_some(), "plain text prompt must produce a cache key");

        // Messages containing `[IMAGE:]` must return None (skip cache).
        let multimodal_messages = vec![
            ChatMessage::system("system prompt"),
            ChatMessage::user("describe this image [IMAGE:/tmp/photo.png]"),
        ];
        let key = agent.response_cache_key_for_messages(&multimodal_messages, "test-model");
        assert!(
            key.is_none(),
            "multimodal prompt with [IMAGE:] marker must skip response cache"
        );
    }

    #[tokio::test]
    async fn turn_streamed_with_steering_commits_streamed_output_before_continuing() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let seen_messages = Arc::new(Mutex::new(Vec::new()));
        let model_provider = Box::new(StreamingSteeringModelProvider {
            seen_messages: seen_messages.clone(),
            call_count: AtomicUsize::new(0),
            fail_on_call: None,
            fail_chat_on_call: None,
            fail_after_delta_on_call: None,
            delay_chat_on_call: None,
        });
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let (steering_tx, mut steering_rx) = tokio::sync::mpsc::channel::<String>(4);
        let handle = zeroclaw_spawn::spawn!(async move {
            agent
                .turn_streamed_with_steering_state("first", event_tx, None, Some(&mut steering_rx))
                .await
        });

        loop {
            match event_rx.recv().await.expect("turn event should arrive") {
                TurnEvent::Chunk { delta } if delta == "draft" => {
                    steering_tx
                        .send("second".into())
                        .await
                        .expect("steering message should enqueue");
                    break;
                }
                _ => {}
            }
        }

        let outcome = handle
            .await
            .expect("turn task should finish")
            .expect("steered turn should succeed");
        assert_eq!(outcome.response, "draftfinal");

        let new_chat_messages: Vec<_> = outcome
            .new_messages
            .iter()
            .filter_map(|msg| match msg {
                ConversationMessage::Chat(message) => {
                    Some((message.role.as_str(), message.content.as_str()))
                }
                _ => None,
            })
            .collect();
        assert!(
            new_chat_messages
                .iter()
                .any(|(role, content)| { *role == "assistant" && *content == "draft" }),
            "already streamed output must be committed before the steering continuation"
        );
        assert!(
            new_chat_messages
                .iter()
                .any(|(role, content)| { *role == "user" && content.contains("second") }),
            "accepted steering must be retained as its own user turn"
        );

        let seen = seen_messages.lock();
        assert_eq!(seen.len(), 2);
        let second_call = &seen[1];
        assert!(
            second_call
                .iter()
                .any(|msg| msg.role == "assistant" && msg.content == "draft"),
            "second provider call must see the committed streamed assistant text"
        );
        assert!(
            second_call
                .iter()
                .filter(|msg| msg.role == "user")
                .any(|msg| msg.content.contains("second")),
            "second provider call must include the accepted steering user message"
        );
    }

    #[tokio::test]
    async fn turn_streamed_with_steering_error_returns_committed_partial_output() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let model_provider = Box::new(StreamingSteeringModelProvider {
            seen_messages: Arc::new(Mutex::new(Vec::new())),
            call_count: AtomicUsize::new(0),
            fail_on_call: Some(2),
            fail_chat_on_call: Some(3),
            fail_after_delta_on_call: None,
            delay_chat_on_call: None,
        });
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let (steering_tx, mut steering_rx) = tokio::sync::mpsc::channel::<String>(4);
        let handle = zeroclaw_spawn::spawn!(async move {
            agent
                .turn_streamed_with_steering_state("first", event_tx, None, Some(&mut steering_rx))
                .await
        });

        loop {
            match event_rx.recv().await.expect("turn event should arrive") {
                TurnEvent::Chunk { delta } if delta == "draft" => {
                    steering_tx
                        .send("second".into())
                        .await
                        .expect("steering message should enqueue");
                    break;
                }
                _ => {}
            }
        }

        let err = handle
            .await
            .expect("turn task should finish")
            .expect_err("second provider call should fail");
        assert_eq!(err.committed_response, "draft");
        assert!(
            err.new_messages.iter().any(|msg| {
                matches!(msg, ConversationMessage::Chat(message) if message.role == "assistant" && message.content == "draft")
            }),
            "committed partial assistant output should be returned for persistence after continuation failure"
        );
        assert!(
            err.new_messages.iter().any(|msg| {
                matches!(msg, ConversationMessage::Chat(message) if message.role == "user" && message.content.contains("second"))
            }),
            "accepted steering user message should still be returned after continuation failure"
        );
    }

    #[tokio::test]
    async fn turn_streamed_error_before_visible_output_falls_back_to_chat() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let seen_messages = Arc::new(Mutex::new(Vec::new()));
        let model_provider = Box::new(StreamingSteeringModelProvider {
            seen_messages: seen_messages.clone(),
            call_count: AtomicUsize::new(0),
            fail_on_call: Some(1),
            fail_chat_on_call: None,
            fail_after_delta_on_call: None,
            delay_chat_on_call: None,
        });
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let handle = zeroclaw_spawn::spawn!(async move {
            agent
                .turn_streamed_with_steering_state("first", event_tx, None, None)
                .await
        });

        let outcome = handle
            .await
            .expect("turn task should finish")
            .expect("pre-output stream failure should fall back to non-streaming chat");
        assert_eq!(outcome.response, "final");
        assert!(
            outcome.new_messages.iter().any(|msg| {
                matches!(msg, ConversationMessage::Chat(message) if message.role == "assistant" && message.content == "final")
            }),
            "new messages should carry the fallback assistant answer"
        );
        assert!(
            !outcome.new_messages.iter().any(|msg| {
                matches!(msg, ConversationMessage::Chat(message) if message.role == "assistant" && message.content.contains(&crate::i18n::get_english_cli_string_with_args("turn-stream-interrupted", &[])))
            }),
            "successful fallback should not persist interrupted stream text"
        );

        let seen = seen_messages.lock();
        assert_eq!(seen.len(), 2);
        assert!(
            !seen[1]
                .iter()
                .any(|msg| { msg.role == "assistant" && msg.content.contains("draft") }),
            "fallback chat must not receive the abandoned stream attempt as prior assistant text"
        );
    }

    #[tokio::test]
    async fn turn_streamed_error_after_delta_preserves_visible_partial() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let model_provider = Box::new(StreamingSteeringModelProvider {
            seen_messages: Arc::new(Mutex::new(Vec::new())),
            call_count: AtomicUsize::new(0),
            fail_on_call: None,
            fail_chat_on_call: None,
            fail_after_delta_on_call: Some(1),
            delay_chat_on_call: None,
        });
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let handle = zeroclaw_spawn::spawn!(async move {
            agent
                .turn_streamed_with_steering_state("first", event_tx, None, None)
                .await
        });

        assert!(
            matches!(
                event_rx.recv().await,
                Some(TurnEvent::Chunk { delta }) if delta == "draft"
            ),
            "the client should see the streamed text before the provider error"
        );

        let err = handle
            .await
            .expect("turn task should finish")
            .expect_err("post-output stream failure should return an error with partial output");
        assert!(
            err.error
                .to_string()
                .contains("synthetic provider failure after delta"),
            "unexpected error: {}",
            err.error
        );
        assert!(
            err.committed_response
                .contains(&crate::i18n::get_english_cli_string_with_args(
                    "turn-stream-interrupted",
                    &[]
                )),
            "persisted partial text should mark that the visible stream was interrupted"
        );
        assert!(
            err.new_messages.iter().any(|msg| {
                matches!(msg, ConversationMessage::Chat(message) if message.role == "assistant" && message.content.contains("draft"))
            }),
            "new messages should carry the visible assistant partial for gateway persistence"
        );
    }

    #[tokio::test]
    async fn turn_streamed_error_before_visible_output_fallback_can_be_cancelled() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let model_provider = Box::new(StreamingSteeringModelProvider {
            seen_messages: Arc::new(Mutex::new(Vec::new())),
            call_count: AtomicUsize::new(0),
            fail_on_call: Some(1),
            fail_chat_on_call: None,
            fail_after_delta_on_call: None,
            delay_chat_on_call: Some(2),
        });
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let cancel_for_task = cancel_token.clone();
        let handle = zeroclaw_spawn::spawn!(async move {
            agent
                .turn_streamed_with_steering_state("first", event_tx, Some(cancel_for_task), None)
                .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        cancel_token.cancel();

        let err = handle
            .await
            .expect("turn task should finish")
            .expect_err("cancelled fallback should return cancellation");
        assert!(
            crate::agent::loop_::is_tool_loop_cancelled(&err.error),
            "unexpected error: {}",
            err.error
        );
        assert_eq!(
            err.committed_response,
            crate::i18n::get_english_cli_string_with_args("turn-interrupted-by-user", &[])
        );
        assert!(
            err.new_messages.iter().any(|msg| {
                matches!(msg, ConversationMessage::Chat(message) if message.role == "assistant" && message.content == crate::i18n::get_english_cli_string_with_args("turn-interrupted-by-user", &[]))
            }),
            "pre-output fallback cancellation should include an interruption marker"
        );
    }

    #[tokio::test]
    async fn turn_streamed_cancel_before_output_returns_interruption_message() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let model_provider = Box::new(StreamingSteeringModelProvider {
            seen_messages: Arc::new(Mutex::new(Vec::new())),
            call_count: AtomicUsize::new(0),
            fail_on_call: None,
            fail_chat_on_call: None,
            fail_after_delta_on_call: None,
            delay_chat_on_call: None,
        });
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let cancel_token = tokio_util::sync::CancellationToken::new();
        cancel_token.cancel();

        let err = agent
            .turn_streamed_with_steering_state("first", event_tx, Some(cancel_token), None)
            .await
            .expect_err("pre-cancelled turn should return cancellation");

        assert!(
            crate::agent::loop_::is_tool_loop_cancelled(&err.error),
            "unexpected error: {}",
            err.error
        );
        assert_eq!(
            err.committed_response,
            crate::i18n::get_english_cli_string_with_args("turn-interrupted-by-user", &[])
        );
        assert!(
            err.new_messages.iter().any(|msg| {
                matches!(msg, ConversationMessage::Chat(message) if message.role == "assistant" && message.content == crate::i18n::get_english_cli_string_with_args("turn-interrupted-by-user", &[]))
            }),
            "cancelled turn should include an assistant interruption marker for persistence"
        );
    }

    #[tokio::test]
    async fn turn_streamed_stream_error_after_delta_emits_llm_response_failure() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let model_provider = Box::new(StreamingSteeringModelProvider {
            seen_messages: Arc::new(Mutex::new(Vec::new())),
            call_count: AtomicUsize::new(0),
            fail_on_call: None,
            fail_chat_on_call: None,
            fail_after_delta_on_call: Some(1),
            delay_chat_on_call: None,
        });
        let capturing = Arc::new(CapturingObserver::default());
        let observer: Arc<dyn Observer> = capturing.clone();
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let err = agent
            .turn_streamed_with_steering_state("test", event_tx, None, None)
            .await
            .expect_err("provider stream failure should be returned");

        assert!(
            err.committed_response.contains("draft")
                && err
                    .committed_response
                    .contains(&crate::i18n::get_english_cli_string_with_args(
                        "turn-stream-interrupted",
                        &[]
                    )),
            "unexpected committed_response: {}",
            err.committed_response
        );

        let events = capturing.events.lock();
        let request = events
            .iter()
            .find(|e| matches!(e, ObserverEvent::LlmRequest { .. }))
            .expect("LlmRequest should have been recorded");
        let response = events
            .iter()
            .find(|e| matches!(e, ObserverEvent::LlmResponse { .. }))
            .expect("LlmResponse should have been recorded");

        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, ObserverEvent::LlmRequest { .. }))
                .count(),
            1,
            "exactly one LlmRequest expected"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, ObserverEvent::LlmResponse { .. }))
                .count(),
            1,
            "exactly one LlmResponse expected"
        );

        let (
            ObserverEvent::LlmRequest {
                model_provider: req_provider,
                model: req_model,
                ..
            },
            ObserverEvent::LlmResponse {
                model_provider: resp_provider,
                model: resp_model,
                success,
                error_message,
                ..
            },
        ) = (request, response)
        else {
            panic!("matched event variants should be LlmRequest and LlmResponse");
        };

        assert!(!success, "LlmResponse on stream error must be a failure");
        assert!(
            error_message.as_deref().is_some_and(|m| !m.is_empty()),
            "failure LlmResponse must carry a non-empty error_message"
        );
        assert_eq!(req_provider, resp_provider, "provider should match");
        assert_eq!(req_model, resp_model, "model should match");
    }

    #[tokio::test]
    async fn turn_streamed_cancel_during_stream_emits_llm_response_failure() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let model_provider = Box::new(StreamingSteeringModelProvider {
            seen_messages: Arc::new(Mutex::new(Vec::new())),
            call_count: AtomicUsize::new(0),
            fail_on_call: None,
            fail_chat_on_call: None,
            fail_after_delta_on_call: None,
            delay_chat_on_call: None,
        });
        let capturing = Arc::new(CapturingObserver::default());
        let observer: Arc<dyn Observer> = capturing.clone();
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let cancel_for_task = cancel_token.clone();

        let canceller = zeroclaw_spawn::spawn!(async move {
            while let Some(event) = event_rx.recv().await {
                if matches!(event, TurnEvent::Chunk { ref delta } if delta == "draft") {
                    cancel_for_task.cancel();
                    break;
                }
            }
            while event_rx.recv().await.is_some() {}
        });

        let err = agent
            .turn_streamed_with_steering_state("test", event_tx, Some(cancel_token), None)
            .await
            .expect_err("cancelled turn should return cancellation");

        canceller.await.expect("canceller task should finish");

        assert!(
            crate::agent::loop_::is_tool_loop_cancelled(&err.error),
            "cancelled turn should carry the cancellation error: {}",
            err.error
        );

        let events = capturing.events.lock();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, ObserverEvent::LlmRequest { .. }))
                .count(),
            1,
            "exactly one LlmRequest expected"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, ObserverEvent::LlmResponse { .. }))
                .count(),
            1,
            "exactly one LlmResponse expected"
        );

        let request = events
            .iter()
            .find(|e| matches!(e, ObserverEvent::LlmRequest { .. }))
            .expect("LlmRequest should have been recorded");
        let response = events
            .iter()
            .find(|e| matches!(e, ObserverEvent::LlmResponse { .. }))
            .expect("LlmResponse should have been recorded");

        let (
            ObserverEvent::LlmRequest {
                model_provider: req_provider,
                model: req_model,
                ..
            },
            ObserverEvent::LlmResponse {
                model_provider: resp_provider,
                model: resp_model,
                success,
                error_message,
                ..
            },
        ) = (request, response)
        else {
            panic!("matched event variants should be LlmRequest and LlmResponse");
        };

        assert!(!success, "cancellation LlmResponse must be a failure");
        assert_eq!(
            error_message.as_deref(),
            Some("request cancelled by user"),
            "cancellation LlmResponse must carry the fixed cancel message"
        );
        assert_eq!(req_provider, resp_provider, "provider should match");
        assert_eq!(req_model, resp_model, "model should match");
    }

    // ── Skill tool registration & excluded_tools filtering ──────────

    /// A mock tool whose name is configurable (unlike `MockTool` which is
    /// always "echo").
    struct NamedMockTool {
        tool_name: String,
    }

    impl NamedMockTool {
        fn new(name: &str) -> Self {
            Self {
                tool_name: name.to_string(),
            }
        }
    }

    #[async_trait]
    impl Tool for NamedMockTool {
        fn name(&self) -> &str {
            &self.tool_name
        }

        fn description(&self) -> &str {
            "mock"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(&self, _args: serde_json::Value) -> Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            })
        }
    }

    fn make_skill(name: &str, tool_names: &[&str]) -> crate::skills::Skill {
        crate::skills::Skill {
            name: name.to_string(),
            description: format!("{name} skill"),
            description_localizations: Default::default(),
            version: "0.1.0".to_string(),
            author: None,
            tags: vec![],
            tools: tool_names
                .iter()
                .map(|t| crate::skills::SkillTool {
                    name: t.to_string(),
                    description: format!("{t} tool"),
                    kind: "shell".to_string(),
                    command: format!("echo {t}"),
                    args: std::collections::HashMap::new(),
                    target: None,
                    locked_args: std::collections::HashMap::new(),
                    timeout_secs: None,
                })
                .collect(),
            prompts: vec![],
            slash_options: Vec::new(),
            location: None,
        }
    }

    #[test]
    fn register_skill_tools_adds_skill_tools_to_registry() {
        let security = Arc::new(crate::security::SecurityPolicy::default());
        let mut tools: Vec<Box<dyn Tool>> = vec![Box::new(NamedMockTool::new("builtin_a"))];

        let skills = vec![make_skill("deploy", &["run", "status"])];
        tools::register_skill_tools(&mut tools, &skills, security);

        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, &["builtin_a", "deploy__run", "deploy__status"]);
    }

    #[test]
    fn register_skill_tools_skips_shadowed_builtins() {
        let security = Arc::new(crate::security::SecurityPolicy::default());
        // Pre-populate with a tool whose name matches what the skill would produce.
        let mut tools: Vec<Box<dyn Tool>> = vec![Box::new(NamedMockTool::new("my_skill__run"))];

        let skills = vec![make_skill("my_skill", &["run"])];
        tools::register_skill_tools(&mut tools, &skills, security);

        // Should still be just 1 tool — the duplicate was skipped.
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "my_skill__run");
    }

    #[test]
    fn register_skill_tools_honors_excluded_tools() {
        // excluded_tools always subtracts — including skill-defined tools (previously
        // skill tools bypassed the policy entirely; theclass, missed for skills).
        let security = Arc::new(crate::security::SecurityPolicy {
            excluded_tools: Some(vec!["deploy__status".to_string()]),
            ..crate::security::SecurityPolicy::default()
        });
        let mut tools: Vec<Box<dyn Tool>> = vec![Box::new(NamedMockTool::new("builtin_a"))];

        let skills = vec![make_skill("deploy", &["run", "status"])];
        tools::register_skill_tools(&mut tools, &skills, security);

        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(
            names.contains(&"deploy__run"),
            "non-excluded skill tool must register, got {names:?}"
        );
        assert!(
            !names.contains(&"deploy__status"),
            "excluded_tools must subtract the skill tool deploy__status, got {names:?}"
        );
    }

    #[test]
    fn register_skill_tools_allowlist_does_not_hide_skills() {
        // The allowlist gates built-ins, NOT skill tools: skills are granted explicitly via
        // skill config, and builtin-kind skill tools are scoped-elevation wrappers meant to
        // stay callable when the raw tool is off the allowlist. A restrictive allowed_tools
        // that omits the skill tool must NOT remove it (only excluded_tools does).
        let security = Arc::new(crate::security::SecurityPolicy {
            allowed_tools: Some(vec!["shell".to_string()]),
            ..crate::security::SecurityPolicy::default()
        });
        let mut tools: Vec<Box<dyn Tool>> = Vec::new();

        let skills = vec![make_skill("deploy", &["run"])];
        tools::register_skill_tools(&mut tools, &skills, security);

        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(
            names.contains(&"deploy__run"),
            "allowlist must not hide an explicitly-granted skill tool, got {names:?}"
        );
    }

    #[test]
    fn from_config_policy_filter_blocks_raw_target_but_keeps_scoped_wrapper() {
        use crate::skills::{Skill, SkillTool};

        let shell: Arc<dyn Tool> = Arc::new(NamedMockTool::new("shell"));
        let file_read: Arc<dyn Tool> = Arc::new(NamedMockTool::new("file_read"));
        // The resolution registry retains the raw tool so the wrapper can
        // delegate to it even after the policy filter removes it below.
        let resolution: Vec<Arc<dyn Tool>> = vec![Arc::clone(&shell), Arc::clone(&file_read)];

        let mut tools: Vec<Box<dyn Tool>> = vec![
            Box::new(crate::tools::ArcToolRef(Arc::clone(&shell))),
            Box::new(crate::tools::ArcToolRef(Arc::clone(&file_read))),
        ];

        // Allowlist the agent to `file_read` only — the gate from_config now
        // applies to built-ins before skills register. (Pre-fix, from_config
        // honored only the denylist, so raw `shell` leaked through.)
        let policy = crate::security::SecurityPolicy {
            allowed_tools: Some(vec!["file_read".to_string()]),
            workspace_dir: std::env::temp_dir(),
            ..crate::security::SecurityPolicy::default()
        };
        crate::agent::loop_::apply_policy_tool_filter(&mut tools, Some(&policy), None);
        assert!(
            !tools.iter().any(|t| t.name() == "shell"),
            "raw shell must be removed by the allowlist on the from_config path"
        );
        assert!(
            tools.iter().any(|t| t.name() == "file_read"),
            "allowlisted file_read must survive the filter"
        );

        let skill = Skill {
            name: "ops".to_string(),
            description: "d".to_string(),
            description_localizations: Default::default(),
            version: "1".to_string(),
            author: None,
            tags: vec![],
            tools: vec![SkillTool {
                name: "use_shell".to_string(),
                description: "scoped shell".to_string(),
                kind: "builtin".to_string(),
                command: String::new(),
                args: std::collections::HashMap::new(),
                target: Some("shell".to_string()),
                locked_args: std::collections::HashMap::new(),
                timeout_secs: None,
            }],
            prompts: vec![],
            slash_options: Vec::new(),
            location: None,
        };
        tools::register_skill_tools_with_context(
            &mut tools,
            &[skill],
            Arc::new(crate::security::SecurityPolicy::default()),
            &resolution,
        );

        assert!(
            !tools.iter().any(|t| t.name() == "shell"),
            "raw shell must STILL be unavailable after skill registration"
        );
        assert!(
            tools.iter().any(|t| t.name() == "ops__use_shell"),
            "the scoped elevation wrapper must remain the only callable path to shell"
        );
    }

    #[test]
    fn excluded_tools_filters_matching_tools() {
        let mut tools: Vec<Box<dyn Tool>> = vec![
            Box::new(NamedMockTool::new("shell")),
            Box::new(NamedMockTool::new("file_write")),
            Box::new(NamedMockTool::new("web_search")),
        ];

        let excluded = ["shell".to_string(), "file_write".to_string()];
        tools.retain(|t| !excluded.iter().any(|ex| ex == t.name()));

        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, &["web_search"]);
    }

    #[test]
    fn excluded_tools_preserves_non_excluded() {
        let mut tools: Vec<Box<dyn Tool>> = vec![
            Box::new(NamedMockTool::new("shell")),
            Box::new(NamedMockTool::new("file_read")),
            Box::new(NamedMockTool::new("web_fetch")),
        ];

        // Exclude only "shell" — the other two should survive.
        let excluded = ["shell".to_string()];
        tools.retain(|t| !excluded.iter().any(|ex| ex == t.name()));

        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, &["file_read", "web_fetch"]);
    }

    #[test]
    fn empty_excluded_tools_preserves_all() {
        let mut tools: Vec<Box<dyn Tool>> = vec![
            Box::new(NamedMockTool::new("shell")),
            Box::new(NamedMockTool::new("file_read")),
        ];

        let excluded: Vec<String> = vec![];
        if !excluded.is_empty() {
            tools.retain(|t| !excluded.iter().any(|ex| ex == t.name()));
        }

        assert_eq!(tools.len(), 2);
    }

    #[tokio::test]
    async fn turn_streamed_returns_new_messages_at_history_limit() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        // Use a small limit so that pre-filling to the limit forces a trim on
        // the very first new turn.
        let agent_config = zeroclaw_config::schema::AliasedAgentConfig {
            resolved: zeroclaw_config::schema::ResolvedRuntime::default(),
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        };

        // Simple streaming provider that returns plain text (no tool calls).
        let provider = Box::new(NarrationStreamModelProvider {
            call_count: Arc::new(Mutex::new(0)),
        });

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .config(agent_config)
            .structured_max_history_messages(4)
            .build()
            .expect("agent builder should succeed with valid config");

        // Pre-fill the history to exactly max_history_messages non-system
        // messages so that adding a new user+assistant pair triggers trim.
        // (system message is added by turn_streamed on first call, so we
        // push user+assistant pairs to simulate a history-at-limit state.)
        agent
            .history
            .push(ConversationMessage::Chat(ChatMessage::system("sys")));
        for i in 0..2 {
            agent
                .history
                .push(ConversationMessage::Chat(ChatMessage::user(format!(
                    "old {i}"
                ))));
            agent
                .history
                .push(ConversationMessage::Chat(ChatMessage::assistant(format!(
                    "old reply {i}"
                ))));
        }
        // History is now: [system, user0, assistant0, user1, assistant1] = 5
        // entries. The structured message limit of 4 means trim fires after
        // adding the new turn.

        let (event_tx, _rx) = tokio::sync::mpsc::channel::<TurnEvent>(8);
        let (_, new_msgs) = agent
            .turn_streamed("new question", event_tx, None)
            .await
            .expect("turn_streamed should succeed");

        // The returned Vec must contain the new user message.
        let has_user = new_msgs
            .iter()
            .any(|m| matches!(m, ConversationMessage::Chat(c) if c.role == "user"));
        assert!(
            has_user,
            "new_msgs must include the user message even after trim; got: {new_msgs:?}"
        );

        // The returned Vec must contain the new assistant reply.
        let has_assistant = new_msgs
            .iter()
            .any(|m| matches!(m, ConversationMessage::Chat(c) if c.role == "assistant"));
        assert!(
            has_assistant,
            "new_msgs must include the assistant reply even after trim; got: {new_msgs:?}"
        );
    }

    #[test]
    fn excluded_tools_then_skill_registration_end_to_end() {
        let security = Arc::new(crate::security::SecurityPolicy::default());
        let mut tools: Vec<Box<dyn Tool>> = vec![
            Box::new(NamedMockTool::new("shell")),
            Box::new(NamedMockTool::new("file_read")),
            Box::new(NamedMockTool::new("web_fetch")),
        ];

        // Step 1: filter excluded tools (mirrors from_config logic)
        let excluded = ["shell".to_string()];
        tools.retain(|t| !excluded.iter().any(|ex| ex == t.name()));

        // Step 2: register skill tools (mirrors from_config logic)
        let skills = vec![make_skill("ops", &["deploy", "rollback"])];
        tools::register_skill_tools(&mut tools, &skills, security);

        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(
            names,
            &["file_read", "web_fetch", "ops__deploy", "ops__rollback"]
        );
    }

    fn observer_event_turn_id(event: &ObserverEvent) -> Option<&str> {
        match event {
            ObserverEvent::AgentStart { turn_id, .. }
            | ObserverEvent::LlmRequest { turn_id, .. }
            | ObserverEvent::LlmResponse { turn_id, .. }
            | ObserverEvent::AgentEnd { turn_id, .. }
            | ObserverEvent::ToolCall { turn_id, .. }
            | ObserverEvent::ToolCallStart { turn_id, .. }
            | ObserverEvent::MemoryRecall { turn_id, .. }
            | ObserverEvent::MemoryStore { turn_id, .. }
            | ObserverEvent::RagRetrieve { turn_id, .. } => turn_id.as_deref(),
            _ => None,
        }
    }

    fn assert_all_events_share_turn_id(
        events: &[ObserverEvent],
        expected_alias: Option<&str>,
        expected_channel: Option<&str>,
    ) {
        let mut turn_ids: Vec<String> = Vec::new();
        for event in events {
            let (variant, channel, agent_alias, turn_id) = match event {
                ObserverEvent::AgentStart {
                    channel,
                    agent_alias,
                    turn_id,
                    ..
                } => ("AgentStart", channel, agent_alias, turn_id),
                ObserverEvent::AgentEnd {
                    channel,
                    agent_alias,
                    turn_id,
                    ..
                } => ("AgentEnd", channel, agent_alias, turn_id),
                ObserverEvent::LlmRequest {
                    channel,
                    agent_alias,
                    turn_id,
                    ..
                } => ("LlmRequest", channel, agent_alias, turn_id),
                ObserverEvent::LlmResponse {
                    channel,
                    agent_alias,
                    turn_id,
                    ..
                } => ("LlmResponse", channel, agent_alias, turn_id),
                ObserverEvent::ToolCallStart {
                    channel,
                    agent_alias,
                    turn_id,
                    ..
                } => ("ToolCallStart", channel, agent_alias, turn_id),
                ObserverEvent::ToolCall {
                    channel,
                    agent_alias,
                    turn_id,
                    ..
                } => ("ToolCall", channel, agent_alias, turn_id),
                ObserverEvent::MemoryRecall {
                    channel,
                    agent_alias,
                    turn_id,
                    ..
                } => ("MemoryRecall", channel, agent_alias, turn_id),
                ObserverEvent::MemoryStore {
                    channel,
                    agent_alias,
                    turn_id,
                    ..
                } => ("MemoryStore", channel, agent_alias, turn_id),
                ObserverEvent::RagRetrieve {
                    channel,
                    agent_alias,
                    turn_id,
                    ..
                } => ("RagRetrieve", channel, agent_alias, turn_id),
                _ => continue,
            };
            assert!(
                channel.is_some(),
                "{variant} observer event must carry channel, got None: {event:?}"
            );
            assert!(
                agent_alias.is_some(),
                "{variant} observer event must carry agent_alias, got None: {event:?}"
            );
            assert!(
                turn_id.is_some(),
                "{variant} observer event must carry turn_id, got None: {event:?}"
            );
            turn_ids.push(turn_id.clone().expect("checked Some above"));
        }

        assert!(!turn_ids.is_empty(), "expected turn events with turn_id");
        let first = &turn_ids[0];
        assert!(
            turn_ids.iter().all(|id| id == first),
            "all turn_ids should be consistent"
        );

        if let Some(alias) = expected_alias {
            for e in events {
                let agent_alias = match e {
                    ObserverEvent::AgentStart { agent_alias, .. }
                    | ObserverEvent::AgentEnd { agent_alias, .. }
                    | ObserverEvent::LlmRequest { agent_alias, .. }
                    | ObserverEvent::LlmResponse { agent_alias, .. }
                    | ObserverEvent::ToolCallStart { agent_alias, .. }
                    | ObserverEvent::ToolCall { agent_alias, .. }
                    | ObserverEvent::MemoryRecall { agent_alias, .. }
                    | ObserverEvent::MemoryStore { agent_alias, .. }
                    | ObserverEvent::RagRetrieve { agent_alias, .. } => agent_alias,
                    _ => continue,
                };
                assert_eq!(
                    agent_alias.as_deref(),
                    Some(alias),
                    "agent_alias should be consistent"
                );
            }
        }

        if let Some(channel) = expected_channel {
            for e in events {
                let ch = match e {
                    ObserverEvent::AgentStart { channel: ch, .. }
                    | ObserverEvent::LlmRequest { channel: ch, .. }
                    | ObserverEvent::LlmResponse { channel: ch, .. }
                    | ObserverEvent::ToolCallStart { channel: ch, .. }
                    | ObserverEvent::ToolCall { channel: ch, .. }
                    | ObserverEvent::AgentEnd { channel: ch, .. }
                    | ObserverEvent::MemoryRecall { channel: ch, .. }
                    | ObserverEvent::MemoryStore { channel: ch, .. }
                    | ObserverEvent::RagRetrieve { channel: ch, .. } => ch,
                    _ => continue,
                };
                assert_eq!(ch.as_deref(), Some(channel), "channel should be consistent");
            }
        }
    }

    fn assert_single_agent_lifecycle(events: &[ObserverEvent]) -> (usize, usize) {
        let starts: Vec<_> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| matches!(event, ObserverEvent::AgentStart { .. }))
            .collect();
        let ends: Vec<_> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| matches!(event, ObserverEvent::AgentEnd { .. }))
            .collect();

        assert_eq!(starts.len(), 1, "expected exactly one AgentStart");
        assert_eq!(ends.len(), 1, "expected exactly one AgentEnd");
        assert!(starts[0].0 < ends[0].0, "AgentEnd must follow AgentStart");
        assert_eq!(
            observer_event_turn_id(starts[0].1),
            observer_event_turn_id(ends[0].1),
            "AgentEnd turn_id must match AgentStart turn_id"
        );

        (starts[0].0, ends[0].0)
    }

    fn agent_end_tokens(
        event: &ObserverEvent,
    ) -> Option<zeroclaw_api::observability_traits::TurnTokenUsage> {
        match event {
            ObserverEvent::AgentEnd { tokens_used, .. } => tokens_used.clone(),
            _ => None,
        }
    }

    #[tokio::test]
    async fn turn_cache_hit_emits_agent_end_with_none_tokens() {
        let tmp = tempfile::tempdir().expect("temp response cache dir");
        let cache = Arc::new(
            zeroclaw_memory::response_cache::ResponseCache::new(tmp.path(), 60, 100)
                .expect("response cache should initialize"),
        );
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem_a: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );
        let mem_b: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let ws_dir = tmp.path().to_path_buf();
        let mut agent_a = Agent::builder()
            .model_provider(Box::new(MockModelProvider {
                responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                    text: Some("cached answer".into()),
                    tool_calls: vec![],
                    usage: Some(zeroclaw_providers::traits::TokenUsage {
                        input_tokens: Some(10),
                        cached_input_tokens: None,
                        output_tokens: Some(5),
                    }),
                    reasoning_content: None,
                }]),
            }))
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem_a)
            .observer(Arc::from(crate::observability::NoopObserver {}) as Arc<dyn Observer>)
            .response_cache(Some(cache.clone()))
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(ws_dir.clone())
            .model_name("test-model".into())
            .temperature(Some(0.0))
            .prompt_builder(SystemPromptBuilder::default())
            .turn_datetime(fixed_response_cache_turn_datetime)
            .build()
            .expect("agent builder should succeed with valid config");

        assert_eq!(agent_a.turn("seed").await.unwrap(), "cached answer");

        let capturing = Arc::new(CapturingObserver::default());
        let observer: Arc<dyn Observer> = capturing.clone();
        let mut agent_b = Agent::builder()
            .model_provider(Box::new(MockModelProvider {
                responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                    text: Some("uncached answer".into()),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                }]),
            }))
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem_b)
            .observer(observer)
            .response_cache(Some(cache))
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(ws_dir)
            .model_name("test-model".into())
            .temperature(Some(0.0))
            .prompt_builder(SystemPromptBuilder::default())
            .turn_datetime(fixed_response_cache_turn_datetime)
            .build()
            .expect("agent builder should succeed with valid config");

        assert_eq!(agent_b.turn("seed").await.unwrap(), "cached answer");

        let events = capturing.events.lock();
        let (_, end_idx) = assert_single_agent_lifecycle(&events);
        assert!(agent_end_tokens(&events[end_idx]).is_none());
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ObserverEvent::LlmRequest { .. })),
            "cache hit should not call the LLM"
        );
    }

    #[tokio::test]
    async fn turn_streamed_cancel_during_tool_execution_emits_agent_end_with_tokens() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );
        let capturing = Arc::new(CapturingObserver::default());
        let observer: Arc<dyn Observer> = capturing.clone();
        let mut agent = Agent::builder()
            .model_provider(Box::new(MockModelProvider {
                responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                    text: Some("I will echo.".into()),
                    tool_calls: vec![zeroclaw_providers::ToolCall {
                        id: "tc1".into(),
                        name: "echo".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    }],
                    usage: Some(zeroclaw_providers::traits::TokenUsage {
                        input_tokens: Some(10),
                        cached_input_tokens: None,
                        output_tokens: Some(5),
                    }),
                    reasoning_content: None,
                }]),
            }))
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(SlowTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let cancel_for_task = cancel_token.clone();
        let handle = zeroclaw_spawn::spawn!(async move {
            agent
                .turn_streamed_with_steering_state(
                    "use echo",
                    event_tx,
                    Some(cancel_for_task),
                    None,
                )
                .await
        });

        while let Some(event) = event_rx.recv().await {
            if matches!(event, TurnEvent::Usage { .. }) {
                cancel_token.cancel();
                break;
            }
        }

        handle
            .await
            .expect("turn task should finish")
            .expect_err("turn should be cancelled before tool execution completes");

        let events = capturing.events.lock();
        let (_, end_idx) = assert_single_agent_lifecycle(&events);
        let tokens = agent_end_tokens(&events[end_idx]).expect("AgentEnd should include tokens");
        assert_eq!(tokens.input_tokens, 10);
        assert_eq!(tokens.output_tokens, 5);
        let llm_response_idx = events
            .iter()
            .position(|event| matches!(event, ObserverEvent::LlmResponse { success: true, .. }))
            .expect("successful LlmResponse should be recorded");
        assert!(
            llm_response_idx < end_idx,
            "AgentEnd must follow LlmResponse"
        );
    }

    #[tokio::test]
    async fn turn_reuses_outer_cost_tracking_context() {
        use crate::agent::cost::{
            TOOL_LOOP_COST_TRACKING_CONTEXT, TOOL_LOOP_TURN_USAGE, ToolLoopCostTrackingContext,
            TurnUsage,
        };
        use crate::cost::CostTracker;
        use std::collections::HashMap;

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );
        let workspace = tempfile::TempDir::new().expect("temp dir");
        let tracker = Arc::new(
            CostTracker::new(
                zeroclaw_config::schema::CostConfig {
                    enabled: true,
                    track_per_agent: true,
                    ..zeroclaw_config::schema::CostConfig::default()
                },
                workspace.path(),
            )
            .expect("cost tracker should initialize"),
        );
        let pricing = Arc::new(HashMap::from([(
            "mock-provider".to_string(),
            HashMap::from([
                ("test-model.input".to_string(), 3.0),
                ("test-model.output".to_string(), 15.0),
            ]),
        )]));
        let cost_context = ToolLoopCostTrackingContext::new(Arc::clone(&tracker), pricing)
            .with_agent_alias("agent-turn");
        let turn_usage = Arc::new(parking_lot::Mutex::new(TurnUsage::default()));

        let mut agent = Agent::builder()
            .model_provider(Box::new(MockModelProvider {
                responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                    text: Some("turn cost".into()),
                    tool_calls: vec![],
                    usage: Some(zeroclaw_providers::traits::TokenUsage {
                        input_tokens: Some(1_000),
                        cached_input_tokens: None,
                        output_tokens: Some(200),
                    }),
                    reasoning_content: None,
                }]),
            }))
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(Arc::from(crate::observability::NoopObserver {}) as Arc<dyn Observer>)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .model_name("test-model".into())
            .model_provider_name("mock-provider".into())
            .agent_alias("agent-turn".into())
            .build()
            .expect("agent builder should succeed with valid config");

        let response = TOOL_LOOP_TURN_USAGE
            .scope(
                Some(Arc::clone(&turn_usage)),
                TOOL_LOOP_COST_TRACKING_CONTEXT.scope(Some(cost_context), agent.turn("hello")),
            )
            .await
            .expect("turn should succeed");

        assert_eq!(response, "turn cost");

        let recorded = *turn_usage.lock();
        assert_eq!(recorded.input_tokens, 1_000);
        assert_eq!(recorded.output_tokens, 200);
        assert!(
            recorded.cost_usd > 0.0,
            "outer turn usage should accumulate non-zero cost from scoped pricing"
        );

        let summary = tracker.get_summary().expect("cost summary");
        assert_eq!(summary.request_count, 1);
        assert_eq!(summary.total_tokens, 1_200);
        assert!(
            summary.session_cost_usd > 0.0,
            "scoped tracker should persist turn usage"
        );
        let agent_summary = tracker
            .get_summary_for_agent("agent-turn")
            .expect("agent-scoped summary");
        assert_eq!(agent_summary.request_count, 1);
        assert!(
            agent_summary.session_cost_usd > 0.0,
            "agent alias should flow through persisted turn usage"
        );
    }

    #[tokio::test]
    async fn turn_streamed_reuses_outer_cost_tracking_context() {
        use crate::agent::cost::{
            TOOL_LOOP_COST_TRACKING_CONTEXT, TOOL_LOOP_TURN_USAGE, ToolLoopCostTrackingContext,
            TurnUsage,
        };
        use crate::cost::CostTracker;
        use std::collections::HashMap;

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );
        let workspace = tempfile::TempDir::new().expect("temp dir");
        let tracker = Arc::new(
            CostTracker::new(
                zeroclaw_config::schema::CostConfig {
                    enabled: true,
                    track_per_agent: true,
                    ..zeroclaw_config::schema::CostConfig::default()
                },
                workspace.path(),
            )
            .expect("cost tracker should initialize"),
        );
        let pricing = Arc::new(HashMap::from([(
            "mock-provider".to_string(),
            HashMap::from([
                ("test-model.input".to_string(), 3.0),
                ("test-model.output".to_string(), 15.0),
            ]),
        )]));
        let cost_context = ToolLoopCostTrackingContext::new(Arc::clone(&tracker), pricing)
            .with_agent_alias("streamed-agent");
        let turn_usage = Arc::new(parking_lot::Mutex::new(TurnUsage::default()));

        let mut agent = Agent::builder()
            .model_provider(Box::new(MockModelProvider {
                responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                    text: Some("streamed cost".into()),
                    tool_calls: vec![],
                    usage: Some(zeroclaw_providers::traits::TokenUsage {
                        input_tokens: Some(1_000),
                        cached_input_tokens: None,
                        output_tokens: Some(200),
                    }),
                    reasoning_content: None,
                }]),
            }))
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(Arc::from(crate::observability::NoopObserver {}) as Arc<dyn Observer>)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .model_name("test-model".into())
            .model_provider_name("mock-provider".into())
            .agent_alias("streamed-agent".into())
            .build()
            .expect("agent builder should succeed with valid config");

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let outcome = TOOL_LOOP_TURN_USAGE
            .scope(
                Some(Arc::clone(&turn_usage)),
                TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                    Some(cost_context),
                    agent.turn_streamed_with_steering_state("hello", event_tx, None, None),
                ),
            )
            .await
            .expect("streamed turn should succeed");

        assert_eq!(outcome.response, "streamed cost");
        while event_rx.recv().await.is_some() {}

        let recorded = *turn_usage.lock();
        assert_eq!(recorded.input_tokens, 1_000);
        assert_eq!(recorded.output_tokens, 200);
        assert!(
            recorded.cost_usd > 0.0,
            "outer turn usage should accumulate non-zero cost from scoped pricing"
        );

        let summary = tracker.get_summary().expect("cost summary");
        assert_eq!(summary.request_count, 1);
        assert_eq!(summary.total_tokens, 1_200);
        assert!(
            summary.session_cost_usd > 0.0,
            "scoped tracker should persist streamed-turn usage"
        );
        let agent_summary = tracker
            .get_summary_for_agent("streamed-agent")
            .expect("agent-scoped summary");
        assert_eq!(agent_summary.request_count, 1);
        assert!(
            agent_summary.session_cost_usd > 0.0,
            "agent alias should flow through persisted streamed-turn usage"
        );
    }

    #[tokio::test]
    async fn turn_llm_error_emits_agent_end() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );
        let capturing = Arc::new(CapturingObserver::default());
        let observer: Arc<dyn Observer> = capturing.clone();
        let mut agent = Agent::builder()
            .model_provider(Box::new(FailingModelProvider))
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .model_name("test-model".into())
            .temperature(Some(0.0))
            .build()
            .expect("agent builder should succeed with valid config");

        let result = agent.turn("hello").await;
        assert!(
            result.is_err(),
            "turn should fail when provider is unavailable"
        );

        let events = capturing.events.lock();
        let (_, end_idx) = assert_single_agent_lifecycle(&events);
        assert!(
            agent_end_tokens(&events[end_idx]).is_none(),
            "AgentEnd should have tokens_used: None on LLM error"
        );
    }

    #[tokio::test]
    async fn turn_events_share_consistent_turn_id() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let model_provider = Box::new(MockModelProvider {
            responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                text: Some("done".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            }]),
        });
        let capturing = Arc::new(CapturingObserver::default());
        let observer: Arc<dyn Observer> = capturing.clone();
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .agent_alias("test-agent".into())
            .auto_save(true)
            .build()
            .expect("agent builder should succeed with valid config");

        let _ = agent.turn("test").await.expect("turn should succeed");

        let events = capturing.events.lock();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ObserverEvent::MemoryStore { .. })),
            "auto_save(true) must cause Agent::turn to emit a MemoryStore event \
             so its (channel, agent_alias, turn_id) triple is actually asserted below"
        );
        assert_all_events_share_turn_id(&events, Some("test-agent"), Some("agent"));
    }

    #[tokio::test]
    async fn streamed_turn_events_share_consistent_turn_id() {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let model_provider = Box::new(MockModelProvider {
            responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                text: Some("done".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            }]),
        });
        let capturing = Arc::new(CapturingObserver::default());
        let observer: Arc<dyn Observer> = capturing.clone();
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .agent_alias("test-agent".into())
            .auto_save(true)
            .build()
            .expect("agent builder should succeed with valid config");

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let _ = agent
            .turn_streamed_with_steering_state("test", event_tx, None, None)
            .await
            .expect("streamed turn should succeed");
        while event_rx.recv().await.is_some() {}

        let events = capturing.events.lock();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ObserverEvent::MemoryStore { .. })),
            "auto_save(true) must cause the streamed turn to emit a MemoryStore event"
        );
        assert_all_events_share_turn_id(&events, Some("test-agent"), Some("agent"));
    }

    use crate::agent::loop_::MODEL_SWITCH_TEST_LOCK as MODEL_SWITCH_TEST_GUARD;

    fn build_test_agent(
        initial_provider_name: &str,
        initial_model_name: &str,
        switch_config: Option<ProviderSwitchConfig>,
    ) -> Agent {
        let provider = Box::new(MockModelProvider {
            responses: Mutex::new(vec![]),
        });
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation"),
        );
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut builder = Agent::builder()
            .model_provider(provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(MockTool)],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .model_provider_name(initial_provider_name.to_string())
            .model_name(initial_model_name.to_string());
        if let Some(cfg) = switch_config {
            builder = builder.provider_switch_config(cfg);
        }
        builder.build().expect("agent builder")
    }

    #[test]
    fn try_apply_pending_model_switch_noop_when_no_pending_request() {
        let _guard = MODEL_SWITCH_TEST_GUARD.lock().unwrap();
        crate::agent::loop_::clear_model_switch_request();

        let mut agent = build_test_agent("openai", "gpt-4o-mini", None);
        let result = agent.try_apply_pending_model_switch("gpt-4o-mini");
        assert_eq!(result, None);
        assert_eq!(agent.model_provider_name, "openai");
        assert_eq!(agent.model_name, "gpt-4o-mini");
    }

    #[test]
    fn try_apply_pending_model_switch_noop_when_identical_to_current() {
        let _guard = MODEL_SWITCH_TEST_GUARD.lock().unwrap();
        crate::agent::loop_::clear_model_switch_request();

        // Pre-set an "equal" switch request.
        {
            let state = crate::agent::loop_::get_model_switch_state();
            let mut guard = state.lock().unwrap();
            *guard = Some(("openai".to_string(), "gpt-4o-mini".to_string()));
        }

        let mut agent = build_test_agent("openai", "gpt-4o-mini", None);
        let result = agent.try_apply_pending_model_switch("gpt-4o-mini");
        assert_eq!(result, None, "same-provider/same-model is a no-op");
        // State must be cleared so a stale equal entry does not linger.
        let still_pending = crate::agent::loop_::get_model_switch_state()
            .lock()
            .unwrap()
            .clone();
        assert_eq!(
            still_pending, None,
            "equal switch request must be cleared after observation"
        );
    }

    #[test]
    fn try_apply_pending_model_switch_preserves_state_without_switch_config() {
        let _guard = MODEL_SWITCH_TEST_GUARD.lock().unwrap();
        crate::agent::loop_::clear_model_switch_request();

        // Pre-set a real switch request.
        {
            let state = crate::agent::loop_::get_model_switch_state();
            let mut guard = state.lock().unwrap();
            *guard = Some(("anthropic".to_string(), "claude-haiku".to_string()));
        }

        // Agent has NO provider_switch_config — cannot rebuild provider.
        let mut agent = build_test_agent("openai", "gpt-4o-mini", None);
        let result = agent.try_apply_pending_model_switch("gpt-4o-mini");

        // Returns None (failed switch), state unchanged on the agent,
        // pending request cleared so we don't retry forever.
        assert_eq!(result, None);
        assert_eq!(
            agent.model_provider_name, "openai",
            "provider_name must NOT change when provider rebuild is not possible"
        );
        assert_eq!(
            agent.model_name, "gpt-4o-mini",
            "model_name must NOT change when provider rebuild is not possible"
        );
        let still_pending = crate::agent::loop_::get_model_switch_state()
            .lock()
            .unwrap()
            .clone();
        assert_eq!(
            still_pending, None,
            "pending switch must be cleared even when rebuild fails, \
             so it does not retrigger on the next iteration"
        );
    }

    #[test]
    fn try_apply_pending_model_switch_succeeds_with_switch_config() {
        let _guard = MODEL_SWITCH_TEST_GUARD.lock().unwrap();
        crate::agent::loop_::clear_model_switch_request();

        // Pre-set a real switch request to a different provider AND model.
        {
            let state = crate::agent::loop_::get_model_switch_state();
            let mut guard = state.lock().unwrap();
            *guard = Some(("ollama".to_string(), "llama3".to_string()));
        }

        let switch_cfg = ProviderSwitchConfig {
            config: Some(std::sync::Arc::new(
                zeroclaw_config::schema::Config::default(),
            )),
        };

        let mut agent = build_test_agent("openai", "gpt-4o-mini", Some(switch_cfg));
        let result = agent.try_apply_pending_model_switch("gpt-4o-mini");

        assert_eq!(
            result.as_deref(),
            Some("llama3"),
            "successful switch must return the new effective model"
        );
        assert_eq!(
            agent.model_provider_name, "ollama",
            "provider_name must reflect the switched provider after success"
        );
        assert_eq!(
            agent.model_name, "llama3",
            "model_name must reflect the switched model after success"
        );
        let still_pending = crate::agent::loop_::get_model_switch_state()
            .lock()
            .unwrap()
            .clone();
        assert_eq!(
            still_pending, None,
            "pending switch must be cleared after a successful switch"
        );
    }

    #[test]
    fn try_apply_pending_model_switch_succeeds_on_provider_only_change() {
        let _guard = MODEL_SWITCH_TEST_GUARD.lock().unwrap();
        crate::agent::loop_::clear_model_switch_request();

        // Same model id, different provider.
        {
            let state = crate::agent::loop_::get_model_switch_state();
            let mut guard = state.lock().unwrap();
            *guard = Some(("ollama".to_string(), "shared-name".to_string()));
        }

        let switch_cfg = ProviderSwitchConfig {
            config: Some(std::sync::Arc::new(
                zeroclaw_config::schema::Config::default(),
            )),
        };

        let mut agent = build_test_agent("openai", "shared-name", Some(switch_cfg));
        let result = agent.try_apply_pending_model_switch("shared-name");

        assert_eq!(
            result.as_deref(),
            Some("shared-name"),
            "provider-only switch must also be treated as a successful switch"
        );
        assert_eq!(
            agent.model_provider_name, "ollama",
            "provider_name must update on a provider-only switch"
        );
        assert_eq!(agent.model_name, "shared-name");
    }

    #[test]
    fn try_apply_pending_model_switch_prefers_route_api_key() {
        let _guard = MODEL_SWITCH_TEST_GUARD.lock().unwrap();
        crate::agent::loop_::clear_model_switch_request();

        {
            let state = crate::agent::loop_::get_model_switch_state();
            let mut guard = state.lock().unwrap();
            *guard = Some(("ollama".to_string(), "tinyllama".to_string()));
        }

        let route = zeroclaw_config::schema::ModelRouteConfig {
            model_provider: "ollama".to_string(),
            model: "tinyllama".to_string(),
            hint: "fast".to_string(),
            api_key: Some("route-specific-key".to_string()),
        };

        let route_config = zeroclaw_config::schema::Config {
            model_routes: vec![route],
            ..zeroclaw_config::schema::Config::default()
        };
        let switch_cfg = ProviderSwitchConfig {
            config: Some(std::sync::Arc::new(route_config)),
        };

        let mut agent = build_test_agent("openai", "gpt-4o-mini", Some(switch_cfg));
        let result = agent.try_apply_pending_model_switch("gpt-4o-mini");

        assert_eq!(
            result.as_deref(),
            Some("tinyllama"),
            "switch must succeed when a model_routes entry matches the target"
        );
        assert_eq!(agent.model_provider_name, "ollama");
    }

    /// Streamed mock whose first call emits a tool call (queuing a model
    /// switch via `ModelSwitchTriggerTool`) and whose later calls emit final
    /// text. `call_count` lets the test prove the original provider is used
    /// for exactly the first call — the next call goes to the switched one.
    struct StreamSwitchTriggerProvider {
        call_count: Arc<Mutex<usize>>,
    }

    #[async_trait]
    impl ModelProvider for StreamSwitchTriggerProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            Ok("ok".into())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<zeroclaw_providers::ChatResponse> {
            // The unified loop drives the streaming wrapper through `chat`
            // (stream events are synthesized post-hoc), so the tool call that
            // queues the switch is emitted here on the first call.
            let mut count = self.call_count.lock();
            *count += 1;
            if *count == 1 {
                Ok(zeroclaw_providers::ChatResponse {
                    text: Some(String::new()),
                    tool_calls: vec![zeroclaw_providers::ToolCall {
                        id: "00000000-0000-0000-0000-000000000002".into(),
                        name: "model_switch_trigger".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                })
            } else {
                // Should not be reached: after the switch, the next call goes
                // to the switched provider, not this one.
                Ok(zeroclaw_providers::ChatResponse {
                    text: Some("original-provider-should-not-be-reused".into()),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                })
            }
        }

        fn supports_native_tools(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
            _options: zeroclaw_providers::traits::StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<zeroclaw_providers::traits::StreamEvent>,
        > {
            use futures_util::stream::{self, StreamExt};
            let mut count = self.call_count.lock();
            *count += 1;
            if *count == 1 {
                // First call: ask to run the tool that queues a model switch.
                let tc = zeroclaw_providers::traits::StreamEvent::ToolCall(
                    zeroclaw_providers::ToolCall {
                        id: "00000000-0000-0000-0000-000000000002".into(),
                        name: "model_switch_trigger".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    },
                );
                stream::iter(vec![
                    Ok(tc),
                    Ok(zeroclaw_providers::traits::StreamEvent::Final),
                ])
                .boxed()
            } else {
                // Should not be reached: after the switch, the next call goes
                // to the switched provider, not this one.
                let chunk = zeroclaw_providers::traits::StreamEvent::TextDelta(
                    zeroclaw_providers::traits::StreamChunk {
                        delta: "original-provider-should-not-be-reused".into(),
                        is_final: false,
                        reasoning: None,
                        token_count: 0,
                    },
                );
                stream::iter(vec![
                    Ok(chunk),
                    Ok(zeroclaw_providers::traits::StreamEvent::Final),
                ])
                .boxed()
            }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for StreamSwitchTriggerProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "StreamSwitchTriggerProvider"
        }
    }

    /// Test tool that queues a pending `model_switch` when executed, standing
    /// in for the real `model_switch` tool during a streamed turn.
    struct ModelSwitchTriggerTool {
        target_provider: String,
        target_model: String,
    }

    #[async_trait]
    impl Tool for ModelSwitchTriggerTool {
        fn name(&self) -> &str {
            "model_switch_trigger"
        }
        fn description(&self) -> &str {
            "test tool: queues a pending model switch"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _args: serde_json::Value) -> Result<crate::tools::ToolResult> {
            let state = crate::agent::loop_::get_model_switch_state();
            *state.lock().unwrap() =
                Some((self.target_provider.clone(), self.target_model.clone()));
            Ok(crate::tools::ToolResult {
                success: true,
                output: "model switch queued".into(),
                error: None,
            })
        }
    }

    #[test]
    fn turn_streamed_applies_pending_model_switch_for_next_call() {
        // Serialize with the other tests that touch the process-wide
        // `MODEL_SWITCH_REQUEST`. The async work runs inside a manually built
        // current-thread runtime so the `std::sync` guard is never held across
        // an `.await` in this (synchronous) test body.
        let _guard = MODEL_SWITCH_TEST_GUARD.lock().unwrap();
        crate::agent::loop_::clear_model_switch_request();

        let initial_calls = Arc::new(Mutex::new(0usize));
        let provider = Box::new(StreamSwitchTriggerProvider {
            call_count: Arc::clone(&initial_calls),
        });

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation"),
        );
        let capturing = Arc::new(CapturingObserver::default());
        let observer: Arc<dyn Observer> = capturing.clone();

        let switch_cfg = ProviderSwitchConfig {
            config: Some(std::sync::Arc::new(zeroclaw_config::schema::Config {
                reliability: zeroclaw_config::schema::ReliabilityConfig {
                    provider_retries: 0,
                    provider_backoff_ms: 0,
                    ..zeroclaw_config::schema::ReliabilityConfig::default()
                },
                ..zeroclaw_config::schema::Config::default()
            })),
        };

        let mut agent = Agent::builder()
            .model_provider(provider)
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![Box::new(ModelSwitchTriggerTool {
                    target_provider: "ollama".to_string(),
                    target_model: "llama3".to_string(),
                })],
            ))
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .model_provider_name("openai".to_string())
            .model_name("gpt-4o-mini".to_string())
            .provider_switch_config(switch_cfg)
            .build()
            .expect("agent builder");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(async {
            let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
            // The turn ultimately errors because the switched provider has no
            // live server; the timeout only guards against an unexpected hang.
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                agent.turn_streamed("please switch the model", event_tx, None),
            )
            .await;
        });

        // `turn_streamed` itself must have consumed the pending switch and
        // committed the rebuilt provider/model via `ProviderSwitchConfig`.
        assert_eq!(
            agent.model_provider_name, "ollama",
            "turn_streamed must commit the switched provider after the tool result"
        );
        assert_eq!(
            agent.model_name, "llama3",
            "turn_streamed must commit the switched model after the tool result"
        );

        // The original provider is used for exactly the first call; the next
        // call in the same turn goes to the switched provider instead.
        assert_eq!(
            *initial_calls.lock(),
            1,
            "the original provider must serve only the first call — the next \
             call must use the switched provider, not the original"
        );

        // The next provider call in the same streamed turn targets the
        // switched provider/model: the `LlmRequest` event is recorded at the
        // top of the post-switch iteration, immediately before that call.
        let events = capturing.events.lock();
        let switched_request = events.iter().any(|e| {
            matches!(
                e,
                ObserverEvent::LlmRequest { model_provider, model, .. }
                    if model_provider == "ollama" && model == "llama3"
            )
        });
        assert!(
            switched_request,
            "turn_streamed must issue the next provider call against the switched \
             provider/model (ollama/llama3); captured events: {events:?}"
        );
        drop(events);

        // The pending switch must be cleared after consumption so it does not
        // retrigger on a later iteration or turn.
        let still_pending = crate::agent::loop_::get_model_switch_state()
            .lock()
            .unwrap()
            .clone();
        assert_eq!(
            still_pending, None,
            "pending switch must be cleared after turn_streamed consumes it"
        );

        crate::agent::loop_::clear_model_switch_request();
    }
}

#[cfg(test)]
mod approval_route_tests {
    use super::*;
    use parking_lot::RwLock;
    use std::collections::HashMap;
    use zeroclaw_api::channel::{ChannelApprovalRequest, ChannelApprovalResponse};
    use zeroclaw_config::autonomy::{ApprovalRoute, OnNoApprover};

    enum StubBehavior {
        Answer(ChannelApprovalResponse),
        NoDecision,
        Slow,
    }

    struct StubChannel {
        name: String,
        behavior: StubBehavior,
    }

    impl zeroclaw_api::attribution::Attributable for StubChannel {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Channel(zeroclaw_api::attribution::ChannelKind::Cli)
        }
        fn alias(&self) -> &str {
            &self.name
        }
    }

    #[async_trait::async_trait]
    impl zeroclaw_api::channel::Channel for StubChannel {
        fn name(&self) -> &str {
            &self.name
        }
        async fn send(&self, _m: &zeroclaw_api::channel::SendMessage) -> anyhow::Result<()> {
            Ok(())
        }
        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn request_approval(
            &self,
            _recipient: &str,
            _request: &ChannelApprovalRequest,
        ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
            match &self.behavior {
                StubBehavior::Answer(resp) => Ok(Some(resp.clone())),
                StubBehavior::NoDecision => Ok(None),
                StubBehavior::Slow => {
                    // Far exceeds the route timeout; with a paused clock the
                    // timeout fires at +timeout_secs virtual time, instantly.
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                    Ok(Some(ChannelApprovalResponse::Approve))
                }
            }
        }
    }

    fn registry(channels: Vec<StubChannel>) -> tools::PerToolChannelHandle {
        let mut map: HashMap<String, Arc<dyn zeroclaw_api::channel::Channel>> = HashMap::new();
        for c in channels {
            map.insert(c.name.clone(), Arc::new(c));
        }
        Arc::new(RwLock::new(map))
    }

    fn req() -> ChannelApprovalRequest {
        ChannelApprovalRequest {
            tool_name: "shell".into(),
            arguments_summary: "rm -rf /".into(),
            raw_arguments: None,
        }
    }

    fn route(approver: &str, policy: OnNoApprover) -> ApprovalRoute {
        ApprovalRoute {
            approver_channel: approver.into(),
            on_no_approver: policy,
            timeout_secs: 1,
        }
    }

    #[tokio::test]
    async fn approver_answer_is_used_and_attributed() {
        let h = registry(vec![StubChannel {
            name: "ops".into(),
            behavior: StubBehavior::Answer(ChannelApprovalResponse::Approve),
        }]);
        match resolve_routed_approval(&h, &route("ops", OnNoApprover::Deny), "r", &req()).await {
            RoutedApproval::Decided { response, decider } => {
                assert_eq!(response, ChannelApprovalResponse::Approve);
                assert_eq!(
                    decider.as_deref(),
                    Some("ops"),
                    "decider names the approver"
                );
            }
            RoutedApproval::Fallthrough => panic!("expected a routed decision"),
        }
    }

    #[tokio::test]
    async fn unregistered_approver_fails_closed_by_default() {
        let h = registry(vec![]);
        match resolve_routed_approval(&h, &route("ops", OnNoApprover::Deny), "r", &req()).await {
            RoutedApproval::Decided { response, decider } => {
                assert_eq!(response, ChannelApprovalResponse::Deny, "fail-closed deny");
                assert!(decider.is_none(), "synthetic deny has no decider");
            }
            RoutedApproval::Fallthrough => panic!("default policy must NOT fall through"),
        }
    }

    #[tokio::test]
    async fn unregistered_approver_inherits_when_opted_in() {
        let h = registry(vec![]);
        let out = resolve_routed_approval(
            &h,
            &route("ops", OnNoApprover::InheritOriginator),
            "r",
            &req(),
        )
        .await;
        assert!(
            matches!(out, RoutedApproval::Fallthrough),
            "InheritOriginator must fall through to the originating fan-out"
        );
    }

    #[tokio::test]
    async fn no_decision_fails_closed() {
        let h = registry(vec![StubChannel {
            name: "ops".into(),
            behavior: StubBehavior::NoDecision,
        }]);
        let out = resolve_routed_approval(&h, &route("ops", OnNoApprover::Deny), "r", &req()).await;
        assert!(matches!(
            out,
            RoutedApproval::Decided {
                response: ChannelApprovalResponse::Deny,
                ..
            }
        ));
    }

    // The route timeout (1s) fires and cancels the stub's long sleep, so this
    // resolves in ~1s of real time without needing tokio's `test-util` clock.
    #[tokio::test]
    async fn slow_approver_times_out_and_fails_closed() {
        let h = registry(vec![StubChannel {
            name: "ops".into(),
            behavior: StubBehavior::Slow,
        }]);
        let out = resolve_routed_approval(&h, &route("ops", OnNoApprover::Deny), "r", &req()).await;
        assert!(matches!(
            out,
            RoutedApproval::Decided {
                response: ChannelApprovalResponse::Deny,
                ..
            }
        ));
    }

    use zeroclaw_api::channel::Channel as _;

    #[tokio::test]
    async fn routed_channel_returns_and_attributes_approver_decision() {
        let h = registry(vec![StubChannel {
            name: "ops".into(),
            behavior: StubBehavior::Answer(ChannelApprovalResponse::Approve),
        }]);
        let bridge = RoutedApprovalChannel::new(h, route("ops", OnNoApprover::Deny));
        let out = bridge
            .request_approval_attributed("r", &req())
            .await
            .unwrap()
            .expect("the approver decided");
        assert_eq!(out.response, ChannelApprovalResponse::Approve);
        assert_eq!(
            out.decided_by.as_deref(),
            Some("ops"),
            "the gate attributes the approval to the deciding channel"
        );
    }

    #[tokio::test]
    async fn routed_channel_fails_closed_when_approver_unregistered() {
        let bridge = RoutedApprovalChannel::new(registry(vec![]), route("ops", OnNoApprover::Deny));
        let out = bridge
            .request_approval_attributed("r", &req())
            .await
            .unwrap()
            .expect("the fail-closed deny is a decision");
        assert_eq!(
            out.response,
            ChannelApprovalResponse::Deny,
            "unreachable approver denies, not auto-approves"
        );
        assert!(
            out.decided_by.is_none(),
            "a bridge-synthesized fail-closed deny has no deciding channel"
        );
    }

    #[tokio::test]
    async fn routed_channel_inherit_returns_none_on_channelless_path() {
        let bridge = RoutedApprovalChannel::new(
            registry(vec![]),
            route("ops", OnNoApprover::InheritOriginator),
        );
        let out = bridge.request_approval("r", &req()).await.unwrap();
        assert_eq!(
            out, None,
            "no originator to inherit; gate applies the non-interactive auto-deny"
        );
    }
}
