use crate::agent::dispatcher::{
    NativeToolDispatcher, ParsedToolCall, ToolDispatcher, ToolExecutionResult, XmlToolDispatcher,
};
use crate::agent::eval::AutoClassifyExt;
use crate::agent::memory_loader::{DefaultMemoryLoader, MemoryLoader};
use crate::agent::prompt::{PromptContext, SystemPromptBuilder};
use crate::approval::{ApprovalManager, ApprovalRequest, ApprovalRequirement, ApprovalResponse};
use crate::observability::{self, Observer, ObserverEvent};
use crate::platform;
use crate::security::SecurityPolicy;
use crate::tools::{self, Tool, ToolSpec};
use anyhow::{Context, Result};
use chrono::{Datelike, Timelike};
use std::collections::{HashMap, VecDeque};
use std::io::Write as IoWrite;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use zeroclaw_config::schema::Config;
use zeroclaw_memory::{self, Memory, MemoryCategory};
use zeroclaw_providers::{self, ChatMessage, ChatRequest, ConversationMessage, ModelProvider};
use zeroclaw_tool_call_parser::strip_think_tags;

// Re-export TurnEvent from zeroclaw-types for backwards compatibility.
pub use zeroclaw_api::agent::TurnEvent;

pub struct Agent {
    model_provider: Box<dyn ModelProvider>,
    tools: Vec<Box<dyn Tool>>,
    tool_specs: Vec<ToolSpec>,
    memory: Arc<dyn Memory>,
    observer: Arc<dyn Observer>,
    prompt_builder: SystemPromptBuilder,
    tool_dispatcher: Box<dyn ToolDispatcher>,
    memory_loader: Box<dyn MemoryLoader>,
    config: zeroclaw_config::schema::AliasedAgentConfig,
    multimodal_config: zeroclaw_config::schema::MultimodalConfig,
    model_name: String,
    model_provider_name: String,
    temperature: f64,
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
    #[allow(dead_code)] // WIP: stored for future runtime tool filtering
    allowed_tools: Option<Vec<String>>,
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
    /// Late-bound channel maps for the four channel-driven tools
    /// (`ask_user`, `reaction`, `escalate_to_human`, `poll`). Held so that
    /// per-session callers (e.g. the ACP server) can register a back-channel
    /// after agent construction. Production paths populate via
    /// `start_channels`; this is the alternate path for environments that
    /// build an Agent directly without `start_channels`.
    channel_handles: AgentChannelHandles,
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
    pub ask_user: Option<tools::ChannelMapHandle>,
    pub reaction: Option<tools::ChannelMapHandle>,
    pub escalate: Option<tools::ChannelMapHandle>,
    pub poll: Option<tools::ChannelMapHandle>,
}

impl AgentChannelHandles {
    /// Register a channel into every populated handle so all four
    /// channel-driven tools can resolve it by name.
    pub fn register_channel(
        &self,
        name: impl Into<String>,
        channel: Arc<dyn zeroclaw_api::channel::Channel>,
    ) {
        let name = name.into();
        for handle in [&self.ask_user, &self.reaction, &self.escalate, &self.poll]
            .into_iter()
            .flatten()
        {
            handle.write().insert(name.clone(), Arc::clone(&channel));
        }
    }

    /// Remove a channel from every populated handle (used on session/stop).
    pub fn unregister_channel(&self, name: &str) {
        for handle in [&self.ask_user, &self.reaction, &self.escalate, &self.poll]
            .into_iter()
            .flatten()
        {
            handle.write().remove(name);
        }
    }

    /// Look up a registered channel by name from any populated channel map.
    pub fn get_channel(&self, name: &str) -> Option<Arc<dyn zeroclaw_api::channel::Channel>> {
        for handle in [&self.ask_user, &self.reaction, &self.escalate, &self.poll]
            .into_iter()
            .flatten()
        {
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
    memory_loader: Option<Box<dyn MemoryLoader>>,
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
            memory_loader: None,
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

    pub fn memory_loader(mut self, memory_loader: Box<dyn MemoryLoader>) -> Self {
        self.memory_loader = Some(memory_loader);
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

    pub fn temperature(mut self, temperature: f64) -> Self {
        self.temperature = Some(temperature);
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
        let tool_specs = tools.iter().map(|tool| tool.spec()).collect();

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
            tool_specs,
            memory: self.memory.ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"missing_field": "memory"})),
                    "AgentBuilder::build missing required field"
                );
                anyhow::Error::msg("memory is required")
            })?,
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
            memory_loader: self
                .memory_loader
                .unwrap_or_else(|| Box::new(DefaultMemoryLoader::default())),
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
            temperature: self.temperature.unwrap_or(0.7),
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
            auto_save: self.auto_save.unwrap_or(false),
            memory_session_id: self.memory_session_id,
            history: Vec::new(),
            classification_config: self.classification_config.unwrap_or_default(),
            available_hints: self.available_hints.unwrap_or_default(),
            route_model_by_hint: self.route_model_by_hint.unwrap_or_default(),
            allowed_tools: allowed,
            response_cache: self.response_cache,
            security_summary: self.security_summary,
            autonomy_level: self
                .autonomy_level
                .unwrap_or(crate::security::AutonomyLevel::Supervised),
            activated_tools: self.activated_tools,
            hook_runner: self.hook_runner,
            approval_manager: self.approval_manager,
            channel_handles: AgentChannelHandles::default(),
        })
    }
}

impl Agent {
    pub fn builder() -> AgentBuilder {
        AgentBuilder::new()
    }

    /// Late-bound channel-map handles for the four channel-driven tools.
    /// Populated by `from_config_with_session_cwd`; empty when an Agent is
    /// constructed via the builder directly. Callers (e.g. the ACP server)
    /// use `channel_handles().register_channel(...)` to wire a back-channel
    /// into all four tool maps in one shot.
    pub fn channel_handles(&self) -> &AgentChannelHandles {
        &self.channel_handles
    }

    pub fn history(&self) -> &[ConversationMessage] {
        &self.history
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
        if self.temperature != 0.0 || self.response_cache.is_none() {
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

    fn drain_steering_messages(
        steering_rx: &mut Option<&mut tokio::sync::mpsc::Receiver<String>>,
    ) -> Vec<String> {
        let Some(rx) = steering_rx.as_deref_mut() else {
            return Vec::new();
        };

        let mut messages = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(message) => messages.push(message),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
            }
        }
        messages
    }

    async fn append_streamed_user_message_to_history(
        &mut self,
        user_message: &str,
        new_msgs: &mut Vec<ConversationMessage>,
    ) {
        let context = self
            .memory_loader
            .load_context(
                self.memory.as_ref(),
                user_message,
                self.memory_session_id.as_deref(),
            )
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

    fn marked_partial_response(partial: &str, marker: &str) -> String {
        if partial.is_empty() {
            marker.to_string()
        } else {
            format!("{partial}\n\n{marker}")
        }
    }

    fn append_streamed_assistant_message_to_history(
        &mut self,
        content: String,
        new_msgs: &mut Vec<ConversationMessage>,
        committed_response: &mut String,
    ) {
        let assistant_msg = ConversationMessage::Chat(ChatMessage::assistant(content.clone()));
        new_msgs.push(assistant_msg.clone());
        self.history.push(assistant_msg);
        committed_response.push_str(&content);
    }

    fn should_send_tool_specs(&self) -> bool {
        self.tool_dispatcher.should_send_tool_specs() && !self.tool_specs.is_empty()
    }

    fn parse_response_for_effective_tools(
        &self,
        response: &zeroclaw_providers::ChatResponse,
    ) -> (String, Vec<ParsedToolCall>) {
        if self.tool_specs.is_empty() {
            return (strip_think_tags(response.text_or_empty()), Vec::new());
        }

        if self.config.strict_tool_parsing && response.tool_calls.is_empty() {
            return (strip_think_tags(response.text_or_empty()), Vec::new());
        }

        self.tool_dispatcher.parse_response(response)
    }

    pub fn set_memory_session_id(&mut self, session_id: Option<String>) {
        self.memory_session_id = session_id;
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
        )
        .await
    }

    /// Build an Agent for direct ACP/WS sessions that have a client approval
    /// back-channel. This keeps shell approval on the runtime-controlled path.
    pub async fn from_config_with_session_cwd_and_mcp_backchannel(
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
            true,
        )
        .await
    }

    async fn from_config_with_session_cwd_and_mcp_approval_mode(
        config: &Config,
        agent_alias: &str,
        session_cwd: Option<&Path>,
        initialize_mcp: bool,
        approval_backchannel: bool,
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
            let mut policy = SecurityPolicy::for_agent(config, agent_alias)?;
            if let Some(session_cwd) = session_cwd {
                policy.workspace_dir = session_cwd.to_path_buf();
            }
            // When a per-session cwd overrides the sandbox root, ensure
            // the per-agent workspace (where skills, identity, and config
            // data live) remains readable. Without this, file_read and
            // search tools are locked out of the agent's workspace the
            // moment the session cwd differs.
            if session_cwd.is_some() {
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
                             resolve to a configured [model_providers.<type>.<alias>] entry"
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

        let (
            mut tools,
            delegate_handle,
            reaction_handle,
            poll_handle,
            ask_user_handle,
            escalate_handle,
        ) = tools::all_tools_with_runtime(
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
        );

        // ── Wire MCP tools (non-fatal) ─────────────────────────────
        // Replicates the same MCP initialization logic used in the CLI
        // and webhook paths (loop_.rs) so that the WebSocket/daemon UI
        // path also has access to MCP tools.
        let mut activated_tools: Option<Arc<std::sync::Mutex<tools::ActivatedToolSet>>> = None;
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
                        let activated =
                            Arc::new(std::sync::Mutex::new(tools::ActivatedToolSet::new()));
                        activated_tools = Some(Arc::clone(&activated));
                        tools.push(Box::new(tools::ToolSearchTool::new(
                            deferred_set,
                            activated,
                        )));
                    } else {
                        let names = registry.tool_names();
                        let mut registered = 0usize;
                        for name in names {
                            if let Some(def) = registry.get_tool_def(&name).await {
                                let wrapper: std::sync::Arc<dyn tools::Tool> =
                                    std::sync::Arc::new(tools::McpToolWrapper::new(
                                        name,
                                        def,
                                        std::sync::Arc::clone(&registry),
                                    ));
                                if let Some(ref handle) = delegate_handle {
                                    handle.write().push(std::sync::Arc::clone(&wrapper));
                                }
                                tools.push(Box::new(tools::ArcToolRef(wrapper)));
                                registered += 1;
                            }
                        }
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            ),
                            &format!(
                                "MCP: {} tool(s) registered from {} server(s)",
                                registered,
                                registry.server_count()
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
                 with no `model` set. Configure [model_providers.{provider_name}.<alias>] \
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

        let dispatcher_choice = agent_cfg.tool_dispatcher.as_str();
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
        let skills = crate::skills::load_skills_for_agent(&config.data_dir, config, agent_alias);
        tools::register_skill_tools(&mut tools, &skills, security.clone());

        let approval_manager = if approval_backchannel {
            ApprovalManager::for_non_interactive_backchannel(risk_profile)
        } else {
            ApprovalManager::for_non_interactive(risk_profile)
        };

        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(tools)
            .memory(memory)
            .observer(observer)
            .response_cache(response_cache)
            .tool_dispatcher(tool_dispatcher)
            .memory_loader(Box::new(DefaultMemoryLoader::new(
                5,
                config.memory.min_relevance_score,
            )))
            .prompt_builder(SystemPromptBuilder::with_defaults())
            .config(agent_cfg.clone())
            .multimodal_config(config.multimodal.clone())
            .model_name(model_name)
            .model_provider_name(provider_name.to_string())
            .temperature(
                agent_model_provider
                    .and_then(|e| e.temperature)
                    .unwrap_or(0.7),
            )
            .workspace_dir(security.workspace_dir.clone())
            .agent_workspace_dir(agent_workspace.clone())
            .classification_config(config.query_classification.clone())
            .available_hints(available_hints)
            .route_model_by_hint(route_model_by_hint)
            .identity_config(agent_cfg.identity.clone())
            .skills(skills)
            .skills_prompt_mode(config.skills.prompt_injection_mode)
            .auto_save(config.memory.auto_save)
            .security_summary(Some(security.prompt_summary()))
            .autonomy_level(risk_profile.level)
            .activated_tools(activated_tools)
            .hook_runner(crate::hooks::build_runner(config))
            .approval_manager(Some(Arc::new(approval_manager)))
            .build()?;

        agent.channel_handles = AgentChannelHandles {
            ask_user: ask_user_handle,
            reaction: reaction_handle,
            escalate: escalate_handle,
            poll: Some(poll_handle),
        };

        Ok(agent)
    }

    fn trim_history(&mut self) {
        let max = self.config.max_history_messages;
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
            let mut drop_count = other_messages.len() - max;

            // Avoid creating orphan ToolResults: if the first message remaining
            // after the drop is a ToolResults, its paired AssistantToolCalls was
            // dropped, so the ToolResults must be dropped too. Otherwise the
            // history would start with a tool_result block whose tool_use_id
            // has no matching tool_use, causing model_providers (e.g. Anthropic) to
            // reject the request with "messages.0.content.0: unexpected
            // tool_use_id found in tool_result blocks".
            while drop_count < other_messages.len()
                && matches!(
                    &other_messages[drop_count],
                    ConversationMessage::ToolResults(_)
                )
            {
                drop_count += 1;
            }

            other_messages.drain(0..drop_count);
        }

        self.history = system_messages;
        self.history.extend(other_messages);
    }

    fn build_system_prompt(&self) -> Result<String> {
        let expose_text_tool_protocol =
            !self.config.strict_tool_parsing || self.tool_dispatcher.should_send_tool_specs();
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

    async fn prepare_provider_messages(
        &self,
        messages: &[ChatMessage],
    ) -> Result<Vec<ChatMessage>> {
        let prepared = zeroclaw_providers::multimodal::prepare_messages_for_provider(
            messages,
            &self.multimodal_config,
        )
        .await?;
        Ok(prepared.messages)
    }

    async fn execute_tool_call(&self, call: &ParsedToolCall) -> ToolExecutionResult {
        let start = Instant::now();

        // ── Hook: before_tool_call (modifying) ──────────────────
        // Mirrors the hook pipeline in run_tool_call_loop (loop_.rs) so that
        // library-integrated runs honour the same hook chain.
        let mut tool_name = call.name.clone();
        let mut tool_args = call.arguments.clone();
        if let Some(ref hooks) = self.hook_runner {
            match hooks
                .run_before_tool_call(tool_name.clone(), tool_args.clone())
                .await
            {
                crate::hooks::HookResult::Continue((n, a)) => {
                    tool_name = n;
                    tool_args = a;
                }
                crate::hooks::HookResult::Cancel(reason) => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"tool": call.name, "reason": reason.to_string()})), "tool call cancelled by hook");
                    return ToolExecutionResult {
                        name: call.name.clone(),
                        output: format!("Cancelled by hook: {reason}"),
                        success: false,
                        tool_call_id: call.tool_call_id.clone(),
                    };
                }
            }
        }

        super::set_runtime_approved_arg(&tool_name, &mut tool_args, false);

        // ── Approval hook ──────────────────────────────────────
        // The ACP/WebSocket Agent path executes tools directly instead of
        // going through run_tool_call_loop. Keep its policy behavior aligned
        // with the shared loop by honoring auto_approve / always_ask here too.
        let mut approval_requirement = self
            .approval_manager
            .as_deref()
            .map(|mgr| mgr.approval_requirement(&tool_name))
            .unwrap_or(ApprovalRequirement::NotRequired);
        if let Some(mgr) = self.approval_manager.as_deref()
            && approval_requirement == ApprovalRequirement::Prompt
        {
            let request = ApprovalRequest {
                tool_name: tool_name.clone(),
                arguments: tool_args.clone(),
            };

            let (decision, decision_channel) = if mgr.is_non_interactive() {
                // Iterate every registered channel looking for one that can
                // handle the approval request. The first Ok(Some(_)) wins.
                // This avoids hard-coding a channel name (e.g. "acp") and
                // naturally supports WS sessions or any future back-channel.
                let ch_request = zeroclaw_api::channel::ChannelApprovalRequest {
                    tool_name: request.tool_name.clone(),
                    arguments_summary: crate::approval::summarize_args(&request.arguments),
                    raw_arguments: Some(request.arguments.clone()),
                };
                let mut channel_decision: Option<zeroclaw_api::channel::ChannelApprovalResponse> =
                    None;
                let mut decision_channel_name = String::new();
                // Collect channels while holding the lock briefly, then drop
                // the lock before any await points so the guard is not Send.
                let channels: Vec<(String, Arc<dyn zeroclaw_api::channel::Channel>)> = self
                    .channel_handles
                    .ask_user
                    .as_ref()
                    .map(|h| {
                        h.read()
                            .iter()
                            .map(|(k, v)| (k.clone(), Arc::clone(v)))
                            .collect()
                    })
                    .unwrap_or_default();
                for (ch_name, ch) in &channels {
                    match ch.request_approval("", &ch_request).await {
                        Ok(Some(r)) => {
                            decision_channel_name = ch_name.clone();
                            channel_decision = Some(r);
                            break;
                        }
                        Ok(None) => continue,
                        Err(e) => {
                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"tool": tool_name, "channel": ch_name, "error": format!("{}", e)})), "channel approval request failed");
                        }
                    }
                }
                let approval = match channel_decision {
                    Some(zeroclaw_api::channel::ChannelApprovalResponse::Approve) => {
                        ApprovalResponse::Yes
                    }
                    Some(zeroclaw_api::channel::ChannelApprovalResponse::AlwaysApprove) => {
                        ApprovalResponse::Always
                    }
                    Some(zeroclaw_api::channel::ChannelApprovalResponse::Deny) => {
                        ApprovalResponse::No
                    }
                    None => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"tool": tool_name})),
                            "no approval channel handled this request — denying. \
                             Configure a back-channel (ACP or WS) that implements \
                             request_approval to enable interactive approval."
                        );
                        ApprovalResponse::No
                    }
                };
                (approval, decision_channel_name)
            } else {
                (mgr.prompt_cli(&request), String::new())
            };

            mgr.record_decision(&tool_name, &tool_args, decision, &decision_channel);

            if decision == ApprovalResponse::No {
                return ToolExecutionResult {
                    name: tool_name,
                    output: "Denied by user.".to_string(),
                    success: false,
                    tool_call_id: call.tool_call_id.clone(),
                };
            }

            if matches!(decision, ApprovalResponse::Yes | ApprovalResponse::Always) {
                approval_requirement = ApprovalRequirement::Approved;
            }
        }
        super::set_runtime_approved_arg(
            &tool_name,
            &mut tool_args,
            approval_requirement == ApprovalRequirement::Approved,
        );

        // Serialize arguments once (after hooks may have mutated them) and
        // use the same string on the observer event so OTel exporters can
        // attach the actual JSON payload to the tool span.
        let args_json = tool_args.to_string();
        let tool_call_id = call.tool_call_id.clone();

        // First try to find tool in static registry, then in activated MCP tools.
        let (result, success) =
            if let Some(tool) = self.tools.iter().find(|t| t.name() == tool_name) {
                match tool.execute(tool_args.clone()).await {
                    Ok(r) => {
                        let (outcome_text, ok) = if r.success {
                            (r.output, true)
                        } else {
                            (format!("Error: {}", r.error.unwrap_or(r.output)), false)
                        };
                        self.observer.record_event(&ObserverEvent::ToolCall {
                            tool: tool_name.clone(),
                            tool_call_id: tool_call_id.clone(),
                            duration: start.elapsed(),
                            success: ok,
                            arguments: Some(args_json.clone()),
                            result: Some(super::loop_::scrub_credentials(&outcome_text)),
                        });
                        (outcome_text, ok)
                    }
                    Err(e) => {
                        let err_text = format!("Error executing {}: {e}", tool_name);
                        self.observer.record_event(&ObserverEvent::ToolCall {
                            tool: tool_name.clone(),
                            tool_call_id: tool_call_id.clone(),
                            duration: start.elapsed(),
                            success: false,
                            arguments: Some(args_json.clone()),
                            result: Some(super::loop_::scrub_credentials(&err_text)),
                        });
                        (err_text, false)
                    }
                }
            } else if let Some(activated_arc) = self.activated_tools.as_ref() {
                let activated_opt = activated_arc.lock().unwrap().get_resolved(&tool_name);
                if let Some(tool) = activated_opt {
                    match tool.execute(tool_args.clone()).await {
                        Ok(r) => {
                            let (outcome_text, ok) = if r.success {
                                (r.output, true)
                            } else {
                                (format!("Error: {}", r.error.unwrap_or(r.output)), false)
                            };
                            self.observer.record_event(&ObserverEvent::ToolCall {
                                tool: tool_name.clone(),
                                tool_call_id: tool_call_id.clone(),
                                duration: start.elapsed(),
                                success: ok,
                                arguments: Some(args_json.clone()),
                                result: Some(super::loop_::scrub_credentials(&outcome_text)),
                            });
                            (outcome_text, ok)
                        }
                        Err(e) => {
                            let err_text = format!("Error executing {}: {e}", tool_name);
                            self.observer.record_event(&ObserverEvent::ToolCall {
                                tool: tool_name.clone(),
                                tool_call_id: tool_call_id.clone(),
                                duration: start.elapsed(),
                                success: false,
                                arguments: Some(args_json.clone()),
                                result: Some(super::loop_::scrub_credentials(&err_text)),
                            });
                            (err_text, false)
                        }
                    }
                } else {
                    (format!("Unknown tool: {}", tool_name), false)
                }
            } else {
                (format!("Unknown tool: {}", tool_name), false)
            };

        let duration = start.elapsed();

        // ── Hook: after_tool_call (void) ─────────────────────────
        if let Some(ref hooks) = self.hook_runner {
            let tool_result_obj = crate::tools::ToolResult {
                success,
                output: result.clone(),
                error: None,
            };
            hooks
                .fire_after_tool_call(&tool_name, &tool_result_obj, duration)
                .await;
        }

        ToolExecutionResult {
            name: tool_name,
            output: result,
            success,
            tool_call_id: call.tool_call_id.clone(),
        }
    }

    async fn execute_tools(&self, calls: &[ParsedToolCall]) -> Vec<ToolExecutionResult> {
        let approval_required = self.approval_manager.as_deref().is_some_and(|mgr| {
            calls
                .iter()
                .any(|call| mgr.needs_approval(call.name.as_str()))
        });
        if !self.config.parallel_tools || approval_required {
            let mut results = Vec::with_capacity(calls.len());
            for call in calls {
                results.push(self.execute_tool_call(call).await);
            }
            return results;
        }

        let futs: Vec<_> = calls
            .iter()
            .map(|call| self.execute_tool_call(call))
            .collect();
        futures_util::future::join_all(futs).await
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
        if let Some(ref ac) = self.config.auto_classify {
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

    pub async fn turn(&mut self, user_message: &str) -> Result<String> {
        if self.history.is_empty() {
            let system_prompt = self.build_system_prompt()?;
            self.history
                .push(ConversationMessage::Chat(ChatMessage::system(
                    system_prompt,
                )));
        }

        let context = self
            .memory_loader
            .load_context(
                self.memory.as_ref(),
                user_message,
                self.memory_session_id.as_deref(),
            )
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

        for _ in 0..self.config.max_tool_iterations {
            let messages = self.tool_dispatcher.to_provider_messages(&self.history);
            let prepared_messages = self.prepare_provider_messages(&messages).await?;

            // Response cache: check before LLM call (only for deterministic, text-only prompts).
            // The key must include the whole provider-visible transcript, not just the last user
            // message, otherwise distinct conversations can collide when their final prompt matches.
            let cache_key =
                self.response_cache_key_for_messages(&prepared_messages, &effective_model);

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

            let response = match self
                .model_provider
                .chat(
                    ChatRequest {
                        messages: &prepared_messages,
                        tools: if self.should_send_tool_specs() {
                            Some(&self.tool_specs)
                        } else {
                            None
                        },
                        thinking: None,
                    },
                    &effective_model,
                    Some(self.temperature),
                )
                .await
            {
                Ok(resp) => resp,
                Err(err) => return Err(err),
            };

            let (text, calls) = self.parse_response_for_effective_tools(&response);
            if calls.is_empty() {
                let final_text = if text.is_empty() && !self.tool_specs.is_empty() {
                    response.text.unwrap_or_default()
                } else {
                    text
                };

                // Store in response cache (text-only, no tool calls)
                if let (Some(cache), Some(key)) = (&self.response_cache, &cache_key) {
                    let token_count = response
                        .usage
                        .as_ref()
                        .and_then(|u| u.output_tokens)
                        .unwrap_or(0);
                    #[allow(clippy::cast_possible_truncation)]
                    let _ = cache.put(key, &effective_model, &final_text, token_count as u32);
                }

                self.history
                    .push(ConversationMessage::Chat(ChatMessage::assistant(
                        final_text.clone(),
                    )));
                self.trim_history();

                return Ok(final_text);
            }

            if !text.is_empty() {
                print!("{text}");
                let _ = std::io::stdout().flush();
            }

            self.history.push(ConversationMessage::AssistantToolCalls {
                text: response.text.clone(),
                tool_calls: response.tool_calls.clone(),
                reasoning_content: response.reasoning_content.clone(),
            });

            let results = self.execute_tools(&calls).await;
            let formatted = self.tool_dispatcher.format_results(&results);
            self.history.push(formatted);
            self.trim_history();
        }

        anyhow::bail!(
            "Agent exceeded maximum tool iterations ({})",
            self.config.max_tool_iterations
        )
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
        let turn_started_at = std::time::Instant::now();
        let mut committed_response = String::new();

        // ── Turn loop ──────────────────────────────────────────────────
        for _ in 0..self.config.max_tool_iterations {
            // Early exit if the caller cancelled this turn (e.g. user abort)
            if cancel_token
                .as_ref()
                .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
            {
                self.append_streamed_assistant_message_to_history(
                    "[interrupted by user]".to_string(),
                    &mut new_msgs,
                    &mut committed_response,
                );
                return Err(StreamedTurnError {
                    error: crate::agent::loop_::ToolLoopCancelled.into(),
                    committed_response,
                    new_messages: new_msgs,
                });
            }

            for steering_message in Self::drain_steering_messages(&mut steering_rx) {
                self.append_streamed_user_message_to_history(&steering_message, &mut new_msgs)
                    .await;
            }

            let messages = self.tool_dispatcher.to_provider_messages(&self.history);
            let prepared_messages = match self.prepare_provider_messages(&messages).await {
                Ok(messages) => messages,
                Err(error) => {
                    return Err(StreamedTurnError {
                        error,
                        committed_response,
                        new_messages: new_msgs,
                    });
                }
            };

            // Response cache check (same as turn): include the full provider-visible transcript.
            let cache_key =
                self.response_cache_key_for_messages(&prepared_messages, &effective_model);

            if let (Some(cache), Some(key)) = (&self.response_cache, &cache_key) {
                if let Ok(Some(cached)) = cache.get(key) {
                    self.observer.record_event(&ObserverEvent::CacheHit {
                        cache_type: "response".into(),
                        tokens_saved: 0,
                    });
                    let cached_msg =
                        ConversationMessage::Chat(ChatMessage::assistant(cached.clone()));
                    new_msgs.push(cached_msg.clone());
                    self.history.push(cached_msg);
                    self.trim_history();
                    self.observer.record_event(&ObserverEvent::TurnComplete);
                    self.observer.record_event(&ObserverEvent::AgentEnd {
                        model_provider: self.model_provider_name.clone(),
                        model: effective_model.clone(),
                        duration: turn_started_at.elapsed(),
                        tokens_used: None,
                        cost_usd: None,
                    });
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

            // ── Streaming LLM call ────────────────────────────────────
            // Try streaming first; if the model_provider returns content we
            // forward deltas.  Otherwise fall back to non-streaming chat.
            use futures_util::StreamExt;

            let stream_opts = zeroclaw_providers::traits::StreamOptions::new(
                self.model_provider.supports_streaming(),
            );
            let mut stream = self.model_provider.stream_chat(
                zeroclaw_providers::ChatRequest {
                    messages: &prepared_messages,
                    tools: if self.should_send_tool_specs() {
                        Some(&self.tool_specs)
                    } else {
                        None
                    },
                    thinking: None,
                },
                &effective_model,
                Some(self.temperature),
                stream_opts,
            );

            let mut streamed_text = String::new();
            let mut streamed_reasoning = String::new();
            let mut streamed_tool_calls: Vec<zeroclaw_providers::traits::ToolCall> = Vec::new();
            let mut streamed_usage: Option<zeroclaw_providers::traits::TokenUsage> = None;
            let mut got_stream = false;
            let mut pre_executed_call_ids: HashMap<String, VecDeque<String>> = HashMap::new();
            let mut was_cancelled = false;

            // Consume the stream, checking for cancellation between chunks.
            // We use a manual loop with `tokio::select!` so that a cancel
            // signal interrupts even while waiting for the next SSE event
            // from the model_provider.
            loop {
                let next_item = stream.next();

                let item = if let Some(ref token) = cancel_token {
                    tokio::select! {
                        biased;
                        () = token.cancelled() => {
                            was_cancelled = true;
                            break;
                        }
                        item = next_item => item,
                    }
                } else {
                    next_item.await
                };

                let Some(item) = item else { break };
                match item {
                    Ok(event) => match event {
                        zeroclaw_providers::traits::StreamEvent::TextDelta(chunk) => {
                            if let Some(reasoning) = chunk.reasoning
                                && !reasoning.is_empty()
                            {
                                // Accumulate for signed-block round-trip on
                                // providers that carry signatures in this
                                // field (Anthropic native-thinking fallback).
                                streamed_reasoning.push_str(&reasoning);
                                let _ = event_tx
                                    .send(TurnEvent::Thinking { delta: reasoning })
                                    .await;
                            }
                            if !chunk.delta.is_empty() {
                                got_stream = true;
                                streamed_text.push_str(&chunk.delta);
                                let _ =
                                    event_tx.send(TurnEvent::Chunk { delta: chunk.delta }).await;
                            }
                        }
                        zeroclaw_providers::traits::StreamEvent::ToolCall(tc) => {
                            got_stream = true;
                            // ToolCall event is sent later (after parse_response) to
                            // avoid duplicates; just collect here.
                            streamed_tool_calls.push(tc);
                        }
                        zeroclaw_providers::traits::StreamEvent::PreExecutedToolCall {
                            name,
                            args,
                        } => {
                            let call_id = uuid::Uuid::new_v4().to_string();
                            pre_executed_call_ids
                                .entry(name.clone())
                                .or_default()
                                .push_back(call_id.clone());
                            let _ = event_tx
                                .send(TurnEvent::ToolCall {
                                    id: call_id,
                                    name,
                                    args: serde_json::from_str(&args).unwrap_or_default(),
                                })
                                .await;
                            // NOT pushed to streamed_tool_calls — already executed by proxy
                        }
                        zeroclaw_providers::traits::StreamEvent::PreExecutedToolResult {
                            name,
                            output,
                        } => {
                            let result_id = pre_executed_call_ids
                                .get_mut(&name)
                                .and_then(|ids| ids.pop_front())
                                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                            let _ = event_tx
                                .send(TurnEvent::ToolResult {
                                    id: result_id,
                                    name,
                                    output,
                                })
                                .await;
                        }
                        zeroclaw_providers::traits::StreamEvent::Usage(usage) => {
                            streamed_usage = Some(usage);
                        }
                        zeroclaw_providers::traits::StreamEvent::Final => break,
                    },
                    Err(error) => {
                        if got_stream || !committed_response.is_empty() {
                            if !streamed_text.is_empty() {
                                let partial = Self::marked_partial_response(
                                    &streamed_text,
                                    "[stream interrupted]",
                                );
                                self.append_streamed_assistant_message_to_history(
                                    partial,
                                    &mut new_msgs,
                                    &mut committed_response,
                                );
                            }
                            return Err(StreamedTurnError {
                                error: anyhow::Error::msg(error.to_string()),
                                committed_response,
                                new_messages: new_msgs,
                            });
                        }
                        break;
                    }
                }
            }
            // Drop the stream so we release the borrow on model_provider.
            drop(stream);

            // If cancelled during streaming, return partial content with
            // the interruption marker appended. The caller (ws.rs) will
            // persist this truncated message and send an abort frame.
            if was_cancelled {
                let partial =
                    Self::marked_partial_response(&streamed_text, "[interrupted by user]");
                self.append_streamed_assistant_message_to_history(
                    partial,
                    &mut new_msgs,
                    &mut committed_response,
                );
                return Err(StreamedTurnError {
                    error: crate::agent::loop_::ToolLoopCancelled.into(),
                    committed_response,
                    new_messages: new_msgs,
                });
            }

            // If streaming produced text, use it as the response and
            // check for tool calls via the dispatcher.
            let response = if got_stream {
                // Build a synthetic ChatResponse from streamed text.
                // `streamed_reasoning` carries signed thinking blocks from
                // providers that emit them via `StreamChunk.reasoning`
                // (Anthropic's native-thinking non-streaming fallback), so
                // the signature round-trip survives into conversation history.
                zeroclaw_providers::ChatResponse {
                    text: Some(streamed_text),
                    tool_calls: streamed_tool_calls,
                    usage: streamed_usage.clone(),
                    reasoning_content: if streamed_reasoning.is_empty() {
                        None
                    } else {
                        Some(streamed_reasoning)
                    },
                }
            } else {
                // Fall back to non-streaming chat, with cancellation guard
                let chat_fut = self.model_provider.chat(
                    ChatRequest {
                        messages: &prepared_messages,
                        tools: if self.should_send_tool_specs() {
                            Some(&self.tool_specs)
                        } else {
                            None
                        },
                        thinking: None,
                    },
                    &effective_model,
                    Some(self.temperature),
                );
                let chat_result = if let Some(ref token) = cancel_token {
                    tokio::select! {
                        biased;
                        () = token.cancelled() => {
                            self.append_streamed_assistant_message_to_history(
                                "[interrupted by user]".to_string(),
                                &mut new_msgs,
                                &mut committed_response,
                            );
                            return Err(StreamedTurnError {
                                error: crate::agent::loop_::ToolLoopCancelled.into(),
                                committed_response,
                                new_messages: new_msgs,
                            });
                        }
                        result = chat_fut => result,
                    }
                } else {
                    chat_fut.await
                };
                match chat_result {
                    Ok(resp) => resp,
                    Err(error) => {
                        return Err(StreamedTurnError {
                            error,
                            committed_response,
                            new_messages: new_msgs,
                        });
                    }
                }
            };

            // Forward per-call token usage so the WS gateway (and any other
            // consumer) can include aggregated usage in the final done frame
            // and write costs.jsonl. Absent when the provider does not surface
            // usage in streaming responses.
            if let Some(ref usage) = response.usage {
                let _ = event_tx
                    .send(TurnEvent::Usage {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cost_usd: None,
                    })
                    .await;
            }

            let (text, mut calls) = self.parse_response_for_effective_tools(&response);
            if calls.is_empty() {
                let final_text = if text.is_empty() && !self.tool_specs.is_empty() {
                    response.text.unwrap_or_default()
                } else {
                    text
                };

                let steering_messages = Self::drain_steering_messages(&mut steering_rx);
                if !steering_messages.is_empty() {
                    if !final_text.is_empty() {
                        let assistant_msg =
                            ConversationMessage::Chat(ChatMessage::assistant(final_text.clone()));
                        new_msgs.push(assistant_msg.clone());
                        self.history.push(assistant_msg);
                        committed_response.push_str(&final_text);
                        self.trim_history();
                    }

                    for steering_message in steering_messages {
                        self.append_streamed_user_message_to_history(
                            &steering_message,
                            &mut new_msgs,
                        )
                        .await;
                    }
                    continue;
                }

                // Store in response cache
                if let (Some(cache), Some(key)) = (&self.response_cache, &cache_key) {
                    let token_count = response
                        .usage
                        .as_ref()
                        .and_then(|u| u.output_tokens)
                        .unwrap_or(0);
                    #[allow(clippy::cast_possible_truncation)]
                    let _ = cache.put(key, &effective_model, &final_text, token_count as u32);
                }

                // If we didn't stream, send the full response as a single chunk
                if !got_stream && !final_text.is_empty() {
                    let _ = event_tx
                        .send(TurnEvent::Chunk {
                            delta: final_text.clone(),
                        })
                        .await;
                }

                new_msgs.push(ConversationMessage::Chat(ChatMessage::assistant(
                    final_text.clone(),
                )));
                self.history
                    .push(ConversationMessage::Chat(ChatMessage::assistant(
                        final_text.clone(),
                    )));
                committed_response.push_str(&final_text);
                self.trim_history();
                self.observer.record_event(&ObserverEvent::TurnComplete);
                self.observer.record_event(&ObserverEvent::AgentEnd {
                    model_provider: self.model_provider_name.clone(),
                    model: effective_model.clone(),
                    duration: turn_started_at.elapsed(),
                    tokens_used: None,
                    cost_usd: None,
                });
                return Ok(StreamedTurnSuccess {
                    response: committed_response,
                    new_messages: new_msgs,
                });
            }

            // Pre-assign stable IDs to tool calls that don't have one
            for call in &mut calls {
                if call.tool_call_id.is_none() {
                    call.tool_call_id = Some(uuid::Uuid::new_v4().to_string());
                }
            }

            // ── Tool calls ─────────────────────────────────────────────
            let tool_call_msg = ConversationMessage::AssistantToolCalls {
                text: response.text.clone(),
                tool_calls: response.tool_calls.clone(),
                reasoning_content: response.reasoning_content.clone(),
            };
            new_msgs.push(tool_call_msg.clone());
            self.history.push(tool_call_msg);

            // Notify about each tool call
            for call in &calls {
                let call_id = call.tool_call_id.as_ref().unwrap().clone();
                let _ = event_tx
                    .send(TurnEvent::ToolCall {
                        id: call_id,
                        name: call.name.clone(),
                        args: call.arguments.clone(),
                    })
                    .await;
            }

            let results = self.execute_tools(&calls).await;

            // Notify about each tool result
            for result in &results {
                let result_id = result.tool_call_id.as_ref().unwrap().clone();
                let _ = event_tx
                    .send(TurnEvent::ToolResult {
                        id: result_id,
                        name: result.name.clone(),
                        output: result.output.clone(),
                    })
                    .await;
            }

            let formatted = self.tool_dispatcher.format_results(&results);
            new_msgs.push(formatted.clone());
            self.history.push(formatted);
            self.trim_history();
        }

        Err(StreamedTurnError {
            error: anyhow::Error::msg(format!(
                "Agent exceeded maximum tool iterations ({})",
                self.config.max_tool_iterations
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

        let listen_handle = tokio::spawn(async move {
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
    temperature: f64,
) -> Result<()> {
    let start = Instant::now();

    let mut effective_config = config;
    if let Some(p) = provider_override {
        // When a model_provider override is specified, ensure that model_provider type exists
        // in models and is set as the first (and only) entry for routing purposes.
        if let Some((type_key, alias_key)) = p.split_once('.') {
            effective_config
                .providers
                .models
                .ensure(type_key, alias_key);
        } else {
            effective_config.providers.models.ensure(&p, "default");
        }
    }
    if let Some(entry) = effective_config.first_model_provider_mut() {
        if let Some(m) = model_override {
            entry.model = Some(m);
        }
        entry.temperature = Some(temperature);
    }

    let mut agent = Agent::from_config(&effective_config, agent_alias).await?;

    let provider_name = effective_config
        .first_model_provider_alias()
        .unwrap_or_else(|| "openrouter.default".to_string());
    // `Agent::from_config` above has already errored if no model could be resolved,
    // so this telemetry line should always find one. We keep `resolve_default_model`
    // as a cheap secondary lookup and emit "<unresolved>" only if nothing matches —
    // never silently substitute a hardcoded vendor model.
    let model_name = effective_config
        .first_model_provider()
        .and_then(|e| e.model.as_deref())
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(ToString::to_string)
        .or_else(|| effective_config.resolve_default_model())
        .unwrap_or_else(|| "<unresolved>".to_string());

    agent.observer.record_event(&ObserverEvent::AgentStart {
        model_provider: provider_name.clone(),
        model: model_name.clone(),
    });

    if let Some(msg) = message {
        let response = agent.run_single(&msg).await?;
        println!("{response}");
    } else {
        agent.run_interactive().await?;
    }

    agent.observer.record_event(&ObserverEvent::AgentEnd {
        model_provider: provider_name,
        model: model_name,
        duration: start.elapsed(),
        tokens_used: None,
        cost_usd: None,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    zeroclaw_api::mock_tool_attribution!(
        CountingTool,
        NamedMockTool,
        MockTool,
        CapturingApprovalArgTool,
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
        fail_after_delta_on_call: Option<usize>,
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
            if self.fail_on_call == Some(call) {
                anyhow::bail!("synthetic provider failure on call {call}");
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

    struct CapturingApprovalArgTool {
        name: &'static str,
        output: &'static str,
        calls: Arc<AtomicUsize>,
        last_args: Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    }

    #[async_trait]
    impl Tool for CapturingApprovalArgTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            self.name
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(&self, args: serde_json::Value) -> Result<crate::tools::ToolResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_args.lock().unwrap() = Some(args);
            Ok(crate::tools::ToolResult {
                success: true,
                output: self.output.into(),
                error: None,
            })
        }
    }

    struct ApprovalChannel {
        response: zeroclaw_api::channel::ChannelApprovalResponse,
        requests: Arc<AtomicUsize>,
    }

    impl ::zeroclaw_api::attribution::Attributable for ApprovalChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::AcpChannel,
            )
        }
        fn alias(&self) -> &str {
            "test"
        }
    }

    #[async_trait]
    impl zeroclaw_api::channel::Channel for ApprovalChannel {
        fn name(&self) -> &str {
            "acp"
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

        async fn request_approval(
            &self,
            _recipient: &str,
            _request: &zeroclaw_api::channel::ChannelApprovalRequest,
        ) -> anyhow::Result<Option<zeroclaw_api::channel::ChannelApprovalResponse>> {
            self.requests.fetch_add(1, Ordering::SeqCst);
            Ok(Some(self.response))
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
            strict_tool_parsing: true,
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
    async fn direct_agent_tool_execution_requests_acp_approval() {
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
        let tool_calls = Arc::new(AtomicUsize::new(0));
        let approval_requests = Arc::new(AtomicUsize::new(0));
        let approval_cfg = zeroclaw_config::schema::RiskProfileConfig {
            always_ask: vec!["echo".into()],
            ..zeroclaw_config::schema::RiskProfileConfig::default()
        };
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(vec![Box::new(CountingTool {
                calls: Arc::clone(&tool_calls),
            })])
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .approval_manager(Some(Arc::new(ApprovalManager::for_non_interactive(
                &approval_cfg,
            ))))
            .build()
            .expect("agent builder should succeed with valid config");

        let handle: tools::ChannelMapHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        agent.channel_handles.ask_user = Some(Arc::clone(&handle));
        let channel: Arc<dyn zeroclaw_api::channel::Channel> = Arc::new(ApprovalChannel {
            response: zeroclaw_api::channel::ChannelApprovalResponse::Approve,
            requests: Arc::clone(&approval_requests),
        });
        handle.write().insert("acp".to_string(), channel);

        let result = agent
            .execute_tool_call(&ParsedToolCall {
                name: "echo".into(),
                arguments: serde_json::json!({"message": "hi"}),
                tool_call_id: Some("tc1".into()),
            })
            .await;

        assert!(result.success);
        assert_eq!(result.output, "tool-out");
        assert_eq!(approval_requests.load(Ordering::SeqCst), 1);
        assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn direct_agent_tool_execution_denies_when_acp_rejects() {
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
        let tool_calls = Arc::new(AtomicUsize::new(0));
        let approval_requests = Arc::new(AtomicUsize::new(0));
        let approval_cfg = zeroclaw_config::schema::RiskProfileConfig {
            always_ask: vec!["echo".into()],
            ..zeroclaw_config::schema::RiskProfileConfig::default()
        };
        let mut agent = Agent::builder()
            .model_provider(model_provider)
            .tools(vec![Box::new(CountingTool {
                calls: Arc::clone(&tool_calls),
            })])
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .approval_manager(Some(Arc::new(ApprovalManager::for_non_interactive(
                &approval_cfg,
            ))))
            .build()
            .expect("agent builder should succeed with valid config");

        let handle: tools::ChannelMapHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        agent.channel_handles.ask_user = Some(Arc::clone(&handle));
        let channel: Arc<dyn zeroclaw_api::channel::Channel> = Arc::new(ApprovalChannel {
            response: zeroclaw_api::channel::ChannelApprovalResponse::Deny,
            requests: Arc::clone(&approval_requests),
        });
        handle.write().insert("acp".to_string(), channel);

        let result = agent
            .execute_tool_call(&ParsedToolCall {
                name: "echo".into(),
                arguments: serde_json::json!({"message": "hi"}),
                tool_call_id: Some("tc1".into()),
            })
            .await;

        assert!(!result.success);
        assert_eq!(result.output, "Denied by user.");
        assert_eq!(approval_requests.load(Ordering::SeqCst), 1);
        assert_eq!(tool_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn direct_agent_shell_does_not_trust_model_supplied_approved_arg() {
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
        let tool_calls = Arc::new(AtomicUsize::new(0));
        let approval_requests = Arc::new(AtomicUsize::new(0));
        let captured_args = Arc::new(std::sync::Mutex::new(None));
        let approval_cfg = zeroclaw_config::schema::RiskProfileConfig::default();
        let mut agent = Agent::builder()
            .model_provider(provider)
            .tools(vec![Box::new(CapturingApprovalArgTool {
                name: "shell",
                output: "shell-out",
                calls: Arc::clone(&tool_calls),
                last_args: Arc::clone(&captured_args),
            })])
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .approval_manager(Some(Arc::new(
                ApprovalManager::for_non_interactive_backchannel(&approval_cfg),
            )))
            .build()
            .expect("agent builder should succeed with valid config");

        let handle: tools::ChannelMapHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        agent.channel_handles.ask_user = Some(Arc::clone(&handle));
        let channel: Arc<dyn zeroclaw_api::channel::Channel> = Arc::new(ApprovalChannel {
            response: zeroclaw_api::channel::ChannelApprovalResponse::Deny,
            requests: Arc::clone(&approval_requests),
        });
        handle.write().insert("acp".to_string(), channel);

        let result = agent
            .execute_tool_call(&ParsedToolCall {
                name: "shell".into(),
                arguments: serde_json::json!({
                    "command": "touch should-not-run",
                    "approved": true
                }),
                tool_call_id: Some("tc1".into()),
            })
            .await;

        assert!(!result.success);
        assert_eq!(result.output, "Denied by user.");
        assert_eq!(approval_requests.load(Ordering::SeqCst), 1);
        assert_eq!(tool_calls.load(Ordering::SeqCst), 0);
        assert!(captured_args.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn direct_agent_shell_marks_args_approved_after_backchannel_approval() {
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
        let tool_calls = Arc::new(AtomicUsize::new(0));
        let approval_requests = Arc::new(AtomicUsize::new(0));
        let captured_args = Arc::new(std::sync::Mutex::new(None));
        let approval_cfg = zeroclaw_config::schema::RiskProfileConfig::default();
        let mut agent = Agent::builder()
            .model_provider(provider)
            .tools(vec![Box::new(CapturingApprovalArgTool {
                name: "shell",
                output: "shell-out",
                calls: Arc::clone(&tool_calls),
                last_args: Arc::clone(&captured_args),
            })])
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .approval_manager(Some(Arc::new(
                ApprovalManager::for_non_interactive_backchannel(&approval_cfg),
            )))
            .build()
            .expect("agent builder should succeed with valid config");

        let handle: tools::ChannelMapHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        agent.channel_handles.ask_user = Some(Arc::clone(&handle));
        let channel: Arc<dyn zeroclaw_api::channel::Channel> = Arc::new(ApprovalChannel {
            response: zeroclaw_api::channel::ChannelApprovalResponse::Approve,
            requests: Arc::clone(&approval_requests),
        });
        handle.write().insert("acp".to_string(), channel);

        let result = agent
            .execute_tool_call(&ParsedToolCall {
                name: "shell".into(),
                arguments: serde_json::json!({
                    "command": "touch should-run-after-human-approval",
                    "approved": false
                }),
                tool_call_id: Some("tc1".into()),
            })
            .await;

        assert!(result.success);
        assert_eq!(result.output, "shell-out");
        assert_eq!(approval_requests.load(Ordering::SeqCst), 1);
        assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
        let args = captured_args
            .lock()
            .unwrap()
            .clone()
            .expect("shell tool should capture executed args");
        assert_eq!(args["approved"], true);
    }

    #[tokio::test]
    async fn direct_agent_shell_keeps_runtime_approval_from_always_allowlist() {
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
        let tool_calls = Arc::new(AtomicUsize::new(0));
        let approval_requests = Arc::new(AtomicUsize::new(0));
        let captured_args = Arc::new(std::sync::Mutex::new(None));
        let approval_cfg = zeroclaw_config::schema::RiskProfileConfig::default();
        let mut agent = Agent::builder()
            .model_provider(provider)
            .tools(vec![Box::new(CapturingApprovalArgTool {
                name: "shell",
                output: "shell-out",
                calls: Arc::clone(&tool_calls),
                last_args: Arc::clone(&captured_args),
            })])
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .approval_manager(Some(Arc::new(
                ApprovalManager::for_non_interactive_backchannel(&approval_cfg),
            )))
            .build()
            .expect("agent builder should succeed with valid config");

        let handle: tools::ChannelMapHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        agent.channel_handles.ask_user = Some(Arc::clone(&handle));
        let channel: Arc<dyn zeroclaw_api::channel::Channel> = Arc::new(ApprovalChannel {
            response: zeroclaw_api::channel::ChannelApprovalResponse::AlwaysApprove,
            requests: Arc::clone(&approval_requests),
        });
        handle.write().insert("acp".to_string(), channel);

        let first_result = agent
            .execute_tool_call(&ParsedToolCall {
                name: "shell".into(),
                arguments: serde_json::json!({
                    "command": "touch should-run-after-always-approval",
                    "approved": false
                }),
                tool_call_id: Some("tc1".into()),
            })
            .await;
        let second_result = agent
            .execute_tool_call(&ParsedToolCall {
                name: "shell".into(),
                arguments: serde_json::json!({
                    "command": "touch should-run-from-allowlist",
                    "approved": false
                }),
                tool_call_id: Some("tc2".into()),
            })
            .await;

        assert!(first_result.success);
        assert!(second_result.success);
        assert_eq!(approval_requests.load(Ordering::SeqCst), 1);
        assert_eq!(tool_calls.load(Ordering::SeqCst), 2);
        let args = captured_args
            .lock()
            .unwrap()
            .clone()
            .expect("shell tool should capture executed args");
        assert_eq!(args["approved"], true);
    }

    #[tokio::test]
    async fn direct_agent_cron_add_does_not_trust_model_supplied_approved_arg() {
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
        let tool_calls = Arc::new(AtomicUsize::new(0));
        let captured_args = Arc::new(std::sync::Mutex::new(None));
        let agent = Agent::builder()
            .model_provider(provider)
            .tools(vec![Box::new(CapturingApprovalArgTool {
                name: "cron_add",
                output: "cron-out",
                calls: Arc::clone(&tool_calls),
                last_args: Arc::clone(&captured_args),
            })])
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let result = agent
            .execute_tool_call(&ParsedToolCall {
                name: "cron_add".into(),
                arguments: serde_json::json!({
                    "command": "echo should-not-be-model-approved",
                    "approved": true
                }),
                tool_call_id: Some("tc1".into()),
            })
            .await;

        assert!(result.success);
        assert_eq!(result.output, "cron-out");
        assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
        let args = captured_args
            .lock()
            .unwrap()
            .clone()
            .expect("cron_add tool should capture executed args");
        assert_eq!(args["approved"], false);
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
        let server_handle = tokio::spawn(async move {
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
        let provider_alias = config
            .first_model_provider_type()
            .expect("model_provider configured above")
            .to_string();
        let agent_cfg = zeroclaw_config::schema::AliasedAgentConfig {
            model_provider: format!("{provider_alias}.default").into(),
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

        assert_eq!(agent.tool_specs.len(), 1);
        assert_eq!(agent.tool_specs[0].name, "echo");
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
            agent.tool_specs.is_empty(),
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
    fn session_cwd_with_runtime_profile_keeps_agent_workspace_readable() {
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, RiskProfileConfig, RuntimeProfileConfig,
        };

        let tmp = tempfile::TempDir::new().expect("temp dir");
        let session = tmp.path().join("session");
        std::fs::create_dir_all(&session).expect("session dir");

        let mut config = Config {
            data_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        config
            .risk_profiles
            .insert("test-risk".to_string(), RiskProfileConfig::default());
        config.runtime_profiles.insert(
            "test-runtime".to_string(),
            RuntimeProfileConfig {
                max_actions_per_hour: 7,
                ..RuntimeProfileConfig::default()
            },
        );
        config.agents.insert(
            "test-agent".to_string(),
            AliasedAgentConfig {
                risk_profile: "test-risk".to_string(),
                runtime_profile: "test-runtime".to_string(),
                ..AliasedAgentConfig::default()
            },
        );

        let agent_workspace = config.agent_workspace_dir("test-agent");
        std::fs::create_dir_all(&agent_workspace).expect("agent workspace dir");
        let skill_file = agent_workspace.join("SKILL.md");
        std::fs::write(&skill_file, "body").expect("skill file");
        let skill_resolved = std::fs::canonicalize(&skill_file).unwrap_or(skill_file);

        let mut policy =
            SecurityPolicy::for_agent(&config, "test-agent").expect("resolvable policy");
        policy.workspace_dir = session.clone();
        policy.allowed_roots.push(agent_workspace);

        assert_eq!(policy.workspace_dir, session);
        assert_eq!(policy.max_actions_per_hour, 7);
        assert!(
            policy.is_resolved_path_allowed(&skill_resolved),
            "agent workspace must stay readable when a session cwd overrides the policy workspace"
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
            max_history_messages: 4,
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
            .temperature(0.0)
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
            .temperature(0.0)
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
            fail_after_delta_on_call: None,
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
        let handle = tokio::spawn(async move {
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
            fail_after_delta_on_call: None,
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
        let handle = tokio::spawn(async move {
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
    async fn turn_streamed_error_after_delta_returns_visible_partial_output() {
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
            fail_after_delta_on_call: Some(1),
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
        let handle = tokio::spawn(async move {
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
            .expect_err("provider stream failure should be returned");
        assert!(
            err.error
                .to_string()
                .contains("synthetic provider failure after delta"),
            "unexpected error: {}",
            err.error
        );
        assert!(
            err.committed_response.contains("draft"),
            "visible streamed text should be committed after a provider stream error"
        );
        assert!(
            err.committed_response.contains("[stream interrupted]"),
            "persisted partial text should mark that the stream did not complete"
        );
        assert!(
            err.new_messages.iter().any(|msg| {
                matches!(msg, ConversationMessage::Chat(message) if message.role == "assistant" && message.content.contains("draft"))
            }),
            "new messages should carry the visible assistant partial for gateway persistence"
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
            fail_after_delta_on_call: None,
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
        assert_eq!(err.committed_response, "[interrupted by user]");
        assert!(
            err.new_messages.iter().any(|msg| {
                matches!(msg, ConversationMessage::Chat(message) if message.role == "assistant" && message.content == "[interrupted by user]")
            }),
            "cancelled turn should include an assistant interruption marker for persistence"
        );
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
            max_history_messages: 4,
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
}
