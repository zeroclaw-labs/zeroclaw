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

pub struct Agent {
    model_provider: Box<dyn ModelProvider>,
    tools: Vec<Box<dyn Tool>>,
    memory: Arc<dyn Memory>,
    observer: Arc<dyn Observer>,
    prompt_builder: SystemPromptBuilder,
    tool_dispatcher: Box<dyn ToolDispatcher>,
    /// Stable half of the engine's memory-context injection policy
    /// (recall limit, relevance floor, budgets). Threaded into `ToolLoop`
    /// as `TurnMemory.cfg` on every turn.
    memory_inject_cfg: crate::agent::memory_inject::MemoryInjectConfig,
    config: zeroclaw_config::schema::AliasedAgentConfig,
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
    tools: Option<Vec<Box<dyn Tool>>>,
    memory: Option<Arc<dyn Memory>>,
    observer: Option<Arc<dyn Observer>>,
    prompt_builder: Option<SystemPromptBuilder>,
    tool_dispatcher: Option<Box<dyn ToolDispatcher>>,
    memory_inject_cfg: Option<crate::agent::memory_inject::MemoryInjectConfig>,
    config: Option<zeroclaw_config::schema::AliasedAgentConfig>,
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

    pub fn tools(mut self, tools: Vec<Box<dyn Tool>>) -> Self {
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
                crate::agent::memory_inject::MemoryInjectConfig {
                    min_relevance_score: zeroclaw_config::schema::MemoryConfig::default()
                        .min_relevance_score,
                    ..Default::default()
                }
            }),
            config: self.config.unwrap_or_default(),
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

    /// Build the enriched user message for this turn (memory context + timestamp
    /// + raw text) and return it as a `ChatMessage`.
    async fn build_enriched_user_message(&mut self, user_message: &str) -> ChatMessage {
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
            });
        }

        let now = self.current_turn_datetime().format("%Y-%m-%d %H:%M:%S %Z");
        let enriched = format!("[{now}] {user_message}");

        ChatMessage::user(enriched)
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
    }

    /// Hydrate the agent with a full `ConversationMessage` history (e.g. restored
    /// from an ACP session store). Preserves all variants including `AssistantToolCalls`
    /// and `ToolResults` — use this for ACP restore; use `seed_history` for flat
    /// channel session hydration.
    pub fn seed_conversation_history(&mut self, messages: Vec<ConversationMessage>) {
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
        self.trim_history();
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
        )
        .await
    }

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
                let (engine, audit) =
                    crate::sop::build_sop_engine(config.sop.clone(), &config.data_dir, mem);
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
        let tools = registry.into_inner();

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

        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(tools)
            .memory(memory.clone())
            .observer(observer)
            .response_cache(response_cache)
            .tool_dispatcher(tool_dispatcher)
            .memory_inject_cfg(crate::agent::memory_inject::MemoryInjectConfig {
                limit: config.effective_memory_recall_limit(agent_alias),
                min_relevance_score: config.memory.min_relevance_score,
                ..Default::default()
            })
            .prompt_builder(SystemPromptBuilder::with_defaults())
            .config(
                config
                    .resolved_agent_config(agent_alias)
                    .unwrap_or_else(|| agent_cfg.clone()),
            )
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

    fn trim_history(&mut self) {
        let max = self.config.resolved.max_history_messages;
        if self.history.len() <= max {
            return;
        }

        let mut system_messages = Vec::new();
        let mut other_messages = Vec::new();

        for msg in self.history.drain(..) {
            match &msg {
                ConversationMessage::Chat(chat) if chat.role == "system" => {
                    system_messages.push(msg);
                }
                _ => other_messages.push(msg),
            }
        }

        if other_messages.len() > max {
            let initial_drop_count = other_messages.len() - max;
            let mut drop_count = initial_drop_count;

            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_attrs(::serde_json::json!({
                        "total_messages": other_messages.len(),
                        "max_history": max,
                        "initial_drop_count": initial_drop_count,
                    })),
                "trim_history: dropping oldest messages"
            );

            let before_orphan_tr = drop_count;
            while drop_count < other_messages.len()
                && matches!(
                    &other_messages[drop_count],
                    ConversationMessage::ToolResults(_)
                )
            {
                drop_count += 1;
            }
            if drop_count > before_orphan_tr {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Agent)
                        .with_attrs(::serde_json::json!({
                            "extra_dropped": drop_count - before_orphan_tr,
                        })),
                    "trim_history: dropped orphan ToolResults at head"
                );
            }

            let before_orphan_ac = drop_count;
            while drop_count < other_messages.len()
                && matches!(
                    &other_messages[drop_count],
                    ConversationMessage::AssistantToolCalls { .. }
                )
            {
                // Also drop the ToolResults that follows this AC (if present)
                drop_count += 1;
                if drop_count < other_messages.len()
                    && matches!(
                        &other_messages[drop_count],
                        ConversationMessage::ToolResults(_)
                    )
                {
                    drop_count += 1;
                }
            }
            if drop_count > before_orphan_ac {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Agent)
                        .with_attrs(::serde_json::json!({
                            "extra_dropped": drop_count - before_orphan_ac,
                        })),
                    "trim_history: dropped orphan AssistantToolCalls at head"
                );
            }

            // Safety: the orphan-removal cascades above can advance
            // drop_count all the way to other_messages.len() when the only
            // non-tool-call entry is the user message at position[0] and
            // initial_drop_count drops it (e.g. max=50, history=[user,
            // AC1, TR1, …, AC25, TR25]).  Sending zero messages to the
            // provider causes a hard 400 "messages: at least one message
            // is required".  When the cascade would wipe everything, skip
            // this trim pass so the conversation stays functional even
            // though it is temporarily over the message limit.
            if drop_count >= other_messages.len() {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Agent)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "history_len": other_messages.len(),
                            "max_history_messages": max,
                        })),
                    "trim_history: orphan-cascade would empty all non-system messages; skipping trim to preserve conversation"
                );
                self.history = system_messages;
                self.history.extend(other_messages);
                return;
            }

            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_attrs(::serde_json::json!({
                        "total_dropped": drop_count,
                        "remaining": other_messages.len() - drop_count,
                    })),
                "trim_history: complete"
            );

            other_messages.drain(0..drop_count);
        }

        self.history = system_messages;
        self.history.extend(other_messages);
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
        let prompt_tools = if expose_text_tool_protocol {
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

        let active_dispatcher = {
            let base_provider_messages = self.tool_dispatcher.to_provider_messages(&self.history);
            let (vision_provider_box, _degrade_strip_images) =
                crate::agent::turn::resolve_vision_provider(
                    self.model_provider.as_ref(),
                    &base_provider_messages,
                    &self.multimodal_config,
                    &self.model_provider_name,
                )?;
            let active_provider: &dyn ModelProvider = vision_provider_box
                .as_deref()
                .unwrap_or(self.model_provider.as_ref());
            tool_dispatcher_for_provider(&self.config, active_provider)
        };

        self.rebuild_system_prompt_for_dispatcher(active_dispatcher.as_ref())?;

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
                self.trim_history();
                return Ok(cached);
            }
            self.observer.record_event(&ObserverEvent::CacheMiss {
                cache_type: "response".into(),
            });
        }

        // Split the provider-visible transcript at the last user message: the
        // prefix becomes the loop's read-only `history`, the suffix (starting
        // at this turn's enriched user message) becomes the mutable
        // `current_turn` working set the loop appends assistant/tool messages
        // to. `provider_messages` already ends with the user message, so the
        // split keeps it as `loop_current_turn[0]`.
        let split = provider_messages
            .iter()
            .rposition(|m| m.role == "user")
            .unwrap_or(provider_messages.len());
        let mut loop_history = provider_messages;
        let mut loop_current_turn: Vec<ChatMessage> = loop_history.split_off(split);

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
                    crate::agent::loop_::run_tool_call_loop_with_current_turn(
                        crate::agent::loop_::ToolLoopWithCurrentTurn {
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
                                    dedup_exempt_tools: &self
                                        .config
                                        .resolved
                                        .tool_call_dedup_exempt,
                                    pacing: &pacing,
                                    strict_tool_parsing: self.config.resolved.strict_tool_parsing,
                                    parallel_tools: self.config.resolved.parallel_tools,
                                    max_tool_result_chars: self
                                        .config
                                        .resolved
                                        .max_tool_result_chars,
                                    context_token_budget: self
                                        .config
                                        .resolved
                                        .effective_context_budget(),
                                    knobs: &knobs,
                                },
                            ),
                            history: &mut loop_history,
                            current_turn: &mut loop_current_turn,
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
                            turn_id: &turn_id,
                        },
                    ),
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

        // Replay the loop's current turn into the conversation history BEFORE
        // propagating any loop error: rounds that already executed carry side
        // effects (tools ran), and the split-history engine keeps them in
        // `loop_current_turn` on error exits too; `replay_loop_messages` reverses
        // the loop's provider encodings back into structured `ConversationMessage`s.
        // Pop the placeholder user message committed above so the
        // potentially hook-modified version from `loop_current_turn[0]` wins.
        self.history.pop();
        for replayed in Self::replay_loop_messages(&loop_current_turn) {
            self.history.push(replayed);
        }
        let response = loop_result?;

        let response = self.append_receipts_block(response, receipt_scope.as_ref());

        // Store in the response cache only when the turn was a single
        // tool-free exchange (user message + exactly one assistant message),
        // mirroring the old "no tool calls" put condition.
        if let (Some(cache), Some(key)) = (&self.response_cache, &cache_key)
            && loop_current_turn.len() == 2
            && loop_current_turn[0].role == "user"
            && loop_current_turn[1].role == "assistant"
        {
            #[allow(clippy::cast_possible_truncation)]
            let _ = cache.put(key, &effective_model, &response, usage.output_tokens as u32);
        }

        self.trim_history();

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

        let user_msg = self.build_enriched_user_message(user_message).await;
        self.history
            .push(ConversationMessage::Chat(user_msg.clone()));

        // `effective_model` is `mut` so a `model_switch` requested mid-turn
        // (handled in the round loop's `ModelSwitchRequested` arm via
        // `try_apply_pending_model_switch`) can rebind it for later rounds
        let mut effective_model = self.classify_model(user_message);
        let turn_id = Self::new_turn_id();
        let mut committed_response = String::new();
        let turn_observer = Arc::clone(&self.observer);
        let mut guard = crate::observability::AgentTurnGuard::start(
            turn_observer.as_ref(),
            self.model_provider_name.clone(),
            effective_model.clone(),
            Some(self.channel_name.clone()),
            self.observer_agent_alias(),
            Some(turn_id.clone()),
        );

        let active_dispatcher = {
            let base_provider_messages = self.tool_dispatcher.to_provider_messages(&self.history);
            let (vision_provider_box, _degrade_strip_images) =
                crate::agent::turn::resolve_vision_provider(
                    self.model_provider.as_ref(),
                    &base_provider_messages,
                    &self.multimodal_config,
                    &self.model_provider_name,
                )
                .map_err(|error| StreamedTurnError {
                    error,
                    committed_response: String::new(),
                    new_messages: vec![ConversationMessage::Chat(user_msg.clone())],
                })?;
            let active_provider: &dyn ModelProvider = vision_provider_box
                .as_deref()
                .unwrap_or(self.model_provider.as_ref());
            tool_dispatcher_for_provider(&self.config, active_provider)
        };

        self.rebuild_system_prompt_for_dispatcher(active_dispatcher.as_ref())
            .map_err(|error| StreamedTurnError {
                error,
                committed_response: String::new(),
                new_messages: vec![ConversationMessage::Chat(user_msg.clone())],
            })?;

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
                self.trim_history();
                self.observer.record_event(&ObserverEvent::TurnComplete);
                committed_response.push_str(&cached);
                return Ok(StreamedTurnSuccess {
                    response: committed_response,
                    new_messages: vec![
                        ConversationMessage::Chat(user_msg.clone()),
                        ConversationMessage::Chat(ChatMessage::assistant(cached.clone())),
                    ],
                });
            }
            self.observer.record_event(&ObserverEvent::CacheMiss {
                cache_type: "response".into(),
            });
        }

        // Split the provider-visible transcript at the last user message: the
        // prefix becomes the loop's read-only `history`, the suffix (starting
        // at this turn's enriched user message) becomes the mutable
        // `current_turn` working set the loop appends assistant/tool messages
        // to. `provider_messages` already ends with the user message, so the
        // split keeps it as `loop_current_turn[0]`.
        let split = provider_messages
            .iter()
            .rposition(|m| m.role == "user")
            .unwrap_or(provider_messages.len());
        let mut loop_history = provider_messages;
        let mut loop_current_turn: Vec<ChatMessage> = loop_history.split_off(split);

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
        // Pop the placeholder user message committed above so the
        // potentially hook-modified version from `loop_current_turn[0]` wins.
        self.history.pop();

        // `replay_start` tracks how much of `loop_current_turn` has already
        // been committed into `self.history`. It begins at 0; the pop
        // above removed the placeholder, and the first round replays the full
        // current turn (including the seed user message at index 0).
        let mut replay_start: usize = 0;
        for round in 0..self.config.resolved.max_tool_iterations {
            // Early exit if the caller cancelled this turn (e.g. user abort)
            if cancel_token
                .as_ref()
                .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
            {
                let marker = crate::i18n::get_required_cli_string("turn-interrupted-by-user");
                loop_current_turn.push(ChatMessage::assistant(marker.clone()));
                self.history.extend(Self::replay_loop_messages(
                    &loop_current_turn[replay_start..],
                ));
                committed_response.push_str(&marker);
                return Err(StreamedTurnError {
                    error: crate::agent::loop_::ToolLoopCancelled.into(),
                    committed_response,
                    new_messages: Self::replay_loop_messages(&loop_current_turn),
                });
            }

            // Steering drain: each accepted mid-turn message becomes its own
            // enriched user turn in the loop's mutable `loop_current_turn` so
            // the next provider call sees the full current-turn transcript.
            for steering_message in crate::agent::loop_::drain_steering_messages(&mut steering_rx) {
                let steering_msg = self.build_enriched_user_message(&steering_message).await;
                loop_current_turn.push(steering_msg);
            }

            // Per-round append-log: the loop appends assistant/tool messages
            // directly into `loop_current_turn`.
            let loop_result = crate::agent::loop_::TOOL_LOOP_COST_TRACKING_CONTEXT
                .scope(
                    Some(cost_context.clone()),
                    crate::agent::tool_receipts::scope_receipts(
                        receipt_scope.clone(),
                        crate::agent::loop_::run_tool_call_loop_with_current_turn(
                            crate::agent::loop_::ToolLoopWithCurrentTurn {
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
                                        hooks: self.hook_runner.as_deref(),
                                        activated_tools: self.activated_tools.as_ref(),
                                        model_switch_callback: Some(
                                            crate::agent::loop_::get_model_switch_state(),
                                        ),
                                        receipt_generator: receipt_scope.as_ref().map(
                                            crate::agent::tool_receipts::ReceiptScope::generator,
                                        ),
                                    },
                                    crate::agent::loop_::ResolvedRuntimeKnobs {
                                        max_tool_iterations: self
                                            .config
                                            .resolved
                                            .max_tool_iterations,
                                        excluded_tools: &[],
                                        dedup_exempt_tools: &self
                                            .config
                                            .resolved
                                            .tool_call_dedup_exempt,
                                        pacing: &pacing,
                                        strict_tool_parsing: self
                                            .config
                                            .resolved
                                            .strict_tool_parsing,
                                        parallel_tools: self.config.resolved.parallel_tools,
                                        max_tool_result_chars: self
                                            .config
                                            .resolved
                                            .max_tool_result_chars,
                                        context_token_budget: self
                                            .config
                                            .resolved
                                            .effective_context_budget(),
                                        knobs: &knobs,
                                    },
                                ),
                                history: &mut loop_history,
                                current_turn: &mut loop_current_turn,
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
                                turn_id: &turn_id,
                            },
                        ),
                    ),
                )
                .await;

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

            let single_text_exchange = round == 0
                && loop_current_turn.len() == 2
                && loop_current_turn[0].role == "user"
                && loop_current_turn[1].role == "assistant";

            // Replay only the slice appended since the last commit into the
            // conversation history. Earlier rounds (and any steering user
            // messages consumed before them) are already durable; replaying the
            // full `loop_current_turn` here would duplicate them.
            self.history.extend(Self::replay_loop_messages(
                &loop_current_turn[replay_start..],
            ));
            replay_start = loop_current_turn.len();

            match loop_result {
                Ok(response) => {
                    // Commit-before-drain: this round's assistant output is in
                    // history (replay above) and committed_response before any
                    // steering continuation is folded in.
                    committed_response.push_str(&response);
                    self.trim_history();

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
                    return Ok(StreamedTurnSuccess {
                        response: committed_response,
                        new_messages: Self::replay_loop_messages(&loop_current_turn),
                    });
                }
                Err(error) => {
                    self.trim_history();
                    if crate::agent::loop_::is_model_switch_requested(&error).is_some()
                        && let Some(new_effective_model) =
                            self.try_apply_pending_model_switch(&effective_model)
                    {
                        effective_model = new_effective_model;
                        continue;
                    }
                    // Rebuild the committed text from the failed round's plain
                    // assistant output (e.g. a persisted stream partial) when
                    // no prior round committed anything.
                    if committed_response.is_empty() {
                        for replayed in Self::replay_loop_messages(&loop_current_turn) {
                            if let ConversationMessage::Chat(message) = &replayed
                                && message.role == "assistant"
                            {
                                committed_response.push_str(&message.content);
                            }
                        }
                    }
                    if crate::agent::loop_::is_tool_loop_cancelled(&error) {
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
                                loop_current_turn.push(ChatMessage::assistant(marker.clone()));
                                // The marker is appended after the round's
                                // normal replay slice, so commit it directly.
                                self.history.push(interruption);
                            }
                        }
                        return Err(StreamedTurnError {
                            error: crate::agent::loop_::ToolLoopCancelled.into(),
                            committed_response,
                            new_messages: Self::replay_loop_messages(&loop_current_turn),
                        });
                    }
                    // Mark the interruption only when nothing was committed —
                    // prior-round text must round-trip unmodified.
                    if committed_response.is_empty() {
                        committed_response.push_str(&crate::i18n::get_required_cli_string(
                            "turn-stream-interrupted",
                        ));
                    }
                    return Err(StreamedTurnError {
                        error,
                        committed_response,
                        new_messages: Self::replay_loop_messages(&loop_current_turn),
                    });
                }
            }
        }

        Err(StreamedTurnError {
            error: anyhow::Error::msg(format!(
                "Agent exceeded maximum tool iterations ({})",
                self.config.resolved.max_tool_iterations
            )),
            committed_response,
            new_messages: Self::replay_loop_messages(&loop_current_turn),
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
            .tools(Vec::new())
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(CountingTool {
                calls: Arc::clone(&calls),
            })])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
                .tools(tools)
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
                .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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

    #[test]
    fn trim_history_does_not_leave_orphan_tool_results() {
        use zeroclaw_providers::{ToolCall, ToolResultMessage};

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        // Force trimming with the boundary landing inside a pair:
        // 5 entries (AC, TR, AC, TR, AC) > 4 → drop_count = 1 → AC1 dropped,
        // TR1 left as an orphan unless the trim guards against it.
        let agent_config = zeroclaw_config::schema::AliasedAgentConfig {
            resolved: zeroclaw_config::schema::ResolvedRuntime {
                max_history_messages: 4,
                ..Default::default()
            },
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        };

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(Box::new(MockModelProvider {
                responses: Mutex::new(vec![]),
            }))
            .tools(vec![Box::new(MockTool)])
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .config(agent_config)
            .build()
            .expect("agent builder should succeed with valid config");

        // Build the history: AC1, TR1, AC2, TR2, AC3 (no trailing TR3).
        for i in 1..=3 {
            agent.history.push(ConversationMessage::AssistantToolCalls {
                text: Some(format!("Calling tool {i}")),
                tool_calls: vec![ToolCall {
                    id: format!("tc{i}"),
                    name: format!("tool{i}"),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                reasoning_content: None,
            });
            // Skip the trailing ToolResults for the last AssistantToolCalls
            // so the entry count is 5, not 6, and the drop boundary lands
            // mid-pair.
            if i < 3 {
                agent
                    .history
                    .push(ConversationMessage::ToolResults(vec![ToolResultMessage {
                        tool_call_id: format!("tc{i}"),
                        content: format!("result{i}"),
                        tool_name: String::new(),
                    }]));
            }
        }

        assert_eq!(agent.history.len(), 5);
        agent.trim_history();

        // After trimming, the surviving history must not start with a
        // ToolResults entry (that would be an orphan whose AssistantToolCalls
        // partner was dropped).
        if let Some(first) = agent.history.first() {
            assert!(
                !matches!(first, ConversationMessage::ToolResults(_)),
                "trim_history left an orphan ToolResults at the head of the \
                 history; this would cause Anthropic to reject the next \
                 request with 'unexpected tool_use_id found in tool_result \
                 blocks'"
            );
        }

        // Every ToolResults entry must be immediately preceded by an
        // AssistantToolCalls entry.
        for window in agent.history.windows(2) {
            if matches!(&window[1], ConversationMessage::ToolResults(_)) {
                assert!(
                    matches!(&window[0], ConversationMessage::AssistantToolCalls { .. }),
                    "ToolResults entry is not preceded by an AssistantToolCalls \
                     entry — pair was split during trim"
                );
            }
        }
    }

    #[test]
    fn trim_history_does_not_leave_orphan_assistant_tool_calls() {
        use zeroclaw_providers::{ToolCall, ToolResultMessage};

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let agent_config = zeroclaw_config::schema::AliasedAgentConfig {
            resolved: zeroclaw_config::schema::ResolvedRuntime {
                max_history_messages: 3,
                ..Default::default()
            },
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        };

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(Box::new(MockModelProvider {
                responses: Mutex::new(vec![]),
            }))
            .tools(vec![Box::new(MockTool)])
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .config(agent_config)
            .build()
            .expect("agent builder should succeed with valid config");

        // user1
        agent.history.push(ConversationMessage::Chat(ChatMessage {
            role: "user".into(),
            content: "hello".into(),
        }));
        // AC1, TR1
        agent.history.push(ConversationMessage::AssistantToolCalls {
            text: Some("Calling tool 1".into()),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "tool1".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            reasoning_content: None,
        });
        agent
            .history
            .push(ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: "tc1".into(),
                content: "result1".into(),
                tool_name: String::new(),
            }]));
        // AC2, TR2
        agent.history.push(ConversationMessage::AssistantToolCalls {
            text: Some("Calling tool 2".into()),
            tool_calls: vec![ToolCall {
                id: "tc2".into(),
                name: "tool2".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            reasoning_content: None,
        });
        agent
            .history
            .push(ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: "tc2".into(),
                content: "result2".into(),
                tool_name: String::new(),
            }]));
        // AC3, TR3
        agent.history.push(ConversationMessage::AssistantToolCalls {
            text: Some("Calling tool 3".into()),
            tool_calls: vec![ToolCall {
                id: "tc3".into(),
                name: "tool3".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            reasoning_content: None,
        });
        agent
            .history
            .push(ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: "tc3".into(),
                content: "result3".into(),
                tool_name: String::new(),
            }]));

        assert_eq!(agent.history.len(), 7);
        agent.trim_history();

        // The head must not be an AssistantToolCalls (orphaned from context)
        if let Some(first) = agent.history.first() {
            assert!(
                !matches!(first, ConversationMessage::AssistantToolCalls { .. }),
                "trim_history left an orphan AssistantToolCalls at the head of \
                 the history; the model would see tool calls with no results"
            );
        }

        // Every ToolResults entry must be immediately preceded by an
        // AssistantToolCalls entry (no split pairs).
        for window in agent.history.windows(2) {
            if matches!(&window[1], ConversationMessage::ToolResults(_)) {
                assert!(
                    matches!(&window[0], ConversationMessage::AssistantToolCalls { .. }),
                    "ToolResults entry is not preceded by an AssistantToolCalls \
                     entry — pair was split during trim"
                );
            }
        }

        // Every AssistantToolCalls must be immediately followed by ToolResults
        // (no orphan ACs).
        for window in agent.history.windows(2) {
            if matches!(&window[0], ConversationMessage::AssistantToolCalls { .. }) {
                assert!(
                    matches!(&window[1], ConversationMessage::ToolResults(_)),
                    "AssistantToolCalls entry is not followed by a ToolResults \
                     entry — orphan tool call would confuse the model"
                );
            }
        }
    }

    #[test]
    fn trim_history_does_not_empty_all_messages_on_full_cascade() {
        use zeroclaw_providers::{ToolCall, ToolResultMessage};

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        let agent_config = zeroclaw_config::schema::AliasedAgentConfig {
            resolved: zeroclaw_config::schema::ResolvedRuntime {
                max_history_messages: 4,
                ..Default::default()
            },
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        };

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(Box::new(MockModelProvider {
                responses: Mutex::new(vec![]),
            }))
            .tools(vec![Box::new(MockTool)])
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .config(agent_config)
            .build()
            .expect("agent builder should succeed with valid config");

        // user
        agent.history.push(ConversationMessage::Chat(ChatMessage {
            role: "user".into(),
            content: "kick off a long tool loop".into(),
        }));
        // AC1, TR1
        agent.history.push(ConversationMessage::AssistantToolCalls {
            text: Some("Calling tool 1".into()),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "tool1".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            reasoning_content: None,
        });
        agent
            .history
            .push(ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: "tc1".into(),
                content: "result1".into(),
                tool_name: String::new(),
            }]));
        // AC2, TR2
        agent.history.push(ConversationMessage::AssistantToolCalls {
            text: Some("Calling tool 2".into()),
            tool_calls: vec![ToolCall {
                id: "tc2".into(),
                name: "tool2".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            reasoning_content: None,
        });
        agent
            .history
            .push(ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: "tc2".into(),
                content: "result2".into(),
                tool_name: String::new(),
            }]));

        assert_eq!(agent.history.len(), 5);
        let before = agent.history.clone();

        agent.trim_history();

        // Load-bearing assertion: trim_history must NOT produce an empty
        // provider-visible conversation. Without the guard this is an empty
        // Vec and the next provider call returns 400.
        assert!(
            !agent.history.is_empty(),
            "trim_history drained every non-system message; the next \
             provider call would fail with 'messages: at least one message \
             is required'"
        );

        assert_eq!(
            agent.history.len(),
            before.len(),
            "trim_history dropped messages despite the orphan cascade \
             reaching other_messages.len(); the guard's contract is to \
             preserve the conversation untouched in this case"
        );

        // Session is temporarily over the configured limit by design. Codify
        // that so a future "tighten trim_history" refactor cannot silently
        // turn the guard back into the empty-messages crash.
        assert!(
            agent.history.len() > agent.config.resolved.max_history_messages,
            "expected history to remain over max_history_messages after the \
             guard fires (that is the documented trade-off); got len={} max={}",
            agent.history.len(),
            agent.config.resolved.max_history_messages,
        );
    }

    #[test]
    fn trim_history_full_cascade_with_system_message_preserves_full_history() {
        use zeroclaw_providers::{ToolCall, ToolResultMessage};

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed with valid config"),
        );

        // Same arithmetic as the previous test: 5 non-system entries with
        // max=4 → initial_drop_count=1, orphan-AC cascade reaches the end.
        let agent_config = zeroclaw_config::schema::AliasedAgentConfig {
            resolved: zeroclaw_config::schema::ResolvedRuntime {
                max_history_messages: 4,
                ..Default::default()
            },
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        };

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(Box::new(MockModelProvider {
                responses: Mutex::new(vec![]),
            }))
            .tools(vec![Box::new(MockTool)])
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .config(agent_config)
            .build()
            .expect("agent builder should succeed with valid config");

        // system (gets partitioned into system_messages by trim_history)
        agent.history.push(ConversationMessage::Chat(ChatMessage {
            role: "system".into(),
            content: "you are a helpful agent".into(),
        }));
        // user
        agent.history.push(ConversationMessage::Chat(ChatMessage {
            role: "user".into(),
            content: "kick off a long tool loop".into(),
        }));
        // AC1, TR1
        agent.history.push(ConversationMessage::AssistantToolCalls {
            text: Some("Calling tool 1".into()),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "tool1".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            reasoning_content: None,
        });
        agent
            .history
            .push(ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: "tc1".into(),
                content: "result1".into(),
                tool_name: String::new(),
            }]));
        // AC2, TR2
        agent.history.push(ConversationMessage::AssistantToolCalls {
            text: Some("Calling tool 2".into()),
            tool_calls: vec![ToolCall {
                id: "tc2".into(),
                name: "tool2".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            reasoning_content: None,
        });
        agent
            .history
            .push(ConversationMessage::ToolResults(vec![ToolResultMessage {
                tool_call_id: "tc2".into(),
                content: "result2".into(),
                tool_name: String::new(),
            }]));

        assert_eq!(agent.history.len(), 6);
        let before_len = agent.history.len();

        agent.trim_history();

        // System message must still be present and at the head — that is
        // where trim_history's partition+restore lands it.
        match agent.history.first() {
            Some(ConversationMessage::Chat(chat)) => assert_eq!(
                chat.role, "system",
                "expected system message at head after restore; got role={:?}",
                chat.role
            ),
            other => panic!(
                "expected Chat(system) at head of restored history, got {:?}",
                other
            ),
        }

        // The non-system half must not have been drained. Total length must
        // equal the pre-trim length: guard's contract is "leave history
        // unchanged" once the system + non-system halves are reassembled.
        assert_eq!(
            agent.history.len(),
            before_len,
            "trim_history dropped messages from the non-system half despite \
             the orphan cascade reaching other_messages.len(); guard must \
             preserve every entry when it fires"
        );

        // At least one non-system message must remain — without this the
        // provider still sees `messages: []` after `convert_messages` lifts
        // the system entry into `system_prompt`.
        let non_system_remaining = agent
            .history
            .iter()
            .filter(|m| !matches!(m, ConversationMessage::Chat(c) if c.role == "system"))
            .count();
        assert!(
            non_system_remaining > 0,
            "trim_history left only the system message; convert_messages \
             would produce messages: [] and the provider call would 400"
        );
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
                    .tools(vec![])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
    async fn turn_streamed_with_steering_does_not_duplicate_history() {
        // Regression for the split-history streaming engine: each accepted
        // steering round appends to `loop_current_turn`, but only the newly
        // added slice must be replayed into the durable `self.history`. If the
        // full `loop_current_turn` is replayed every round, seed user/assistant
        // messages appear multiple times.
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
            .tools(vec![Box::new(MockTool)])
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let _history_before = agent.history.len();

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let (steering_tx, mut steering_rx) = tokio::sync::mpsc::channel::<String>(4);
        let handle = zeroclaw_spawn::spawn!(async move {
            let result = agent
                .turn_streamed_with_steering_state("first", event_tx, None, Some(&mut steering_rx))
                .await;
            (agent, result)
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

        let (agent, outcome) = handle.await.expect("turn task should finish");
        let outcome = outcome.expect("steered turn should succeed");

        let _history_after = agent.history.len();

        // Extract the committed chat messages. This is the durable transcript
        // that matters for the no-duplicates regression.
        let committed: Vec<_> = agent
            .history
            .iter()
            .filter_map(|msg| match msg {
                ConversationMessage::Chat(message) => {
                    Some((message.role.as_str(), message.content.as_str()))
                }
                _ => None,
            })
            .collect();

        // Build a frequency map keyed by a coarse content tag. User messages
        // are enriched with a timestamp prefix, so match by substring.
        let first_user_count = committed
            .iter()
            .filter(|(role, content)| *role == "user" && content.contains("first"))
            .count();
        let second_user_count = committed
            .iter()
            .filter(|(role, content)| *role == "user" && content.contains("second"))
            .count();
        let draft_assistant_count = committed
            .iter()
            .filter(|(role, content)| *role == "assistant" && *content == "draft")
            .count();
        let final_assistant_count = committed
            .iter()
            .filter(|(role, content)| *role == "assistant" && *content == "final")
            .count();

        assert_eq!(
            first_user_count, 1,
            "seed user message must appear exactly once in history"
        );
        assert_eq!(
            second_user_count, 1,
            "steering user message must appear exactly once in history"
        );
        assert_eq!(
            draft_assistant_count, 1,
            "first assistant response must appear exactly once in history"
        );
        assert_eq!(
            final_assistant_count, 1,
            "second assistant response must appear exactly once in history"
        );

        // Also assert the turn's reported new_messages are the full turn.
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
        assert_eq!(
            new_chat_messages.len(),
            4,
            "new_messages must contain four chat messages"
        );
        assert!(
            new_chat_messages[0].0 == "user" && new_chat_messages[0].1.contains("first"),
            "new_messages[0] must be the seed user message"
        );
        assert!(
            new_chat_messages[1] == ("assistant", "draft"),
            "new_messages[1] must be the first assistant response"
        );
        assert!(
            new_chat_messages[2].0 == "user" && new_chat_messages[2].1.contains("second"),
            "new_messages[2] must be the accepted steering user message"
        );
        assert!(
            new_chat_messages[3] == ("assistant", "final"),
            "new_messages[3] must be the second assistant response"
        );

        // The durable history tail must equal new_messages (modulo timestamp
        // enrichment of user messages).
        let tail = &committed[committed.len().saturating_sub(new_chat_messages.len())..];
        assert_eq!(
            tail.len(),
            new_chat_messages.len(),
            "durable history tail and new_messages must have same length"
        );
        for (actual, expected) in tail.iter().zip(new_chat_messages.iter()) {
            assert_eq!(
                actual.0, expected.0,
                "durable history tail role must match new_messages"
            );
            if actual.0 == "user" {
                assert!(
                    actual.1.contains(expected.1),
                    "durable history tail user content must contain new_messages user content"
                );
            } else {
                assert_eq!(
                    actual.1, expected.1,
                    "durable history tail assistant content must equal new_messages"
                );
            }
        }

        // The regression: each turn-related message appears exactly once in the
        // durable history. No message is duplicated across steering rounds.
        assert_eq!(
            first_user_count, 1,
            "seed user message must appear exactly once in history"
        );
        assert_eq!(
            second_user_count, 1,
            "steering user message must appear exactly once in history"
        );
        assert_eq!(
            draft_assistant_count, 1,
            "first assistant response must appear exactly once in history"
        );
        assert_eq!(
            final_assistant_count, 1,
            "second assistant response must appear exactly once in history"
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            resolved: zeroclaw_config::schema::ResolvedRuntime {
                max_history_messages: 4,
                ..Default::default()
            },
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        };

        // Simple streaming provider that returns plain text (no tool calls).
        let provider = Box::new(NarrationStreamModelProvider {
            call_count: Arc::new(Mutex::new(0)),
        });

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .model_provider(provider)
            .tools(vec![Box::new(MockTool)])
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .config(agent_config)
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
        // entries.  max_history_messages=4 means trim fires after adding the
        // new turn.

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
            | ObserverEvent::ToolCallStart { turn_id, .. } => turn_id.as_deref(),
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
                    | ObserverEvent::ToolCall { agent_alias, .. } => agent_alias,
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
                    | ObserverEvent::AgentEnd { channel: ch, .. } => ch,
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(SlowTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(MockTool)])
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .agent_alias("test-agent".into())
            .build()
            .expect("agent builder should succeed with valid config");

        let _ = agent.turn("test").await.expect("turn should succeed");

        let events = capturing.events.lock();
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
            .tools(vec![Box::new(MockTool)])
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
            .tools(vec![Box::new(ModelSwitchTriggerTool {
                target_provider: "ollama".to_string(),
                target_model: "llama3".to_string(),
            })])
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
