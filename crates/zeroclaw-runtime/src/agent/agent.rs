use crate::agent::dispatcher::{NativeToolDispatcher, ToolDispatcher, XmlToolDispatcher};
use crate::agent::eval::AutoClassifyExt;
use crate::agent::prompt::{PromptContext, SystemPromptBuilder};
use crate::approval::ApprovalManager;
use crate::observability::{self, Observer, ObserverEvent};
use crate::platform;
use crate::security::SecurityPolicy;
use crate::tools::{self, Tool};
use anyhow::{Context, Result};
use chrono::{Datelike, Timelike};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use zeroclaw_config::schema::Config;
use zeroclaw_memory::{self, Memory, MemoryCategory};
#[cfg(test)]
use zeroclaw_providers::ChatRequest;
use zeroclaw_providers::{
    self, ChatMessage, ConversationMessage, ModelProvider, ToolResultMessage,
};

// Re-export TurnEvent from zeroclaw-types for backwards compatibility.
pub use zeroclaw_api::agent::TurnEvent;

/// Build a fresh `ModelProvider` box for a dotted `<type>.<alias>` reference,
/// resolving the model from the override (when supplied) or the configured
/// entry. Mirrors the model_provider-construction path in [`Agent::from_config`]
/// so a live session switch produces the same wiring a fresh agent would.
/// Returns the built box plus the resolved `(model_provider_name, model_name)`
/// for attribution.
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

struct TurnGuard {
    observer: Arc<dyn Observer>,
    model_provider: String,
    model: String,
    turn_id: Option<String>,
    turn_started_at: Instant,
    agent_alias: Option<String>,
    total_input_tokens: u64,
    total_output_tokens: u64,
    saw_usage: bool,
    done: bool,
}

impl TurnGuard {
    fn fire(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        self.observer.record_event(&ObserverEvent::AgentEnd {
            model_provider: self.model_provider.clone(),
            model: self.model.clone(),
            duration: self.turn_started_at.elapsed(),
            tokens_used: self.saw_usage.then_some(
                zeroclaw_api::observability_traits::TurnTokenUsage {
                    input_tokens: self.total_input_tokens,
                    output_tokens: self.total_output_tokens,
                },
            ),
            cost_usd: None,
            channel: None,
            agent_alias: self.agent_alias.clone(),
            turn_id: self.turn_id.clone(),
        });
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.fire();
    }
}

pub struct Agent {
    model_provider: Box<dyn ModelProvider>,
    tools: Vec<Box<dyn Tool>>,
    memory: Arc<dyn Memory>,
    observer: Arc<dyn Observer>,
    prompt_builder: SystemPromptBuilder,
    tool_dispatcher: Box<dyn ToolDispatcher>,
    memory_strategy: Arc<dyn zeroclaw_api::memory_traits::MemoryStrategy>,
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
    /// Activated MCP tools for deferred loading mode.
    /// When MCP deferred loading is enabled, tools are activated via `tool_search`
    /// and stored here for lookup during tool execution.
    activated_tools: Option<Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    /// Hook runner for tool-call auditing and lifecycle side effects.
    /// See issue #5462.
    hook_runner: Option<Arc<crate::hooks::HookRunner>>,
    /// Approval manager for direct Agent execution paths such as ACP.
    approval_manager: Option<Arc<ApprovalManager>>,
    /// Agent alias, retained for opening attribution spans at external turn
    /// call sites (ACP, gateway WS) where the alias is otherwise unavailable.
    agent_alias: String,
    /// Late-bound channel maps for the four channel-driven tools
    /// (`ask_user`, `reaction`, `escalate_to_human`, `poll`). Held so that
    /// per-session callers (e.g. the ACP server) can register a back-channel
    /// after agent construction. Production paths populate via
    /// `start_channels`; this is the alternate path for environments that
    /// build an Agent directly without `start_channels`.
    channel_handles: AgentChannelHandles,
    /// Per-session cache for resolved local image data URIs, threaded into
    /// the turn loop so each unique local image file is read + base64-encoded
    /// at most once per session even though the multimodal pipeline re-walks
    /// the full conversation history on every turn and tool iteration.
    image_cache: zeroclaw_providers::multimodal::LocalImageCache,
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

/// Bundle of late-bound channel-map handles owned by an Agent. Cloning is
/// cheap (Arc clones); the underlying maps are shared with the live tools.
#[derive(Clone, Default)]
pub struct AgentChannelHandles {
    pub ask_user: Option<tools::PerToolChannelHandle>,
    pub reaction: tools::PerToolChannelHandle,
    pub poll: Option<tools::PerToolChannelHandle>,
    pub escalate: Option<tools::PerToolChannelHandle>,
}

impl AgentChannelHandles {
    /// Return references to all populated per-tool channel handles.
    fn populated_handles(&self) -> Vec<Option<&tools::PerToolChannelHandle>> {
        vec![
            self.ask_user.as_ref(),
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
    memory_strategy: Option<Arc<dyn zeroclaw_api::memory_traits::MemoryStrategy>>,
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
    activated_tools: Option<Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    hook_runner: Option<Arc<crate::hooks::HookRunner>>,
    approval_manager: Option<Arc<ApprovalManager>>,
    agent_alias: Option<String>,
    exclude_memory: bool,
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
            memory_strategy: None,
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
            activated_tools: None,
            hook_runner: None,
            approval_manager: None,
            agent_alias: None,
            exclude_memory: false,
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

    pub fn memory_strategy(
        mut self,
        memory_strategy: Arc<dyn zeroclaw_api::memory_traits::MemoryStrategy>,
    ) -> Self {
        self.memory_strategy = Some(memory_strategy);
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

    pub fn activated_tools(
        mut self,
        activated: Option<Arc<std::sync::Mutex<tools::ActivatedToolSet>>>,
    ) -> Self {
        self.activated_tools = activated;
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

    /// Exclude persistent memory from this agent. When set, the memory
    /// backend is replaced with `NoneMemory`, auto-save is forced off, and
    /// all `memory_*` tools are stripped from the tool set. Used by ACP
    /// sessions, which rely on session history for context rather than the
    /// agent's long-term memory.
    pub fn exclude_memory(mut self, exclude: bool) -> Self {
        self.exclude_memory = exclude;
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

        let workspace_dir = self
            .workspace_dir
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("."));

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
        // No-memory sessions must not retain a caller-provided strategy that
        // still closes over persistent memory.
        let memory_strategy = if exclude_memory {
            None
        } else {
            self.memory_strategy
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
            memory_strategy: memory_strategy.unwrap_or_else(|| {
                Arc::new(
                    crate::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                        memory.clone(),
                        zeroclaw_config::schema::MemoryConfig::default(),
                        workspace_dir.clone(),
                    ),
                )
            }),
            config: self.config.unwrap_or_default(),
            multimodal_config: self.multimodal_config.unwrap_or_default(),
            // No silent vendor-default model. Callers that construct `Agent` via the
            // builder must set `model_name` explicitly (via `.model_name(...)` or via
            // `Agent::from_config`, which resolves from `[model_providers]`). The sentinel
            // keeps the field non-empty so accidental dispatch surfaces a clear 4xx
            // rather than misrouting to a real vendor model.
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
            autonomy_level: self
                .autonomy_level
                .unwrap_or(crate::security::AutonomyLevel::Supervised),
            activated_tools: self.activated_tools,
            hook_runner: self.hook_runner,
            approval_manager: self.approval_manager,
            agent_alias: self.agent_alias.unwrap_or_default(),
            channel_handles: AgentChannelHandles::default(),
            image_cache: zeroclaw_providers::multimodal::LocalImageCache::new(),
        })
    }
}

impl Agent {
    pub fn builder() -> AgentBuilder {
        AgentBuilder::new()
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

    /// Late-bound channel-map handles for the five channel-driven tools.
    /// Populated by `from_config_with_session_cwd`; empty when an Agent is
    /// constructed via the builder directly. Callers (e.g. the ACP server)
    /// use `channel_handles().register_channel(...)` to wire a back-channel
    /// into all five tool maps in one shot.
    pub fn channel_handles(&self) -> &AgentChannelHandles {
        &self.channel_handles
    }

    /// Populate late-bound channel-map handles with configured channels.
    ///
    /// Seeds `ask_user`, `reaction`, `poll`, and `escalate`
    /// handles from the provided map. Called by CLI and orchestrator paths
    /// after agent construction but before the agent loop starts.
    ///
    /// Returns the list of registered channel names for logging.
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

    fn response_cache_key_for_messages(
        &self,
        messages: &[ChatMessage],
        effective_model: &str,
    ) -> Option<String> {
        if self.temperature != Some(0.0) || self.response_cache.is_none() {
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
    ) {
        let context = self
            .memory_strategy
            .load_context(user_message, self.memory_session_id.as_deref())
            .await
            .unwrap_or_default();

        if self.auto_save {
            let _ = self
                .memory
                .store(
                    "user_msg",
                    user_message,
                    MemoryCategory::Conversation,
                    self.memory_session_id.as_deref(),
                )
                .await;
        }

        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
        let enriched = if context.is_empty() {
            format!("[{now}] {user_message}")
        } else {
            format!("{context}[{now}] {user_message}")
        };

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

    /// Return the names of all registered tools.  Test-only — avoids
    /// exposing `Box<dyn Tool>` across the crate boundary.
    #[cfg(test)]
    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name()).collect()
    }

    /// Hydrate the agent with prior chat messages (e.g. from a session backend).
    ///
    /// Ensures a system prompt is prepended if history is empty, then appends all
    /// non-system messages from the seed. System messages in the seed are skipped
    /// to avoid duplicating the system prompt.
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

    /// Build an Agent with an optional per-session working directory override.
    ///
    /// `session_cwd`, when supplied, becomes [`SecurityPolicy::workspace_dir`]
    /// for this agent — i.e. the boundary used by file_read/write/edit and the
    /// cwd used by the shell tool. Memory storage, identity files, scheduled
    /// task DBs, and other on-disk state continue to live under
    /// `config.data_dir`.
    ///
    /// This is what ACP sessions use to pin tool path resolution to the
    /// IDE-provided `cwd` without relocating the agent's data directory.
    pub async fn from_config_with_session_cwd(
        config: &Config,
        agent_alias: &str,
        session_cwd: Option<&Path>,
    ) -> Result<Self> {
        Self::from_config_with_session_cwd_and_mcp(config, agent_alias, session_cwd, true).await
    }

    /// Build an Agent while optionally skipping eager MCP initialization.
    ///
    /// ACP clients expect `session/new` to return promptly. User-configured
    /// MCP servers are external processes/services and can block startup while
    /// they time out, so ACP uses this with `initialize_mcp = false`.
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
        )
        .await
    }

    /// Build an Agent for direct ACP/WS sessions that have a client approval
    /// back-channel. This keeps shell approval on the runtime-controlled path.
    ///
    /// When `exclude_memory` is `true`, the agent is constructed without
    /// persistent memory: `NoneMemory` backend, auto-save off, and all
    /// `memory_*` tools stripped. ACP sessions pass `true`.
    pub async fn from_config_with_session_cwd_and_mcp_backchannel(
        config: &Config,
        agent_alias: &str,
        session_cwd: Option<&Path>,
        initialize_mcp: bool,
        exclude_memory: bool,
    ) -> Result<Self> {
        Self::from_config_with_session_cwd_and_mcp_approval_mode(
            config,
            agent_alias,
            session_cwd,
            initialize_mcp,
            true,
            exclude_memory,
            None,
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
    ) -> Result<Self> {
        Self::from_config_with_session_cwd_and_mcp_approval_mode(
            config,
            agent_alias,
            session_cwd,
            initialize_mcp,
            true,
            exclude_memory,
            tui_env,
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
        // Seed the agent's bootstrap files (AGENTS.md / SOUL.md /
        // IDENTITY.md / USER.md / TOOLS.md / BOOTSTRAP.md) on first
        // run. Idempotent — never overwrites existing files; only
        // fills in the gaps so a freshly-created agent has a basic
        // identity to load.
        if let Err(e) = zeroclaw_config::schema::ensure_bootstrap_files(&agent_workspace).await {
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
            // When a per-session cwd overrides the sandbox root, ensure
            // the per-agent workspace (where skills, identity, and config
            // data live) remains readable. Without this, file_read and
            // search tools are locked out of the agent's workspace the
            // moment the session cwd differs.
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

        let all_tools_result = tools::all_tools_with_runtime(
            Arc::new(config.clone()),
            &security,
            risk_profile,
            agent_alias,
            runtime,
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
            None,
            false,
            tui_env,
        );
        let mut tools = all_tools_result.tools;
        let delegate_handle = all_tools_result.delegate_handle;
        let ask_user_handle = all_tools_result.ask_user_handle;
        let reaction_handle = all_tools_result.reaction_handle;
        let poll_handle = all_tools_result.poll_handle;
        let escalate_handle = all_tools_result.escalate_handle;

        // ── Built-in SecurityPolicy tool gate (parity with agent::run) ──
        // Apply the agent's allowlist (`allowed_tools`) AND denylist
        // (`excluded_tools`) to the built-in registry *before* MCP tools and
        // skill tools are added. `from_config` (ws.rs / daemon) bypasses the
        // channel orchestrator and previously enforced only the risk-profile
        // denylist (further below) on this path — never the allowlist — so an
        // agent allowlisted to e.g. `file_read` still kept raw `shell` /
        // `file_write`. Filtering here, before skill registration, is also
        // what lets a scoped elevation wrapper survive: the raw target is
        // removed while the distinct prefixed `{skill}__{tool}` wrapper is
        // appended later. MCP tools are initialized after this built-in
        // filter, then MCP registration and deferred discovery apply the same
        // SecurityPolicy explicitly so denied MCP tools do not surface.
        let before_policy_filter = tools.len();
        crate::agent::loop_::apply_policy_tool_filter(&mut tools, Some(security.as_ref()), None);
        if tools.len() != before_policy_filter {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "before": before_policy_filter,
                        "retained": tools.len(),
                        "policy_allowed": security.allowed_tools.as_ref().map(|v| v.len()),
                        "policy_excluded": security.excluded_tools.as_ref().map(|v| v.len()),
                    })),
                "Applied SecurityPolicy built-in tool filter (from_config path)"
            );
        }

        // ── Wire MCP tools (non-fatal) ─────────────────────────────
        // Replicates the same MCP initialization logic used in the CLI
        // and webhook paths (loop_.rs) so that the WebSocket/daemon UI
        // path also has access to MCP tools.
        let mut activated_tools: Option<Arc<std::sync::Mutex<tools::ActivatedToolSet>>> = None;
        // Resolution-only MCP wrappers for skill MCP elevation (kind = "mcp").
        let mut mcp_elevation_arcs: Vec<Arc<dyn tools::Tool>> = Vec::new();
        if initialize_mcp && config.mcp.enabled && !config.mcp.servers.is_empty() {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!(
                    "Initializing MCP client — {} server(s) configured",
                    config.mcp.servers.len()
                )
            );
            match tools::McpRegistry::connect_all(&config.mcp.servers).await {
                Ok(registry) => {
                    let registry = std::sync::Arc::new(registry);
                    mcp_elevation_arcs = tools::collect_mcp_elevation_arcs(&registry).await;
                    let mcp_policy =
                        crate::agent::loop_::mcp_tool_access_policy(security.as_ref(), None);
                    if config.mcp.deferred_loading {
                        let deferred_set = tools::DeferredMcpToolSet::from_registry(
                            std::sync::Arc::clone(&registry),
                        )
                        .await;
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            ),
                            &format!(
                                "MCP deferred: {} tool stub(s) from {} server(s)",
                                deferred_set.len(),
                                registry.server_count()
                            )
                        );
                        let allowed_stub_count = crate::agent::loop_::mcp_allowed_tool_count(
                            deferred_set
                                .stubs
                                .iter()
                                .map(|stub| stub.prefixed_name.as_str()),
                            mcp_policy.as_ref(),
                        );
                        if allowed_stub_count > 0 {
                            let activated =
                                Arc::new(std::sync::Mutex::new(tools::ActivatedToolSet::new()));
                            activated_tools = Some(Arc::clone(&activated));
                            let mut tool_search =
                                tools::ToolSearchTool::new(deferred_set, activated);
                            if let Some(policy) = mcp_policy {
                                tool_search = tool_search.with_access_policy(policy);
                            }
                            tools.push(Box::new(tool_search));
                        }
                    } else {
                        let names = registry.tool_names();
                        let mut registered = 0usize;
                        let mut skipped = 0usize;
                        for name in names {
                            if !crate::agent::loop_::eager_mcp_tool_allowed(
                                &name,
                                mcp_policy.as_ref(),
                            ) {
                                skipped += 1;
                                continue;
                            }
                            if let Some(def) = registry.get_tool_def(&name).await {
                                let wrapper: std::sync::Arc<dyn tools::Tool> =
                                    std::sync::Arc::new(tools::McpToolWrapper::new(
                                        name,
                                        def,
                                        std::sync::Arc::clone(&registry),
                                    ));
                                if crate::agent::loop_::register_eager_mcp_tool_if_allowed(
                                    wrapper,
                                    &mut tools,
                                    delegate_handle.as_ref(),
                                    mcp_policy.as_ref(),
                                ) {
                                    registered += 1;
                                }
                            }
                        }
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            ),
                            &format!(
                                "MCP: {} tool(s) registered from {} server(s), {} skipped by policy",
                                registered,
                                registry.server_count(),
                                skipped
                            )
                        );
                    }
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "MCP registry failed to initialize"
                    );
                }
            }
        }

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

        let dispatcher_choice = agent_cfg.resolved.tool_dispatcher.as_str();
        let tool_dispatcher: Box<dyn ToolDispatcher> = match dispatcher_choice {
            "native" => Box::new(NativeToolDispatcher),
            "xml" => Box::new(XmlToolDispatcher),
            _ if model_provider.supports_native_tools() => Box::new(NativeToolDispatcher),
            _ => Box::new(XmlToolDispatcher),
        };

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

        // Filter out tools excluded by this agent's risk profile. The
        // channel orchestrator applies this for channel-driven runs, but
        // Agent::from_config (used by ws.rs) doesn't go through that path.
        let excluded = &risk_profile.excluded_tools;
        if !excluded.is_empty() {
            tools.retain(|t| !excluded.iter().any(|ex| ex == t.name()));
        }

        // Load skills and register them as callable tools so WebSocket/daemon
        // sessions can execute them (not just describe them in the prompt).
        // Bundle-aware so `[agents.<alias>].skill_bundles` aliases resolve
        // through to `[skill_bundles.<alias>].directory` (defaulting to
        // `<install>/shared/skills/<alias>/`).
        let skills = crate::skills::load_skills_for_agent_from_config(config, agent_alias);
        // Resolution registry = built-in arcs + resolution-only MCP wrappers, so
        // skill elevation (kind = "builtin" / "mcp") can resolve either target.
        let skill_resolution_registry: Vec<Arc<dyn tools::Tool>> = all_tools_result
            .unfiltered_tool_arcs
            .iter()
            .cloned()
            .chain(mcp_elevation_arcs.iter().cloned())
            .collect();
        tools::register_skill_tools_with_context(
            &mut tools,
            &skills,
            security.clone(),
            &skill_resolution_registry,
        );

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
            .memory_strategy(Arc::new(
                crate::agent::memory_strategy::DefaultMemoryStrategy::with_config_and_limit(
                    memory.clone(),
                    config.memory.clone(),
                    security.workspace_dir.clone(),
                    config.effective_memory_recall_limit(agent_alias),
                ),
            ))
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
            .skills_prompt_mode(config.skills.prompt_injection_mode)
            .auto_save(config.memory.auto_save)
            .exclude_memory(exclude_memory)
            .security_summary(Some(security.prompt_summary()))
            .autonomy_level(risk_profile.level)
            .activated_tools(activated_tools)
            .hook_runner(if config.hooks.enabled {
                let mut runner = crate::hooks::HookRunner::new();
                if config.hooks.builtin.command_logger {
                    runner.register(Box::new(crate::hooks::builtin::CommandLoggerHook::new()));
                }
                if config.hooks.builtin.webhook_audit.enabled {
                    runner.register(Box::new(crate::hooks::builtin::WebhookAuditHook::new(
                        config.hooks.builtin.webhook_audit.clone(),
                    )));
                }
                Some(Arc::new(runner))
            } else {
                None
            })
            .approval_manager(Some(Arc::new(approval_manager)))
            .build()?;

        // Wire per-tool channel-map handles into the agent so callers (e.g.
        // the ACP server) can register back-channels after construction.
        agent.channel_handles = AgentChannelHandles {
            ask_user: ask_user_handle,
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

            // Avoid creating orphan ToolResults: if the first message remaining
            // after the drop is a ToolResults, its paired AssistantToolCalls was
            // dropped, so the ToolResults must be dropped too. Otherwise the
            // history would start with a tool_result block whose tool_use_id
            // has no matching tool_use, causing model_providers (e.g. Anthropic) to
            // reject the request with "messages.0.content.0: unexpected
            // tool_use_id found in tool_result blocks".
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

            // Symmetric guard: avoid orphan AssistantToolCalls at the new head.
            // If the first kept message is an AssistantToolCalls, the model sees
            // tool calls it made but never received results for (the paired
            // ToolResults was already dropped above or fell outside the window).
            // This corrupts the conversation and causes unpredictable behaviour
            // — the model may retry tools, hallucinate results, or go off-rails.
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

    fn build_system_prompt(&self) -> Result<String> {
        let expose_text_tool_protocol = !self.config.resolved.strict_tool_parsing
            || self.tool_dispatcher.should_send_tool_specs();
        let no_tools: Vec<Box<dyn Tool>> = Vec::new();
        let prompt_tools = if expose_text_tool_protocol {
            &self.tools
        } else {
            &no_tools
        };
        let instructions = self.tool_dispatcher.prompt_instructions(prompt_tools);
        let ctx = PromptContext {
            workspace_dir: &self.workspace_dir,
            agent_workspace_dir: &self.agent_workspace_dir,
            model_name: &self.model_name,
            tools: prompt_tools,
            skills: &self.skills,
            skills_prompt_mode: self.skills_prompt_mode,
            identity_config: Some(&self.identity_config),
            dispatcher_instructions: &instructions,
            sends_native_tool_specs: self.tool_dispatcher.should_send_tool_specs()
                && !prompt_tools.is_empty(),
            security_summary: self.security_summary.clone(),
            autonomy_level: self.autonomy_level,
        };
        self.prompt_builder.build(&ctx)
    }

    // Superseded by the loop's prepare/approve/execute pipeline (#7415).

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

    /// Reconstruct [`ConversationMessage`]s from the loop's provider-transcript
    /// encodings so wrapper-maintained histories keep their structured shape.
    ///
    /// Assistant messages carrying the loop's JSON tool-call encoding
    /// (`{"content", "tool_calls", "reasoning_content"?}`) round-trip into
    /// [`ConversationMessage::AssistantToolCalls`]; `role=tool` messages carry
    /// JSON tool results (an array for native parallel calls, a single object
    /// per call otherwise), and consecutive ones merge into a single
    /// [`ConversationMessage::ToolResults`] entry per tool round — the shape
    /// the dispatchers always produced; everything else round-trips as a
    /// plain chat message.
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
        // Refuse empty/whitespace-only turns. An empty `user_message` would
        // append a user-role message containing only the timestamp prefix
        // (`[<now>] `) to history, leaving the model to face a system prompt
        // immediately followed by a blank user turn. On Claude this surfaces
        // as the underlying `<<HUMAN_CONVERSATION_START>>` template sentinel
        // bleeding into the visible response ("there's no human turn yet…"),
        // because the model has nothing to respond to and narrates the
        // structural marker instead. Stopping it here keeps history clean
        // and prevents wasted model spend on garbage turns.
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

        let context = self
            .memory_strategy
            .load_context(user_message, self.memory_session_id.as_deref())
            .await
            .unwrap_or_default();

        if self.auto_save {
            let _ = self
                .memory
                .store(
                    "user_msg",
                    user_message,
                    MemoryCategory::Conversation,
                    self.memory_session_id.as_deref(),
                )
                .await;
        }

        let now = chrono::Local::now();
        let (year, month, day) = (now.year(), now.month(), now.day());
        let (hour, minute, second) = (now.hour(), now.minute(), now.second());
        let tz = now.format("%Z");
        let date_str =
            format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} {tz}");

        let enriched = if context.is_empty() {
            format!("[CURRENT DATE & TIME: {date_str}]\n\n{user_message}")
        } else {
            format!("[CURRENT DATE & TIME: {date_str}]\n\n{context}\n\n{user_message}")
        };

        self.history
            .push(ConversationMessage::Chat(ChatMessage::user(enriched)));

        let effective_model = self.classify_model(user_message);

        let turn_id = Self::new_turn_id();
        let turn_started_at = Instant::now();

        self.observer.record_event(&ObserverEvent::AgentStart {
            model_provider: self.model_provider_name.clone(),
            model: effective_model.clone(),
            channel: None,
            agent_alias: self.observer_agent_alias(),
            turn_id: Some(turn_id.clone()),
        });

        let mut guard = TurnGuard {
            observer: Arc::clone(&self.observer),
            model_provider: self.model_provider_name.clone(),
            model: effective_model.clone(),
            turn_id: Some(turn_id.clone()),
            turn_started_at,
            agent_alias: self.observer_agent_alias(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            saw_usage: false,
            done: false,
        };

        // Response cache: check once before entering the loop (only for
        // deterministic, text-only prompts). The key must include the whole
        // provider-visible transcript, not just the last user message,
        // otherwise distinct conversations can collide when their final
        // prompt matches. Keyed on the raw provider transcript — multimodal
        // preparation is a per-iteration loop concern now.
        let provider_messages = self.tool_dispatcher.to_provider_messages(&self.history);
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

        // Usage-only cost context: the loop's per-call recording accumulates
        // token totals here so AgentEnd keeps reporting them, without
        // persisting cost records or enforcing budgets (this path never did
        // either). The loop call below must stay a plain `.await` on this
        // task — caller-scoped task-locals (thread id, session key, tool
        // choice / thinking overrides) silently vanish across a spawn.
        let cost_context = crate::agent::loop_::ToolLoopCostTrackingContext::usage_only();
        let loop_result = crate::agent::loop_::TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(
                Some(cost_context.clone()),
                crate::agent::loop_::run_tool_call_loop(
                    self.model_provider.as_ref(),
                    &mut loop_history,
                    &self.tools,
                    self.observer.as_ref(),
                    &self.model_provider_name,
                    &effective_model,
                    self.temperature,
                    false,
                    self.approval_manager.as_deref(),
                    "cli",
                    None,
                    &self.multimodal_config,
                    self.config.resolved.max_tool_iterations,
                    None,
                    None,
                    self.hook_runner.as_deref(),
                    &[],
                    &self.config.resolved.tool_call_dedup_exempt,
                    self.activated_tools.as_ref(),
                    None,
                    &pacing,
                    self.config.resolved.strict_tool_parsing,
                    self.config.resolved.parallel_tools,
                    self.config.resolved.max_tool_result_chars,
                    self.config.resolved.max_context_tokens,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(&mut loop_new_messages),
                    &knobs,
                    Some(&mut self.image_cache),
                ),
            )
            .await;

        // Feed the accumulated per-call usage into the AgentEnd guard before
        // any return below drops it — including the error path, which must
        // still report usage from calls that succeeded earlier in the turn.
        let usage = cost_context.snapshot_turn_usage();
        if usage.input_tokens > 0 || usage.output_tokens > 0 {
            guard.total_input_tokens = usage.input_tokens;
            guard.total_output_tokens = usage.output_tokens;
            guard.saw_usage = true;
        }
        // Replay the loop's transcript additions into the conversation
        // history BEFORE propagating any loop error: rounds that already
        // executed carry side effects (tools ran), and the pre-consolidation
        // engine pushed them into `self.history` incrementally per iteration,
        // so they survived a later-iteration provider failure. The loop's
        // `new_messages_out` append-log is populated on error exits too;
        // `replay_loop_messages` reverses the loop's provider encodings back
        // into structured `ConversationMessage`s.
        for replayed in Self::replay_loop_messages(&loop_new_messages) {
            self.history.push(replayed);
        }
        let response = loop_result?;

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

        self.trim_history();

        Ok(response)
    }

    /// Execute a single agent turn while streaming intermediate events.
    ///
    /// Behaves identically to [`turn`](Self::turn) but forwards [`TurnEvent`]s
    /// through the provided channel so callers (e.g. the WebSocket gateway)
    /// can relay incremental updates to clients.
    ///
    /// The returned tuple contains the final assistant response string and all
    /// new [`ConversationMessage`]s added during this turn (captured before
    /// any `trim_history` call so callers can persist them correctly even when
    /// the history is already at its configured limit).
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
        /// Routes the loop's single-channel approval callback through every
        /// registered ask_user back-channel — the first decisive answer wins —
        /// preserving the multi-channel iteration of the old direct execution
        /// path (ACP and WS sessions register their approval back-channels
        /// here at session start; hard-coding one name would break the other).
        struct AskUserApprovalBridge {
            handles: tools::PerToolChannelHandle,
            // The back-channel that answered the most recent request, so the
            // approval audit records the deciding surface (WS, ACP, …) rather
            // than the loop's static "cli" channel name. See
            // `last_decision_channel` below and `gate_tool_approval`.
            last_decision: parking_lot::Mutex<Option<String>>,
        }

        impl ::zeroclaw_api::attribution::Attributable for AskUserApprovalBridge {
            fn role(&self) -> ::zeroclaw_api::attribution::Role {
                ::zeroclaw_api::attribution::Role::Channel(
                    ::zeroclaw_api::attribution::ChannelKind::Cli,
                )
            }
            fn alias(&self) -> &str {
                "agent-approval-bridge"
            }
        }

        #[async_trait::async_trait]
        impl zeroclaw_api::channel::Channel for AskUserApprovalBridge {
            fn name(&self) -> &str {
                "agent-approval-bridge"
            }

            fn last_decision_channel(&self) -> Option<String> {
                self.last_decision.lock().clone()
            }

            async fn send(
                &self,
                _message: &zeroclaw_api::channel::SendMessage,
            ) -> anyhow::Result<()> {
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
                recipient: &str,
                request: &zeroclaw_api::channel::ChannelApprovalRequest,
            ) -> anyhow::Result<Option<zeroclaw_api::channel::ChannelApprovalResponse>>
            {
                let channels: Vec<(String, Arc<dyn zeroclaw_api::channel::Channel>)> = self
                    .handles
                    .read()
                    .iter()
                    .map(|(name, channel)| (name.clone(), Arc::clone(channel)))
                    .collect();
                // Clear the previous decision's attribution; only a decisive
                // answer below sets it, so an all-`None` fan-out leaves it unset
                // and the gate falls back to the loop's channel name.
                *self.last_decision.lock() = None;
                for (channel_name, channel) in &channels {
                    match channel.request_approval(recipient, request).await {
                        Ok(Some(response)) => {
                            *self.last_decision.lock() = Some(channel_name.clone());
                            return Ok(Some(response));
                        }
                        Ok(None) => continue,
                        Err(e) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                .with_attrs(::serde_json::json!({
                                    "tool": request.tool_name,
                                    "channel": channel_name,
                                    "error": format!("{}", e),
                                })),
                                "channel approval request failed"
                            );
                        }
                    }
                }
                Ok(None)
            }
        }

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
        self.append_streamed_user_message_to_history(user_message, &mut new_msgs)
            .await;

        let effective_model = self.classify_model(user_message);
        let turn_started_at = Instant::now();
        let turn_id = Self::new_turn_id();
        let mut committed_response = String::new();

        self.observer.record_event(&ObserverEvent::AgentStart {
            model_provider: self.model_provider_name.clone(),
            model: effective_model.clone(),
            channel: None,
            agent_alias: self.observer_agent_alias(),
            turn_id: Some(turn_id.clone()),
        });

        let mut guard = TurnGuard {
            observer: Arc::clone(&self.observer),
            model_provider: self.model_provider_name.clone(),
            model: effective_model.clone(),
            turn_id: Some(turn_id.clone()),
            turn_started_at,
            agent_alias: self.observer_agent_alias(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            saw_usage: false,
            done: false,
        };

        // Response cache: check once before the round loop, keyed on the full
        // provider-visible transcript (matches `Agent::turn`; the old code
        // re-checked every iteration, but only a transcript identical to a
        // previously cached single-exchange turn can ever hit).
        let provider_messages = self.tool_dispatcher.to_provider_messages(&self.history);
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
                self.trim_history();
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
                Box::new(AskUserApprovalBridge {
                    handles: Arc::clone(handles),
                    last_decision: parking_lot::Mutex::new(None),
                }) as Box<dyn zeroclaw_api::channel::Channel>
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

        // Usage-only cost context, as in `Agent::turn`: per-call token totals
        // feed the AgentEnd guard without persisting cost records or
        // enforcing budgets (this path never did either). One context spans
        // every round, so its snapshot is cumulative. The loop calls below
        // must stay plain `.await`s on this task — caller-scoped task-locals
        // (ws.rs session key, rpc thread id, tool-choice/thinking overrides)
        // silently vanish across a spawn.
        let cost_context = crate::agent::loop_::ToolLoopCostTrackingContext::usage_only();

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
                return Err(StreamedTurnError {
                    error: crate::agent::loop_::ToolLoopCancelled.into(),
                    committed_response,
                    new_messages: new_msgs,
                });
            }

            // Steering drain: each accepted mid-turn message becomes its own
            // enriched user turn in both transcripts before the next round.
            for steering_message in crate::agent::loop_::drain_steering_messages(&mut steering_rx) {
                self.append_streamed_user_message_to_history(&steering_message, &mut new_msgs)
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
            let loop_result = crate::agent::loop_::TOOL_LOOP_COST_TRACKING_CONTEXT
                .scope(
                    Some(cost_context.clone()),
                    crate::agent::loop_::run_tool_call_loop(
                        self.model_provider.as_ref(),
                        &mut loop_history,
                        &self.tools,
                        self.observer.as_ref(),
                        &self.model_provider_name,
                        &effective_model,
                        self.temperature,
                        true,
                        self.approval_manager.as_deref(),
                        "cli",
                        None,
                        &self.multimodal_config,
                        self.config.resolved.max_tool_iterations,
                        cancel_token.clone(),
                        None,
                        self.hook_runner.as_deref(),
                        &[],
                        &self.config.resolved.tool_call_dedup_exempt,
                        self.activated_tools.as_ref(),
                        None,
                        &pacing,
                        self.config.resolved.strict_tool_parsing,
                        self.config.resolved.parallel_tools,
                        self.config.resolved.max_tool_result_chars,
                        self.config.resolved.max_context_tokens,
                        None,
                        approval_bridge.as_deref(),
                        None,
                        None,
                        Some(event_tx.clone()),
                        None,
                        Some(&mut round_added),
                        &knobs,
                        Some(&mut self.image_cache),
                    ),
                )
                .await;

            // Feed cumulative usage into the AgentEnd guard before any return
            // below drops it — the error paths must still report usage from
            // calls that succeeded earlier in the turn.
            let usage = cost_context.snapshot_turn_usage();
            if usage.input_tokens > 0 || usage.output_tokens > 0 {
                guard.total_input_tokens = usage.input_tokens;
                guard.total_output_tokens = usage.output_tokens;
                guard.saw_usage = true;
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
                    return Ok(StreamedTurnSuccess {
                        response: committed_response,
                        new_messages: new_msgs,
                    });
                }
                Err(error) => {
                    self.trim_history();
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
                    if crate::agent::loop_::is_tool_loop_cancelled(&error) {
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
                        return Err(StreamedTurnError {
                            error: crate::agent::loop_::ToolLoopCancelled.into(),
                            committed_response,
                            new_messages: new_msgs,
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
                        new_messages: new_msgs,
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
    let start = Instant::now();

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

    let (provider_name, model_name) =
        match effective_config.resolved_model_provider_for_agent(agent_alias) {
            Some((ty, _alias, entry)) => {
                let model = entry
                    .model
                    .as_deref()
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .map(ToString::to_string)
                    .or_else(|| effective_config.resolve_default_model())
                    .unwrap_or_else(|| "<unresolved>".to_string());
                (ty.to_string(), model)
            }
            None => (
                provider_override.unwrap_or_else(|| "unknown".to_string()),
                effective_config
                    .resolve_default_model()
                    .unwrap_or_else(|| "<unresolved>".to_string()),
            ),
        };

    agent.observer.record_event(&ObserverEvent::AgentStart {
        model_provider: provider_name.clone(),
        model: model_name.clone(),
        channel: None,
        agent_alias: None,
        turn_id: None,
    });

    let _run_guard = TurnGuard {
        observer: Arc::clone(&agent.observer),
        model_provider: provider_name,
        model: model_name,
        turn_id: None,
        turn_started_at: start,
        agent_alias: None,
        total_input_tokens: 0,
        total_output_tokens: 0,
        saw_usage: false,
        done: false,
    };

    if let Some(msg) = message {
        let response = agent.run_single(&msg).await?;
        println!("{response}");
    } else {
        agent.run_interactive().await?;
    }

    Ok(())
}

// #7415 safety net (child module so fixtures can reach Agent internals the
// same way `mod tests` does).
#[cfg(test)]
#[path = "safety_net.rs"]
mod safety_net;

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
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

    zeroclaw_api::mock_tool_attribution!(CountingTool, NamedMockTool, MockTool, SlowTool,);

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
            // Use the `custom:<url>` model_provider — it builds an
            // OpenAiCompatibleModelProvider routed through the `compat`
            // closure, which is the only path that actually wires
            // `extra_headers` onto outgoing requests. (The native
            // `openai` factory ignores extra_headers; OpenRouter
            // hardcodes the upstream URL.)
            // Custom-URL model_provider: type is the canonical `custom` slot,
            // operator URL goes in the `uri` field (post-Phase 6
            // operators no longer put URLs in the outer type key).
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
            risk_profile: "test-profile".to_string(),
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
                risk_profile: "test-profile".to_string(),
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

    /// When a per-session cwd overrides the sandbox root, workspace files must
    /// stay readable. Regression guard for the `allowed_roots.push` fix in
    /// `from_config_with_session_cwd` (issue #6516).
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

    /// Provider that emits TWO native tool calls in a single assistant turn,
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

    /// With parallel_tools disabled (the default) and no approval manager, a
    /// turn carrying multiple tool calls must dispatch them strictly serially:
    /// each ToolCall start event is immediately followed by its own ToolResult
    /// before the next call's start event. The pre-fix code emitted all start
    /// events up front, then all results — which floods the front-end IPC on a
    /// large multi-call turn. Order is the contract this test pins.
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

    /// Reproduction test for the orphan-tool_results trim bug.
    ///
    /// `trim_history` previously dropped the oldest N entries blindly. When
    /// the boundary fell in the middle of an `AssistantToolCalls` /
    /// `ToolResults` pair, the call side was dropped while the result side
    /// remained — leaving an orphan `ToolResults` at the head of the
    /// history. The next model_provider request then started with a `tool_result`
    /// block that had no matching `tool_use`, which Anthropic rejects with:
    ///
    ///   `messages.0.content.0: unexpected tool_use_id found in tool_result blocks`
    ///
    /// To reliably reproduce the bug we need the drop boundary to fall in
    /// the middle of a pair. Five entries (`AC1, TR1, AC2, TR2, AC3`) with
    /// `max = 4` makes `drop_count = 1`, which removes `AC1` and leaves
    /// `TR1` as an orphan at the head.
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

        // Set up so the trim boundary lands between a TR and its following
        // AC, leaving the AC at the head without a preceding user message
        // context. More importantly, test the case where after the orphan-TR
        // guard fires, an AC ends up at position 0.
        //
        // History: [user1, AC1, TR1, AC2, TR2, user2, AC3, TR3]
        // len=8, max=4, drop_count=4 → drops user1, AC1, TR1, AC2
        // Position 4 = TR2 → orphan-TR guard bumps to 5
        // Position 5 = user2 → stops (user2 is fine)
        //
        // But we want to test the AC-at-head case. So:
        // History: [user1, AC1, TR1, AC2, TR2, AC3, TR3]
        // len=7, max=3, drop_count=4 → drops user1, AC1, TR1, AC2
        // Position 4 = TR2 → orphan-TR guard bumps to 5
        // Position 5 = AC3 → NEW guard should bump to 6 (drop AC3)
        // Position 6 = TR3 → NEW guard should bump to 7 (drop TR3)
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

    /// Regression test for the orphan-cascade-empties-everything 400.
    ///
    /// When `max_history_messages` is small relative to a long single-turn
    /// tool loop, the orphan-removal cascades in `trim_history` can advance
    /// `drop_count` all the way to `other_messages.len()`. Before the guard,
    /// `other_messages.drain(0..drop_count)` then emptied the entire
    /// non-system history, `convert_messages` lifted the system entry into
    /// `system_prompt`, and the provider received `messages: []` — a hard
    /// 400 `"messages: at least one message is required"` from Anthropic.
    ///
    /// Minimal repro of the cascade:
    ///   history = [user, AC1, TR1, AC2, TR2]   (no system message)
    ///   max_history_messages = 4
    ///   other_messages.len() = 5, initial_drop_count = 1 → drops `user`
    ///   orphan-TR cascade: pos 1 = AC1, not TR → no-op
    ///   orphan-AC cascade: pos 1 = AC1, pos 2 = TR1, pos 3 = AC2,
    ///                      pos 4 = TR2 → drop_count = 5
    ///   drop_count (5) >= other_messages.len() (5) → guard MUST fire.
    ///
    /// Without the guard this test fails: `agent.history` ends up empty,
    /// which is exactly the shape that crashes the provider call.
    /// With the guard, the original history is preserved unchanged so the
    /// session stays over-limit-but-functional.
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

        // When the full cascade triggers the guard the contract is "skip the
        // trim, leave history exactly as it was, accept being temporarily
        // over the limit." Lock that exact behavior — anything weaker (e.g.
        // partial trim) would silently regress the orphan-pair invariants
        // the other trim tests cover.
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

    /// Same cascade-to-empty path, but with a system message present.
    ///
    /// Verifies the guard's restore path puts the conversation back together
    /// in the right order (`system_messages` first, then the original
    /// non-system entries) rather than dropping the system message on the
    /// floor or returning the slices reversed. Without the guard the system
    /// message survives (it is held aside in `system_messages`) but the
    /// non-system half is still drained — so `agent.history` would end up as
    /// just `[system]` and `convert_messages` would lift it into
    /// `system_prompt`, again producing `messages: []`.
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

    /// When the model returns narration text alongside tool calls, the agent
    /// must store exactly ONE assistant history entry (AssistantToolCalls) —
    /// not a plain Chat(assistant) followed by AssistantToolCalls. The latter
    /// pattern causes model_providers that enforce role-alternation to reject the
    /// next request with a consecutive-assistant-role error.
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
                })
                .collect(),
            prompts: vec![],
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
    fn from_config_policy_filter_blocks_raw_target_but_keeps_scoped_wrapper() {
        // Path-level boundary for the from_config (ws.rs / daemon) path. The
        // SecurityPolicy allow/deny gate now runs over the built-in registry
        // before skill tools are registered (parity with agent::run), so an
        // agent allowlisted to `file_read` does NOT keep raw `shell`, while the
        // skill's scoped wrapper — distinct prefixed name — remains the only
        // callable path to that capability. This exercises the exact
        // apply_policy_tool_filter + register_skill_tools_with_context sequence
        // from_config performs.
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
            }],
            prompts: vec![],
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

    /// Regression test: `turn_streamed` must return the new messages in its
    /// second tuple element even when `trim_history` fires and removes old
    /// entries from the front of the history.  Before the fix, callers that
    /// sliced `history[pre_len..]` after the turn would get an empty slice
    /// because trim had shifted the tail back to `pre_len`.
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
            | ObserverEvent::ToolCall { turn_id, .. } => turn_id.as_deref(),
            _ => None,
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
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .model_name("test-model".into())
            .temperature(Some(0.0))
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
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .model_name("test-model".into())
            .temperature(Some(0.0))
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
            .build()
            .expect("agent builder should succeed with valid config");

        let _ = agent.turn("test").await.expect("turn should succeed");

        let events = capturing.events.lock();
        let turn_ids: Vec<&str> = events.iter().filter_map(observer_event_turn_id).collect();
        assert!(!turn_ids.is_empty(), "turn events should carry turn_id");
        let first = turn_ids[0];
        assert!(
            turn_ids.iter().all(|turn_id| *turn_id == first),
            "all turn_ids should be consistent"
        );
    }
}
