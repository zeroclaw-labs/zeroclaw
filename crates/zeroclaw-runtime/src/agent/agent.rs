use crate::agent::dispatcher::{
    NativeToolDispatcher, ParsedToolCall, ToolDispatcher, ToolExecutionResult, XmlToolDispatcher,
};
use crate::agent::eval::AutoClassifyExt;
use crate::agent::memory_loader::{DefaultMemoryLoader, MemoryLoader};
use crate::agent::prompt::{PromptContext, SystemPromptBuilder};
use crate::observability::{self, Observer, ObserverEvent};
use crate::platform;
use crate::security::SecurityPolicy;
use crate::tools::{self, Tool, ToolSpec};
use anyhow::Result;
use chrono::{Datelike, Timelike};
use std::collections::{HashMap, VecDeque};
use std::io::Write as IoWrite;
use std::sync::Arc;
use std::time::Instant;
use zeroclaw_config::schema::Config;
use zeroclaw_memory::{self, Memory, MemoryCategory};
use zeroclaw_providers::{self, ChatMessage, ChatRequest, ConversationMessage, Provider};

// Re-export TurnEvent from zeroclaw-types for backwards compatibility.
pub use zeroclaw_api::agent::TurnEvent;

pub struct Agent {
    provider: Box<dyn Provider>,
    tools: Vec<Box<dyn Tool>>,
    tool_specs: Vec<ToolSpec>,
    memory: Arc<dyn Memory>,
    observer: Arc<dyn Observer>,
    prompt_builder: SystemPromptBuilder,
    tool_dispatcher: Box<dyn ToolDispatcher>,
    memory_loader: Box<dyn MemoryLoader>,
    config: zeroclaw_config::schema::AgentConfig,
    model_name: String,
    temperature: f64,
    workspace_dir: std::path::PathBuf,
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
}

pub struct AgentBuilder {
    provider: Option<Box<dyn Provider>>,
    tools: Option<Vec<Box<dyn Tool>>>,
    memory: Option<Arc<dyn Memory>>,
    observer: Option<Arc<dyn Observer>>,
    prompt_builder: Option<SystemPromptBuilder>,
    tool_dispatcher: Option<Box<dyn ToolDispatcher>>,
    memory_loader: Option<Box<dyn MemoryLoader>>,
    config: Option<zeroclaw_config::schema::AgentConfig>,
    model_name: Option<String>,
    temperature: Option<f64>,
    workspace_dir: Option<std::path::PathBuf>,
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
}

impl Default for AgentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentBuilder {
    pub fn new() -> Self {
        Self {
            provider: None,
            tools: None,
            memory: None,
            observer: None,
            prompt_builder: None,
            tool_dispatcher: None,
            memory_loader: None,
            config: None,
            model_name: None,
            temperature: None,
            workspace_dir: None,
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
        }
    }

    pub fn provider(mut self, provider: Box<dyn Provider>) -> Self {
        self.provider = Some(provider);
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

    pub fn config(mut self, config: zeroclaw_config::schema::AgentConfig) -> Self {
        self.config = Some(config);
        self
    }

    pub fn model_name(mut self, model_name: String) -> Self {
        self.model_name = Some(model_name);
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

    pub fn build(self) -> Result<Agent> {
        let mut tools = self
            .tools
            .ok_or_else(|| anyhow::anyhow!("tools are required"))?;
        let allowed = self.allowed_tools.clone();
        if let Some(ref allow_list) = allowed {
            tools.retain(|t| allow_list.iter().any(|name| name == t.name()));
        }
        let tool_specs = tools.iter().map(|tool| tool.spec()).collect();

        Ok(Agent {
            provider: self
                .provider
                .ok_or_else(|| anyhow::anyhow!("provider is required"))?,
            tools,
            tool_specs,
            memory: self
                .memory
                .ok_or_else(|| anyhow::anyhow!("memory is required"))?,
            observer: self
                .observer
                .ok_or_else(|| anyhow::anyhow!("observer is required"))?,
            prompt_builder: self
                .prompt_builder
                .unwrap_or_else(SystemPromptBuilder::with_defaults),
            tool_dispatcher: self
                .tool_dispatcher
                .ok_or_else(|| anyhow::anyhow!("tool_dispatcher is required"))?,
            memory_loader: self
                .memory_loader
                .unwrap_or_else(|| Box::new(DefaultMemoryLoader::default())),
            config: self.config.unwrap_or_default(),
            // No silent vendor-default model. Callers that construct `Agent` via the
            // builder must set `model_name` explicitly (via `.model_name(...)` or via
            // `Agent::from_config`, which resolves from `[providers]`). The sentinel
            // keeps the field non-empty so accidental dispatch surfaces a clear 4xx
            // rather than misrouting to a real vendor model.
            model_name: self.model_name.unwrap_or_else(|| "<unconfigured>".into()),
            temperature: self.temperature.unwrap_or(0.7),
            workspace_dir: self
                .workspace_dir
                .unwrap_or_else(|| std::path::PathBuf::from(".")),
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
        })
    }
}

impl Agent {
    pub fn builder() -> AgentBuilder {
        AgentBuilder::new()
    }

    pub fn history(&self) -> &[ConversationMessage] {
        &self.history
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
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

    pub async fn from_config(config: &Config) -> Result<Self> {
        let observer: Arc<dyn Observer> =
            Arc::from(observability::create_observer(&config.observability));
        let runtime: Arc<dyn platform::RuntimeAdapter> =
            Arc::from(platform::create_runtime(&config.runtime)?);
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));

        let fallback_provider_ag = config.providers.fallback_provider();
        let memory: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory_with_storage_and_routes(
                &config.memory,
                &config.providers.embedding_routes,
                Some(&config.storage.provider.config),
                &config.workspace_dir,
                fallback_provider_ag.and_then(|e| e.api_key.as_deref()),
            )?);

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
            _reaction_handle,
            _channel_map_handle,
            _ask_user_handle,
            _escalate_handle,
        ) = tools::all_tools_with_runtime(
            Arc::new(config.clone()),
            &security,
            runtime,
            memory.clone(),
            composio_key,
            composio_entity_id,
            &config.browser,
            &config.http_request,
            &config.web_fetch,
            &config.workspace_dir,
            &config.agents,
            fallback_provider_ag.and_then(|e| e.api_key.as_deref()),
            config,
            None,
        );

        // ── Wire MCP tools (non-fatal) ─────────────────────────────
        // Replicates the same MCP initialization logic used in the CLI
        // and webhook paths (loop_.rs) so that the WebSocket/daemon UI
        // path also has access to MCP tools.
        let mut activated_tools: Option<Arc<std::sync::Mutex<tools::ActivatedToolSet>>> = None;
        if config.mcp.enabled && !config.mcp.servers.is_empty() {
            tracing::info!(
                "Initializing MCP client — {} server(s) configured",
                config.mcp.servers.len()
            );
            match tools::McpRegistry::connect_all(&config.mcp.servers).await {
                Ok(registry) => {
                    let registry = std::sync::Arc::new(registry);
                    if config.mcp.deferred_loading {
                        let deferred_set = tools::DeferredMcpToolSet::from_registry(
                            std::sync::Arc::clone(&registry),
                        )
                        .await;
                        tracing::info!(
                            "MCP deferred: {} tool stub(s) from {} server(s)",
                            deferred_set.len(),
                            registry.server_count()
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
                        tracing::info!(
                            "MCP: {} tool(s) registered from {} server(s)",
                            registered,
                            registry.server_count()
                        );
                    }
                }
                Err(e) => {
                    tracing::error!("MCP registry failed to initialize: {e:#}");
                }
            }
        }

        let provider_name = config.providers.fallback_type().unwrap_or("openrouter");

        let model_name = match fallback_provider_ag
            .and_then(|e| e.model.as_deref())
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            Some(m) => m.to_string(),
            None => match config.providers.resolve_default_model() {
                Some(m) => {
                    tracing::warn!(
                        provider = provider_name,
                        model = %m,
                        "fallback provider has no `model` set; using first configured \
                         providers.models entry as default. Set [providers.models.{provider_name}] \
                         model = \"...\" to silence this warning.",
                    );
                    m
                }
                None => {
                    anyhow::bail!(
                        "no model configured: providers.fallback = {:?} resolves with no model, \
                         and no [[providers.models.*]] entry has a `model` field set. \
                         Configure at least one [providers.models.<name>] model = \"...\" \
                         or define a [[model_routes]] hint.",
                        config.providers.fallback,
                    )
                }
            },
        };

        let provider_runtime_options =
            zeroclaw_providers::provider_runtime_options_from_config(config);

        let provider: Box<dyn Provider> = zeroclaw_providers::create_routed_provider_with_options(
            provider_name,
            fallback_provider_ag.and_then(|e| e.api_key.as_deref()),
            fallback_provider_ag.and_then(|e| e.base_url.as_deref()),
            &config.reliability,
            &config.providers.model_routes,
            &model_name,
            &provider_runtime_options,
        )?;

        let dispatcher_choice = config.agent.tool_dispatcher.as_str();
        let tool_dispatcher: Box<dyn ToolDispatcher> = match dispatcher_choice {
            "native" => Box::new(NativeToolDispatcher),
            "xml" => Box::new(XmlToolDispatcher),
            _ if provider.supports_native_tools() => Box::new(NativeToolDispatcher),
            _ => Box::new(XmlToolDispatcher),
        };

        let route_model_by_hint: HashMap<String, String> = config
            .providers
            .model_routes
            .iter()
            .map(|route| (route.hint.clone(), route.model.clone()))
            .collect();
        let available_hints: Vec<String> = route_model_by_hint.keys().cloned().collect();

        let response_cache = if config.memory.response_cache_enabled {
            zeroclaw_memory::response_cache::ResponseCache::with_hot_cache(
                &config.workspace_dir,
                config.memory.response_cache_ttl_minutes,
                config.memory.response_cache_max_entries,
                config.memory.response_cache_hot_entries,
            )
            .ok()
            .map(Arc::new)
        } else {
            None
        };

        // Filter out excluded tools (non_cli_excluded_tools). The channel
        // orchestrator applies this, but Agent::from_config (used by ws.rs)
        // doesn't go through that path.
        let excluded = &config.autonomy.non_cli_excluded_tools;
        if !excluded.is_empty() {
            tools.retain(|t| !excluded.iter().any(|ex| ex == t.name()));
        }

        // Load skills and register them as callable tools so WebSocket/daemon
        // sessions can execute them (not just describe them in the prompt).
        let skills = crate::skills::load_skills_with_config(&config.workspace_dir, config);
        tools::register_skill_tools(&mut tools, &skills, security.clone());

        Agent::builder()
            .provider(provider)
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
            .config(config.agent.clone())
            .model_name(model_name)
            .temperature(
                fallback_provider_ag
                    .and_then(|e| e.temperature)
                    .unwrap_or(0.7),
            )
            .workspace_dir(config.workspace_dir.clone())
            .classification_config(config.query_classification.clone())
            .available_hints(available_hints)
            .route_model_by_hint(route_model_by_hint)
            .identity_config(config.identity.clone())
            .skills(skills)
            .skills_prompt_mode(config.skills.prompt_injection_mode)
            .auto_save(config.memory.auto_save)
            .security_summary(Some(security.prompt_summary()))
            .autonomy_level(config.autonomy.level)
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
            .build()
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
            // has no matching tool_use, causing providers (e.g. Anthropic) to
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
        let instructions = self.tool_dispatcher.prompt_instructions(&self.tools);
        let ctx = PromptContext {
            workspace_dir: &self.workspace_dir,
            model_name: &self.model_name,
            tools: &self.tools,
            skills: &self.skills,
            skills_prompt_mode: self.skills_prompt_mode,
            identity_config: Some(&self.identity_config),
            dispatcher_instructions: &instructions,
            security_summary: self.security_summary.clone(),
            autonomy_level: self.autonomy_level,
        };
        self.prompt_builder.build(&ctx)
    }

    async fn execute_tool_call(&self, call: &ParsedToolCall) -> ToolExecutionResult {
        let start = Instant::now();

        // ── Hook: before_tool_call (modifying) ──────────────────
        // Mirrors the hook pipeline in run_tool_call_loop (loop_.rs) so that
        // library-integrated runs honour the same hook chain.  See #5462.
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
                    tracing::info!(
                        tool = %call.name, %reason,
                        "tool call cancelled by hook"
                    );
                    return ToolExecutionResult {
                        name: call.name.clone(),
                        output: format!("Cancelled by hook: {reason}"),
                        success: false,
                        tool_call_id: call.tool_call_id.clone(),
                    };
                }
            }
        }

        // First try to find tool in static registry, then in activated MCP tools.
        let (result, success) =
            if let Some(tool) = self.tools.iter().find(|t| t.name() == tool_name) {
                match tool.execute(tool_args.clone()).await {
                    Ok(r) => {
                        self.observer.record_event(&ObserverEvent::ToolCall {
                            tool: tool_name.clone(),
                            duration: start.elapsed(),
                            success: r.success,
                        });
                        if r.success {
                            (r.output, true)
                        } else {
                            (format!("Error: {}", r.error.unwrap_or(r.output)), false)
                        }
                    }
                    Err(e) => {
                        self.observer.record_event(&ObserverEvent::ToolCall {
                            tool: tool_name.clone(),
                            duration: start.elapsed(),
                            success: false,
                        });
                        (format!("Error executing {}: {e}", tool_name), false)
                    }
                }
            } else if let Some(activated_arc) = self.activated_tools.as_ref() {
                let activated_opt = activated_arc.lock().unwrap().get_resolved(&tool_name);
                if let Some(tool) = activated_opt {
                    match tool.execute(tool_args.clone()).await {
                        Ok(r) => {
                            self.observer.record_event(&ObserverEvent::ToolCall {
                                tool: tool_name.clone(),
                                duration: start.elapsed(),
                                success: r.success,
                            });
                            if r.success {
                                (r.output, true)
                            } else {
                                (format!("Error: {}", r.error.unwrap_or(r.output)), false)
                            }
                        }
                        Err(e) => {
                            self.observer.record_event(&ObserverEvent::ToolCall {
                                tool: tool_name.clone(),
                                duration: start.elapsed(),
                                success: false,
                            });
                            (format!("Error executing {}: {e}", tool_name), false)
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
        if !self.config.parallel_tools {
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
            tracing::info!(
                target: "query_classification",
                hint = decision.hint.as_str(),
                model = resolved_model,
                rule_priority = decision.priority,
                message_length = user_message.len(),
                "Classified message route"
            );
            return format!("hint:{}", decision.hint);
        }

        // Fallback: auto-classify by complexity when no rule matched.
        if let Some(ref ac) = self.config.auto_classify {
            let tier = super::eval::estimate_complexity(user_message);
            if let Some(hint) = ac.hint_for(tier)
                && self.available_hints.contains(&hint.to_string())
            {
                tracing::info!(
                    target: "query_classification",
                    hint = hint,
                    complexity = ?tier,
                    message_length = user_message.len(),
                    "Auto-classified by complexity"
                );
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

            // Response cache: check before LLM call (only for deterministic, text-only prompts)
            let cache_key = if self.temperature == 0.0 {
                self.response_cache.as_ref().map(|_| {
                    let last_user = messages
                        .iter()
                        .rfind(|m| m.role == "user")
                        .map(|m| m.content.as_str())
                        .unwrap_or("");
                    let system = messages
                        .iter()
                        .find(|m| m.role == "system")
                        .map(|m| m.content.as_str());
                    zeroclaw_memory::response_cache::ResponseCache::cache_key(
                        &effective_model,
                        system,
                        last_user,
                    )
                })
            } else {
                None
            };

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
                .provider
                .chat(
                    ChatRequest {
                        messages: &messages,
                        tools: if self.tool_dispatcher.should_send_tool_specs() {
                            Some(&self.tool_specs)
                        } else {
                            None
                        },
                    },
                    &effective_model,
                    Some(self.temperature),
                )
                .await
            {
                Ok(resp) => resp,
                Err(err) => return Err(err),
            };

            let (text, calls) = self.tool_dispatcher.parse_response(&response);
            if calls.is_empty() {
                let final_text = if text.is_empty() {
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
    /// The returned `String` is the final, complete assistant response — the
    /// same value that `turn` would return.
    pub async fn turn_streamed(
        &mut self,
        user_message: &str,
        event_tx: tokio::sync::mpsc::Sender<TurnEvent>,
        cancel_token: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<String> {
        // ── Preamble (identical to turn) ───────────────────────────────
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

        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
        let enriched = if context.is_empty() {
            format!("[{now}] {user_message}")
        } else {
            format!("{context}[{now}] {user_message}")
        };

        self.history
            .push(ConversationMessage::Chat(ChatMessage::user(enriched)));

        let effective_model = self.classify_model(user_message);

        // ── Turn loop ──────────────────────────────────────────────────
        for _ in 0..self.config.max_tool_iterations {
            // Early exit if the caller cancelled this turn (e.g. user abort)
            if cancel_token
                .as_ref()
                .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
            {
                return Err(crate::agent::loop_::ToolLoopCancelled.into());
            }

            let messages = self.tool_dispatcher.to_provider_messages(&self.history);

            // Response cache check (same as turn)
            let cache_key = if self.temperature == 0.0 {
                self.response_cache.as_ref().map(|_| {
                    let last_user = messages
                        .iter()
                        .rfind(|m| m.role == "user")
                        .map(|m| m.content.as_str())
                        .unwrap_or("");
                    let system = messages
                        .iter()
                        .find(|m| m.role == "system")
                        .map(|m| m.content.as_str());
                    zeroclaw_memory::response_cache::ResponseCache::cache_key(
                        &effective_model,
                        system,
                        last_user,
                    )
                })
            } else {
                None
            };

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

            // ── Streaming LLM call ────────────────────────────────────
            // Try streaming first; if the provider returns content we
            // forward deltas.  Otherwise fall back to non-streaming chat.
            use futures_util::StreamExt;

            let stream_opts = zeroclaw_providers::traits::StreamOptions::new(true);
            let mut stream = self.provider.stream_chat(
                zeroclaw_providers::ChatRequest {
                    messages: &messages,
                    tools: if self.tool_dispatcher.should_send_tool_specs() {
                        Some(&self.tool_specs)
                    } else {
                        None
                    },
                },
                &effective_model,
                Some(self.temperature),
                stream_opts,
            );

            let mut streamed_text = String::new();
            let mut streamed_tool_calls: Vec<zeroclaw_providers::traits::ToolCall> = Vec::new();
            let mut got_stream = false;
            let mut pre_executed_call_ids: HashMap<String, VecDeque<String>> = HashMap::new();
            let mut was_cancelled = false;

            // Consume the stream, checking for cancellation between chunks.
            // We use a manual loop with `tokio::select!` so that a cancel
            // signal interrupts even while waiting for the next SSE event
            // from the provider.
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
                        zeroclaw_providers::traits::StreamEvent::Final => break,
                    },
                    Err(_) => break,
                }
            }
            // Drop the stream so we release the borrow on provider.
            drop(stream);

            // If cancelled during streaming, return partial content with
            // the interruption marker appended. The caller (ws.rs) will
            // persist this truncated message and send an abort frame.
            if was_cancelled {
                let partial = if streamed_text.is_empty() {
                    "[interrupted by user]".to_string()
                } else {
                    format!("{streamed_text}\n\n[interrupted by user]")
                };
                self.history
                    .push(ConversationMessage::Chat(ChatMessage::assistant(
                        partial.clone(),
                    )));
                return Err(crate::agent::loop_::ToolLoopCancelled.into());
            }

            // If streaming produced text, use it as the response and
            // check for tool calls via the dispatcher.
            let response = if got_stream {
                // Build a synthetic ChatResponse from streamed text
                zeroclaw_providers::ChatResponse {
                    text: Some(streamed_text),
                    tool_calls: streamed_tool_calls,
                    usage: None,
                    reasoning_content: None,
                }
            } else {
                // Fall back to non-streaming chat, with cancellation guard
                let chat_fut = self.provider.chat(
                    ChatRequest {
                        messages: &messages,
                        tools: if self.tool_dispatcher.should_send_tool_specs() {
                            Some(&self.tool_specs)
                        } else {
                            None
                        },
                    },
                    &effective_model,
                    Some(self.temperature),
                );
                let chat_result = if let Some(ref token) = cancel_token {
                    tokio::select! {
                        biased;
                        () = token.cancelled() => {
                            return Err(crate::agent::loop_::ToolLoopCancelled.into());
                        }
                        result = chat_fut => result,
                    }
                } else {
                    chat_fut.await
                };
                match chat_result {
                    Ok(resp) => resp,
                    Err(err) => return Err(err),
                }
            };

            let (text, mut calls) = self.tool_dispatcher.parse_response(&response);
            if calls.is_empty() {
                let final_text = if text.is_empty() {
                    response.text.unwrap_or_default()
                } else {
                    text
                };

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

                self.history
                    .push(ConversationMessage::Chat(ChatMessage::assistant(
                        final_text.clone(),
                    )));
                self.trim_history();

                return Ok(final_text);
            }

            // Pre-assign stable IDs to tool calls that don't have one
            for call in &mut calls {
                if call.tool_call_id.is_none() {
                    call.tool_call_id = Some(uuid::Uuid::new_v4().to_string());
                }
            }

            // ── Tool calls ─────────────────────────────────────────────
            self.history.push(ConversationMessage::AssistantToolCalls {
                text: response.text.clone(),
                tool_calls: response.tool_calls.clone(),
                reasoning_content: response.reasoning_content.clone(),
            });

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
            self.history.push(formatted);
            self.trim_history();
        }

        anyhow::bail!(
            "Agent exceeded maximum tool iterations ({})",
            self.config.max_tool_iterations
        )
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
    message: Option<String>,
    provider_override: Option<String>,
    model_override: Option<String>,
    temperature: f64,
) -> Result<()> {
    let start = Instant::now();

    let mut effective_config = config;
    if let Some(p) = provider_override {
        effective_config.providers.fallback = vec![p];
    }
    if let Some(fallback) = effective_config.providers.fallback.first().cloned()
        && let Some((type_key, alias_key)) = fallback.split_once('.')
        && let Some(entry) = effective_config
            .providers
            .models
            .get_mut(type_key)
            .and_then(|m| m.get_mut(alias_key))
    {
        if let Some(m) = model_override {
            entry.model = Some(m);
        }
        entry.temperature = Some(temperature);
    }

    let mut agent = Agent::from_config(&effective_config).await?;

    let provider_name = effective_config
        .providers
        .fallback_type()
        .unwrap_or("openrouter")
        .to_string();
    // `Agent::from_config` above has already errored if no model could be resolved,
    // so this telemetry line should always find one. We keep `resolve_default_model`
    // as a cheap secondary lookup and emit "<unresolved>" only if nothing matches —
    // never silently substitute a hardcoded vendor model.
    let model_name = effective_config
        .providers
        .fallback_provider()
        .and_then(|e| e.model.as_deref())
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(ToString::to_string)
        .or_else(|| effective_config.providers.resolve_default_model())
        .unwrap_or_else(|| "<unresolved>".to_string());

    agent.observer.record_event(&ObserverEvent::AgentStart {
        provider: provider_name.clone(),
        model: model_name.clone(),
    });

    if let Some(msg) = message {
        let response = agent.run_single(&msg).await?;
        println!("{response}");
    } else {
        agent.run_interactive().await?;
    }

    agent.observer.record_event(&ObserverEvent::AgentEnd {
        provider: provider_name,
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

    struct MockProvider {
        responses: Mutex<Vec<zeroclaw_providers::ChatResponse>>,
    }

    #[async_trait]
    impl Provider for MockProvider {
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

    struct ModelCaptureProvider {
        responses: Mutex<Vec<zeroclaw_providers::ChatResponse>>,
        seen_models: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Provider for ModelCaptureProvider {
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

    #[tokio::test]
    async fn turn_without_tools_returns_text() {
        let provider = Box::new(MockProvider {
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
            .provider(provider)
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
    async fn turn_with_native_dispatcher_handles_tool_results_variant() {
        let provider = Box::new(MockProvider {
            responses: Mutex::new(vec![
                zeroclaw_providers::ChatResponse {
                    text: Some(String::new()),
                    tool_calls: vec![zeroclaw_providers::ToolCall {
                        id: "tc1".into(),
                        name: "echo".into(),
                        arguments: "{}".into(),
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
            .provider(provider)
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
        let provider = Box::new(ModelCaptureProvider {
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
            .provider(provider)
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
        let addr = listener.local_addr().unwrap();
        let server_handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let tmp = TempDir::new().expect("temp dir");
        let workspace_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();

        let mut config = zeroclaw_config::schema::Config {
            workspace_dir,
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        config.providers.fallback = vec![format!("custom:http://{addr}")];
        {
            let entry = config.ensure_fallback_provider();
            entry.api_key = Some("test-key".to_string());
            entry.model = Some("test-model".to_string());
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

        let mut agent = Agent::from_config(&config)
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

    #[test]
    fn builder_allowed_tools_none_keeps_all_tools() {
        let provider = Box::new(MockProvider {
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
            .provider(provider)
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
        let provider = Box::new(MockProvider {
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
            .provider(provider)
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

    #[test]
    fn seed_history_prepends_system_and_skips_system_from_seed() {
        let provider = Box::new(MockProvider {
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
            .provider(provider)
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

    /// Mock provider that captures whether tool specs were passed to `stream_chat`
    /// and returns a tool call followed by a text response through the stream.
    struct StreamToolCaptureProvider {
        tools_received: Arc<Mutex<Vec<bool>>>,
        call_count: Arc<Mutex<usize>>,
    }

    #[async_trait]
    impl Provider for StreamToolCaptureProvider {
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

    #[tokio::test]
    async fn turn_streamed_passes_tool_specs_to_provider() {
        let tools_received = Arc::new(Mutex::new(Vec::new()));
        let provider = Box::new(StreamToolCaptureProvider {
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
            .provider(provider)
            .tools(vec![Box::new(MockTool)])
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed with valid config");

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        let response = agent
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

    struct PreExecutedToolProvider;

    #[async_trait]
    impl Provider for PreExecutedToolProvider {
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

    #[tokio::test]
    async fn pre_executed_tool_results_keep_ids_when_calls_overlap() {
        let provider = Box::new(PreExecutedToolProvider);

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
            .provider(provider)
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

    /// Reproduction test for the orphan-tool_results trim bug.
    ///
    /// `trim_history` previously dropped the oldest N entries blindly. When
    /// the boundary fell in the middle of an `AssistantToolCalls` /
    /// `ToolResults` pair, the call side was dropped while the result side
    /// remained — leaving an orphan `ToolResults` at the head of the
    /// history. The next provider request then started with a `tool_result`
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
        let agent_config = zeroclaw_config::schema::AgentConfig {
            max_history_messages: 4,
            ..zeroclaw_config::schema::AgentConfig::default()
        };

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .provider(Box::new(MockProvider {
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
    /// pattern causes providers that enforce role-alternation to reject the
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

        let provider = Box::new(MockProvider {
            responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                text: Some("I will echo the message.".into()),
                tool_calls: vec![zeroclaw_providers::ToolCall {
                    id: "tc1".into(),
                    name: "echo".into(),
                    arguments: "{}".into(),
                }],
                usage: None,
                reasoning_content: None,
            }]),
        });

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .provider(provider)
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
    struct NarrationStreamProvider {
        call_count: Arc<Mutex<usize>>,
    }

    #[async_trait]
    impl Provider for NarrationStreamProvider {
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

        let provider = Box::new(NarrationStreamProvider {
            call_count: Arc::new(Mutex::new(0)),
        });

        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut agent = Agent::builder()
            .provider(provider)
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
        assert_eq!(names, &["builtin_a", "deploy.run", "deploy.status"]);
    }

    #[test]
    fn register_skill_tools_skips_shadowed_builtins() {
        let security = Arc::new(crate::security::SecurityPolicy::default());
        // Pre-populate with a tool whose name matches what the skill would produce.
        let mut tools: Vec<Box<dyn Tool>> = vec![Box::new(NamedMockTool::new("my_skill.run"))];

        let skills = vec![make_skill("my_skill", &["run"])];
        tools::register_skill_tools(&mut tools, &skills, security);

        // Should still be just 1 tool — the duplicate was skipped.
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "my_skill.run");
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
            &["file_read", "web_fetch", "ops.deploy", "ops.rollback"]
        );
    }
}
