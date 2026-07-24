use crate::agent::loop_::{
    LoopKnobs, ResolvedAgentExecution, ResolvedIo, ResolvedModelAccess, ResolvedRuntimeKnobs,
    TOOL_LOOP_SESSION_KEY, ToolLoop, apply_text_tool_prompt_policy, run_tool_call_loop,
};
use crate::agent::prompt::{PromptContext, SystemPromptBuilder};
use crate::observability::traits::{Observer, ObserverEvent, ObserverMetric};
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::schema::{
    AliasedAgentConfig, Config, DelegateExecutionMode, DelegateToolConfig, ModelProviderConfig,
    ResolvedRuntime, RiskProfileConfig, RuntimeProfileConfig, SkillBundleConfig,
};
use zeroclaw_log::Instrument as _;
use zeroclaw_memory::Memory;
use zeroclaw_providers::{self, ChatMessage, ModelProvider, ProviderDispatch};
use zeroclaw_tools::memory_export::MemoryExportTool;
use zeroclaw_tools::memory_forget::MemoryForgetTool;
use zeroclaw_tools::memory_purge::MemoryPurgeTool;
use zeroclaw_tools::memory_recall::MemoryRecallTool;
use zeroclaw_tools::memory_store::MemoryStoreTool;

fn current_tool_loop_session_key() -> Option<String> {
    TOOL_LOOP_SESSION_KEY.try_with(Clone::clone).ok().flatten()
}

async fn scope_delegate_session_key<F>(session_key: Option<String>, future: F) -> F::Output
where
    F: std::future::Future,
{
    TOOL_LOOP_SESSION_KEY.scope(session_key, future).await
}

/// Serializable result of a background delegate task.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BackgroundDelegateResult {
    pub task_id: String,
    pub agent: String,
    pub status: BackgroundTaskStatus,
    pub output: Option<String>,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// Status of a background delegate task.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundResultState {
    Running,
    Completed,
    Failed,
    Cancelled,
    Lost,
    TimedOut,
}

impl BackgroundResultState {
    fn from_file_status(status: &BackgroundTaskStatus) -> Self {
        match status {
            BackgroundTaskStatus::Running => Self::Running,
            BackgroundTaskStatus::Completed => Self::Completed,
            BackgroundTaskStatus::Failed => Self::Failed,
            BackgroundTaskStatus::Cancelled => Self::Cancelled,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Lost => "lost",
            Self::TimedOut => "timed_out",
        }
    }

    fn is_success(self) -> bool {
        self == Self::Completed
    }

    fn is_pending(self) -> bool {
        self == Self::Running
    }

    fn is_failure(self) -> bool {
        !matches!(self, Self::Running | Self::Completed)
    }
}

pub struct DelegateTool {
    agents: Arc<HashMap<String, AliasedAgentConfig>>,
    security: Arc<SecurityPolicy>,
    /// Global credential (from config.api_key) used when an agent has none set.
    global_credential: Option<String>,
    /// ModelProvider runtime options inherited from root config.
    provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions,
    /// Depth at which this tool instance lives in the delegation chain.
    depth: u32,
    /// Parent tool registry for agentic sub-agents.
    parent_tools: Arc<RwLock<Vec<Arc<dyn Tool>>>>,
    /// Runtime adapter used to build target-owned registries for independent
    /// agentic delegation.
    runtime: Option<Arc<dyn crate::platform::RuntimeAdapter>>,
    /// Inherited multimodal handling config for sub-agent loops.
    multimodal_config: zeroclaw_config::schema::MultimodalConfig,
    /// Global delegate tool config providing default timeout values.
    delegate_config: DelegateToolConfig,
    /// Workspace directory inherited from the root agent context.
    workspace_dir: PathBuf,
    /// Cancellation token for cascade control of background tasks.
    cancellation_token: CancellationToken,
    /// Optional memory instance for namespace isolation on delegate agents.
    memory: Option<Arc<dyn Memory>>,
    /// nested model provider map for brain resolution.
    providers_models: Arc<HashMap<String, HashMap<String, ModelProviderConfig>>>,
    /// named risk profiles for delegation depth and timeout resolution.
    risk_profiles: Arc<HashMap<String, RiskProfileConfig>>,
    /// named runtime profiles for agentic/tools/iteration resolution.
    runtime_profiles: Arc<HashMap<String, RuntimeProfileConfig>>,
    /// named skill bundles for skills-directory resolution.
    skill_bundles: Arc<HashMap<String, SkillBundleConfig>>,
    /// Optional handle to the loaded root config used to resolve delegate
    /// reachability, target mode, and per-target `SecurityPolicy` at delegate
    /// time. When unset (legacy unit-test constructors), DelegateTool falls
    /// back to using `self.security` for the spawned inner DelegateTool.
    root_config: Option<Arc<Config>>,
    /// Alias of the agent that owns this DelegateTool. Excluded from the
    /// advertised roster so an agent is never offered itself as a
    /// delegation target. Empty when unset (legacy unit-test constructors).
    caller_alias: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DelegateAdmission {
    /// This call entered through the user-visible `delegate` tool and must run
    /// caller-side tool authorization plus target reachability checks.
    Required,
    Prevalidated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DelegateAction {
    Delegate,
    CheckResult,
    ListResults,
    CancelTask,
    AwaitSessions,
}

impl DelegateAction {
    const ALL: [Self; 5] = [
        Self::Delegate,
        Self::CheckResult,
        Self::ListResults,
        Self::CancelTask,
        Self::AwaitSessions,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Delegate => "delegate",
            Self::CheckResult => "check_result",
            Self::ListResults => "list_results",
            Self::CancelTask => "cancel_task",
            Self::AwaitSessions => "await_sessions",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|action| action.as_str() == value)
    }

    fn schema_values() -> Vec<&'static str> {
        Self::ALL.into_iter().map(Self::as_str).collect()
    }

    fn usage() -> String {
        Self::schema_values().join("/")
    }
}

struct IndependentTargetTools {
    tools: Vec<Box<dyn Tool>>,
    /// The deferred-MCP + pinned-resources system-prompt section (empty unless
    /// the target has granted MCP bundles under deferred loading).
    deferred_section: String,
    /// Live handle to the deferred-MCP activated set (Some only when a deferred
    /// `tool_search` tool was registered), threaded into the sub-agent turn loop.
    activated_handle: Option<Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    workspace_dir: PathBuf,
    skills: Vec<crate::skills::Skill>,
}

impl DelegateTool {
    /// Canonical tool name. Referenced by `REENTRANT_AGENT_TOOLS` so a
    /// rename cannot desync the two.
    pub const NAME: &'static str = "delegate";
    const MAX_AWAIT_SESSIONS_TIMEOUT: Duration = Duration::from_secs(120);
    const MAX_AWAIT_SESSION_TASK_IDS: usize = 128;
    const INDEPENDENT_ALWAYS_ASK_DOC_REF: &'static str =
        "ZeroClaw docs, \"Delegation & SubAgents\" > \"What's not supported\"";

    pub fn new(
        agents: HashMap<String, AliasedAgentConfig>,
        global_credential: Option<String>,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        Self::new_with_options(
            agents,
            global_credential,
            security,
            zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        )
    }

    pub fn new_with_options(
        agents: HashMap<String, AliasedAgentConfig>,
        global_credential: Option<String>,
        security: Arc<SecurityPolicy>,
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions,
    ) -> Self {
        Self {
            agents: Arc::new(agents),
            security,
            global_credential,
            provider_runtime_options,
            depth: 0,
            parent_tools: Arc::new(RwLock::new(Vec::new())),
            runtime: None,
            multimodal_config: zeroclaw_config::schema::MultimodalConfig::default(),
            delegate_config: DelegateToolConfig::default(),
            workspace_dir: PathBuf::new(),
            cancellation_token: CancellationToken::new(),
            memory: None,
            providers_models: Arc::new(HashMap::new()),
            risk_profiles: Arc::new(HashMap::new()),
            runtime_profiles: Arc::new(HashMap::new()),
            skill_bundles: Arc::new(HashMap::new()),
            root_config: None,
            caller_alias: String::new(),
        }
    }

    /// Create a DelegateTool for a sub-agent (with incremented depth).
    /// When sub-agents eventually get their own tool registry, construct
    /// their DelegateTool via this method with `depth: parent.depth + 1`.
    pub fn with_depth(
        agents: HashMap<String, AliasedAgentConfig>,
        global_credential: Option<String>,
        security: Arc<SecurityPolicy>,
        depth: u32,
    ) -> Self {
        Self::with_depth_and_options(
            agents,
            global_credential,
            security,
            depth,
            zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        )
    }

    pub fn with_depth_and_options(
        agents: HashMap<String, AliasedAgentConfig>,
        global_credential: Option<String>,
        security: Arc<SecurityPolicy>,
        depth: u32,
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions,
    ) -> Self {
        Self {
            agents: Arc::new(agents),
            security,
            global_credential,
            provider_runtime_options,
            depth,
            parent_tools: Arc::new(RwLock::new(Vec::new())),
            runtime: None,
            multimodal_config: zeroclaw_config::schema::MultimodalConfig::default(),
            delegate_config: DelegateToolConfig::default(),
            workspace_dir: PathBuf::new(),
            cancellation_token: CancellationToken::new(),
            memory: None,
            providers_models: Arc::new(HashMap::new()),
            risk_profiles: Arc::new(HashMap::new()),
            runtime_profiles: Arc::new(HashMap::new()),
            skill_bundles: Arc::new(HashMap::new()),
            root_config: None,
            caller_alias: String::new(),
        }
    }

    /// Attach parent tools used to build sub-agent allowlist registries.
    pub fn with_parent_tools(mut self, parent_tools: Arc<RwLock<Vec<Arc<dyn Tool>>>>) -> Self {
        self.parent_tools = parent_tools;
        self
    }

    /// Attach the runtime adapter used to build target-owned tools for
    /// independent agentic delegation.
    pub fn with_runtime(mut self, runtime: Arc<dyn crate::platform::RuntimeAdapter>) -> Self {
        self.runtime = Some(runtime);
        self
    }

    /// Attach multimodal configuration for sub-agent tool loops.
    pub fn with_multimodal_config(
        mut self,
        config: zeroclaw_config::schema::MultimodalConfig,
    ) -> Self {
        self.multimodal_config = config;
        self
    }

    /// Attach global delegate tool configuration for default timeout values.
    pub fn with_delegate_config(mut self, config: DelegateToolConfig) -> Self {
        self.delegate_config = config;
        self
    }

    /// Return a shared handle to the parent tools list.
    /// Callers can push additional tools (e.g. MCP wrappers) after construction.
    pub fn parent_tools_handle(&self) -> Arc<RwLock<Vec<Arc<dyn Tool>>>> {
        Arc::clone(&self.parent_tools)
    }

    /// Attach the workspace directory for system prompt enrichment.
    pub fn with_workspace_dir(mut self, workspace_dir: PathBuf) -> Self {
        self.workspace_dir = workspace_dir;
        self
    }

    fn agent_workspace(&self, agent_alias: &str) -> Option<PathBuf> {
        self.root_config
            .as_ref()
            .map(|cfg| cfg.agent_workspace_dir(agent_alias))
    }

    /// Attach a cancellation token for cascade control of background tasks.
    /// When the token is cancelled, all background sub-agents are aborted.
    pub fn with_cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = token;
        self
    }

    /// Return the cancellation token for external cascade control.
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }

    /// Attach memory for namespace isolation on delegate agents.
    pub fn with_memory(mut self, memory: Arc<dyn Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Attach nested model provider map for brain resolution.
    pub fn with_providers_models(
        mut self,
        m: HashMap<String, HashMap<String, ModelProviderConfig>>,
    ) -> Self {
        self.providers_models = Arc::new(m);
        self
    }

    /// Attach risk profiles for depth/timeout resolution.
    pub fn with_risk_profiles(mut self, m: HashMap<String, RiskProfileConfig>) -> Self {
        self.risk_profiles = Arc::new(m);
        self
    }

    /// Attach runtime profiles for agentic/tools/iteration resolution.
    pub fn with_runtime_profiles(mut self, m: HashMap<String, RuntimeProfileConfig>) -> Self {
        self.runtime_profiles = Arc::new(m);
        self
    }

    /// Attach skill bundles for skills-directory resolution.
    pub fn with_skill_bundles(mut self, m: HashMap<String, SkillBundleConfig>) -> Self {
        self.skill_bundles = Arc::new(m);
        self
    }

    /// Attach the loaded root config so DelegateTool can resolve delegate
    /// reachability, target mode, and per-target `SecurityPolicy` from the
    /// canonical agent config at delegate time.
    pub fn with_root_config(mut self, config: Arc<Config>) -> Self {
        self.root_config = Some(config);
        self
    }

    /// Set the owning agent's alias so it can be excluded from the
    /// advertised delegation roster (an agent must never delegate to
    /// itself).
    pub fn with_caller_alias(mut self, alias: impl Into<String>) -> Self {
        self.caller_alias = alias.into();
        self
    }

    fn policy_for_target(&self, target_alias: &str) -> anyhow::Result<Arc<SecurityPolicy>> {
        let Some(config) = self.root_config.as_ref() else {
            return Ok(Arc::clone(&self.security));
        };
        if !self.security.delegation_policy.permits() {
            let remediation = if self.security.risk_profile_name.trim().is_empty() {
                "set the caller risk profile's delegation_policy mode = \"allow\"".to_string()
            } else {
                format!(
                    "set [risk_profiles.{}].delegation_policy mode = \"allow\"",
                    self.security.risk_profile_name
                )
            };
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "target_agent": target_alias,
                        "caller_alias": self.caller_alias,
                        "caller_risk_profile": self.security.risk_profile_name,
                    })),
                "delegate refused: caller delegation_policy forbids delegation"
            );
            return Err(anyhow::Error::msg(format!(
                "delegation is forbidden for caller {:?} by risk profile {:?} \
                 delegation_policy; {remediation}",
                self.caller_alias, self.security.risk_profile_name
            )));
        }

        // Resolve reachability and execution mode through `Config` so
        // admission follows the same canonical roster advertised to callers.
        let Some(target_mode) = config.delegate_target_mode(&self.caller_alias, target_alias)
        else {
            let error = self.unreachable_target_error(config, target_alias);
            let caller_profile = config
                .agents
                .get(&self.caller_alias)
                .map(|agent| agent.risk_profile.trim())
                .unwrap_or_default();
            let target_profile = config
                .agents
                .get(target_alias)
                .map(|agent| agent.risk_profile.trim())
                .unwrap_or_default();
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "target_agent": target_alias,
                        "caller_alias": self.caller_alias,
                        "caller_risk_profile": caller_profile,
                        "target_risk_profile": target_profile,
                    })),
                "delegate refused: target not in caller's reachable set"
            );
            return Err(anyhow::Error::msg(error));
        };

        let mut target_policy = SecurityPolicy::for_agent(config, target_alias).map_err(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "target_agent": target_alias,
                        "caller_alias": self.caller_alias,
                        "error": format!("{}", e),
                    })),
                "delegate: could not resolve target's security policy"
            );
            anyhow::Error::msg(format!(
                "could not resolve security policy for delegate target {target_alias:?}: {e}"
            ))
        })?;

        if target_mode == DelegateExecutionMode::Bounded {
            target_policy.tracker = self.security.tracker.clone();

            if self.security.risk_profile_name == target_policy.risk_profile_name {
                target_policy.workspace_dir = self.security.workspace_dir.clone();
            }
        }

        Ok(Arc::new(target_policy))
    }

    fn unreachable_target_error(&self, config: &Config, target_alias: &str) -> String {
        let Some(caller) = config.agents.get(&self.caller_alias) else {
            return format!(
                "delegate target {target_alias:?} is not reachable because caller {:?} \
                 is not present in the loaded agents config",
                self.caller_alias
            );
        };

        let Some(target) = config.agents.get(target_alias) else {
            return format!(
                "delegate target {target_alias:?} is not reachable from {:?}: \
                 no agent with that alias exists in the loaded config",
                self.caller_alias
            );
        };

        let explicitly_configured = caller
            .delegates
            .iter()
            .any(|target| target.agent().trim() == target_alias);

        if !target.enabled {
            return format!(
                "delegate target {target_alias:?} is not reachable from {:?}: \
                 the target agent is disabled",
                self.caller_alias
            );
        }

        let caller_profile = caller.risk_profile.trim();
        let target_profile = target.risk_profile.trim();
        if caller.delegate_same_risk_profile
            && !explicitly_configured
            && !caller_profile.is_empty()
            && !target_profile.is_empty()
            && caller_profile != target_profile
        {
            return format!(
                "delegate target {target_alias:?} is not reachable from {:?}: \
                 different risk profile (caller uses {caller_profile:?}, target uses \
                 {target_profile:?}). delegate_same_risk_profile only reaches agents \
                 with the same risk profile; add an explicit [agents.{}].delegates \
                 entry with the intended mode, or change one agent's risk_profile.",
                self.caller_alias, self.caller_alias
            );
        }

        if !caller.delegate_same_risk_profile && !explicitly_configured {
            return format!(
                "delegate target {target_alias:?} is not reachable from {:?}: \
                 delegate_same_risk_profile is disabled and the target is not listed \
                 in [agents.{}].delegates",
                self.caller_alias, self.caller_alias
            );
        }

        format!(
            "delegate target {target_alias:?} is not reachable from {:?}; \
             add it to [agents.{}].delegates or share a risk profile with \
             delegate_same_risk_profile enabled",
            self.caller_alias, self.caller_alias
        )
    }

    fn mode_for_target(&self, target_alias: &str) -> DelegateExecutionMode {
        self.root_config
            .as_ref()
            .and_then(|config| config.delegate_target_mode(&self.caller_alias, target_alias))
            .unwrap_or(DelegateExecutionMode::Bounded)
    }

    fn independent_always_ask_refusal(&self, target_alias: &str) -> Option<ToolResult> {
        let config = self.root_config.as_ref()?;
        if config.delegate_target_mode(&self.caller_alias, target_alias)
            != Some(DelegateExecutionMode::Independent)
        {
            return None;
        }

        let target_agent = config.agents.get(target_alias)?;
        let target_risk_profile = target_agent.risk_profile.trim();
        if target_risk_profile.is_empty() {
            return None;
        }

        let profile = config.risk_profiles.get(target_risk_profile)?;
        let always_ask_entries: Vec<String> = profile
            .always_ask
            .iter()
            .map(|entry| entry.trim())
            .filter(|entry| !entry.is_empty())
            .map(str::to_string)
            .collect();
        if always_ask_entries.is_empty() {
            return None;
        }
        let always_ask_label = always_ask_entries.join(", ");

        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "error_key": "delegate.independent_always_ask_unsupported",
                    "caller_alias": self.caller_alias,
                    "target_agent": target_alias,
                    "target_risk_profile": target_risk_profile,
                    "always_ask": always_ask_entries.clone(),
                })),
            "delegate refused: independent target has always_ask entries"
        );

        Some(ToolResult {
            success: false,
            output: ToolOutput::default(),
            error: Some(format!(
                "delegate target {target_alias:?} cannot run in independent mode from {:?}: \
                 risk profile {target_risk_profile:?} has always_ask entries ({}). \
                 See {}.",
                self.caller_alias,
                always_ask_label,
                Self::INDEPENDENT_ALWAYS_ASK_DOC_REF
            )),
        })
    }

    fn build_target_provider(
        &self,
        model_provider: &str,
        provider_type: &str,
        credential: Option<&str>,
    ) -> anyhow::Result<Box<dyn ModelProvider>> {
        if let Some(config) = self.root_config.as_deref()
            && let Some((family, alias)) = model_provider.split_once('.')
        {
            let mut options =
                zeroclaw_providers::provider_runtime_options_for_alias(config, family, alias);
            if options.zeroclaw_dir.is_none() {
                options.zeroclaw_dir = self.provider_runtime_options.zeroclaw_dir.clone();
            }
            return zeroclaw_providers::create_model_provider_for_alias(
                config, family, alias, credential, &options,
            );
        }
        zeroclaw_providers::create_model_provider_with_options(
            provider_type,
            credential,
            &self.provider_runtime_options,
        )
    }

    async fn memory_for_target_agent(
        &self,
        agent_name: &str,
    ) -> anyhow::Result<Option<Arc<dyn Memory>>> {
        let Some(config) = self.root_config.as_deref() else {
            return Ok(self.memory.clone());
        };

        let api_key = config
            .resolved_model_provider_for_agent(agent_name)
            .and_then(|(_, _, cfg)| cfg.api_key.as_deref());
        zeroclaw_memory::create_memory_for_agent(config, agent_name, api_key)
            .await
            .map(Some)
    }

    fn memory_tools_for_target(
        memory: Arc<dyn Memory>,
        security: Arc<SecurityPolicy>,
    ) -> Vec<Box<dyn Tool>> {
        vec![
            Box::new(MemoryStoreTool::new(memory.clone(), security.clone())),
            Box::new(MemoryRecallTool::new(memory.clone())),
            Box::new(MemoryForgetTool::new(memory.clone(), security.clone())),
            Box::new(MemoryExportTool::new(memory.clone())),
            Box::new(MemoryPurgeTool::new(memory, security)),
        ]
    }

    async fn independent_agentic_tools_for_target(
        &self,
        agent_name: &str,
        target_policy: Arc<SecurityPolicy>,
    ) -> anyhow::Result<IndependentTargetTools> {
        let config = self
            .root_config
            .as_ref()
            .ok_or_else(|| anyhow::Error::msg("independent delegation requires root config"))?;
        let runtime =
            self.runtime.as_ref().cloned().ok_or_else(|| {
                anyhow::Error::msg("independent delegation requires runtime adapter")
            })?;
        let risk_profile = config
            .risk_profile_for_agent(agent_name)
            .cloned()
            .ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "Agent '{agent_name}' is agentic but its risk profile is not configured"
                ))
            })?;
        let memory = self
            .memory_for_target_agent(agent_name)
            .await?
            .ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "Failed to initialize memory for independent delegate target '{agent_name}'"
                ))
            })?;
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
        let target_api_key = config
            .resolved_model_provider_for_agent(agent_name)
            .and_then(|(_, _, provider)| provider.api_key.as_deref());

        let all_tools_result = crate::tools::all_tools_with_runtime(
            Arc::clone(config),
            &target_policy,
            &risk_profile,
            agent_name,
            runtime.clone(),
            memory,
            composio_key,
            composio_entity_id,
            &config.browser,
            &config.http_request,
            &config.web_fetch,
            &target_policy.workspace_dir,
            &config.agents,
            target_api_key,
            config,
            None,
            false,
            None,
            None,
            None,
            None,
        );

        let target_workspace = config.agent_workspace_dir(agent_name);
        let skills = crate::skills::load_skills_for_agent_from_config(config, agent_name);

        let assembled = crate::tools::scoped::ScopedToolRegistry::assemble(
            crate::tools::scoped::ScopedAssembly {
                config,
                agent_alias: agent_name,
                security: &target_policy,
                built: all_tools_result,
                skills: &skills,
                runtime,
                caller_allowed: None,
                connect_mcp: true,
                connect_peripherals: false,
                exclude_memory: false,
                list_deferred_mcp_specs: false,
                emit_assembly_logs: true,
                // Delegate: targets are short-lived independent chat
                // sessions with no cross-turn reuse contract, so the
                // per-call `connect_all` is the correct choice. The
                // daemon heartbeat worker is the only `mcp_registry`
                // supplier.
                mcp_registry: None,
            },
        )
        .await;
        // Independent delegation injects one combined MCP prompt block: the harness
        // composes the deferred tool-search listing with any pinned MCP resources, so
        // this can no longer silently lose pinned resources the way a raw-field
        // destructure could (see `ScopedAssembled::combined_mcp_prompt_section`).
        let deferred_section = assembled.combined_mcp_prompt_section();
        let crate::tools::scoped::ScopedAssembled {
            registry,
            activated_handle,
            ..
        } = assembled;
        let mut tools = registry.into_inner();
        tools.retain(|tool| tool.name() != Self::NAME);
        Ok(IndependentTargetTools {
            tools,
            deferred_section,
            activated_handle,
            workspace_dir: target_workspace,
            skills,
        })
    }

    /// Resolve `model_provider` ("type.alias") → (provider_type, credential, model, temperature).
    fn resolve_brain(&self, model_provider: &str) -> (String, Option<String>, String, Option<f64>) {
        if let Some((type_key, alias_key)) = model_provider.split_once('.')
            && let Some(alias_map) = self.providers_models.get(type_key)
            && let Some(cfg) = alias_map.get(alias_key)
        {
            return (
                type_key.to_string(),
                if cfg.requires_openai_auth {
                    cfg.api_key.clone()
                } else {
                    cfg.api_key
                        .clone()
                        .or_else(|| self.global_credential.clone())
                },
                cfg.model.clone().unwrap_or_default(),
                cfg.temperature,
            );
        }
        let type_key = model_provider
            .split_once('.')
            .map_or(model_provider, |(t, _)| t);
        (
            type_key.to_string(),
            self.global_credential.clone(),
            String::new(),
            None,
        )
    }

    /// Resolve max delegation depth from the named runtime profile (default: 3).
    fn resolve_max_depth(&self, runtime_profile: &str) -> u32 {
        if runtime_profile.is_empty() {
            return 3;
        }
        self.runtime_profiles
            .get(runtime_profile)
            .map(|p| p.max_delegation_depth)
            .filter(|&d| d > 0)
            .unwrap_or(3)
    }

    /// Resolve per-call delegation timeout from the named runtime profile.
    fn resolve_delegation_timeout(&self, runtime_profile: &str) -> Option<u64> {
        if runtime_profile.is_empty() {
            return None;
        }
        self.runtime_profiles
            .get(runtime_profile)
            .and_then(|p| p.delegation_timeout_secs)
    }

    /// Resolve agentic run timeout from the named runtime profile.
    fn resolve_agentic_timeout_secs(&self, runtime_profile: &str) -> Option<u64> {
        if runtime_profile.is_empty() {
            return None;
        }
        self.runtime_profiles
            .get(runtime_profile)
            .and_then(|p| p.agentic_timeout_secs)
    }

    /// Resolve agentic mode flag from the named runtime profile (default: false).
    fn resolve_agentic(&self, runtime_profile: &str) -> bool {
        if runtime_profile.is_empty() {
            return false;
        }
        self.runtime_profiles
            .get(runtime_profile)
            .map(|p| p.agentic)
            .unwrap_or(false)
    }

    fn resolve_loop_runtime(
        &self,
        agent_alias: &str,
        agent_config: &AliasedAgentConfig,
    ) -> ResolvedRuntime {
        if let Some(root_config) = self.root_config.as_ref()
            && let Some(resolved_config) = root_config.resolved_agent_config(agent_alias)
        {
            return resolved_config.resolved;
        }

        let mut resolved = agent_config.resolved.clone();

        if let Some(profile) = self
            .runtime_profiles
            .get(agent_config.runtime_profile.as_str())
        {
            if profile.max_tool_iterations > 0 {
                resolved.max_tool_iterations = profile.max_tool_iterations;
            }
            if let Some(max_context_tokens) = profile.max_context_tokens {
                resolved.max_context_tokens = max_context_tokens;
            }
            if let Some(parallel_tools) = profile.parallel_tools {
                resolved.parallel_tools = parallel_tools;
            }
            if let Some(max_tool_result_chars) = profile.max_tool_result_chars {
                resolved.max_tool_result_chars = max_tool_result_chars;
            }
            resolved.strict_tool_parsing = profile.strict_tool_parsing;
        }

        resolved
    }

    fn resolve_tool_policy(&self, risk_profile: &str) -> Option<SecurityPolicy> {
        if risk_profile.is_empty() {
            return None;
        }

        let profile = self.risk_profiles.get(risk_profile)?;
        Some(SecurityPolicy {
            allowed_tools: if profile.allowed_tools.is_empty() {
                None
            } else {
                Some(profile.allowed_tools.clone())
            },
            excluded_tools: if profile.excluded_tools.is_empty() {
                None
            } else {
                Some(profile.excluded_tools.clone())
            },
            ..SecurityPolicy::default()
        })
    }

    fn delegate_admits_with_mcp(policy: &SecurityPolicy, name: &str) -> bool {
        let denied = policy
            .excluded_tools
            .as_ref()
            .is_some_and(|list| list.iter().any(|t| t == name));
        if denied {
            return false;
        }
        match policy.allowed_tools.as_ref() {
            None => true,
            Some(list) if list.is_empty() => false,
            Some(list) => list.iter().any(|t| t == name) || name.contains("__"),
        }
    }

    /// Resolve every configured skill bundle alias to its directory.
    /// Empty list / no matches → caller falls back to the workspace default.
    fn resolve_skill_bundle_dirs(&self, bundle_aliases: &[String]) -> Vec<String> {
        bundle_aliases
            .iter()
            .filter(|a| !a.is_empty())
            .filter_map(|a| self.skill_bundles.get(a).and_then(|b| b.directory.clone()))
            .collect()
    }

    /// Directory where background delegate results are stored.
    fn results_dir(&self) -> PathBuf {
        self.workspace_dir.join("delegate_results")
    }

    async fn write_result_atomic(
        result_path: &Path,
        result: &BackgroundDelegateResult,
    ) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec_pretty(result)?;
        let tmp_path = result_path.with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4()));
        tokio::fs::write(&tmp_path, &bytes).await?;
        tokio::fs::rename(&tmp_path, result_path).await?;
        Ok(())
    }

    /// Validate that a user-provided task_id is a valid UUID to prevent
    /// path traversal attacks (e.g. `../../etc/passwd`).
    fn validate_task_id(task_id: &str) -> Result<(), String> {
        if uuid::Uuid::parse_str(task_id).is_err() {
            return Err(format!("Invalid task_id '{task_id}': must be a valid UUID"));
        }
        Ok(())
    }
}

#[async_trait]
impl Tool for DelegateTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "Delegate a subtask to a specialized agent. Use when: a task benefits from a different model \
         (e.g. fast summarization, deep reasoning, code generation). The sub-agent runs a single \
         prompt by default; with agentic=true it can iterate with a filtered tool-call loop. \
         Supports background execution (returns a task_id immediately), batched background waits \
         (await_sessions), and parallel execution (runs multiple agents concurrently)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let delegation_permitted = self.security.delegation_policy.permits();
        let caller_profile = self.security.risk_profile_name.as_str();
        let mut agent_names: Vec<String> = if !delegation_permitted {
            Vec::new()
        } else if let Some(config) = self.root_config.as_ref() {
            config.reachable_delegate_targets(&self.caller_alias)
        } else {
            let mut names: Vec<String> = self
                .agents
                .iter()
                .filter(|(name, _)| name.as_str() != self.caller_alias.as_str())
                .filter(|(_, cfg)| cfg.risk_profile.trim() == caller_profile)
                .map(|(name, _)| name.clone())
                .collect();
            names.sort_unstable();
            names
        };
        agent_names.sort_unstable();
        agent_names.dedup();
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "action": {
                    "type": "string",
                    "enum": DelegateAction::schema_values(),
                    "description": "Action to perform. Default: 'delegate'. Use 'check_result' to \
                                    retrieve a background task result, 'await_sessions' to wait for \
                                    multiple background results, 'list_results' to list all background \
                                    tasks, 'cancel_task' to cancel a running background task.",
                    "default": DelegateAction::Delegate.as_str()
                },
                "agent": {
                    "type": "string",
                    "minLength": 1,
                    "description": format!(
                        "Name of the agent to delegate to. Available: {}",
                        if agent_names.is_empty() {
                            "(none configured)".to_string()
                        } else {
                            agent_names.join(", ")
                        }
                    )
                },
                "prompt": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The task/prompt to send to the sub-agent"
                },
                "context": {
                    "type": "string",
                    "description": "Optional context to prepend (e.g. relevant code, prior findings)"
                },
                "background": {
                    "type": "boolean",
                    "description": "When true, the sub-agent runs in a background tokio task and \
                                    returns a task_id immediately. Results are stored to \
                                    workspace/delegate_results/{task_id}.json.",
                    "default": false
                },
                "parallel": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Array of agent names to run concurrently with the same prompt. \
                                    Returns all results when all agents complete. Cannot be combined \
                                    with 'background'."
                },
                "task_id": {
                    "type": "string",
                    "description": "Task ID for check_result/cancel_task actions (returned by \
                                    background delegation)."
                },
                "task_ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "maxItems": Self::MAX_AWAIT_SESSION_TASK_IDS,
                    "description": "Task IDs for await_sessions."
                },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": Self::MAX_AWAIT_SESSIONS_TIMEOUT.as_millis(),
                    "description": "Maximum milliseconds for await_sessions to wait before returning partial results. Capped at 120000."
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action_value = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| DelegateAction::Delegate.as_str());
        let Some(action) = DelegateAction::parse(action_value) else {
            return Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some(format!(
                    "Unknown action '{action_value}'. Use {}.",
                    DelegateAction::usage()
                )),
            });
        };

        match action {
            DelegateAction::CheckResult => return self.handle_check_result(&args).await,
            DelegateAction::ListResults => return self.handle_list_results().await,
            DelegateAction::CancelTask => return self.handle_cancel_task(&args).await,
            DelegateAction::AwaitSessions => return self.handle_await_sessions(&args).await,
            DelegateAction::Delegate => {}
        }

        // --- Parallel mode ---
        if let Some(parallel_agents) = args.get("parallel").and_then(|v| v.as_array()) {
            return self.execute_parallel(parallel_agents, &args).await;
        }

        // --- Single-agent delegation (synchronous or background) ---
        let agent_name = args
            .get("agent")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "agent"})),
                    "tool argument validation failed"
                );

                anyhow::Error::msg("Missing 'agent' parameter")
            })?;

        if agent_name.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("'agent' parameter must not be empty".into()),
            });
        }

        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "prompt"})),
                    "tool argument validation failed"
                );

                anyhow::Error::msg("Missing 'prompt' parameter")
            })?;

        if prompt.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("'prompt' parameter must not be empty".into()),
            });
        }

        let background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if background {
            return self.execute_background(agent_name, prompt, &args).await;
        }

        // --- Synchronous delegation (original path) ---
        self.execute_sync(agent_name, prompt, &args).await
    }
}

impl DelegateTool {
    /// Original synchronous delegation path (extracted for reuse).
    async fn execute_sync(
        &self,
        agent_name: &str,
        prompt: &str,
        args: &serde_json::Value,
    ) -> anyhow::Result<ToolResult> {
        self.execute_sync_with_admission(agent_name, prompt, args, DelegateAdmission::Required)
            .await
    }

    async fn execute_sync_with_admission(
        &self,
        agent_name: &str,
        prompt: &str,
        args: &serde_json::Value,
        admission: DelegateAdmission,
    ) -> anyhow::Result<ToolResult> {
        let context = args
            .get("context")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");

        // Look up agent config
        let agent_config = match self.agents.get(agent_name) {
            Some(cfg) => cfg,
            None => {
                let available: Vec<&str> =
                    self.agents.keys().map(|s: &String| s.as_str()).collect();
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "Unknown agent '{agent_name}'. Available agents: {}",
                        if available.is_empty() {
                            "(none configured)".to_string()
                        } else {
                            available.join(", ")
                        }
                    )),
                });
            }
        };

        // Resolve profile references
        let max_depth = self.resolve_max_depth(&agent_config.runtime_profile);
        let (provider_type, credential, model, temperature) =
            self.resolve_brain(&agent_config.model_provider);
        let agentic = self.resolve_agentic(&agent_config.runtime_profile);

        // Check recursion depth (immutable — set at construction, incremented for sub-agents)
        if self.depth >= max_depth {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Delegation depth limit reached ({depth}/{max}). \
                     Cannot delegate further to prevent infinite loops.",
                    depth = self.depth,
                    max = max_depth
                )),
            });
        }

        if admission == DelegateAdmission::Required {
            if let Err(error) = self
                .security
                .enforce_tool_operation(ToolOperation::Act, "delegate")
            {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(error),
                });
            }

            if let Err(e) = self.policy_for_target(agent_name) {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("{e:#}")),
                });
            }
            if let Some(refusal) = self.independent_always_ask_refusal(agent_name) {
                return Ok(refusal);
            }
        }

        // Create model_provider for this agent
        let model_provider: Box<dyn ModelProvider> = match self.build_target_provider(
            &agent_config.model_provider,
            &provider_type,
            credential.as_deref(),
        ) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "Failed to create model_provider '{provider_type}' for agent '{agent_name}': {e}"
                    )),
                });
            }
        };

        // Build the message
        let full_prompt = if context.is_empty() {
            prompt.to_string()
        } else {
            format!("[Context]\n{context}\n\n[Task]\n{prompt}")
        };

        // Agentic mode: run full tool-call loop with allowlisted tools.
        if agentic {
            return self
                .execute_agentic_with_admission(
                    agent_name,
                    agent_config,
                    &provider_type,
                    &model,
                    &*model_provider,
                    &full_prompt,
                    temperature,
                    admission,
                )
                .await;
        }

        // Build enriched system prompt for non-agentic sub-agent.
        let enriched_system_prompt = self.build_enriched_system_prompt(
            agent_name,
            agent_config,
            &model,
            &[],
            &self.workspace_dir,
            false,
            None,
        );
        let system_prompt_ref = enriched_system_prompt.as_deref();

        // Wrap the model_provider call in a timeout to prevent indefinite blocking
        let timeout_secs = self
            .resolve_delegation_timeout(&agent_config.runtime_profile)
            .unwrap_or(self.delegate_config.timeout_secs);
        let dispatcher = ProviderDispatch::from_ref(&*model_provider);
        let result = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            dispatcher.chat_with_system(system_prompt_ref, &full_prompt, &model, temperature),
        )
        .await;

        let result = match result {
            Ok(inner) => inner,
            Err(_elapsed) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "Agent '{agent_name}' timed out after {timeout_secs}s"
                    )),
                });
            }
        };

        match result {
            Ok(response) => {
                let mut rendered = response;
                if rendered.trim().is_empty() {
                    rendered = "[Empty response]".to_string();
                }

                Ok(ToolResult {
                    success: true,
                    output:
                        format!("[Agent '{agent_name}' ({provider_type}/{model})]\n{rendered}",)
                            .into(),
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Agent '{agent_name}' failed: {e}",)),
            }),
        }
    }
}

impl DelegateTool {
    // ── Background Execution ────────────────────────────────────────

    /// Spawn a sub-agent in a background tokio task. Returns a task_id immediately.
    /// The result is persisted to `workspace/delegate_results/{task_id}.json`.
    async fn execute_background(
        &self,
        agent_name: &str,
        prompt: &str,
        args: &serde_json::Value,
    ) -> anyhow::Result<ToolResult> {
        // Validate agent exists and check depth/security before spawning
        let agent_config = match self.agents.get(agent_name) {
            Some(cfg) => cfg.clone(),
            None => {
                let available: Vec<&str> =
                    self.agents.keys().map(|s: &String| s.as_str()).collect();
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "Unknown agent '{agent_name}'. Available agents: {}",
                        if available.is_empty() {
                            "(none configured)".to_string()
                        } else {
                            available.join(", ")
                        }
                    )),
                });
            }
        };

        let max_depth = self.resolve_max_depth(&agent_config.runtime_profile);
        if self.depth >= max_depth {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Delegation depth limit reached ({depth}/{max}).",
                    depth = self.depth,
                    max = max_depth
                )),
            });
        }

        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "delegate")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        let target_policy = match self.policy_for_target(agent_name) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("{e:#}")),
                });
            }
        };
        if let Some(refusal) = self.independent_always_ask_refusal(agent_name) {
            return Ok(refusal);
        }

        // Runaway backstop: refuse a new background delegation once too many are already in
        // flight (each is a full agent loop). The in-flight set is the live cancel-token map.
        if Self::at_background_capacity(
            Self::background_task_cancels().lock().len(),
            Self::MAX_CONCURRENT_BACKGROUND_DELEGATIONS,
        ) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Too many background delegations in flight (limit {}). Wait for some to \
                     finish (check_result) or cancel one (cancel_task) before starting more.",
                    Self::MAX_CONCURRENT_BACKGROUND_DELEGATIONS
                )),
            });
        }

        let task_id = uuid::Uuid::new_v4().to_string();
        let results_dir = self.results_dir();
        tokio::fs::create_dir_all(&results_dir).await?;

        let context = args
            .get("context")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");
        let full_prompt = if context.is_empty() {
            prompt.to_string()
        } else {
            format!("[Context]\n{context}\n\n[Task]\n{prompt}")
        };

        let started_at = chrono::Utc::now().to_rfc3339();
        let agent_name_owned = agent_name.to_string();

        // Write initial "running" status
        let initial_result = BackgroundDelegateResult {
            task_id: task_id.clone(),
            agent: agent_name_owned.clone(),
            status: BackgroundTaskStatus::Running,
            output: None,
            error: None,
            started_at: started_at.clone(),
            finished_at: None,
        };
        let result_path = results_dir.join(format!("{task_id}.json"));
        Self::write_result_atomic(&result_path, &initial_result).await?;

        // EPIC-A supervision: register the task in the durable control-plane BEFORE the
        // spawn, so a crash between here and the spawn is recoverable by the reaper. A
        // no-op when not running under a booted daemon (the plane is absent).
        if let Some(cp) = crate::control_plane::control_plane() {
            let _ = cp
                .store
                .create(crate::control_plane::TaskRecord {
                    id: task_id.clone(),
                    kind: crate::control_plane::TaskKind::Delegate,
                    agent: agent_name_owned.clone(),
                    status: crate::control_plane::TaskStatus::Running,
                    owner_pid: std::process::id(),
                    owner_boot_id: cp.boot_id.clone(),
                    heartbeat_at: None,
                    depth: self.depth,
                    parent_id: None,
                    originator_route: None,
                    delivered: false,
                    idem_key: None,
                    principal_id: None,
                    started_at: started_at.clone(),
                    finished_at: None,
                })
                .await;
        }

        let agents = Arc::clone(&self.agents);
        let security = target_policy;
        let global_credential = self.global_credential.clone();
        let provider_runtime_options = self.provider_runtime_options.clone();
        // Monotonic descent: was `self.depth` (verbatim copy), which left the
        // `self.depth >= max_depth` check inert — a chain of background delegations never
        // escalated depth. Matches the documented `with_depth(parent.depth + 1)` intent.
        // Behavior change: deep background re-delegation now saturates at `max_delegation_depth`.
        let depth = self.depth + 1;
        let parent_tools = Arc::clone(&self.parent_tools);
        let runtime = self.runtime.clone();
        let multimodal_config = self.multimodal_config.clone();
        let delegate_config = self.delegate_config.clone();
        let workspace_dir = self.workspace_dir.clone();
        let child_token = self.cancellation_token.child_token();
        // Register the live token so `cancel_task` can actually abort THIS task (removed
        // when it settles, in the spawned closure below).
        Self::background_task_cancels()
            .lock()
            .insert(task_id.clone(), child_token.clone());
        let task_id_clone = task_id.clone();
        let providers_models = Arc::clone(&self.providers_models);
        let risk_profiles = Arc::clone(&self.risk_profiles);
        let runtime_profiles = Arc::clone(&self.runtime_profiles);
        let skill_bundles = Arc::clone(&self.skill_bundles);
        let root_config = self.root_config.clone();
        let caller_alias = self.caller_alias.clone();
        let memory = self.memory.clone();
        let parent_session_key = current_tool_loop_session_key();
        let __zc_delegate_alias = agent_name_owned.clone();

        zeroclaw_spawn::spawn!(
            scope_delegate_session_key(parent_session_key, async move {
                let inner = DelegateTool {
                    agents,
                    security,
                    global_credential,
                    provider_runtime_options,
                    depth,
                    parent_tools,
                    runtime,
                    multimodal_config,
                    delegate_config,
                    workspace_dir: workspace_dir.clone(),
                    cancellation_token: child_token.clone(),
                    memory,
                    providers_models,
                    risk_profiles,
                    runtime_profiles,
                    skill_bundles,
                    root_config,
                    caller_alias,
                };

                let args_inner = json!({
                    "agent": agent_name_owned,
                    "prompt": full_prompt,
                });

                // Race the delegation against cancellation
                let outcome = tokio::select! {
                    () = child_token.cancelled() => {
                        Err("Cancelled by parent session".to_string())
                    }
                    result = Box::pin(inner.execute_sync_with_admission(
                        &agent_name_owned,
                        &full_prompt,
                        &args_inner,
                        DelegateAdmission::Prevalidated,
                    )) => {
                        match result {
                            Ok(tool_result) => {
                                if tool_result.success {
                                    Ok(tool_result.output.into_string())
                                } else {
                                    Err(tool_result.error.unwrap_or_else(|| "Unknown error".into()))
                                }
                            }
                            Err(e) => Err(e.to_string()),
                        }
                    }
                };

                let finished_at = chrono::Utc::now().to_rfc3339();
                let final_result = match outcome {
                    Ok(output) => BackgroundDelegateResult {
                        task_id: task_id_clone.clone(),
                        agent: agent_name_owned,
                        status: BackgroundTaskStatus::Completed,
                        output: Some(output),
                        error: None,
                        started_at,
                        finished_at: Some(finished_at),
                    },
                    Err(err) => {
                        let status = if err.contains("Cancelled") {
                            BackgroundTaskStatus::Cancelled
                        } else {
                            BackgroundTaskStatus::Failed
                        };
                        BackgroundDelegateResult {
                            task_id: task_id_clone.clone(),
                            agent: agent_name_owned,
                            status,
                            output: None,
                            error: Some(err),
                            started_at,
                            finished_at: Some(finished_at),
                        }
                    }
                };

                let result_path = results_dir.join(format!("{}.json", task_id_clone));
                let _ = DelegateTool::write_result_atomic(&result_path, &final_result).await;

                if let Some(cp) = crate::control_plane::control_plane() {
                    let cp_status = match final_result.status {
                        BackgroundTaskStatus::Completed => {
                            crate::control_plane::TaskStatus::Completed
                        }
                        BackgroundTaskStatus::Failed => crate::control_plane::TaskStatus::Failed,
                        BackgroundTaskStatus::Cancelled => {
                            crate::control_plane::TaskStatus::Cancelled
                        }
                        BackgroundTaskStatus::Running => crate::control_plane::TaskStatus::Running,
                    };
                    let _ = cp
                        .store
                        .update_status(
                            &task_id_clone,
                            cp_status,
                            final_result.output.clone(),
                            final_result.error.clone(),
                        )
                        .await;
                }

                // Drop the live cancel token now the task has settled.
                Self::background_task_cancels()
                    .lock()
                    .remove(&task_id_clone);
            })
            .instrument(::zeroclaw_log::attribution_span!(
                &crate::agent::AgentAttribution(__zc_delegate_alias.as_str())
            ))
        );

        Ok(ToolResult {
            success: true,
            output: format!(
                "Background task started for agent '{agent_name}'.\n\
                 task_id: {task_id}\n\
                 Use action='check_result' with task_id='{task_id}' to retrieve the result."
            )
            .into(),
            error: None,
        })
    }

    // ── Parallel Execution ──────────────────────────────────────────

    /// Run multiple agents concurrently with the same prompt.
    async fn execute_parallel(
        &self,
        parallel_agents: &[serde_json::Value],
        args: &serde_json::Value,
    ) -> anyhow::Result<ToolResult> {
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "prompt"})),
                    "tool argument validation failed"
                );

                anyhow::Error::msg("Missing 'prompt' parameter for parallel execution")
            })?;

        if prompt.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("'prompt' parameter must not be empty".into()),
            });
        }

        let agent_names: Vec<String> = parallel_agents
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect();

        if agent_names.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("'parallel' array must contain at least one agent name".into()),
            });
        }

        // Validate all agents exist before starting any
        for name in &agent_names {
            if !self.agents.contains_key(name) {
                let available: Vec<&str> =
                    self.agents.keys().map(|s: &String| s.as_str()).collect();
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "Unknown agent '{name}' in parallel list. Available: {}",
                        if available.is_empty() {
                            "(none configured)".to_string()
                        } else {
                            available.join(", ")
                        }
                    )),
                });
            }
        }

        for name in &agent_names {
            // Validate the whole fan-out before any spawn. A single blocked
            // target should fail the entire parallel request rather than
            // launching a partial set of child agents and then reporting mixed
            // results.
            if let Err(e) = self.policy_for_target(name) {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("{e:#}")),
                });
            }
            if let Some(refusal) = self.independent_always_ask_refusal(name) {
                return Ok(refusal);
            }
        }

        let parent_receipt_scope = crate::agent::tool_receipts::TOOL_LOOP_RECEIPT_CONTEXT
            .try_with(Clone::clone)
            .ok()
            .flatten();
        let parent_session_key = current_tool_loop_session_key();

        // Spawn all agents concurrently
        let mut handles = Vec::with_capacity(agent_names.len());
        for agent_name in &agent_names {
            let agents = Arc::clone(&self.agents);
            let security = Arc::clone(&self.security);
            let global_credential = self.global_credential.clone();
            let provider_runtime_options = self.provider_runtime_options.clone();
            // Monotonic descent on the parallel path — was `self.depth` (verbatim copy),
            // leaving the `>= max_depth` check inert (see the background path above).
            // Behavior change: deep parallel re-delegation now saturates at `max_delegation_depth`.
            let depth = self.depth + 1;
            let parent_tools = Arc::clone(&self.parent_tools);
            let runtime = self.runtime.clone();
            let multimodal_config = self.multimodal_config.clone();
            let delegate_config = self.delegate_config.clone();
            let workspace_dir = self.workspace_dir.clone();
            let cancellation_token = self.cancellation_token.child_token();
            let agent_name = agent_name.clone();
            let prompt = prompt.to_string();
            let args_clone = args.clone();
            let providers_models = Arc::clone(&self.providers_models);
            let risk_profiles = Arc::clone(&self.risk_profiles);
            let runtime_profiles = Arc::clone(&self.runtime_profiles);
            let skill_bundles = Arc::clone(&self.skill_bundles);
            let receipt_scope = parent_receipt_scope.clone();
            let root_config = self.root_config.clone();
            let caller_alias = self.caller_alias.clone();
            let session_key = parent_session_key.clone();
            let memory = self.memory.clone();
            let __zc_delegate_alias = agent_name.clone();

            handles.push(zeroclaw_spawn::spawn!(
                async move {
                    let inner = DelegateTool {
                        agents,
                        security,
                        global_credential,
                        provider_runtime_options,
                        depth,
                        parent_tools,
                        runtime,
                        multimodal_config,
                        delegate_config,
                        workspace_dir,
                        cancellation_token,
                        memory,
                        providers_models,
                        risk_profiles,
                        runtime_profiles,
                        skill_bundles,
                        root_config,
                        caller_alias,
                    };
                    let agent_name_for_return = agent_name.clone();
                    let result = scope_delegate_session_key(session_key, async move {
                        crate::agent::tool_receipts::TOOL_LOOP_RECEIPT_CONTEXT
                            .scope(receipt_scope, async move {
                                Box::pin(inner.execute_sync(&agent_name, &prompt, &args_clone))
                                    .await
                            })
                            .await
                    })
                    .await;
                    (agent_name_for_return, result)
                }
                .instrument(::zeroclaw_log::attribution_span!(
                    &crate::agent::AgentAttribution(__zc_delegate_alias.as_str())
                ))
            ));
        }

        // Collect all results
        let mut outputs = Vec::with_capacity(handles.len());
        let mut all_success = true;

        for handle in handles {
            match handle.await {
                Ok((agent_name, Ok(tool_result))) => {
                    if !tool_result.success {
                        all_success = false;
                    }
                    outputs.push(format!(
                        "--- {agent_name} (success={}) ---\n{}{}",
                        tool_result.success,
                        tool_result.output,
                        tool_result
                            .error
                            .map(|e| format!("\nError: {e}"))
                            .unwrap_or_default()
                    ));
                }
                Ok((agent_name, Err(e))) => {
                    all_success = false;
                    outputs.push(format!("--- {agent_name} (success=false) ---\nError: {e}"));
                }
                Err(e) => {
                    all_success = false;
                    outputs.push(format!("--- [join error] ---\n{e}"));
                }
            }
        }

        Ok(ToolResult {
            success: all_success,
            output: format!(
                "[Parallel delegation: {} agents]\n\n{}",
                agent_names.len(),
                outputs.join("\n\n")
            )
            .into(),
            error: if all_success {
                None
            } else {
                Some("One or more parallel agents failed".into())
            },
        })
    }

    // ── Result Retrieval ────────────────────────────────────────────

    async fn reconciled_loss_label(
        task_id: &str,
        file_status: &BackgroundTaskStatus,
    ) -> Option<&'static str> {
        let cp = crate::control_plane::control_plane()?;
        Self::reconciled_loss_label_with(task_id, file_status, cp.store.as_ref()).await
    }

    /// Store-injected core of [`Self::reconciled_loss_label`] — kept separate from the
    /// process-global accessor so it is unit-testable against an in-memory store.
    async fn reconciled_loss_label_with(
        task_id: &str,
        file_status: &BackgroundTaskStatus,
        store: &dyn crate::control_plane::TaskRegistry,
    ) -> Option<&'static str> {
        if *file_status != BackgroundTaskStatus::Running {
            return None;
        }
        match store.get(task_id).await.ok().flatten()?.status {
            crate::control_plane::TaskStatus::Lost => Some("lost"),
            crate::control_plane::TaskStatus::TimedOut => Some("timed_out"),
            _ => None,
        }
    }

    async fn read_background_result(
        &self,
        task_id: &str,
    ) -> anyhow::Result<Option<BackgroundDelegateResult>> {
        let result_path = self.results_dir().join(format!("{task_id}.json"));
        let content = match tokio::fs::read_to_string(&result_path).await {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let result = serde_json::from_str(&content)?;
        Ok(Some(result))
    }

    async fn background_result_view(
        task_id: &str,
        result: BackgroundDelegateResult,
    ) -> anyhow::Result<(BackgroundResultState, serde_json::Value)> {
        if let Some(label) = Self::reconciled_loss_label(task_id, &result.status).await {
            let state = match label {
                "lost" => BackgroundResultState::Lost,
                "timed_out" => BackgroundResultState::TimedOut,
                _ => BackgroundResultState::from_file_status(&result.status),
            };
            return Ok((
                state,
                json!({
                    "task_id": task_id,
                    "agent": result.agent,
                    "status": label,
                    "started_at": result.started_at,
                    "note": "the owning daemon exited or the task exceeded its max runtime; \
                             reconciled by the supervision reaper",
                }),
            ));
        }
        let state = BackgroundResultState::from_file_status(&result.status);
        Ok((state, serde_json::to_value(result)?))
    }

    fn task_ids_from_args(args: &serde_json::Value) -> anyhow::Result<Vec<String>> {
        let values = args
            .get("task_ids")
            .and_then(|value| value.as_array())
            .ok_or_else(|| anyhow::Error::msg("Missing 'task_ids' parameter for await_sessions"))?;
        if values.len() > Self::MAX_AWAIT_SESSION_TASK_IDS {
            return Err(anyhow::Error::msg(format!(
                "'task_ids' must contain no more than {} task ids",
                Self::MAX_AWAIT_SESSION_TASK_IDS
            )));
        }
        let mut task_ids = Vec::with_capacity(values.len());
        let mut seen = HashSet::with_capacity(values.len());
        for value in values {
            let Some(task_id) = value.as_str() else {
                return Err(anyhow::Error::msg("'task_ids' must contain only strings"));
            };
            Self::validate_task_id(task_id).map_err(anyhow::Error::msg)?;
            if !seen.insert(task_id) {
                return Err(anyhow::Error::msg(format!(
                    "Duplicate task_id '{task_id}' in task_ids"
                )));
            }
            task_ids.push(task_id.to_string());
        }
        if task_ids.is_empty() {
            return Err(anyhow::Error::msg(
                "'task_ids' must contain at least one task id",
            ));
        }
        Ok(task_ids)
    }

    fn await_timeout(args: &serde_json::Value) -> anyhow::Result<Duration> {
        let Some(value) = args.get("timeout_ms") else {
            return Ok(Duration::from_millis(30_000));
        };
        let Some(timeout_ms) = value.as_u64() else {
            return Err(anyhow::Error::msg("'timeout_ms' must be an integer"));
        };
        let timeout = Duration::from_millis(timeout_ms);
        if timeout > Self::MAX_AWAIT_SESSIONS_TIMEOUT {
            return Err(anyhow::Error::msg(format!(
                "'timeout_ms' must be no more than {}",
                Self::MAX_AWAIT_SESSIONS_TIMEOUT.as_millis()
            )));
        }
        Ok(timeout)
    }

    /// Retrieve the result of a background delegate task by task_id.
    async fn handle_check_result(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let task_id = args
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "task_id"})),
                    "tool argument validation failed"
                );

                anyhow::Error::msg("Missing 'task_id' parameter for check_result")
            })?;

        if let Err(e) = Self::validate_task_id(task_id) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(e),
            });
        }

        let Some(result) = self.read_background_result(task_id).await? else {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("No result found for task_id '{task_id}'")),
            });
        };
        let error = result.error.clone();
        let (state, value) = Self::background_result_view(task_id, result).await?;
        let success = state.is_success();

        Ok(ToolResult {
            success,
            output: serde_json::to_string_pretty(&value)?.into(),
            error: if success {
                None
            } else if let Some(error) = error {
                Some(error)
            } else if state.is_failure() {
                Some(format!(
                    "background task is {} and will not complete",
                    state.as_str()
                ))
            } else {
                None
            },
        })
    }

    async fn handle_await_sessions(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let task_ids = match Self::task_ids_from_args(args) {
            Ok(task_ids) => task_ids,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new().into(),
                    error: Some(error.to_string()),
                });
            }
        };
        let timeout = match Self::await_timeout(args) {
            Ok(timeout) => timeout,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new().into(),
                    error: Some(error.to_string()),
                });
            }
        };
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let mut results = Vec::new();
            let mut pending = Vec::new();
            let mut missing = Vec::new();
            let mut failed = Vec::new();

            for task_id in &task_ids {
                let Some(result) = self.read_background_result(task_id).await? else {
                    missing.push(task_id.clone());
                    continue;
                };
                let (state, value) = Self::background_result_view(task_id, result).await?;
                if state.is_pending() {
                    pending.push(task_id.clone());
                } else if state.is_failure() {
                    failed.push(task_id.clone());
                }
                results.push(value);
            }

            let waiting = !pending.is_empty() || !missing.is_empty();
            let timed_out = waiting && tokio::time::Instant::now() >= deadline;
            if !waiting || timed_out {
                let completed = results
                    .iter()
                    .filter(|result| result.get("status") == Some(&json!("completed")))
                    .count();
                let success = missing.is_empty() && pending.is_empty() && failed.is_empty();
                let error = if success {
                    None
                } else if timed_out {
                    Some("one or more background tasks are still pending or missing".into())
                } else {
                    Some("one or more background tasks failed or were cancelled".into())
                };
                return Ok(ToolResult {
                    success,
                    output: serde_json::to_string_pretty(&json!({
                        "status": if timed_out { "timeout" } else { "complete" },
                        "completed": completed,
                        "pending": pending,
                        "missing": missing,
                        "failed": failed,
                        "results": results,
                    }))?
                    .into(),
                    error,
                });
            }

            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// List all background delegate task results.
    async fn handle_list_results(&self) -> anyhow::Result<ToolResult> {
        let results_dir = self.results_dir();
        if !results_dir.exists() {
            return Ok(ToolResult {
                success: true,
                output: "No background delegate results found.".into(),
                error: None,
            });
        }

        let mut entries = tokio::fs::read_dir(&results_dir).await?;
        let mut results = Vec::new();

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json")
                && let Ok(content) = tokio::fs::read_to_string(&path).await
                && let Ok(result) = serde_json::from_str::<BackgroundDelegateResult>(&content)
            {
                // Surface the reconciled loss state (lost/timed_out) for a task whose flat
                // file still says `Running` but whose owning daemon died / timed out.
                let status =
                    match Self::reconciled_loss_label(&result.task_id, &result.status).await {
                        Some(label) => json!(label),
                        None => json!(result.status),
                    };
                results.push(json!({
                    "task_id": result.task_id,
                    "agent": result.agent,
                    "status": status,
                    "started_at": result.started_at,
                    "finished_at": result.finished_at,
                }));
            }
        }

        if results.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No background delegate results found.".into(),
                error: None,
            });
        }

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&results)?.into(),
            error: None,
        })
    }

    fn background_task_cancels() -> &'static parking_lot::Mutex<HashMap<String, CancellationToken>>
    {
        static M: std::sync::OnceLock<parking_lot::Mutex<HashMap<String, CancellationToken>>> =
            std::sync::OnceLock::new();
        M.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
    }

    /// Runaway backstop: the maximum number of background delegations allowed in flight at
    /// once across the process. Each is a full agent loop, so this guards against a model
    /// (or a runaway loop) spawning unbounded background agent runs; normal use stays well
    /// under it.
    const MAX_CONCURRENT_BACKGROUND_DELEGATIONS: usize = 128;

    /// Pure predicate for the runaway backstop — separated from the live token-map read so
    /// it is unit-testable. `cap == 0` disables the backstop.
    fn at_background_capacity(in_flight: usize, cap: usize) -> bool {
        cap != 0 && in_flight >= cap
    }

    /// Cancel a running background task by task_id.
    async fn handle_cancel_task(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let task_id = args
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "task_id"})),
                    "tool argument validation failed"
                );

                anyhow::Error::msg("Missing 'task_id' parameter for cancel_task")
            })?;

        if let Err(e) = Self::validate_task_id(task_id) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(e),
            });
        }

        let result_path = self.results_dir().join(format!("{task_id}.json"));
        if !result_path.exists() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("No task found for task_id '{task_id}'")),
            });
        }

        // Read current status
        let content = tokio::fs::read_to_string(&result_path).await?;
        let mut result: BackgroundDelegateResult = serde_json::from_str(&content)?;

        if result.status != BackgroundTaskStatus::Running {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Task '{task_id}' is not running (status: {:?})",
                    result.status
                )),
            });
        }

        // Actually abort the running task by signalling its registered cancel token —
        // this cascades into the task's `tokio::select!`, which settles it as Cancelled.
        // Falls back to file-marking when the task already settled (token absent).
        let aborted = Self::background_task_cancels()
            .lock()
            .remove(task_id)
            .inspect(CancellationToken::cancel)
            .is_some();

        result.status = BackgroundTaskStatus::Cancelled;
        result.error = Some("Cancelled by user request".into());
        result.finished_at = Some(chrono::Utc::now().to_rfc3339());
        Self::write_result_atomic(&result_path, &result).await?;

        // Reconcile the durable supervision registry so the supervised view agrees.
        if let Some(cp) = crate::control_plane::control_plane() {
            let _ = cp
                .store
                .update_status(
                    task_id,
                    crate::control_plane::TaskStatus::Cancelled,
                    None,
                    Some("cancelled by user request".into()),
                )
                .await;
        }

        Ok(ToolResult {
            success: true,
            output: if aborted {
                format!("Task '{task_id}' cancelled: the running task was aborted.").into()
            } else {
                format!("Task '{task_id}' marked cancelled (it had already settled).").into()
            },
            error: None,
        })
    }

    /// Cancel all background tasks (cascade control).
    /// Call this when the parent session ends.
    pub fn cancel_all_background_tasks(&self) {
        self.cancellation_token.cancel();
    }

    fn compose_independent_system_prompt(
        base: Option<String>,
        mut deferred_section: String,
        native_tools: bool,
        strict_tool_parsing: bool,
    ) -> Option<String> {
        let mut ignored_tool_descs: Vec<(&str, &str)> = Vec::new();
        apply_text_tool_prompt_policy(
            native_tools,
            strict_tool_parsing,
            &mut ignored_tool_descs,
            &mut deferred_section,
        );
        if deferred_section.is_empty() {
            return base;
        }
        match base {
            Some(mut p) => {
                p.push_str("\n\n");
                p.push_str(&deferred_section);
                Some(p)
            }
            None => Some(deferred_section),
        }
    }

    fn build_enriched_system_prompt(
        &self,
        agent_alias: &str,
        agent_config: &AliasedAgentConfig,
        model_name: &str,
        sub_tools: &[Box<dyn Tool>],
        workspace_dir: &Path,
        sends_native_tool_specs: bool,
        skills_override: Option<&[crate::skills::Skill]>,
    ) -> Option<String> {
        let resolved_skills: Vec<crate::skills::Skill>;
        let skills: &[crate::skills::Skill] = match skills_override {
            Some(s) => s,
            None => {
                let bundle_dirs = self.resolve_skill_bundle_dirs(&agent_config.skill_bundles);
                resolved_skills = if bundle_dirs.is_empty() {
                    let default_dir = crate::skills::skills_dir(workspace_dir);
                    crate::skills::load_skills_from_directory(&default_dir, false).0
                } else {
                    bundle_dirs
                        .into_iter()
                        .flat_map(|dir| {
                            crate::skills::load_skills_from_directory(
                                &workspace_dir.join(dir),
                                false,
                            )
                            .0
                        })
                        .collect()
                };
                &resolved_skills
            }
        };

        // Determine shell policy instructions when the `shell` tool is in the
        // effective tool list.
        let empty_tools: &[Box<dyn Tool>] = &[];
        let expose_text_tools =
            sends_native_tool_specs || !agent_config.resolved.strict_tool_parsing;
        let prompt_tools = if expose_text_tools {
            sub_tools
        } else {
            empty_tools
        };
        let has_shell = prompt_tools.iter().any(|t| t.name() == "shell");
        let shell_policy = if has_shell {
            "## Shell Policy\n\n\
             - Prefer non-destructive commands. Use `trash` over `rm` where possible.\n\
             - Do not run commands that exfiltrate data or modify system-critical paths.\n\
             - Avoid interactive commands that block on stdin.\n\
             - Quote paths that may contain spaces."
                .to_string()
        } else {
            String::new()
        };

        // Build structured operational context using SystemPromptBuilder sections.
        let ctx = PromptContext {
            workspace_dir,
            agent_workspace_dir: workspace_dir,
            model_name,
            tools: prompt_tools,
            skills,
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            identity_config: None,
            dispatcher_instructions: "",
            sends_native_tool_specs: sends_native_tool_specs && !prompt_tools.is_empty(),

            security_summary: None,
            autonomy_level: crate::security::AutonomyLevel::default(),
        };

        let builder = SystemPromptBuilder::default()
            .add_section(Box::new(crate::agent::prompt::ToolsSection))
            .add_section(Box::new(crate::agent::prompt::SafetySection))
            .add_section(Box::new(crate::agent::prompt::SkillsSection))
            .add_section(Box::new(crate::agent::prompt::WorkspaceSection))
            .add_section(Box::new(crate::agent::prompt::DateTimeSection));

        let mut enriched = builder.build(&ctx).unwrap_or_default();

        if !shell_policy.is_empty() {
            enriched.push_str(&shell_policy);
            enriched.push_str("\n\n");
        }

        if let Some(target_workspace) = self.agent_workspace(agent_alias) {
            let identity_files = [
                "AGENTS.md",
                "SOUL.md",
                "IDENTITY.md",
                "USER.md",
                "BOOTSTRAP.md",
            ];
            for filename in identity_files {
                let path = target_workspace.join(filename);
                if let Ok(contents) = std::fs::read_to_string(&path) {
                    let trimmed = contents.trim();
                    if !trimmed.is_empty() {
                        enriched.push_str(trimmed);
                        enriched.push_str("\n\n");
                    }
                }
            }
        }

        let trimmed = enriched.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }

    #[cfg(test)]
    async fn execute_agentic(
        &self,
        agent_name: &str,
        agent_config: &AliasedAgentConfig,
        provider_type: &str,
        model: &str,
        model_provider: &dyn ModelProvider,
        full_prompt: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ToolResult> {
        self.execute_agentic_with_admission(
            agent_name,
            agent_config,
            provider_type,
            model,
            model_provider,
            full_prompt,
            temperature,
            DelegateAdmission::Required,
        )
        .await
    }

    async fn execute_agentic_with_admission(
        &self,
        agent_name: &str,
        agent_config: &AliasedAgentConfig,
        provider_type: &str,
        model: &str,
        model_provider: &dyn ModelProvider,
        full_prompt: &str,
        temperature: Option<f64>,
        admission: DelegateAdmission,
    ) -> anyhow::Result<ToolResult> {
        let Some(tool_policy) = self.resolve_tool_policy(&agent_config.risk_profile) else {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Agent '{agent_name}' is agentic but risk_profile '{}' is not configured",
                    agent_config.risk_profile
                )),
            });
        };

        let target_policy = match admission {
            DelegateAdmission::Required => match self.policy_for_target(agent_name) {
                Ok(policy) => policy,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(format!("{e:#}")),
                    });
                }
            },
            DelegateAdmission::Prevalidated => Arc::clone(&self.security),
        };
        let target_mode = self.mode_for_target(agent_name);
        // Deferred-MCP side-channels for an INDEPENDENT target: its sub-agent turn must
        // inject the deferred-tools prompt section and thread the activated set, exactly as
        // a fresh target turn does. Bounded delegation leaves these empty (it starts from
        // the parent's already-built registry, not the target's assembled one).
        let mut sub_deferred_section = String::new();
        let mut sub_activated: Option<Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>> = None;
        // For an INDEPENDENT target, build the sub-agent's system prompt (skills, identity)
        // from the TARGET's workspace, not the caller's - so skill *prompt* content matches
        // the skill *tools* assembled above. `None` for bounded delegation, which keeps the
        // caller's `self.workspace_dir`.
        let mut sub_workspace: Option<PathBuf> = None;
        // The target's canonical skills (Some for independent), so the prompt's SkillsSection
        // describes exactly the assembled skill tools rather than the local bundle resolver's
        // narrower view. None for bounded delegation (local resolution).
        let mut sub_skills: Option<Vec<crate::skills::Skill>> = None;
        let sub_tools: Vec<Box<dyn Tool>> = match target_mode {
            DelegateExecutionMode::Independent => {
                match self
                    .independent_agentic_tools_for_target(agent_name, Arc::clone(&target_policy))
                    .await
                {
                    Ok(independent) => {
                        sub_deferred_section = independent.deferred_section;
                        sub_activated = independent.activated_handle;
                        sub_workspace = Some(independent.workspace_dir);
                        sub_skills = Some(independent.skills);
                        independent.tools
                    }
                    Err(e) => {
                        return Ok(ToolResult {
                            success: false,
                            output: ToolOutput::default(),
                            error: Some(format!(
                                "Failed to initialize independent delegate tools for target '{agent_name}': {e:#}"
                            )),
                        });
                    }
                }
            }
            DelegateExecutionMode::Bounded => {
                let needs_memory_tools = {
                    let parent_tools = self.parent_tools.read();
                    parent_tools.iter().any(|tool| {
                        self.security.is_tool_allowed(tool.name())
                            && zeroclaw_tools::MEMORY_TOOL_NAMES.contains(&tool.name())
                            && Self::delegate_admits_with_mcp(&tool_policy, tool.name())
                    })
                };
                let mut target_memory_tools: HashMap<String, Box<dyn Tool>> = if needs_memory_tools
                {
                    match self.memory_for_target_agent(agent_name).await {
                        Ok(Some(memory)) => Self::memory_tools_for_target(memory, target_policy)
                            .into_iter()
                            .map(|tool| (tool.name().to_string(), tool))
                            .collect(),
                        Ok(None) => HashMap::new(),
                        Err(e) => {
                            return Ok(ToolResult {
                                success: false,
                                output: ToolOutput::default(),
                                error: Some(format!(
                                    "Failed to initialize memory for delegate target '{agent_name}': {e:#}"
                                )),
                            });
                        }
                    }
                } else {
                    HashMap::new()
                };

                let parent_tools = self.parent_tools.read();
                parent_tools
                    .iter()
                    .filter(|tool| tool.name() != Self::NAME)
                    .filter(|tool| self.security.is_tool_allowed(tool.name()))
                    .filter(|tool| Self::delegate_admits_with_mcp(&tool_policy, tool.name()))
                    .map(|tool| {
                        target_memory_tools.remove(tool.name()).unwrap_or_else(|| {
                            Box::new(ToolArcRef::new(tool.clone())) as Box<dyn Tool>
                        })
                    })
                    .collect()
            }
        };

        let loop_runtime = self.resolve_loop_runtime(agent_name, agent_config);
        let mut prompt_agent_config = agent_config.clone();
        prompt_agent_config.resolved = loop_runtime.clone();

        // Build enriched system prompt with tools, skills, workspace, datetime context.
        // Independent delegation builds it from the TARGET's workspace (`sub_workspace`), so
        // the skill prompt content matches the target's skill tools; bounded delegation
        // keeps the caller's `self.workspace_dir`.
        let prompt_workspace = sub_workspace.as_deref().unwrap_or(&self.workspace_dir);
        let enriched_system_prompt = self.build_enriched_system_prompt(
            agent_name,
            &prompt_agent_config,
            model,
            &sub_tools,
            prompt_workspace,
            model_provider.supports_native_tools(),
            sub_skills.as_deref(),
        );
        // Independent delegates surface the target's deferred MCP tools the way a fresh
        // target turn does. See `compose_independent_system_prompt`: it applies the turn
        // engine's text-tool prompt policy to the deferred section (so a non-native strict
        // target suppresses it, exactly as a fresh turn would) and then appends it.
        let enriched_system_prompt = Self::compose_independent_system_prompt(
            enriched_system_prompt,
            sub_deferred_section,
            model_provider.supports_native_tools(),
            loop_runtime.strict_tool_parsing,
        );

        let mut history = Vec::new();
        if let Some(system_prompt) = enriched_system_prompt.as_ref() {
            history.push(ChatMessage::system(system_prompt.clone()));
        }
        history.push(ChatMessage::user(full_prompt.to_string()));

        let noop_observer = NoopObserver;

        let agentic_timeout_secs = self
            .resolve_agentic_timeout_secs(&agent_config.runtime_profile)
            .unwrap_or(self.delegate_config.agentic_timeout_secs);
        let receipt_scope = crate::agent::tool_receipts::TOOL_LOOP_RECEIPT_CONTEXT
            .try_with(Clone::clone)
            .ok()
            .flatten();
        let receipt_generator = receipt_scope.as_ref().map(|s| &s.generator);
        let collected_receipts = receipt_scope.as_ref().map(|s| s.collector.as_ref());
        let turn_id = uuid::Uuid::new_v4().to_string();
        let result = tokio::time::timeout(
            Duration::from_secs(agentic_timeout_secs),
            run_tool_call_loop(ToolLoop {
                sop_reassembly: None,
                exec: ResolvedAgentExecution::resolve(
                    ResolvedModelAccess {
                        model_provider,
                        provider_name: provider_type,
                        model,
                        temperature,
                    },
                    ResolvedIo {
                        tools_registry: &sub_tools,
                        observer: &noop_observer,
                        silent: true,
                        approval: None,
                        multimodal_config: &self.multimodal_config,
                        // Full config so the delegated sub-agent's vision route
                        // resolves the configured `vision_model_provider`'s alias
                        // options (the `vision` override, endpoint URI, credentials),
                        // exactly as the parent turn does. `None` only on the
                        // configless test builder (`root_config` unset).
                        config: self.root_config.as_deref(),
                        hooks: None,
                        // Thread the target's deferred-MCP activated set so `tool_search`
                        // can activate the target's deferred tools mid-turn (Some only for
                        // an independent target with granted deferred-MCP bundles).
                        activated_tools: sub_activated.as_ref(),
                        model_switch_callback: None,
                        // delegate subagents don't support approval
                        receipt_generator,
                    },
                    ResolvedRuntimeKnobs {
                        max_tool_iterations: loop_runtime.max_tool_iterations,
                        excluded_tools: &[],
                        dedup_exempt_tools: tool_policy.excluded_tools.as_deref().unwrap_or(&[]),
                        pacing: &zeroclaw_config::schema::PacingConfig::default(),
                        strict_tool_parsing: loop_runtime.strict_tool_parsing,
                        parallel_tools: loop_runtime.parallel_tools,
                        max_tool_result_chars: loop_runtime.max_tool_result_chars,
                        // Keep delegate subagent context pruning aligned with top-level
                        // agents instead of preserving the old disabled-by-zero path.
                        context_token_budget: loop_runtime.max_context_tokens,
                        knobs: &LoopKnobs::default(),
                    },
                ),
                history: &mut history,
                channel_name: "delegate",
                channel_reply_target: None,
                cancellation_token: Some(self.cancellation_token.child_token()),
                on_delta: None,
                shared_budget: None,
                // TODO thread from parent in future
                channel: None,
                collected_receipts,
                event_tx: None,
                steering: None,
                new_messages_out: None,
                image_cache: None,
                // Phase 1: stamp Internal/Trusted. Per-transport
                // stamping lands in a later phase.
                memory: None,
                ingress: zeroclaw_api::ingress::IngressContext::sub_turn(),
                agent_alias: Some(agent_name),
                parent_agent_alias: None,
                turn_id: &turn_id,
            })
            .instrument(::zeroclaw_log::attribution_span!(
                &crate::agent::AgentAttribution(agent_name)
            )),
        )
        .await;

        match result {
            Ok(Ok(response)) => {
                let rendered = if response.trim().is_empty() {
                    "[Empty response]".to_string()
                } else {
                    response
                };

                Ok(ToolResult {
                    success: true,
                    output: format!(
                        "[Agent '{agent_name}' ({provider_type}/{model}, agentic)]\n{rendered}",
                    )
                    .into(),
                    error: None,
                })
            }
            Ok(Err(e)) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Agent '{agent_name}' failed: {e}")),
            }),
            Err(_) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Agent '{agent_name}' timed out after {agentic_timeout_secs}s"
                )),
            }),
        }
    }
}

struct ToolArcRef {
    inner: Arc<dyn Tool>,
}

impl ToolArcRef {
    fn new(inner: Arc<dyn Tool>) -> Self {
        Self { inner }
    }
}

impl ::zeroclaw_api::attribution::Attributable for ToolArcRef {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        self.inner.role()
    }
    fn alias(&self) -> &str {
        self.inner.alias()
    }
}

#[async_trait]
impl Tool for ToolArcRef {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.inner.parameters_schema()
    }

    fn output_schema(&self) -> Option<serde_json::Value> {
        self.inner.output_schema()
    }

    fn param_domains(&self) -> Vec<(&'static str, ::zeroclaw_api::tool::OptionDomain)> {
        self.inner.param_domains()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        self.inner.execute(args).await
    }
}

struct NoopObserver;

impl Observer for NoopObserver {
    fn record_event(&self, _event: &ObserverEvent) {}

    fn record_metric(&self, _metric: &ObserverMetric) {}

    fn name(&self) -> &str {
        "noop"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::RuntimeAdapter;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use crate::tools::{MemoryRecallTool, MemoryStoreTool};
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;
    use tokio::time::{Instant, sleep};
    use zeroclaw_config::schema::{
        Config, CustomModelProviderConfig, DEFAULT_DELEGATE_AGENTIC_TIMEOUT_SECS,
        DEFAULT_DELEGATE_TIMEOUT_SECS, DelegateExecutionMode, DelegateTargetConfig,
        ModelProviderConfig,
    };
    use zeroclaw_memory::{AgentScopedMemory, SqliteMemory};
    use zeroclaw_providers::{ChatRequest, ChatResponse, ToolCall};

    zeroclaw_api::mock_tool_attribution!(EchoTool, FakeMcpTool);

    #[tokio::test]
    async fn reconciled_loss_label_surfaces_registry_truth() {
        use crate::control_plane::{
            SqliteTaskStore, TaskKind, TaskRecord, TaskRegistry, TaskStatus,
        };
        let store = SqliteTaskStore::new_in_memory().unwrap();
        let rec = |id: &str, status: TaskStatus| TaskRecord {
            id: id.into(),
            kind: TaskKind::Delegate,
            agent: "main".into(),
            status,
            owner_pid: 0,
            owner_boot_id: "b".into(),
            heartbeat_at: None,
            depth: 0,
            parent_id: None,
            originator_route: None,
            delivered: false,
            idem_key: None,
            principal_id: None,
            started_at: "2026-06-21T00:00:00Z".into(),
            finished_at: None,
        };
        store.create(rec("lost", TaskStatus::Lost)).await.unwrap();
        store
            .create(rec("timed", TaskStatus::TimedOut))
            .await
            .unwrap();
        store
            .create(rec("alive", TaskStatus::Running))
            .await
            .unwrap();

        // Flat file says Running + registry reconciled to a loss state → surface the loss.
        assert_eq!(
            DelegateTool::reconciled_loss_label_with(
                "lost",
                &BackgroundTaskStatus::Running,
                &store
            )
            .await,
            Some("lost")
        );
        assert_eq!(
            DelegateTool::reconciled_loss_label_with(
                "timed",
                &BackgroundTaskStatus::Running,
                &store
            )
            .await,
            Some("timed_out")
        );
        // Registry still Running → nothing to overlay.
        assert_eq!(
            DelegateTool::reconciled_loss_label_with(
                "alive",
                &BackgroundTaskStatus::Running,
                &store
            )
            .await,
            None
        );
        // The flat file already wrote a terminal state → it is authoritative, no overlay.
        assert_eq!(
            DelegateTool::reconciled_loss_label_with(
                "lost",
                &BackgroundTaskStatus::Completed,
                &store
            )
            .await,
            None
        );
        // Unknown task → None.
        assert_eq!(
            DelegateTool::reconciled_loss_label_with(
                "missing",
                &BackgroundTaskStatus::Running,
                &store
            )
            .await,
            None
        );
    }

    #[tokio::test]
    async fn background_cancel_token_aborts_and_clears() {
        let token = CancellationToken::new();
        let key = "test-cancel-unique-1";
        DelegateTool::background_task_cancels()
            .lock()
            .insert(key.into(), token.clone());
        // cancel_task-style lookup: remove + signal the live token
        let aborted = DelegateTool::background_task_cancels()
            .lock()
            .remove(key)
            .inspect(CancellationToken::cancel)
            .is_some();
        assert!(aborted, "a registered task token is found and aborted");
        assert!(
            token.is_cancelled(),
            "the running task's token is signalled"
        );
        assert!(
            DelegateTool::background_task_cancels()
                .lock()
                .remove(key)
                .is_none(),
            "the token is gone after cancellation"
        );
        // An unknown id is a no-op (cancel_task falls back to file-marking).
        assert!(
            DelegateTool::background_task_cancels()
                .lock()
                .remove("test-cancel-missing")
                .is_none()
        );
    }

    #[test]
    fn background_capacity_backstop() {
        assert!(!DelegateTool::at_background_capacity(0, 128));
        assert!(!DelegateTool::at_background_capacity(127, 128));
        assert!(DelegateTool::at_background_capacity(128, 128));
        assert!(DelegateTool::at_background_capacity(200, 128));
        // cap 0 disables the backstop
        assert!(!DelegateTool::at_background_capacity(10_000, 0));
    }

    struct DelegateTestRuntime;

    impl RuntimeAdapter for DelegateTestRuntime {
        fn name(&self) -> &str {
            "delegate-test-runtime"
        }

        fn has_shell_access(&self) -> bool {
            true
        }

        fn has_filesystem_access(&self) -> bool {
            true
        }

        fn storage_path(&self) -> PathBuf {
            std::env::temp_dir()
        }

        fn supports_long_running(&self) -> bool {
            false
        }

        fn build_shell_command(
            &self,
            command: &str,
            workspace_dir: &Path,
        ) -> anyhow::Result<tokio::process::Command> {
            let mut cmd = tokio::process::Command::new("echo");
            cmd.arg(command);
            cmd.current_dir(workspace_dir);
            Ok(cmd)
        }
    }

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    fn security_allowing() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            delegation_policy: zeroclaw_config::autonomy::DelegationPolicy {
                mode: zeroclaw_config::autonomy::DelegationMode::Allow,
            },
            ..SecurityPolicy::default()
        })
    }

    fn sample_agents() -> HashMap<String, AliasedAgentConfig> {
        let mut agents = HashMap::new();
        agents.insert(
            "researcher".to_string(),
            AliasedAgentConfig {
                model_provider: "ollama.researcher".into(),
                ..Default::default()
            },
        );
        agents.insert(
            "coder".to_string(),
            AliasedAgentConfig {
                model_provider: "openrouter.coder".into(),
                ..Default::default()
            },
        );
        agents
    }

    async fn wait_for_terminal_background_result(
        workspace: &Path,
        task_id: &str,
    ) -> BackgroundDelegateResult {
        let result_path = workspace
            .join("delegate_results")
            .join(format!("{task_id}.json"));
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut last_result = None;

        loop {
            if let Ok(content) = std::fs::read_to_string(&result_path)
                && let Ok(result) = serde_json::from_str::<BackgroundDelegateResult>(&content)
            {
                if result.status != BackgroundTaskStatus::Running {
                    return result;
                }
                last_result = Some(result);
            }

            if Instant::now() >= deadline {
                panic!(
                    "Background task {task_id} did not finish before timeout; last result: {last_result:?}"
                );
            }

            sleep(Duration::from_millis(50)).await;
        }
    }

    fn background_result(
        task_id: &str,
        status: BackgroundTaskStatus,
        output: Option<&str>,
        error: Option<&str>,
    ) -> BackgroundDelegateResult {
        let finished_at = if status == BackgroundTaskStatus::Running {
            None
        } else {
            Some("2026-06-29T12:00:01Z".to_string())
        };
        BackgroundDelegateResult {
            task_id: task_id.to_string(),
            agent: "researcher".to_string(),
            status,
            output: output.map(str::to_string),
            error: error.map(str::to_string),
            started_at: "2026-06-29T12:00:00Z".to_string(),
            finished_at,
        }
    }

    fn write_background_result(workspace: &Path, result: &BackgroundDelegateResult) {
        let results_dir = workspace.join("delegate_results");
        std::fs::create_dir_all(&results_dir).unwrap();
        std::fs::write(
            results_dir.join(format!("{}.json", result.task_id)),
            serde_json::to_vec_pretty(result).unwrap(),
        )
        .unwrap();
    }

    #[derive(Default)]
    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo_tool"
        }

        fn description(&self) -> &str {
            "Echoes the `value` argument."
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "value": {"type": "string"}
                },
                "required": ["value"]
            })
        }

        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            let value = args
                .get("value")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            Ok(ToolResult {
                success: true,
                output: format!("echo:{value}").into(),
                error: None,
            })
        }
    }

    struct OneToolThenFinalModelProvider;

    #[async_trait]
    impl ModelProvider for OneToolThenFinalModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("unused".to_string())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            let has_tool_message = request.messages.iter().any(|m| m.role == "tool");
            if has_tool_message {
                Ok(ChatResponse {
                    text: Some("done".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                })
            } else {
                Ok(ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "call_1".to_string(),
                        name: "echo_tool".to_string(),
                        arguments: "{\"value\":\"ping\"}".to_string(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                })
            }
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for OneToolThenFinalModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "OneToolThenFinalModelProvider"
        }
    }

    struct EchoToolResultThenFinalModelProvider {
        tool_message: std::sync::Mutex<Option<String>>,
    }

    impl EchoToolResultThenFinalModelProvider {
        fn new() -> Self {
            Self {
                tool_message: std::sync::Mutex::new(None),
            }
        }

        fn tool_message(&self) -> Option<String> {
            self.tool_message.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ModelProvider for EchoToolResultThenFinalModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("unused".to_string())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            if let Some(tool_message) = request.messages.iter().find(|m| m.role == "tool") {
                *self.tool_message.lock().unwrap() = Some(tool_message.content.clone());
                Ok(ChatResponse {
                    text: Some("done".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                })
            } else {
                Ok(ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "call_1".to_string(),
                        name: "echo_tool".to_string(),
                        arguments: format!("{{\"value\":\"{}\"}}", "tool-result-limit ".repeat(16)),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                })
            }
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for EchoToolResultThenFinalModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "EchoToolResultThenFinalModelProvider"
        }
    }

    struct TextFallbackToolModelProvider;

    #[async_trait]
    impl ModelProvider for TextFallbackToolModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("unused".to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some(
                    r#"<tool_call>{"name":"echo_tool","arguments":{"value":"ignored"}}</tool_call>"#
                        .to_string(),
                ),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for TextFallbackToolModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "TextFallbackToolModelProvider"
        }
    }

    struct InfiniteToolCallModelProvider;

    #[async_trait]
    impl ModelProvider for InfiniteToolCallModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("unused".to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "loop".to_string(),
                    name: "echo_tool".to_string(),
                    arguments: "{\"value\":\"x\"}".to_string(),
                    extra_content: None,
                }],
                usage: None,
                reasoning_content: None,
            })
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for InfiniteToolCallModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "InfiniteToolCallModelProvider"
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
        ) -> anyhow::Result<String> {
            Ok("unused".to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            Err(anyhow::Error::msg("model_provider boom"))
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

    fn agentic_agent_config() -> AliasedAgentConfig {
        AliasedAgentConfig {
            model_provider: "openrouter.agentic".into(),
            risk_profile: "agentic_test".into(),
            runtime_profile: "agentic_test".into(),
            ..Default::default()
        }
    }

    fn agentic_runtime_profiles(max_iterations: usize) -> HashMap<String, RuntimeProfileConfig> {
        let mut profiles = HashMap::new();
        profiles.insert(
            "agentic_test".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                max_tool_iterations: max_iterations,
                ..Default::default()
            },
        );
        profiles
    }

    fn agentic_risk_profiles(allowed_tools: Vec<String>) -> HashMap<String, RiskProfileConfig> {
        agentic_risk_profiles_with_excluded(allowed_tools, Vec::new())
    }

    fn agentic_risk_profiles_with_excluded(
        allowed_tools: Vec<String>,
        excluded_tools: Vec<String>,
    ) -> HashMap<String, RiskProfileConfig> {
        let mut profiles = HashMap::new();
        profiles.insert(
            "agentic_test".to_string(),
            RiskProfileConfig {
                allowed_tools,
                excluded_tools,
                ..Default::default()
            },
        );
        profiles
    }

    struct DelegateMemoryFixture {
        _tmp: TempDir,
        inner_memory: Arc<SqliteMemory>,
        caller_uuid: String,
        target_uuid: String,
        workspace_dir: PathBuf,
        tool: DelegateTool,
        target_config: AliasedAgentConfig,
    }

    fn scoped_sqlite_memory(inner: Arc<SqliteMemory>, agent_id: &str) -> Arc<dyn Memory> {
        let inner_dyn: Arc<dyn Memory> = inner;
        Arc::new(AgentScopedMemory::new(
            inner_dyn,
            agent_id.to_string(),
            Vec::<String>::new(),
        ))
    }

    fn memory_parent_tools(
        memory: Arc<dyn Memory>,
        security: Arc<SecurityPolicy>,
    ) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(MemoryStoreTool::new(memory.clone(), security.clone())),
            Arc::new(MemoryRecallTool::new(memory)),
        ]
    }

    async fn delegate_memory_fixture(model_uri: Option<String>) -> DelegateMemoryFixture {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};

        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        let workspace_dir = tmp.path().join("workspace");
        let mut root_config = Config {
            data_dir: data_dir.clone(),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        let model_provider_config = ModelProviderConfig {
            uri: model_uri,
            model: Some("delegate-test-model".to_string()),
            api_key: Some("delegate-test-key".to_string()),
            timeout_secs: Some(2),
            ..ModelProviderConfig::default()
        };
        root_config.providers.models.custom.insert(
            "local".to_string(),
            CustomModelProviderConfig {
                base: model_provider_config.clone(),
            },
        );
        root_config.risk_profiles.insert(
            "agentic_test".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                allowed_tools: vec!["memory_store".to_string(), "memory_recall".to_string()],
                ..RiskProfileConfig::default()
            },
        );
        root_config.runtime_profiles.insert(
            "agentic_test".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                max_tool_iterations: 5,
                ..RuntimeProfileConfig::default()
            },
        );
        let target_config = AliasedAgentConfig {
            model_provider: "custom.local".into(),
            risk_profile: "agentic_test".into(),
            runtime_profile: "agentic_test".into(),
            ..AliasedAgentConfig::default()
        };
        root_config
            .agents
            .insert("caller".to_string(), target_config.clone());
        root_config
            .agents
            .insert("target".to_string(), target_config.clone());

        let inner_memory = Arc::new(SqliteMemory::new("delegate-test", &data_dir).unwrap());
        let caller_uuid = inner_memory.ensure_agent_uuid("caller").await.unwrap();
        let target_uuid = inner_memory.ensure_agent_uuid("target").await.unwrap();
        let root_config = Arc::new(root_config);
        let caller_security = Arc::new(SecurityPolicy::for_agent(&root_config, "caller").unwrap());
        let caller_memory = scoped_sqlite_memory(inner_memory.clone(), &caller_uuid);
        let mut providers_models: HashMap<String, HashMap<String, ModelProviderConfig>> =
            HashMap::new();
        providers_models
            .entry("custom".to_string())
            .or_default()
            .insert("local".to_string(), model_provider_config);

        let tool = DelegateTool::new(
            root_config.agents.clone(),
            None,
            Arc::clone(&caller_security),
        )
        .with_root_config(Arc::clone(&root_config))
        .with_workspace_dir(workspace_dir.clone())
        .with_memory(Arc::clone(&caller_memory))
        .with_parent_tools(Arc::new(RwLock::new(memory_parent_tools(
            caller_memory,
            caller_security,
        ))))
        .with_providers_models(providers_models)
        .with_risk_profiles(root_config.risk_profiles.clone())
        .with_runtime_profiles(root_config.runtime_profiles.clone())
        .with_caller_alias("caller");

        DelegateMemoryFixture {
            _tmp: tmp,
            inner_memory,
            caller_uuid,
            target_uuid,
            workspace_dir,
            tool,
            target_config,
        }
    }

    struct MemoryStoreRecallThenFinalModelProvider {
        key: &'static str,
        content: &'static str,
    }

    #[async_trait]
    impl ModelProvider for MemoryStoreRecallThenFinalModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("unused".to_string())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            let tool_message_count = request.messages.iter().filter(|m| m.role == "tool").count();
            match tool_message_count {
                0 => Ok(ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "call_store".to_string(),
                        name: "memory_store".to_string(),
                        arguments: serde_json::json!({
                            "key": self.key,
                            "content": self.content,
                            "category": "core"
                        })
                        .to_string(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                }),
                1 => Ok(ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "call_recall".to_string(),
                        name: "memory_recall".to_string(),
                        arguments: serde_json::json!({
                            "query": self.key,
                            "limit": 5
                        })
                        .to_string(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                }),
                _ => Ok(ChatResponse {
                    text: Some("memory workflow done".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                }),
            }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for MemoryStoreRecallThenFinalModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "MemoryStoreRecallThenFinalModelProvider"
        }
    }

    fn chat_completion_tool_call(
        name: &str,
        id: &str,
        arguments: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": arguments.to_string()
                        }
                    }]
                }
            }]
        })
    }

    struct LocalChatServer {
        uri: String,
        _task: tokio::task::JoinHandle<()>,
    }

    async fn read_http_request(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
        use tokio::io::AsyncReadExt;

        let mut buf = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let n = socket.read(&mut chunk).await.unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            let Some(header_end) = buf.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&buf[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if buf.len() >= header_end + 4 + content_length {
                break;
            }
        }
        buf
    }

    async fn write_json_response(socket: &mut tokio::net::TcpStream, body: serde_json::Value) {
        use tokio::io::AsyncWriteExt;

        let body = body.to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    }

    async fn start_memory_tool_chat_server(key: &str, content: &str) -> LocalChatServer {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("http://{}", listener.local_addr().unwrap());
        let responses = vec![
            chat_completion_tool_call(
                "memory_store",
                "call_store",
                serde_json::json!({
                    "key": key,
                    "content": content,
                    "category": "core"
                }),
            ),
            chat_completion_tool_call(
                "memory_recall",
                "call_recall",
                serde_json::json!({
                    "query": key,
                    "limit": 5
                }),
            ),
            serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "memory workflow done"
                    }
                }]
            }),
        ];

        let task = zeroclaw_spawn::spawn!(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let _request = read_http_request(&mut socket).await;
                write_json_response(&mut socket, response).await;
            }
        });

        LocalChatServer { uri, _task: task }
    }

    async fn start_final_chat_server(contents: Vec<&'static str>) -> LocalChatServer {
        // Minimal OpenAI-compatible responder for tests that only need to prove
        // which delegate path ran. Each expected child turn consumes one final
        // assistant response in order.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("http://{}", listener.local_addr().unwrap());
        let responses: Vec<_> = contents
            .into_iter()
            .map(|content| {
                serde_json::json!({
                    "choices": [{
                        "message": {
                            "content": content
                        }
                    }]
                })
            })
            .collect();

        let task = zeroclaw_spawn::spawn!(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let _request = read_http_request(&mut socket).await;
                write_json_response(&mut socket, response).await;
            }
        });

        LocalChatServer { uri, _task: task }
    }

    async fn assert_stored_for_target_only(fixture: &DelegateMemoryFixture, key: &str) {
        // The memory backend can store the same key under multiple agent UUIDs.
        // Scope bugs are therefore silent unless the test checks both the target
        // positive case and the caller negative case.
        let target_entry = fixture
            .inner_memory
            .get_for_agent(key, &fixture.target_uuid)
            .await
            .unwrap();
        assert!(
            target_entry.is_some(),
            "delegated memory tools must write to the target agent scope"
        );
        let caller_entry = fixture
            .inner_memory
            .get_for_agent(key, &fixture.caller_uuid)
            .await
            .unwrap();
        assert!(
            caller_entry.is_none(),
            "delegated memory tools must not write to the caller agent scope"
        );
    }

    #[test]
    fn name_and_schema() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        assert_eq!(tool.name(), "delegate");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["agent"].is_object());
        assert!(schema["properties"]["prompt"].is_object());
        assert!(schema["properties"]["context"].is_object());
        assert!(schema["properties"]["background"].is_object());
        assert!(schema["properties"]["parallel"].is_object());
        assert!(schema["properties"]["action"].is_object());
        assert!(schema["properties"]["task_id"].is_object());
        // required is empty because different actions need different params
        let required = schema["required"].as_array().unwrap();
        assert!(required.is_empty());
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["properties"]["agent"]["minLength"], json!(1));
        assert_eq!(schema["properties"]["prompt"]["minLength"], json!(1));
    }

    #[test]
    fn description_not_empty() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn schema_lists_agent_names() {
        let tool = DelegateTool::new(sample_agents(), None, security_allowing());
        let schema = tool.parameters_schema();
        let desc = schema["properties"]["agent"]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("researcher") || desc.contains("coder"));
    }

    #[test]
    fn schema_roster_filtered_by_delegation_policy() {
        // When delegation is permitted, every configured agent (minus the
        // caller) is advertised — reachability is gated by shared risk
        // profile at delegation time, not by a per-agent roster allow-list.
        let tool = DelegateTool::new(sample_agents(), None, security_allowing());
        let schema = tool.parameters_schema();
        let desc = schema["properties"]["agent"]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("researcher"));
        assert!(desc.contains("coder"));

        // When delegation is forbidden, the roster is empty.
        let forbidden =
            DelegateTool::new(sample_agents(), None, Arc::new(SecurityPolicy::default()));
        let forbidden_schema = forbidden.parameters_schema();
        let forbidden_desc = forbidden_schema["properties"]["agent"]["description"]
            .as_str()
            .unwrap();
        assert!(!forbidden_desc.contains("researcher"));
        assert!(!forbidden_desc.contains("coder"));
    }

    #[test]
    fn schema_roster_lists_only_same_risk_profile_peers() {
        // Three agents: two on "alpha", one on "beta". Caller is on "alpha".
        let mut agents = HashMap::new();
        agents.insert(
            "alpha_peer".to_string(),
            AliasedAgentConfig {
                risk_profile: "alpha".into(),
                ..Default::default()
            },
        );
        agents.insert(
            "alpha_self".to_string(),
            AliasedAgentConfig {
                risk_profile: "alpha".into(),
                ..Default::default()
            },
        );
        agents.insert(
            "beta_outsider".to_string(),
            AliasedAgentConfig {
                risk_profile: "beta".into(),
                ..Default::default()
            },
        );

        // Caller on "alpha" with delegation allowed; it owns "alpha_self".
        let mut policy = SecurityPolicy {
            delegation_policy: zeroclaw_config::autonomy::DelegationPolicy {
                mode: zeroclaw_config::autonomy::DelegationMode::Allow,
            },
            ..SecurityPolicy::default()
        };
        policy.risk_profile_name = "alpha".into();
        let mut tool = DelegateTool::new(agents, None, Arc::new(policy));
        tool.caller_alias = "alpha_self".to_string();

        let desc = tool.parameters_schema()["properties"]["agent"]["description"]
            .as_str()
            .unwrap()
            .to_string();

        // Same-profile peer is listed.
        assert!(desc.contains("alpha_peer"), "{desc}");
        // Delegator excludes itself.
        assert!(!desc.contains("alpha_self"), "{desc}");
        // Off-profile agent is excluded.
        assert!(!desc.contains("beta_outsider"), "{desc}");
    }

    #[test]
    fn schema_excludes_caller_alias_from_roster() {
        // An agent must never be offered itself as a delegation target,
        // even when the delegation_policy would otherwise permit it.
        let tool = DelegateTool::new(sample_agents(), None, security_allowing())
            .with_caller_alias("researcher");
        let schema = tool.parameters_schema();
        let desc = schema["properties"]["agent"]["description"]
            .as_str()
            .unwrap();
        assert!(!desc.contains("researcher"));
        assert!(desc.contains("coder"));
    }

    #[test]
    fn schema_empty_roster_when_delegation_forbidden() {
        // Default policy forbids delegation, so no configured agent
        // should be advertised.
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let schema = tool.parameters_schema();
        let desc = schema["properties"]["agent"]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("none configured"));
    }

    fn roster_schema_config() -> Arc<zeroclaw_config::schema::Config> {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{AliasedAgentConfig, Config, RiskProfileConfig};
        let root =
            std::env::temp_dir().join(format!("zeroclaw-delegate-policy-{}", uuid::Uuid::new_v4()));
        let mut config = Config {
            data_dir: root.join("data"),
            config_path: root.join("config.toml"),
            ..Config::default()
        };
        config.risk_profiles.insert(
            "shared".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                ..RiskProfileConfig::default()
            },
        );
        config
            .risk_profiles
            .insert("lore".to_string(), RiskProfileConfig::default());
        for (alias, profile) in [
            ("aaa", "shared"),
            ("aaatools", "shared"),
            ("aaalore", "lore"),
        ] {
            config.agents.insert(
                alias.to_string(),
                AliasedAgentConfig {
                    risk_profile: profile.into(),
                    model_provider: "ollama.default".into(),
                    ..AliasedAgentConfig::default()
                },
            );
        }
        Arc::new(config)
    }

    fn roster_tool(config: Arc<zeroclaw_config::schema::Config>) -> DelegateTool {
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "aaa").expect("caller policy resolves"));
        DelegateTool::new(
            config
                .agents
                .iter()
                .map(|(n, a)| (n.clone(), a.clone()))
                .collect(),
            None,
            caller_policy,
        )
        .with_root_config(config)
        .with_caller_alias("aaa")
    }

    #[test]
    fn schema_roster_advertises_same_profile_peer() {
        let tool = roster_tool(roster_schema_config());
        let desc = tool.parameters_schema()["properties"]["agent"]["description"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(desc.contains("aaatools"), "{desc}");
        assert!(!desc.contains("aaalore"), "{desc}");
        assert!(!desc.contains("aaa,") && !desc.ends_with("aaa"), "{desc}");
    }

    #[test]
    fn schema_roster_advertises_explicit_cross_profile_target() {
        let mut config = (*roster_schema_config()).clone();
        config.agents.get_mut("aaa").unwrap().delegates =
            vec![DelegateTargetConfig::bounded("aaalore")];
        let tool = roster_tool(Arc::new(config));
        let desc = tool.parameters_schema()["properties"]["agent"]["description"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(desc.contains("aaalore"), "{desc}");
        assert!(desc.contains("aaatools"), "{desc}");
    }

    #[test]
    fn schema_roster_opt_out_hides_peers_keeps_explicit() {
        let mut config = (*roster_schema_config()).clone();
        let aaa = config.agents.get_mut("aaa").unwrap();
        aaa.delegate_same_risk_profile = false;
        aaa.delegates = vec![DelegateTargetConfig::bounded("aaalore")];
        let tool = roster_tool(Arc::new(config));
        let desc = tool.parameters_schema()["properties"]["agent"]["description"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(desc.contains("aaalore"), "{desc}");
        assert!(!desc.contains("aaatools"), "{desc}");
    }

    #[tokio::test]
    async fn missing_agent_param() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool.execute(json!({"prompt": "test"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn missing_prompt_param() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool.execute(json!({"agent": "researcher"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn unknown_agent_returns_error() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool
            .execute(json!({"agent": "nonexistent", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown agent"));
    }

    #[tokio::test]
    async fn depth_limit_enforced() {
        let tool = DelegateTool::with_depth(sample_agents(), None, test_security(), 3);
        let result = tool
            .execute(json!({"agent": "researcher", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("depth limit"));
    }

    #[tokio::test]
    async fn depth_limit_at_default_max() {
        // Default max_depth is 3; at depth=3 the agent should be blocked.
        let tool = DelegateTool::with_depth(sample_agents(), None, test_security(), 3);
        let result = tool
            .execute(json!({"agent": "coder", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("depth limit"));
    }

    #[test]
    fn empty_agents_schema() {
        let tool = DelegateTool::new(HashMap::new(), None, test_security());
        let schema = tool.parameters_schema();
        let desc = schema["properties"]["agent"]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("none configured"));
    }

    #[tokio::test]
    async fn invalid_provider_returns_error() {
        let mut agents = HashMap::new();
        agents.insert(
            "broken".to_string(),
            AliasedAgentConfig {
                model_provider: "totally-invalid-provider.default".into(),
                ..Default::default()
            },
        );
        let tool = DelegateTool::new(agents, None, test_security());
        let result = tool
            .execute(json!({"agent": "broken", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap()
                .contains("Failed to create model_provider")
        );
    }

    #[tokio::test]
    async fn blank_agent_rejected() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool
            .execute(json!({"agent": "  ", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("must not be empty"));
    }

    #[tokio::test]
    async fn blank_prompt_rejected() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool
            .execute(json!({"agent": "researcher", "prompt": "  \t  "}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("must not be empty"));
    }

    #[tokio::test]
    async fn whitespace_agent_name_trimmed_and_found() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        // " researcher " with surrounding whitespace — after trim becomes "researcher"
        let result = tool
            .execute(json!({"agent": " researcher ", "prompt": "test"}))
            .await
            .unwrap();
        // Should find "researcher" after trim — will fail at model_provider level
        // since ollama isn't running, but must NOT get "Unknown agent".
        assert!(
            result.error.is_none()
                || !result
                    .error
                    .as_deref()
                    .unwrap_or("")
                    .contains("Unknown agent")
        );
    }

    #[tokio::test]
    async fn delegation_blocked_in_readonly_mode() {
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = DelegateTool::new(sample_agents(), None, readonly);
        let result = tool
            .execute(json!({"agent": "researcher", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("read-only mode")
        );
    }

    #[tokio::test]
    async fn delegation_blocked_when_rate_limited() {
        let limited = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let tool = DelegateTool::new(sample_agents(), None, limited);
        let result = tool
            .execute(json!({"agent": "researcher", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Rate limit exceeded")
        );
    }

    #[tokio::test]
    async fn delegate_context_is_prepended_to_prompt() {
        let mut agents = HashMap::new();
        agents.insert(
            "tester".to_string(),
            AliasedAgentConfig {
                model_provider: "invalid-for-test.default".into(),
                ..Default::default()
            },
        );
        let tool = DelegateTool::new(agents, None, test_security());
        let result = tool
            .execute(json!({
                "agent": "tester",
                "prompt": "do something",
                "context": "some context data"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Failed to create model_provider")
        );
    }

    #[tokio::test]
    async fn delegate_empty_context_omits_prefix() {
        let mut agents = HashMap::new();
        agents.insert(
            "tester".to_string(),
            AliasedAgentConfig {
                model_provider: "invalid-for-test.default".into(),
                ..Default::default()
            },
        );
        let tool = DelegateTool::new(agents, None, test_security());
        let result = tool
            .execute(json!({
                "agent": "tester",
                "prompt": "do something",
                "context": ""
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Failed to create model_provider")
        );
    }

    #[test]
    fn delegate_depth_construction() {
        let tool = DelegateTool::with_depth(sample_agents(), None, test_security(), 5);
        assert_eq!(tool.depth, 5);
    }

    #[tokio::test]
    async fn delegate_no_agents_configured() {
        let tool = DelegateTool::new(HashMap::new(), None, test_security());
        let result = tool
            .execute(json!({"agent": "any", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("none configured"));
    }

    #[tokio::test]
    async fn agentic_mode_empty_allowed_tools_inherits_caller_registry() {
        // Empty allowed_tools now means "inherit": the target runs with the
        // caller's already-filtered tools instead of being rejected
        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(Vec::new()))
            .with_parent_tools(Arc::new(RwLock::new(vec![
                Arc::new(EchoTool),
                Arc::new(DelegateTool::new(HashMap::new(), None, test_security())),
            ])));

        let model_provider = ToolCountModelProvider { expected_tools: 1 };
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &model_provider,
                "run",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(result.success, "got: {:?}", result.error);
        assert!(result.output.contains("(openrouter/model-test, agentic)"));
    }

    #[tokio::test]
    async fn agentic_mode_empty_allowed_tools_empty_registry_runs_without_tools() {
        // Empty allowed_tools means "inherit", but an empty inherited registry is
        // still a valid agentic run. The fallback is a tool-less loop, not a
        // configuration error.
        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(Vec::new()));
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &FinalOnlyModelProvider,
                "test",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(result.success, "got: {:?}", result.error);
        assert!(result.output.contains("delegate saw tool"));
    }

    #[tokio::test]
    async fn agentic_mode_empty_allowed_tools_respects_excluded_tools_without_aborting() {
        // `excluded_tools` still applies to the inherited parent registry. If it
        // filters every candidate out, agentic execution should continue without
        // tools rather than failing admission.
        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles_with_excluded(
                Vec::new(),
                vec!["echo_tool".to_string()],
            ))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));
        let policy = tool
            .resolve_tool_policy("agentic_test")
            .expect("policy resolves");
        assert!(!DelegateTool::delegate_admits_with_mcp(
            &policy,
            "echo_tool"
        ));
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &FinalOnlyModelProvider,
                "test",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(result.success, "got: {:?}", result.error);
        assert!(result.output.contains("delegate saw tool"));
    }

    #[tokio::test]
    async fn agentic_mode_padded_allowed_tool_name_remains_exact_and_runs_without_match() {
        // Tool identifiers are exact names, not forgiving user input. Padding an
        // allowed_tools entry must not accidentally admit a real tool after
        // trimming; the result is a valid no-tool child loop.
        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(vec![" echo_tool ".to_string()]))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));
        let policy = tool
            .resolve_tool_policy("agentic_test")
            .expect("policy resolves");
        assert!(!DelegateTool::delegate_admits_with_mcp(
            &policy,
            "echo_tool"
        ));
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &FinalOnlyModelProvider,
                "test",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(result.success, "got: {:?}", result.error);
        assert!(result.output.contains("delegate saw tool"));
    }

    #[tokio::test]
    async fn agentic_mode_unmatched_allowed_tools_runs_without_tools() {
        // A configured allowlist can name tools absent from the parent registry.
        // That should produce an empty child registry, not an error, because the
        // target may still complete without tool calls.
        let config = agentic_agent_config();
        let allowed = vec!["missing_tool".to_string()];
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(allowed))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));
        let policy = tool
            .resolve_tool_policy("agentic_test")
            .expect("policy resolves");
        assert!(!DelegateTool::delegate_admits_with_mcp(
            &policy,
            "echo_tool"
        ));
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &FinalOnlyModelProvider,
                "test",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(result.success, "got: {:?}", result.error);
        assert!(result.output.contains("delegate saw tool"));
    }

    #[tokio::test]
    async fn execute_agentic_runs_tool_call_loop_with_filtered_tools() {
        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(vec!["echo_tool".to_string()]))
            .with_parent_tools(Arc::new(RwLock::new(vec![
                Arc::new(EchoTool),
                Arc::new(DelegateTool::new(HashMap::new(), None, test_security())),
            ])));

        let model_provider = ToolCountModelProvider { expected_tools: 1 };
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &model_provider,
                "run",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("(openrouter/model-test, agentic)"));
        assert!(result.output.contains("tool count matched: 1"));
    }

    #[tokio::test]
    async fn execute_agentic_rebinds_memory_tools_to_target_agent_scope() {
        // Memory tools are stateful even when they come from the parent registry.
        // Agentic delegation must rebind them to the target alias so a child
        // cannot write into the caller's memory namespace.
        let fixture = delegate_memory_fixture(None).await;
        let model_provider = MemoryStoreRecallThenFinalModelProvider {
            key: "sync-key",
            content: "sync target memory",
        };

        let result = fixture
            .tool
            .execute_agentic(
                "target",
                &fixture.target_config,
                "custom",
                "delegate-test-model",
                &model_provider,
                "store and recall target memory",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(result.success, "agentic delegate failed: {result:?}");
        assert!(result.output.contains("memory workflow done"));
        assert_stored_for_target_only(&fixture, "sync-key").await;
    }

    #[tokio::test]
    async fn background_agentic_delegate_rebinds_memory_tools_to_target_agent_scope() {
        // Same memory-scope invariant as the sync path, but through the detached
        // task worker that runs after a task id is returned to the caller.
        let server =
            start_memory_tool_chat_server("background-key", "background target memory").await;
        let fixture = delegate_memory_fixture(Some(server.uri.clone())).await;

        let result = fixture
            .tool
            .execute(json!({
                "agent": "target",
                "prompt": "store and recall target memory",
                "background": true
            }))
            .await
            .unwrap();

        assert!(result.success, "background delegate failed: {result:?}");
        let task_id = result
            .output
            .lines()
            .find(|line| line.starts_with("task_id:"))
            .unwrap()
            .trim_start_matches("task_id: ")
            .trim();
        let bg_result = wait_for_terminal_background_result(&fixture.workspace_dir, task_id).await;
        assert_eq!(bg_result.status, BackgroundTaskStatus::Completed);
        assert!(
            bg_result
                .output
                .as_deref()
                .unwrap_or_default()
                .contains("memory workflow done")
        );
        assert_stored_for_target_only(&fixture, "background-key").await;
    }

    #[tokio::test]
    async fn parallel_agentic_delegate_rebinds_memory_tools_to_target_agent_scope() {
        // Parallel fan-out gets its own coverage because each spawned worker
        // rebuilds a delegate tool instance before entering the agentic loop.
        let server = start_memory_tool_chat_server("parallel-key", "parallel target memory").await;
        let fixture = delegate_memory_fixture(Some(server.uri.clone())).await;

        let result = fixture
            .tool
            .execute(json!({
                "parallel": ["target"],
                "prompt": "store and recall target memory"
            }))
            .await
            .unwrap();

        assert!(result.success, "parallel delegate failed: {result:?}");
        assert!(result.output.contains("memory workflow done"));
        assert_stored_for_target_only(&fixture, "parallel-key").await;
    }

    #[tokio::test]
    async fn parallel_delegate_runs_with_caller_authorization_not_child_authorization() {
        // Parallel independent fan-out starts with caller admission for the
        // delegate tool, then each child runs with its own target policy. This
        // guards the earlier bug where child-side policy blocked valid targets
        // before the independent mode switch could take effect.
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};

        let server = start_final_chat_server(vec!["reviewer-ok", "sysadmin-ok"]).await;
        let tmp = TempDir::new().unwrap();
        let model_provider_config = ModelProviderConfig {
            uri: Some(server.uri.clone()),
            model: Some("parallel-test-model".to_string()),
            api_key: Some("parallel-test-key".to_string()),
            timeout_secs: Some(2),
            ..ModelProviderConfig::default()
        };
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.providers.models.custom.insert(
            "local".to_string(),
            CustomModelProviderConfig {
                base: model_provider_config.clone(),
            },
        );
        config.risk_profiles.insert(
            "caller_profile".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                allowed_tools: vec![DelegateTool::NAME.to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "reviewer_readonly".to_string(),
            RiskProfileConfig {
                allowed_tools: vec!["file_read".to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "sysadmin_yolo".to_string(),
            RiskProfileConfig {
                allowed_tools: vec!["shell".to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.agents.insert(
            "caller".to_string(),
            AliasedAgentConfig {
                model_provider: "custom.local".into(),
                risk_profile: "caller_profile".into(),
                delegates: vec![
                    DelegateTargetConfig {
                        agent: "reviewer".to_string(),
                        mode: DelegateExecutionMode::Independent,
                    },
                    DelegateTargetConfig {
                        agent: "sysadmin".to_string(),
                        mode: DelegateExecutionMode::Independent,
                    },
                ],
                ..AliasedAgentConfig::default()
            },
        );
        for (alias, risk_profile) in [
            ("reviewer", "reviewer_readonly"),
            ("sysadmin", "sysadmin_yolo"),
        ] {
            config.agents.insert(
                alias.to_string(),
                AliasedAgentConfig {
                    model_provider: "custom.local".into(),
                    risk_profile: risk_profile.into(),
                    ..AliasedAgentConfig::default()
                },
            );
        }
        let config = Arc::new(config);
        let mut providers_models: HashMap<String, HashMap<String, ModelProviderConfig>> =
            HashMap::new();
        providers_models
            .entry("custom".to_string())
            .or_default()
            .insert("local".to_string(), model_provider_config);
        let caller_security =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let tool = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_security))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_providers_models(providers_models)
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone());

        let result = tool
            .execute(json!({
                "parallel": ["reviewer", "sysadmin"],
                "prompt": "fan out"
            }))
            .await
            .unwrap();

        assert!(result.success, "parallel delegate failed: {result:?}");
        assert!(result.output.contains("reviewer-ok"), "{result:?}");
        assert!(result.output.contains("sysadmin-ok"), "{result:?}");
    }

    #[tokio::test]
    async fn background_agentic_delegate_runs_with_caller_authorization_not_child_authorization() {
        // Background bounded admission happens before the task id is returned;
        // the detached worker must not reinterpret that request as a child-side
        // self-delegation decision after it starts.
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};

        let server = start_final_chat_server(vec!["background-ok"]).await;
        let tmp = TempDir::new().unwrap();
        let workspace_dir = tmp.path().join("workspace");
        let model_provider_config = ModelProviderConfig {
            uri: Some(server.uri.clone()),
            model: Some("background-test-model".to_string()),
            api_key: Some("background-test-key".to_string()),
            timeout_secs: Some(2),
            ..ModelProviderConfig::default()
        };
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.providers.models.custom.insert(
            "local".to_string(),
            CustomModelProviderConfig {
                base: model_provider_config.clone(),
            },
        );
        config.risk_profiles.insert(
            "caller_profile".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                allowed_tools: vec![DelegateTool::NAME.to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config
            .risk_profiles
            .insert("target_profile".to_string(), RiskProfileConfig::default());
        config.runtime_profiles.insert(
            "target_agentic".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                max_tool_iterations: 2,
                ..RuntimeProfileConfig::default()
            },
        );
        config.agents.insert(
            "caller".to_string(),
            AliasedAgentConfig {
                model_provider: "custom.local".into(),
                risk_profile: "caller_profile".into(),
                delegates: vec![DelegateTargetConfig {
                    agent: "target".to_string(),
                    mode: DelegateExecutionMode::Bounded,
                }],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "target".to_string(),
            AliasedAgentConfig {
                model_provider: "custom.local".into(),
                risk_profile: "target_profile".into(),
                runtime_profile: "target_agentic".into(),
                ..AliasedAgentConfig::default()
            },
        );
        let config = Arc::new(config);
        let mut providers_models: HashMap<String, HashMap<String, ModelProviderConfig>> =
            HashMap::new();
        providers_models
            .entry("custom".to_string())
            .or_default()
            .insert("local".to_string(), model_provider_config);
        let caller_security =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let tool = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_security))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_workspace_dir(workspace_dir.clone())
            .with_providers_models(providers_models)
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone());

        let result = tool
            .execute(json!({
                "agent": "target",
                "prompt": "run in background",
                "background": true
            }))
            .await
            .unwrap();

        assert!(result.success, "background delegate failed: {result:?}");
        let task_id = result
            .output
            .lines()
            .find(|line| line.starts_with("task_id:"))
            .unwrap()
            .trim_start_matches("task_id: ")
            .trim();
        let bg_result = wait_for_terminal_background_result(&workspace_dir, task_id).await;

        assert_eq!(
            bg_result.status,
            BackgroundTaskStatus::Completed,
            "{bg_result:?}"
        );
        assert!(
            bg_result
                .output
                .as_deref()
                .unwrap_or_default()
                .contains("background-ok"),
            "{bg_result:?}"
        );
        assert!(bg_result.error.is_none(), "{bg_result:?}");
    }

    #[tokio::test]
    async fn execute_agentic_strict_tool_parsing_uses_target_agent_policy() {
        // Strict parsing is target runtime policy. If the parent path leaked its
        // own prompt/tool settings, text fallback tool calls could execute in a
        // child that intentionally disabled them.
        let config = agentic_agent_config();
        let mut runtime_profiles = agentic_runtime_profiles(10);
        runtime_profiles
            .get_mut("agentic_test")
            .unwrap()
            .strict_tool_parsing = true;
        let prompt_tools: Vec<Box<dyn Tool>> = vec![Box::new(EchoTool)];
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(runtime_profiles)
            .with_risk_profiles(agentic_risk_profiles(vec!["echo_tool".to_string()]))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));
        let mut prompt_config = config.clone();
        prompt_config.resolved = tool.resolve_loop_runtime("agentic", &config);

        let prompt = tool
            .build_enriched_system_prompt(
                "agentic",
                &prompt_config,
                "model-test",
                &prompt_tools,
                Path::new("/tmp"),
                false,
                None,
            )
            .expect("prompt should render");
        assert!(
            !prompt.contains("## Tools"),
            "strict delegate prompt should not advertise text tool instructions"
        );
        assert!(
            !prompt.contains("echo_tool"),
            "strict delegate prompt should hide text-only tool schemas"
        );

        let model_provider = TextFallbackToolModelProvider;
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &model_provider,
                "run",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(
            result.output.contains("<tool_call>"),
            "strict subagent should return fallback-looking text unchanged"
        );
        assert!(
            !result.output.contains("echo:ignored"),
            "strict subagent must not execute text fallback tool calls"
        );
    }

    #[tokio::test]
    async fn execute_agentic_excludes_delegate_even_if_allowlisted() {
        // Recursive agentic delegation is still unsupported. Even if the target
        // profile allowlists `delegate`, the child registry must strip it before
        // the tool loop starts.
        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(vec!["delegate".to_string()]))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(DelegateTool::new(
                HashMap::new(),
                None,
                test_security(),
            ))])));

        let model_provider = OneToolThenFinalModelProvider;
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &model_provider,
                "run",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(result.success, "got: {:?}", result.error);
    }

    #[tokio::test]
    async fn execute_agentic_respects_max_iterations() {
        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(2))
            .with_risk_profiles(agentic_risk_profiles(vec!["echo_tool".to_string()]))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));

        let model_provider = InfiniteToolCallModelProvider;
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &model_provider,
                "run",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("maximum tool iterations (2)")
        );
    }

    #[tokio::test]
    async fn execute_agentic_applies_target_profile_tool_result_limit() {
        let config = agentic_agent_config();
        let mut runtime_profiles = agentic_runtime_profiles(10);
        runtime_profiles
            .get_mut("agentic_test")
            .unwrap()
            .max_tool_result_chars = Some(80);
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(runtime_profiles)
            .with_risk_profiles(agentic_risk_profiles(vec!["echo_tool".to_string()]))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));

        let model_provider = EchoToolResultThenFinalModelProvider::new();
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &model_provider,
                "run",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(result.success);
        let tool_message = model_provider
            .tool_message()
            .expect("tool message captured");
        assert!(
            tool_message.contains("characters truncated"),
            "delegate sub-loop should apply the target runtime profile's max_tool_result_chars, got: {}",
            tool_message
        );
    }

    #[tokio::test]
    async fn execute_agentic_forwards_receipt_scope_into_subagent_loop() {
        use crate::agent::tool_receipts::{
            ReceiptGenerator, ReceiptScope, TOOL_LOOP_RECEIPT_CONTEXT,
        };

        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(vec!["echo_tool".to_string()]))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));

        let collector: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let scope = ReceiptScope {
            generator: ReceiptGenerator::new(),
            collector: Arc::clone(&collector),
        };

        let model_provider = OneToolThenFinalModelProvider;
        let result = TOOL_LOOP_RECEIPT_CONTEXT
            .scope(Some(scope), async {
                tool.execute_agentic(
                    "agentic",
                    &config,
                    "test-provider",
                    "test-model",
                    &model_provider,
                    "run",
                    Some(0.2),
                )
                .await
            })
            .await
            .unwrap();

        assert!(
            result.success,
            "delegate sub-loop must complete: {result:?}"
        );
        let receipts = collector.lock().unwrap();
        assert_eq!(
            receipts.len(),
            1,
            "expected exactly one receipt for the single echo_tool sub-call, got: {:?}",
            receipts.as_slice()
        );
        assert!(
            receipts[0].starts_with("echo_tool: zc-receipt-"),
            "sub-tool receipt must be tagged with the tool name and a zc-receipt- HMAC token, got: {}",
            receipts[0]
        );
    }

    #[tokio::test]
    async fn delegate_spawn_helper_forwards_session_key() {
        let seen = TOOL_LOOP_SESSION_KEY
            .scope(Some("channel_session".to_string()), async {
                let session_key = current_tool_loop_session_key();
                zeroclaw_spawn::spawn!(async move {
                    scope_delegate_session_key(session_key, async {
                        current_tool_loop_session_key()
                    })
                    .await
                })
                .await
                .unwrap()
            })
            .await;

        assert_eq!(seen.as_deref(), Some("channel_session"));
    }

    #[tokio::test]
    async fn execute_agentic_emits_no_receipts_when_scope_absent() {
        // Backward-compat for callers without a scoped receipt context (CLI,
        // background spawn that does not forward scope, tests). The sub-loop
        // must run unsigned and the agent output must not carry a
        // `[receipt: ` trailer.
        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(vec!["echo_tool".to_string()]))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));

        let model_provider = OneToolThenFinalModelProvider;
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "test-provider",
                "test-model",
                &model_provider,
                "run",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(
            !result.output.contains("[receipt: "),
            "no receipt trailer must appear in agent output when receipts are disabled, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn execute_agentic_propagates_provider_errors() {
        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(vec!["echo_tool".to_string()]))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));

        let model_provider = FailingModelProvider;
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &model_provider,
                "run",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("model_provider boom")
        );
    }

    /// MCP tools pushed into the shared parent_tools handle after DelegateTool
    /// construction must be visible to the sub-agent tool list.
    #[derive(Default)]
    struct FakeMcpTool;

    #[async_trait]
    impl Tool for FakeMcpTool {
        fn name(&self) -> &str {
            "mcp_fake"
        }

        fn description(&self) -> &str {
            "Fake MCP tool for testing."
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }

        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: "mcp_fake_output".into(),
                error: None,
            })
        }
    }

    struct McpToolThenFinalModelProvider;

    #[async_trait]
    impl ModelProvider for McpToolThenFinalModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("unused".to_string())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            let has_tool_message = request.messages.iter().any(|m| m.role == "tool");
            if has_tool_message {
                Ok(ChatResponse {
                    text: Some("mcp done".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                })
            } else {
                Ok(ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "call_mcp".to_string(),
                        name: "mcp_fake".to_string(),
                        arguments: "{}".to_string(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                })
            }
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for McpToolThenFinalModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "McpToolThenFinalModelProvider"
        }
    }

    struct FinalOnlyModelProvider;

    #[async_trait]
    impl ModelProvider for FinalOnlyModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("delegate saw tool".to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some("delegate saw tool".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for FinalOnlyModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "FinalOnlyModelProvider"
        }
    }

    struct ToolCountModelProvider {
        expected_tools: usize,
    }

    #[async_trait]
    impl ModelProvider for ToolCountModelProvider {
        fn supports_native_tools(&self) -> bool {
            true
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(format!("tool count matched: {}", self.expected_tools))
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            let actual_tools = request.tools.map_or(0, |tools| tools.len());
            assert_eq!(
                actual_tools, self.expected_tools,
                "unexpected delegated tool count"
            );
            Ok(ChatResponse {
                text: Some(format!("tool count matched: {actual_tools}")),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ToolCountModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "ToolCountModelProvider"
        }
    }

    #[tokio::test]
    async fn mcp_tools_included_in_subagent_tool_list() {
        // Build DelegateTool with NO parent tools initially
        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(vec!["mcp_fake".to_string()]))
            .with_parent_tools(Arc::new(RwLock::new(Vec::new())));

        // Simulate late MCP tool injection via the shared handle
        let handle = tool.parent_tools_handle();
        handle.write().push(Arc::new(FakeMcpTool));

        let model_provider = McpToolThenFinalModelProvider;
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &model_provider,
                "run mcp",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(result.success, "Expected success, got: {:?}", result.error);
        assert!(
            result.output.contains("mcp done"),
            "Expected output containing 'mcp done', got: {}",
            result.output
        );
    }

    #[test]
    fn delegate_admits_with_mcp_auto_admits_double_underscore_mcp_names() {
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_risk_profiles(agentic_risk_profiles(vec!["shell".to_string()]))
            .with_parent_tools(Arc::new(RwLock::new(Vec::new())));

        let policy = tool
            .resolve_tool_policy("agentic_test")
            .expect("agentic_test risk profile is configured");

        // The explicit allow-list entry is admitted.
        assert!(
            DelegateTool::delegate_admits_with_mcp(&policy, "shell"),
            "explicit allow-list entry must be admitted"
        );
        // A runtime-discovered MCP wrapper (matching `<server>__<tool>`) is
        // auto-admitted even though it is not in `allowed_tools`. This is
        // the destructive capability the reviewer called out.
        assert!(
            DelegateTool::delegate_admits_with_mcp(&policy, "filesystem__write_file"),
            "double-underscore MCP name must be auto-admitted"
        );
        // Non-MCP names outside the allow-list still get rejected.
        assert!(
            !DelegateTool::delegate_admits_with_mcp(&policy, "memory_recall"),
            "non-MCP names outside allow-list must be rejected"
        );
    }

    #[test]
    fn caller_allowed_narrowing_excludes_mcp_capability_tools() {
        use zeroclaw_tools::tool_search::ToolAccessPolicy;
        let policy = ToolAccessPolicy::from_security(
            Some(&["shell".to_string()]),
            None,
            Some(&["shell".to_string()]),
        )
        .expect("policy");
        assert!(policy.is_tool_allowed("shell"));
        assert!(!policy.is_tool_allowed("mcp_resources"));
        assert!(!policy.is_tool_allowed("mcp_prompts"));
    }

    #[test]
    fn delegate_admits_with_mcp_honors_excluded_tools_for_auto_admitted_mcp() {
        let mut profiles = HashMap::new();
        profiles.insert(
            "agentic_test".to_string(),
            RiskProfileConfig {
                allowed_tools: vec!["shell".to_string()],
                excluded_tools: vec!["filesystem__write_file".to_string()],
                ..Default::default()
            },
        );

        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_risk_profiles(profiles)
            .with_parent_tools(Arc::new(RwLock::new(Vec::new())));

        let policy = tool
            .resolve_tool_policy("agentic_test")
            .expect("agentic_test risk profile is configured");

        assert!(
            DelegateTool::delegate_admits_with_mcp(&policy, "shell"),
            "non-excluded allow-list entry must be admitted"
        );
        assert!(
            !DelegateTool::delegate_admits_with_mcp(&policy, "filesystem__write_file"),
            "excluded_tools must block auto-admitted MCP name"
        );
    }

    #[test]
    fn delegate_admits_with_mcp_honors_excluded_tools_for_explicit_allow_list_entries() {
        let mut profiles = HashMap::new();
        profiles.insert(
            "agentic_test".to_string(),
            RiskProfileConfig {
                allowed_tools: vec!["shell".to_string(), "memory_recall".to_string()],
                excluded_tools: vec!["shell".to_string()],
                ..Default::default()
            },
        );

        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_risk_profiles(profiles)
            .with_parent_tools(Arc::new(RwLock::new(Vec::new())));

        let policy = tool
            .resolve_tool_policy("agentic_test")
            .expect("agentic_test risk profile is configured");

        assert!(
            !DelegateTool::delegate_admits_with_mcp(&policy, "shell"),
            "excluded entry must be rejected even when allow-listed"
        );
        assert!(
            DelegateTool::delegate_admits_with_mcp(&policy, "memory_recall"),
            "non-excluded entry must be admitted"
        );
    }

    #[tokio::test]
    async fn deferred_mcp_activation_updates_delegate_parent_tools() {
        let config = agentic_agent_config();
        let parent_tools: Arc<RwLock<Vec<Arc<dyn Tool>>>> = Arc::new(RwLock::new(Vec::new()));
        let delegate = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(vec![
                "mcp_service_a__list_projects".to_string(),
            ]))
            .with_parent_tools(Arc::clone(&parent_tools));

        let activated = Arc::new(std::sync::Mutex::new(crate::tools::ActivatedToolSet::new()));
        let deferred = crate::tools::DeferredMcpToolSet {
            stubs: vec![{
                let def = zeroclaw_tools::mcp_protocol::McpToolDef {
                    name: "list_projects".to_string(),
                    description: Some("List projects".to_string()),
                    input_schema: serde_json::json!({"type": "object", "properties": {}}),
                };
                zeroclaw_tools::mcp_deferred::DeferredMcpToolStub::new(
                    "mcp_service_a__list_projects".to_string(),
                    def,
                )
            }],
            registry: Arc::new(
                zeroclaw_tools::mcp_client::McpRegistry::connect_all(&[])
                    .await
                    .unwrap(),
            ),
        };
        let handle = Arc::clone(&parent_tools);
        let tool_search = crate::tools::ToolSearchTool::new(deferred, Arc::clone(&activated))
            .with_activation_hook(Arc::new(move |tool| {
                let mut tools = handle.write();
                if !tools.iter().any(|existing| existing.name() == tool.name()) {
                    tools.push(tool);
                }
            }));

        let search = tool_search
            .execute(serde_json::json!({"query": "select:mcp_service_a__list_projects"}))
            .await
            .unwrap();
        assert!(search.success);

        {
            let tools = parent_tools.read();
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0].name(), "mcp_service_a__list_projects");
        }

        let model_provider = FinalOnlyModelProvider;
        let result = delegate
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &model_provider,
                "run mcp",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(result.success, "Expected success, got: {:?}", result.error);
        assert!(
            result.output.contains("delegate saw tool"),
            "Expected final output from delegate loop, got: {}",
            result.output
        );
    }

    #[test]
    fn enriched_prompt_includes_tools_workspace_date() {
        let config = AliasedAgentConfig {
            model_provider: "openrouter.test".into(),
            ..Default::default()
        };

        let tools: Vec<Box<dyn Tool>> = vec![Box::new(EchoTool)];
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_enrich_test_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_workspace_dir(workspace.clone());

        let prompt = tool
            .build_enriched_system_prompt(
                "alpha",
                &config,
                "test-model",
                &tools,
                &workspace,
                false,
                None,
            )
            .unwrap();

        assert!(prompt.contains("## Tools"), "should contain tools section");
        assert!(prompt.contains("echo_tool"), "should list allowed tools");
        assert!(
            prompt.contains("## Workspace"),
            "should contain workspace section"
        );
        assert!(
            prompt.contains(&workspace.display().to_string()),
            "should contain workspace path"
        );
        assert!(
            prompt.contains("## CRITICAL CONTEXT: CURRENT DATE"),
            "should contain date section"
        );
        assert!(!prompt.contains("CURRENT DATE & TIME"));
        assert!(!prompt.contains("Time:"));
        assert!(!prompt.contains("ISO 8601:"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn enriched_prompt_includes_shell_policy_when_shell_present() {
        let config = AliasedAgentConfig::default();

        struct MockShellTool;
        impl ::zeroclaw_api::attribution::Attributable for MockShellTool {
            fn role(&self) -> ::zeroclaw_api::attribution::Role {
                ::zeroclaw_api::attribution::Role::Tool(
                    ::zeroclaw_api::attribution::ToolKind::Shell,
                )
            }
            fn alias(&self) -> &str {
                <Self as Tool>::name(self)
            }
        }
        #[async_trait]
        impl Tool for MockShellTool {
            fn name(&self) -> &str {
                "shell"
            }
            fn description(&self) -> &str {
                "Execute shell commands"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                json!({"type": "object"})
            }
            async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: ToolOutput::default(),
                    error: None,
                })
            }
        }

        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockShellTool)];
        let workspace = std::env::temp_dir();

        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_workspace_dir(workspace.to_path_buf());

        let prompt = tool
            .build_enriched_system_prompt(
                "alpha",
                &config,
                "test-model",
                &tools,
                &workspace,
                false,
                None,
            )
            .unwrap();

        assert!(
            prompt.contains("## Shell Policy"),
            "should contain shell policy when shell tool is present"
        );
    }

    #[test]
    fn parent_tools_handle_returns_shared_reference() {
        let tool = DelegateTool::new(HashMap::new(), None, test_security()).with_parent_tools(
            Arc::new(RwLock::new(vec![Arc::new(EchoTool) as Arc<dyn Tool>])),
        );

        let handle = tool.parent_tools_handle();
        assert_eq!(handle.read().len(), 1);

        // Push a new tool via the handle
        handle.write().push(Arc::new(FakeMcpTool));
        assert_eq!(handle.read().len(), 2);
    }

    // ── Configurable timeout tests ──────────────────────────────────

    #[test]
    fn delegate_timeout_defaults_come_from_delegate_config() {
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_delegate_config(DelegateToolConfig::default());
        assert_eq!(
            tool.delegate_config.timeout_secs,
            DEFAULT_DELEGATE_TIMEOUT_SECS
        );
        assert_eq!(
            tool.delegate_config.agentic_timeout_secs,
            DEFAULT_DELEGATE_AGENTIC_TIMEOUT_SECS
        );
    }

    #[test]
    fn enriched_prompt_omits_shell_policy_without_shell_tool() {
        let config = AliasedAgentConfig::default();

        let tools: Vec<Box<dyn Tool>> = vec![Box::new(EchoTool)];
        let workspace = std::env::temp_dir();

        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_workspace_dir(workspace.to_path_buf());

        let prompt = tool
            .build_enriched_system_prompt(
                "alpha",
                &config,
                "test-model",
                &tools,
                &workspace,
                false,
                None,
            )
            .unwrap();

        assert!(
            !prompt.contains("## Shell Policy"),
            "should not contain shell policy when shell tool is absent"
        );
    }

    #[test]
    fn config_validation_accepts_minimal_agent() {
        let mut config = zeroclaw_config::schema::Config::default();
        // model_provider must reference a real entry under
        // providers.models — the validator (correctly) rejects dangling refs.
        config.providers.models.ollama.insert(
            "default".into(),
            zeroclaw_config::schema::OllamaModelProviderConfig::default(),
        );
        config.risk_profiles.insert(
            "default".into(),
            zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        config.agents.insert(
            "ok".into(),
            AliasedAgentConfig {
                model_provider: "ollama.default".into(),
                risk_profile: "default".into(),
                ..Default::default()
            },
        );
        assert!(
            config.validate().is_ok(),
            "validate: {:?}",
            config.validate()
        );
    }

    #[test]
    fn enriched_prompt_loads_skills_from_scoped_directory() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_skills_test_{}",
            uuid::Uuid::new_v4()
        ));
        let scoped_skills_dir = workspace.join("skills/code-review");
        std::fs::create_dir_all(scoped_skills_dir.join("lint-check")).unwrap();
        std::fs::write(
            scoped_skills_dir.join("lint-check/SKILL.toml"),
            "[skill]\nname = \"lint-check\"\ndescription = \"Run lint checks\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();

        let config = AliasedAgentConfig {
            skill_bundles: vec!["code_review".to_string()],
            ..Default::default()
        };

        let mut skill_bundles = HashMap::new();
        skill_bundles.insert(
            "code_review".to_string(),
            SkillBundleConfig {
                directory: Some("skills/code-review".to_string()),
                ..Default::default()
            },
        );

        let tools: Vec<Box<dyn Tool>> = vec![Box::new(EchoTool)];

        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_skill_bundles(skill_bundles)
            .with_workspace_dir(workspace.clone());

        let prompt = tool
            .build_enriched_system_prompt(
                "alpha",
                &config,
                "test-model",
                &tools,
                &workspace,
                false,
                None,
            )
            .unwrap();

        assert!(
            prompt.contains("lint-check"),
            "should contain skills from scoped directory"
        );

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn enriched_prompt_falls_back_to_default_skills_dir() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_fallback_test_{}",
            uuid::Uuid::new_v4()
        ));
        let default_skills_dir = workspace.join("skills");
        std::fs::create_dir_all(default_skills_dir.join("deploy")).unwrap();
        std::fs::write(
            default_skills_dir.join("deploy/SKILL.toml"),
            "[skill]\nname = \"deploy\"\ndescription = \"Deploy safely\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();

        let config = AliasedAgentConfig::default();

        let tools: Vec<Box<dyn Tool>> = vec![Box::new(EchoTool)];

        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_workspace_dir(workspace.clone());

        let prompt = tool
            .build_enriched_system_prompt(
                "alpha",
                &config,
                "test-model",
                &tools,
                &workspace,
                false,
                None,
            )
            .unwrap();

        assert!(
            prompt.contains("deploy"),
            "should contain skills from default workspace skills/ directory"
        );

        let _ = std::fs::remove_dir_all(workspace);
    }

    // ── Background and Parallel execution tests ─────────────────────

    #[tokio::test]
    async fn background_delegation_returns_task_id() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_bg_test_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());
        let result = tool
            .execute(json!({
                "agent": "researcher",
                "prompt": "test background",
                "background": true
            }))
            .await
            .unwrap();

        // The agent will fail at model_provider level (ollama not running),
        // but the background task should be spawned and return a task_id.
        assert!(result.success);
        assert!(result.output.contains("task_id:"));
        assert!(result.output.contains("Background task started"));

        // Wait a moment for the background task to write its result
        tokio::time::sleep(Duration::from_millis(200)).await;

        // The results directory should exist
        assert!(workspace.join("delegate_results").exists());

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn background_unknown_agent_rejected() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_bg_unknown_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());
        let result = tool
            .execute(json!({
                "agent": "nonexistent",
                "prompt": "test",
                "background": true
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown agent"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn check_result_missing_task_id() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_check_noid_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());
        let result = tool.execute(json!({"action": "check_result"})).await;

        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn check_result_nonexistent_task() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_check_miss_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());
        // Use a valid UUID format that doesn't correspond to any real task
        let fake_uuid = uuid::Uuid::new_v4().to_string();
        let result = tool
            .execute(json!({
                "action": "check_result",
                "task_id": fake_uuid
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("No result found"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn await_sessions_schema_exposes_action_and_inputs() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let schema = tool.parameters_schema();
        let action_enum = schema
            .pointer("/properties/action/enum")
            .and_then(|value| value.as_array())
            .expect("action enum exists");
        assert!(action_enum.iter().any(|value| value == "await_sessions"));
        assert_eq!(
            schema.pointer("/properties/task_ids/maxItems"),
            Some(&json!(DelegateTool::MAX_AWAIT_SESSION_TASK_IDS))
        );
        assert_eq!(
            schema.pointer("/properties/timeout_ms/maximum"),
            Some(&json!(120000))
        );
    }

    #[tokio::test]
    async fn await_sessions_returns_completed_results() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_await_done_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let first = uuid::Uuid::new_v4().to_string();
        let second = uuid::Uuid::new_v4().to_string();
        write_background_result(
            &workspace,
            &background_result(
                &first,
                BackgroundTaskStatus::Completed,
                Some("first output"),
                None,
            ),
        );
        write_background_result(
            &workspace,
            &background_result(
                &second,
                BackgroundTaskStatus::Completed,
                Some("second output"),
                None,
            ),
        );

        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());
        let result = tool
            .execute(json!({
                "action": "await_sessions",
                "task_ids": [first, second],
                "timeout_ms": 0
            }))
            .await
            .unwrap();
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();

        assert!(result.success, "got error: {:?}", result.error);
        assert_eq!(output["status"], "complete");
        assert_eq!(output["completed"], 2);
        assert_eq!(output["results"].as_array().unwrap().len(), 2);
        assert!(result.error.is_none());

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn await_sessions_reports_failed_results() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_await_failed_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let task_id = uuid::Uuid::new_v4().to_string();
        write_background_result(
            &workspace,
            &background_result(
                &task_id,
                BackgroundTaskStatus::Failed,
                None,
                Some("model failed"),
            ),
        );

        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());
        let result = tool
            .execute(json!({
                "action": "await_sessions",
                "task_ids": [task_id],
                "timeout_ms": 0
            }))
            .await
            .unwrap();
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();

        assert!(!result.success);
        assert_eq!(output["status"], "complete");
        assert_eq!(output["failed"].as_array().unwrap().len(), 1);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("failed")
        );

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn await_sessions_times_out_with_pending_results() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_await_pending_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let done = uuid::Uuid::new_v4().to_string();
        let pending = uuid::Uuid::new_v4().to_string();
        write_background_result(
            &workspace,
            &background_result(&done, BackgroundTaskStatus::Completed, Some("done"), None),
        );
        write_background_result(
            &workspace,
            &background_result(&pending, BackgroundTaskStatus::Running, None, None),
        );

        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());
        let result = tool
            .execute(json!({
                "action": "await_sessions",
                "task_ids": [done, pending],
                "timeout_ms": 0
            }))
            .await
            .unwrap();
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();

        assert!(!result.success);
        assert_eq!(output["status"], "timeout");
        assert_eq!(output["completed"], 1);
        assert_eq!(output["pending"].as_array().unwrap().len(), 1);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("pending")
        );

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn await_sessions_reports_missing_tasks() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_await_missing_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let missing = uuid::Uuid::new_v4().to_string();

        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());
        let result = tool
            .execute(json!({
                "action": "await_sessions",
                "task_ids": [missing],
                "timeout_ms": 0
            }))
            .await
            .unwrap();
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();

        assert!(!result.success);
        assert_eq!(output["status"], "timeout");
        assert_eq!(output["missing"].as_array().unwrap().len(), 1);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("missing")
        );

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn await_sessions_rejects_duplicate_task_ids() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_await_duplicate_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let task_id = uuid::Uuid::new_v4().to_string();
        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());
        let result = tool
            .execute(json!({
                "action": "await_sessions",
                "task_ids": [task_id, task_id],
                "timeout_ms": 0
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.output.is_empty());
        assert!(result.error.unwrap().contains("Duplicate task_id"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn await_sessions_rejects_invalid_task_ids() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_await_invalid_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());
        let result = tool
            .execute(json!({
                "action": "await_sessions",
                "task_ids": ["../../../etc/shadow"],
                "timeout_ms": 0
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.output.is_empty());
        assert!(result.error.unwrap().contains("Invalid task_id"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn await_sessions_rejects_too_many_task_ids() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_await_many_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let task_ids: Vec<String> = (0..=DelegateTool::MAX_AWAIT_SESSION_TASK_IDS)
            .map(|_| uuid::Uuid::new_v4().to_string())
            .collect();
        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());
        let result = tool
            .execute(json!({
                "action": "await_sessions",
                "task_ids": task_ids,
                "timeout_ms": 0
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.output.is_empty());
        assert!(result.error.unwrap().contains("no more than"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn await_sessions_rejects_invalid_timeout_ms() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_await_bad_timeout_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let task_id = uuid::Uuid::new_v4().to_string();
        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());
        let result = tool
            .execute(json!({
                "action": "await_sessions",
                "task_ids": [task_id],
                "timeout_ms": "later"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.output.is_empty());
        assert!(
            result
                .error
                .unwrap()
                .contains("'timeout_ms' must be an integer")
        );

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn await_sessions_rejects_timeout_ms_over_cap() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_await_timeout_cap_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let task_id = uuid::Uuid::new_v4().to_string();
        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());
        let result = tool
            .execute(json!({
                "action": "await_sessions",
                "task_ids": [task_id],
                "timeout_ms": 120001
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.output.is_empty());
        assert!(result.error.unwrap().contains("no more than 120000"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn list_results_empty() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_list_empty_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());
        let result = tool
            .execute(json!({"action": "list_results"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("No background delegate results"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn parallel_empty_list_rejected() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool
            .execute(json!({
                "parallel": [],
                "prompt": "test"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("at least one agent"));
    }

    #[tokio::test]
    async fn parallel_unknown_agent_rejected() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool
            .execute(json!({
                "parallel": ["researcher", "nonexistent"],
                "prompt": "test"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown agent"));
    }

    #[tokio::test]
    async fn parallel_missing_prompt_rejected() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool
            .execute(json!({
                "parallel": ["researcher"]
            }))
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn unknown_action_rejected() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool
            .execute(json!({"action": "invalid_action"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn cancel_task_nonexistent() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_cancel_miss_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());
        // Use a valid UUID format that doesn't correspond to any real task
        let fake_uuid = uuid::Uuid::new_v4().to_string();
        let result = tool
            .execute(json!({
                "action": "cancel_task",
                "task_id": fake_uuid
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("No task found"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn cancellation_token_accessor() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let token = tool.cancellation_token();
        assert!(!token.is_cancelled());

        tool.cancel_all_background_tasks();
        assert!(token.is_cancelled());
    }

    #[test]
    fn with_cancellation_token_replaces_default() {
        let custom_token = CancellationToken::new();
        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_cancellation_token(custom_token.clone());

        assert!(!tool.cancellation_token().is_cancelled());
        custom_token.cancel();
        assert!(tool.cancellation_token().is_cancelled());
    }

    #[tokio::test]
    async fn background_task_result_persisted_to_disk() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_bg_persist_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());

        let result = tool
            .execute(json!({
                "agent": "researcher",
                "prompt": "persistence test",
                "background": true
            }))
            .await
            .unwrap();

        assert!(result.success);

        // Extract task_id from output
        let task_id = result
            .output
            .lines()
            .find(|l| l.starts_with("task_id:"))
            .unwrap()
            .trim_start_matches("task_id: ")
            .trim();

        // Check that the result file exists
        let result_path = workspace
            .join("delegate_results")
            .join(format!("{task_id}.json"));
        assert!(
            result_path.exists(),
            "Result file should exist at {result_path:?}"
        );

        // Read and parse the result
        let bg_result = wait_for_terminal_background_result(&workspace, task_id).await;
        assert_eq!(bg_result.task_id, task_id);
        assert_eq!(bg_result.agent, "researcher");
        // The task will have failed because ollama isn't running, but it should be persisted
        assert!(
            bg_result.status == BackgroundTaskStatus::Completed
                || bg_result.status == BackgroundTaskStatus::Failed
        );
        assert!(bg_result.finished_at.is_some());

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn check_result_retrieves_persisted_background_result() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_check_retrieve_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());

        // Start background task
        let result = tool
            .execute(json!({
                "agent": "researcher",
                "prompt": "retrieval test",
                "background": true
            }))
            .await
            .unwrap();

        let task_id = result
            .output
            .lines()
            .find(|l| l.starts_with("task_id:"))
            .unwrap()
            .trim_start_matches("task_id: ")
            .trim()
            .to_string();

        // Wait for background task
        let _ = wait_for_terminal_background_result(&workspace, &task_id).await;

        // Check result
        let check = tool
            .execute(json!({
                "action": "check_result",
                "task_id": task_id
            }))
            .await
            .unwrap();

        // The output should contain the serialized result
        assert!(check.output.contains(&task_id));
        assert!(check.output.contains("researcher"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn list_results_includes_background_tasks() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_list_tasks_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());

        // Start a background task
        let result = tool
            .execute(json!({
                "agent": "researcher",
                "prompt": "list test",
                "background": true
            }))
            .await
            .unwrap();
        assert!(result.success);
        let task_id = result
            .output
            .lines()
            .find(|l| l.starts_with("task_id:"))
            .unwrap()
            .trim_start_matches("task_id: ")
            .trim();

        // Wait for task to complete
        let _ = wait_for_terminal_background_result(&workspace, task_id).await;

        // List results
        let list = tool
            .execute(json!({"action": "list_results"}))
            .await
            .unwrap();

        assert!(list.success);
        assert!(list.output.contains("researcher"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn default_action_is_delegate() {
        // Calling without action should behave like "delegate"
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let result = tool
            .execute(json!({"agent": "researcher", "prompt": "test"}))
            .await
            .unwrap();
        // Should proceed to delegation (will fail at model_provider since ollama isn't running)
        // but should NOT fail with "Unknown action" error
        assert!(
            result.error.is_none()
                || !result
                    .error
                    .as_deref()
                    .unwrap_or("")
                    .contains("Unknown action")
        );
    }

    #[tokio::test]
    async fn check_result_rejects_path_traversal() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_traversal_check_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());
        let result = tool
            .execute(json!({
                "action": "check_result",
                "task_id": "../../etc/passwd"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("Invalid task_id"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[tokio::test]
    async fn cancel_task_rejects_path_traversal() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_delegate_traversal_cancel_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_workspace_dir(workspace.clone());
        let result = tool
            .execute(json!({
                "action": "cancel_task",
                "task_id": "../../../etc/shadow"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("Invalid task_id"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    fn config_with_two_agents(
        caller_alias: &str,
        caller_max_actions: u32,
        target_alias: &str,
        target_max_actions: u32,
    ) -> Arc<zeroclaw_config::schema::Config> {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, RiskProfileConfig, RuntimeProfileConfig,
        };
        let root = std::env::temp_dir().join(format!(
            "zeroclaw-delegate-narrowed-{}",
            uuid::Uuid::new_v4()
        ));
        let mut config = Config {
            data_dir: root.join("data"),
            config_path: root.join("config.toml"),
            ..Config::default()
        };
        // The caller delegates from the `narrow` profile, so that profile must
        // allow delegation before reachability/mode checks run.
        config.risk_profiles.insert(
            "narrow".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                ..RiskProfileConfig::default()
            },
        );
        config
            .risk_profiles
            .insert("wide".to_string(), RiskProfileConfig::default());
        config.runtime_profiles.insert(
            "narrow".to_string(),
            RuntimeProfileConfig {
                max_actions_per_hour: caller_max_actions,
                ..RuntimeProfileConfig::default()
            },
        );
        config.runtime_profiles.insert(
            "wide".to_string(),
            RuntimeProfileConfig {
                max_actions_per_hour: target_max_actions,
                ..RuntimeProfileConfig::default()
            },
        );
        let pick = |above: bool| if above { "wide" } else { "narrow" }.to_string();
        config.agents.insert(
            caller_alias.to_string(),
            AliasedAgentConfig {
                risk_profile: "narrow".into(),
                runtime_profile: "narrow".into(),
                model_provider: "ollama.caller".into(),
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            target_alias.to_string(),
            AliasedAgentConfig {
                risk_profile: pick(target_max_actions > caller_max_actions).into(),
                runtime_profile: pick(target_max_actions > caller_max_actions).into(),
                model_provider: "ollama.target".into(),
                ..AliasedAgentConfig::default()
            },
        );
        Arc::new(config)
    }

    fn config_with_always_ask_delegate(mode: DelegateExecutionMode) -> Arc<Config> {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{RiskProfileConfig, RuntimeProfileConfig};

        let root = std::env::temp_dir().join(format!(
            "zeroclaw-delegate-always-ask-{}",
            uuid::Uuid::new_v4()
        ));
        let mut config = Config {
            data_dir: root.join("data"),
            config_path: root.join("config.toml"),
            ..Config::default()
        };
        config.risk_profiles.insert(
            "caller_profile".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "target_profile".to_string(),
            RiskProfileConfig {
                always_ask: vec![" shell ".to_string(), String::new()],
                ..RiskProfileConfig::default()
            },
        );
        config
            .risk_profiles
            .insert("peer_profile".to_string(), RiskProfileConfig::default());
        config.runtime_profiles.insert(
            "bounded".to_string(),
            RuntimeProfileConfig {
                max_delegation_depth: 3,
                ..RuntimeProfileConfig::default()
            },
        );
        config.agents.insert(
            "caller".to_string(),
            AliasedAgentConfig {
                risk_profile: "caller_profile".into(),
                runtime_profile: "bounded".into(),
                model_provider: "ollama.caller".into(),
                delegates: vec![
                    DelegateTargetConfig {
                        agent: "target".to_string(),
                        mode,
                    },
                    DelegateTargetConfig {
                        agent: "peer".to_string(),
                        mode: DelegateExecutionMode::Independent,
                    },
                ],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "target".to_string(),
            AliasedAgentConfig {
                risk_profile: "target_profile".into(),
                runtime_profile: "bounded".into(),
                model_provider: "ollama.target".into(),
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "peer".to_string(),
            AliasedAgentConfig {
                risk_profile: "peer_profile".into(),
                runtime_profile: "bounded".into(),
                model_provider: "ollama.peer".into(),
                ..AliasedAgentConfig::default()
            },
        );
        Arc::new(config)
    }

    fn delegate_tool_for_config(config: Arc<Config>) -> DelegateTool {
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        DelegateTool::new(config.agents.clone(), None, caller_policy)
            .with_root_config(config)
            .with_caller_alias("caller")
    }

    #[tokio::test]
    async fn independent_delegate_rejects_target_always_ask() {
        // Synchronous path: the runtime must refuse an independent child before
        // the target turn starts, and the refusal must name the operator-facing
        // cause instead of a generic reachability failure.
        let config = config_with_always_ask_delegate(DelegateExecutionMode::Independent);
        let tool = delegate_tool_for_config(config);

        let result = tool
            .execute(json!({
                "agent": "target",
                "prompt": "check the system",
            }))
            .await
            .unwrap();

        let error = result.error.expect("independent always_ask must reject");
        assert!(!result.success);
        assert!(
            error.contains(
                "delegate target \"target\" cannot run in independent mode from \"caller\""
            ),
            "expected target/caller context, got: {error}"
        );
        assert!(
            error.contains("risk profile \"target_profile\" has always_ask entries (shell)"),
            "expected risk profile and trimmed always_ask entries, got: {error}"
        );
        assert!(
            error.contains("ZeroClaw docs, \"Delegation & SubAgents\" > \"What's not supported\""),
            "expected docs section reference, got: {error}"
        );
    }

    #[tokio::test]
    async fn bounded_delegate_does_not_trigger_target_always_ask_guard() {
        // The blocker is scoped to independent mode only. Bounded delegates
        // still use the normal parent-mediated tool path, so this helper must
        // stay silent for the same target/profile pair.
        let config = config_with_always_ask_delegate(DelegateExecutionMode::Bounded);
        let tool = delegate_tool_for_config(config);

        tool.policy_for_target("target")
            .expect("bounded explicit target remains reachable");
        assert!(
            tool.independent_always_ask_refusal("target").is_none(),
            "bounded mode must leave always_ask handling to the normal approval path"
        );
    }

    #[tokio::test]
    async fn background_independent_delegate_rejects_always_ask_before_task_id() {
        // Background admission is observable: returning a task id would imply a
        // child was accepted and may now ask for approval. Refuse before the
        // result file/task-id surface exists.
        let config = config_with_always_ask_delegate(DelegateExecutionMode::Independent);
        let tool = delegate_tool_for_config(config);

        let result = tool
            .execute(json!({
                "agent": "target",
                "prompt": "check the system",
                "background": true,
            }))
            .await
            .unwrap();

        let error = result.error.expect("background always_ask must reject");
        assert!(!result.success);
        assert!(
            error.contains("always_ask entries (shell)"),
            "expected always_ask refusal, got: {error}"
        );
        assert!(
            !result.output.contains("task_id:"),
            "background refusal must not return a task id, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn parallel_independent_delegate_rejects_always_ask_before_spawning() {
        // Parallel fan-out must be all-or-nothing for admission. If any target
        // is independently blocked by always_ask, do not start the other
        // otherwise-valid child.
        let config = config_with_always_ask_delegate(DelegateExecutionMode::Independent);
        let tool = delegate_tool_for_config(config);

        let result = tool
            .execute(json!({
                "parallel": ["peer", "target"],
                "prompt": "check both systems",
            }))
            .await
            .unwrap();

        let error = result.error.expect("parallel always_ask must reject");
        assert!(!result.success);
        assert!(
            error.contains(
                "delegate target \"target\" cannot run in independent mode from \"caller\""
            ),
            "expected target/caller refusal, got: {error}"
        );
        assert!(
            result.output.is_empty(),
            "parallel refusal must happen before fan-out output is built, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn delegate_rejects_cross_profile_target_not_in_roster() {
        // This covers the diagnostic branch where delegate_same_risk_profile is
        // true, but the target differs by profile and lacks an explicit roster
        // entry. The error must tell operators it is a profile mismatch.
        let config = config_with_two_agents("caller", 5, "target", 50);
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let mut delegate_agents = HashMap::new();
        for (name, agent) in &config.agents {
            delegate_agents.insert(name.clone(), agent.clone());
        }
        let tool = DelegateTool::new(delegate_agents, None, caller_policy)
            .with_root_config(config.clone())
            .with_caller_alias("caller");

        let err = tool
            .policy_for_target("target")
            .expect_err("cross-profile target outside the roster must be rejected");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("not reachable"),
            "expected not-reachable rejection, got: {chain}"
        );
        assert!(
            chain.contains("different risk profile"),
            "expected risk-profile mismatch diagnostic, got: {chain}"
        );
        assert!(
            chain.contains("\"narrow\"") && chain.contains("\"wide\""),
            "expected caller and target risk profiles in diagnostic, got: {chain}"
        );
    }

    #[tokio::test]
    async fn delegate_forbidden_policy_reports_caller_and_profile() {
        // Top-level delegation_policy remains the first gate. Its diagnostic
        // should point at the exact risk profile key to edit, before any target
        // reachability details are considered.
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};

        let mut config = (*config_with_two_agents("caller", 5, "target", 5)).clone();
        config
            .risk_profiles
            .get_mut("narrow")
            .unwrap()
            .delegation_policy = DelegationPolicy {
            mode: DelegationMode::Forbidden,
        };
        let config = Arc::new(config);
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let mut delegate_agents = HashMap::new();
        for (name, agent) in &config.agents {
            delegate_agents.insert(name.clone(), agent.clone());
        }
        let tool = DelegateTool::new(delegate_agents, None, caller_policy)
            .with_root_config(config)
            .with_caller_alias("caller");

        let err = tool
            .policy_for_target("target")
            .expect_err("forbidden caller delegation policy must reject before reachability");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("delegation is forbidden for caller \"caller\""),
            "expected caller alias in forbidden-policy diagnostic, got: {chain}"
        );
        assert!(
            chain.contains("risk profile \"narrow\""),
            "expected caller risk profile in forbidden-policy diagnostic, got: {chain}"
        );
        assert!(
            chain.contains("[risk_profiles.narrow].delegation_policy mode = \"allow\""),
            "expected exact remediation path in forbidden-policy diagnostic, got: {chain}"
        );
    }

    #[tokio::test]
    async fn bounded_delegate_allows_explicit_cross_profile_target_that_widens_policy() {
        // Bounded delegation is now tool-bounded rather than policy-bounded:
        // listing the target clears the reachability gate even when the target
        // has a wider runtime policy. Bounded agentic execution applies the
        // parent tool registry ceiling later.
        let config = config_with_two_agents("caller", 5, "target", 50);
        let mut config = (*config).clone();
        config
            .agents
            .get_mut("caller")
            .unwrap()
            .delegates
            .push(DelegateTargetConfig::bounded("target"));
        let config = Arc::new(config);
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let mut delegate_agents = HashMap::new();
        for (name, agent) in &config.agents {
            delegate_agents.insert(name.clone(), agent.clone());
        }
        let tool = DelegateTool::new(delegate_agents, None, caller_policy)
            .with_root_config(config.clone())
            .with_caller_alias("caller");

        let resolved = tool
            .policy_for_target("target")
            .expect("wider cross-profile bounded delegate must resolve");
        assert_eq!(resolved.risk_profile_name, "wide");

        let bucket_key = "bounded-cross-profile-budget-test";
        let max = 2u32;
        for _ in 0..max {
            assert!(
                tool.security.tracker.record_within(bucket_key, max),
                "caller's first {max} actions fit within the shared budget"
            );
        }
        assert!(
            !resolved.tracker.record_within(bucket_key, max),
            "bounded cross-profile delegates must still share the caller's action tracker"
        );
    }

    #[tokio::test]
    async fn delegate_allows_independent_cross_profile_target_that_escalates() {
        // Independent delegation intentionally bypasses the parent's
        // non-escalation ceiling. The target still resolves a normal target-owned
        // policy; it just does not share the caller's exhausted tracker.
        let config = config_with_two_agents("caller", 5, "target", 50);
        let mut config = (*config).clone();
        config
            .agents
            .get_mut("caller")
            .unwrap()
            .delegates
            .push(DelegateTargetConfig {
                agent: "target".to_string(),
                mode: DelegateExecutionMode::Independent,
            });
        let config = Arc::new(config);
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let mut delegate_agents = HashMap::new();
        for (name, agent) in &config.agents {
            delegate_agents.insert(name.clone(), agent.clone());
        }
        let tool = DelegateTool::new(delegate_agents, None, Arc::clone(&caller_policy))
            .with_root_config(config.clone())
            .with_caller_alias("caller");

        let bucket_key = "independent-budget-test";
        let max = 2u32;
        for _ in 0..max {
            assert!(
                caller_policy.tracker.record_within(bucket_key, max),
                "caller's first {max} actions fit within its own budget"
            );
        }

        let resolved = tool
            .policy_for_target("target")
            .expect("independent explicit cross-profile delegate must resolve");
        assert_eq!(resolved.risk_profile_name, "wide");
        assert!(
            resolved.tracker.record_within(bucket_key, max),
            "independent delegate target must not share the caller's exhausted action tracker"
        );
    }

    #[tokio::test]
    async fn delegate_allows_explicit_cross_profile_target_that_narrows() {
        // A bounded explicit delegate may use a different, narrower profile;
        // the caller's filtered tool registry still remains the agentic ceiling.
        let config = config_with_two_agents("caller", 50, "target", 5);
        let mut config = (*config).clone();
        config
            .agents
            .get_mut("caller")
            .unwrap()
            .delegates
            .push(DelegateTargetConfig::bounded("target"));
        let config = Arc::new(config);
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let mut delegate_agents = HashMap::new();
        for (name, agent) in &config.agents {
            delegate_agents.insert(name.clone(), agent.clone());
        }
        let tool = DelegateTool::new(delegate_agents, None, caller_policy)
            .with_root_config(config.clone())
            .with_caller_alias("caller");

        let resolved = tool
            .policy_for_target("target")
            .expect("narrowed explicit cross-profile delegate must resolve");
        assert_eq!(resolved.risk_profile_name, "narrow");
    }

    #[tokio::test]
    async fn delegate_target_inherits_caller_action_tracker() {
        // Baseline bounded behavior: even when caller and target have matching
        // profiles, delegation must not mint a fresh action budget. Independent
        // mode has its own test that intentionally differs from this.
        let config = config_with_two_agents("caller", 5, "target", 5);
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let mut delegate_agents = HashMap::new();
        for (name, agent) in &config.agents {
            delegate_agents.insert(name.clone(), agent.clone());
        }
        let tool = DelegateTool::new(delegate_agents, None, Arc::clone(&caller_policy))
            .with_root_config(config.clone())
            .with_caller_alias("caller");

        let bucket_key = "shared-budget-test";
        let max = 2u32;
        for _ in 0..max {
            assert!(
                caller_policy.tracker.record_within(bucket_key, max),
                "caller's first {max} actions fit within the shared budget"
            );
        }

        let target_policy = tool
            .policy_for_target("target")
            .expect("bounded target resolves");
        assert!(
            !target_policy.tracker.record_within(bucket_key, max),
            "delegated target must consume from the caller's bucket; spawning the target should not reset the budget"
        );
    }

    #[tokio::test]
    async fn delegate_target_inherits_caller_session_workspace_dir() {
        let config = config_with_two_agents("caller", 5, "target", 5);

        // Build the caller's policy the way the interactive builders
        // do: config-derived, then session_cwd override.
        let session_cwd = PathBuf::from("/tmp/zeroclaw-test-delegate-session-cwd-7263");
        let mut caller_policy =
            SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves");
        caller_policy.workspace_dir = session_cwd.clone();
        let caller_policy = Arc::new(caller_policy);

        // Sanity: the target's config-derived workspace must differ so
        // the assertion below is actually exercising the inheritance,
        // not a coincidental match.
        let target_config_workspace = config.agent_workspace_dir("target");
        assert_ne!(
            session_cwd, target_config_workspace,
            "test precondition: session cwd must differ from target's config workspace"
        );

        let mut delegate_agents = HashMap::new();
        for (name, agent) in &config.agents {
            delegate_agents.insert(name.clone(), agent.clone());
        }
        let tool = DelegateTool::new(delegate_agents, None, Arc::clone(&caller_policy))
            .with_root_config(config.clone())
            .with_caller_alias("caller");

        let target_policy = tool
            .policy_for_target("target")
            .expect("same-profile target resolves");
        assert_eq!(
            target_policy.workspace_dir, session_cwd,
            "delegated target must inherit the caller's session cwd; \
             regression for issue #7263"
        );
    }

    #[tokio::test]
    async fn independent_delegate_target_keeps_own_workspace_dir() {
        // Same-profile bounded delegates inherit the caller's session workspace
        // for interactive workflows. Independent delegates act like a fresh run
        // of the target agent, so the target keeps its configured workspace.
        let config = config_with_two_agents("caller", 5, "target", 5);
        let mut config = (*config).clone();
        config
            .agents
            .get_mut("caller")
            .unwrap()
            .delegates
            .push(DelegateTargetConfig {
                agent: "target".to_string(),
                mode: DelegateExecutionMode::Independent,
            });
        let config = Arc::new(config);

        let session_cwd = PathBuf::from("/tmp/zeroclaw-test-independent-delegate-session-cwd");
        let mut caller_policy =
            SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves");
        caller_policy.workspace_dir = session_cwd.clone();
        let caller_policy = Arc::new(caller_policy);

        let target_config_workspace = config.agent_workspace_dir("target");
        assert_ne!(
            session_cwd, target_config_workspace,
            "test precondition: session cwd must differ from target's config workspace"
        );

        let mut delegate_agents = HashMap::new();
        for (name, agent) in &config.agents {
            delegate_agents.insert(name.clone(), agent.clone());
        }
        let tool = DelegateTool::new(delegate_agents, None, Arc::clone(&caller_policy))
            .with_root_config(config.clone())
            .with_caller_alias("caller");

        let target_policy = tool
            .policy_for_target("target")
            .expect("independent same-profile target resolves");
        assert_eq!(
            target_policy.workspace_dir, target_config_workspace,
            "independent delegate target must keep its own configured workspace"
        );
    }

    #[tokio::test]
    async fn independent_delegate_target_uses_target_risk_profile_restrictions() {
        // Independent mode should not be confused with unrestricted mode. It
        // removes the caller ceiling, then applies the target's own policy
        // fields exactly as a fresh target-agent run would.
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{AliasedAgentConfig, Config, RiskProfileConfig};

        let tmp = TempDir::new().unwrap();
        let target_extra_root = tmp.path().join("target-extra-root");
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.risk_profiles.insert(
            "caller".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                allowed_commands: vec!["caller-only".to_string()],
                allowed_roots: vec![tmp.path().join("caller-extra-root").display().to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "target".to_string(),
            RiskProfileConfig {
                allowed_commands: vec!["target-only".to_string()],
                allowed_roots: vec![target_extra_root.display().to_string()],
                forbidden_paths: vec![tmp.path().join("target-forbidden").display().to_string()],
                allowed_tools: vec!["shell".to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.agents.insert(
            "caller".to_string(),
            AliasedAgentConfig {
                risk_profile: "caller".into(),
                model_provider: "ollama.caller".into(),
                delegates: vec![DelegateTargetConfig {
                    agent: "target".to_string(),
                    mode: DelegateExecutionMode::Independent,
                }],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "target".to_string(),
            AliasedAgentConfig {
                risk_profile: "target".into(),
                model_provider: "ollama.target".into(),
                ..AliasedAgentConfig::default()
            },
        );
        let config = Arc::new(config);
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let tool = DelegateTool::new(config.agents.clone(), None, caller_policy)
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller");

        let target_policy = tool
            .policy_for_target("target")
            .expect("independent target policy resolves");

        assert_eq!(target_policy.risk_profile_name, "target");
        assert_eq!(target_policy.allowed_commands, vec!["target-only"]);
        assert!(
            target_policy.allowed_roots.contains(&target_extra_root),
            "target policy must retain target allowed_roots"
        );
        assert!(
            target_policy
                .forbidden_paths
                .iter()
                .any(|path| path.ends_with("target-forbidden")),
            "target policy must retain target forbidden_paths"
        );
        assert_eq!(
            target_policy.allowed_tools.as_deref(),
            Some(&["shell".to_string()][..])
        );
    }

    #[tokio::test]
    async fn bounded_cross_profile_agentic_tools_are_capped_by_parent_registry() {
        // Target asks for `shell`, caller can delegate but only has EchoTool in
        // its registry. Bounded mode must not synthesize target-owned tools
        // just because the target risk profile names them.
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, RiskProfileConfig, RuntimeProfileConfig,
        };

        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.risk_profiles.insert(
            "caller".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                allowed_tools: vec!["echo_tool".to_string(), DelegateTool::NAME.to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "target".to_string(),
            RiskProfileConfig {
                allowed_tools: vec!["shell".to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.runtime_profiles.insert(
            "agentic".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                ..RuntimeProfileConfig::default()
            },
        );
        config.agents.insert(
            "caller".to_string(),
            AliasedAgentConfig {
                risk_profile: "caller".into(),
                model_provider: "ollama.caller".into(),
                delegates: vec![DelegateTargetConfig::bounded("target")],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "target".to_string(),
            AliasedAgentConfig {
                risk_profile: "target".into(),
                runtime_profile: "agentic".into(),
                model_provider: "ollama.target".into(),
                ..AliasedAgentConfig::default()
            },
        );
        let config = Arc::new(config);
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let tool = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));
        let target_config = config
            .agents
            .get("target")
            .expect("target agent exists")
            .clone();

        let result = tool
            .execute_agentic(
                "target",
                &target_config,
                "ollama",
                "test-model",
                &ToolCountModelProvider { expected_tools: 0 },
                "run shell",
                None,
            )
            .await
            .unwrap();

        assert!(result.success, "got: {:?}", result.error);
    }

    #[tokio::test]
    async fn bounded_agentic_tools_are_capped_by_caller_policy() {
        // Stronger ceiling case: EchoTool is present in the parent registry but
        // the caller policy only admits `delegate`, so bounded child tools are
        // empty even though the target profile would allow EchoTool.
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, RiskProfileConfig, RuntimeProfileConfig,
        };

        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.risk_profiles.insert(
            "caller".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                allowed_tools: vec![DelegateTool::NAME.to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "target".to_string(),
            RiskProfileConfig {
                allowed_tools: vec!["echo_tool".to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.runtime_profiles.insert(
            "agentic".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                ..RuntimeProfileConfig::default()
            },
        );
        config.agents.insert(
            "caller".to_string(),
            AliasedAgentConfig {
                risk_profile: "caller".into(),
                model_provider: "ollama.caller".into(),
                delegates: vec![DelegateTargetConfig::bounded("target")],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "target".to_string(),
            AliasedAgentConfig {
                risk_profile: "target".into(),
                runtime_profile: "agentic".into(),
                model_provider: "ollama.target".into(),
                ..AliasedAgentConfig::default()
            },
        );
        let config = Arc::new(config);
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let tool = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_parent_tools(Arc::new(RwLock::new(vec![
                Arc::new(EchoTool),
                Arc::new(DelegateTool::new(HashMap::new(), None, caller_policy)),
            ])));
        let target_config = config
            .agents
            .get("target")
            .expect("target agent exists")
            .clone();

        let result = tool
            .execute_agentic(
                "target",
                &target_config,
                "ollama",
                "test-model",
                &ToolCountModelProvider { expected_tools: 0 },
                "run echo",
                None,
            )
            .await
            .unwrap();

        assert!(result.success, "got: {:?}", result.error);
    }

    #[tokio::test]
    async fn independent_agentic_tools_use_target_registry_not_parent_registry() {
        // Parent registry intentionally contains only EchoTool. Independent
        // agentic delegation must ignore that parent ceiling and build the
        // child loop from the target agent's own allowed tool registry.
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, RiskProfileConfig, RuntimeProfileConfig,
        };

        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.risk_profiles.insert(
            "caller".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                allowed_tools: vec!["echo_tool".to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "target".to_string(),
            RiskProfileConfig {
                allowed_tools: vec!["shell".to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.runtime_profiles.insert(
            "agentic".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                ..RuntimeProfileConfig::default()
            },
        );
        config.agents.insert(
            "caller".to_string(),
            AliasedAgentConfig {
                risk_profile: "caller".into(),
                model_provider: "ollama.caller".into(),
                delegates: vec![DelegateTargetConfig {
                    agent: "target".to_string(),
                    mode: DelegateExecutionMode::Independent,
                }],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "target".to_string(),
            AliasedAgentConfig {
                risk_profile: "target".into(),
                runtime_profile: "agentic".into(),
                model_provider: "ollama.target".into(),
                ..AliasedAgentConfig::default()
            },
        );
        let config = Arc::new(config);
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let tool = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_runtime(Arc::new(DelegateTestRuntime))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));
        let target_policy = tool
            .policy_for_target("target")
            .expect("independent target policy resolves");

        let tools = tool
            .independent_agentic_tools_for_target("target", target_policy)
            .await
            .expect("target-owned registry builds")
            .tools;
        let tool_names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();

        assert!(
            tool_names.contains(&"shell"),
            "independent target must receive tools from its own allowed_tools, got {tool_names:?}"
        );
        assert!(
            !tool_names.contains(&"delegate"),
            "independent agentic delegates must still strip delegate recursion"
        );
        assert!(
            !tool_names.contains(&"echo_tool"),
            "independent target must not inherit parent-only tools"
        );
    }

    #[tokio::test]
    async fn independent_delegate_receives_target_skill_tools() {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, RiskProfileConfig, RuntimeProfileConfig,
        };

        let tmp = TempDir::new().unwrap();
        // A skill with one shell tool, in the TARGET agent's workspace.
        let target_ws = tmp.path().join("target-workspace");
        let skill_dir = target_ws.join("skills").join("pdfify");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.toml"),
            r#"[skill]
name = "pdfify"
description = "test skill for independent-delegate skill wiring"
version = "0.1.0"

[[tools]]
name = "run"
description = "run pdfify"
kind = "shell"
command = "echo hi"
"#,
        )
        .unwrap();

        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.risk_profiles.insert(
            "caller".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                allowed_tools: vec!["echo_tool".to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "target".to_string(),
            RiskProfileConfig {
                allowed_tools: vec!["shell".to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.runtime_profiles.insert(
            "agentic".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                ..RuntimeProfileConfig::default()
            },
        );
        config.agents.insert(
            "caller".to_string(),
            AliasedAgentConfig {
                risk_profile: "caller".into(),
                model_provider: "ollama.caller".into(),
                delegates: vec![DelegateTargetConfig {
                    agent: "target".to_string(),
                    mode: DelegateExecutionMode::Independent,
                }],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "target".to_string(),
            AliasedAgentConfig {
                risk_profile: "target".into(),
                runtime_profile: "agentic".into(),
                model_provider: "ollama.target".into(),
                workspace: zeroclaw_config::multi_agent::AgentWorkspaceConfig {
                    path: Some(target_ws.clone()),
                    ..Default::default()
                },
                ..AliasedAgentConfig::default()
            },
        );
        let config = Arc::new(config);
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let tool = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_runtime(Arc::new(DelegateTestRuntime))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));
        let target_policy = tool
            .policy_for_target("target")
            .expect("independent target policy resolves");

        let independent = tool
            .independent_agentic_tools_for_target("target", target_policy)
            .await
            .expect("target-owned registry builds");
        let names: Vec<String> = independent
            .tools
            .iter()
            .map(|t| t.name().to_string())
            .collect();

        assert!(
            names.iter().any(|n| n == "pdfify__run"),
            "independent delegate must expose the target's skill tools (fails with skills:&[]); got {names:?}"
        );
        // Theinvariants still hold alongside the new skill tools.
        assert!(
            names.iter().any(|n| n == "shell"),
            "target built-in must still be present, got {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "delegate"),
            "delegate must still be stripped (no recursion), got {names:?}"
        );
        // The returned workspace is the TARGET's config workspace - the caller threads it
        // into build_enriched_system_prompt so the skill PROMPT content is built from the
        // same workspace as the skill TOOLS above (not the caller's). Guards against the
        // tools-from-B / prompt-from-A split.
        assert_eq!(
            independent.workspace_dir,
            config.agent_workspace_dir("target"),
            "independent delegate prompt must be built from the target's workspace"
        );
        assert_eq!(
            independent.workspace_dir, target_ws,
            "target workspace must resolve to the configured target-workspace path"
        );
    }

    // Finding: an independent delegate to a non-native, strict-tool-parsing target must
    // suppress the deferred-MCP prompt section exactly as a fresh target turn does
    // (apply_text_tool_prompt_policy clears it), instead of advertising `tool_search`
    // stubs the target cannot use. compose_independent_system_prompt centralizes that.
    #[test]
    fn independent_prompt_respects_text_tool_policy_for_deferred_section() {
        let base = || Some("BASE PROMPT".to_string());
        let deferred = "== DEFERRED MCP: call tool_search ==".to_string();

        // Native provider: deferred section is appended verbatim.
        let native = DelegateTool::compose_independent_system_prompt(
            base(),
            deferred.clone(),
            true, // native_tools
            true, // strict_tool_parsing (ignored when native)
        )
        .unwrap();
        assert!(
            native.contains("BASE PROMPT") && native.contains("DEFERRED MCP"),
            "native target must keep the deferred section, got: {native:?}"
        );

        // Non-native but NOT strict: text tool protocol is exposed, deferred kept.
        let lenient = DelegateTool::compose_independent_system_prompt(
            base(),
            deferred.clone(),
            false, // non-native
            false, // not strict
        )
        .unwrap();
        assert!(
            lenient.contains("DEFERRED MCP"),
            "non-native non-strict target must keep the deferred section, got: {lenient:?}"
        );

        // Non-native AND strict: the fresh-turn policy CLEARS the deferred section, so the
        // delegate prompt must be the base only - no tool_search advertisement.
        let strict = DelegateTool::compose_independent_system_prompt(
            base(),
            deferred.clone(),
            false, // non-native
            true,  // strict
        )
        .unwrap();
        assert_eq!(
            strict, "BASE PROMPT",
            "non-native strict target must NOT get the deferred section, got: {strict:?}"
        );
        assert!(
            !strict.contains("DEFERRED MCP") && !strict.contains("tool_search"),
            "strict delegate prompt must not advertise deferred MCP, got: {strict:?}"
        );

        // Empty deferred section is a no-op regardless of policy.
        assert_eq!(
            DelegateTool::compose_independent_system_prompt(base(), String::new(), false, false),
            base()
        );
        // No base prompt + non-empty deferred (native) becomes the deferred section alone.
        assert_eq!(
            DelegateTool::compose_independent_system_prompt(
                None,
                "ONLY DEFERRED".to_string(),
                true,
                false
            ),
            Some("ONLY DEFERRED".to_string())
        );
    }

    #[tokio::test]
    async fn delegate_without_root_config_falls_back_to_caller_policy() {
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let resolved = tool
            .policy_for_target("researcher")
            .expect("fallback path returns caller policy unchanged");
        assert!(
            Arc::ptr_eq(&resolved, &tool.security),
            "without root_config the helper returns the caller's Arc verbatim"
        );
    }

    /// Build a config where `caller` (`broad` profile) can delegate, but
    /// `target` is a different-profile peer that is not in the explicit
    /// delegate roster. This exercises the reachable-set rejection path.
    fn config_with_narrowed_target() -> Arc<zeroclaw_config::schema::Config> {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{AliasedAgentConfig, Config, RiskProfileConfig};
        let mut config = Config::default();
        config.risk_profiles.insert(
            "broad".to_string(),
            RiskProfileConfig {
                allowed_commands: vec!["git".into(), "cargo".into()],
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "narrow".to_string(),
            RiskProfileConfig {
                allowed_commands: vec!["git".into()],
                ..RiskProfileConfig::default()
            },
        );
        config.agents.insert(
            "caller".to_string(),
            AliasedAgentConfig {
                risk_profile: "broad".into(),
                model_provider: "ollama.caller".into(),
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "target".to_string(),
            AliasedAgentConfig {
                risk_profile: "narrow".into(),
                model_provider: "ollama.target".into(),
                ..AliasedAgentConfig::default()
            },
        );
        Arc::new(config)
    }

    #[tokio::test]
    async fn delegate_rejects_cross_profile_target_absent_from_roster_even_when_authorized() {
        // Caller is authorized to delegate (delegation_policy = allow) and
        // the target is on a narrower profile, but it is not listed in the
        // caller's delegates roster and is not a same-profile peer, so the
        // reachability gate must refuse.
        let config = config_with_narrowed_target();
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let mut delegate_agents = HashMap::new();
        for (name, agent) in &config.agents {
            delegate_agents.insert(name.clone(), agent.clone());
        }
        let tool = DelegateTool::new(delegate_agents, None, caller_policy)
            .with_root_config(config.clone())
            .with_caller_alias("caller");

        let err = tool
            .policy_for_target("target")
            .expect_err("cross-profile target outside the roster must be rejected");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("not reachable"),
            "expected not-reachable rejection, got: {chain}"
        );
        assert!(
            chain.contains("different risk profile"),
            "expected risk-profile mismatch diagnostic, got: {chain}"
        );
        assert!(
            chain.contains("\"broad\"") && chain.contains("\"narrow\""),
            "expected caller and target risk profiles in diagnostic, got: {chain}"
        );
    }

    #[tokio::test]
    async fn delegate_builds_target_provider_with_its_declared_wire_api() {
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, CustomModelProviderConfig, ModelProviderConfig, WireApi,
        };
        let mut config = Config::default();
        config.providers.models.custom.insert(
            "vllm".to_string(),
            CustomModelProviderConfig {
                base: ModelProviderConfig {
                    uri: Some("http://10.0.0.15:8000/v1".to_string()),
                    model: Some("Qwen3.6-27B".to_string()),
                    wire_api: Some(WireApi::Responses),
                    ..ModelProviderConfig::default()
                },
            },
        );
        config.agents.insert(
            "target".to_string(),
            AliasedAgentConfig {
                model_provider: "custom.vllm".into(),
                ..AliasedAgentConfig::default()
            },
        );
        let config = Arc::new(config);

        let tool = DelegateTool::new(sample_agents(), None, test_security())
            .with_root_config(Arc::clone(&config));

        // Drives the exact build path `run` takes. With root_config + a
        // dotted model_provider, the alias-aware factory must read the
        // target's `custom.vllm` entry and honor wire_api = responses.
        let provider = tool
            .build_target_provider("custom.vllm", "custom", None)
            .expect("target provider builds offline");
        assert_eq!(
            provider.default_wire_api(),
            "responses",
            "delegate must build the target with its declared responses wire API"
        );

        let stale = zeroclaw_providers::create_model_provider_with_options(
            "custom",
            None,
            &tool.provider_runtime_options,
        );
        let stale_is_responses = stale
            .map(|p| p.default_wire_api() == "responses")
            .unwrap_or(false);
        assert!(
            !stale_is_responses,
            "bare factory must NOT yield a responses provider — proves the alias path is load-bearing"
        );
    }

    struct FileReadTool;
    #[async_trait]
    impl Tool for FileReadTool {
        fn name(&self) -> &str {
            "file_read"
        }
        fn description(&self) -> &str {
            "Read a file."
        }
        fn parameters_schema(&self) -> serde_json::Value {
            json!({"type": "object", "properties": {}})
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: "read".into(),
                error: None,
            })
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for FileReadTool {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Tool(::zeroclaw_api::attribution::ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            <Self as Tool>::name(self)
        }
    }

    struct FileWriteTool;
    #[async_trait]
    impl Tool for FileWriteTool {
        fn name(&self) -> &str {
            "file_write"
        }
        fn description(&self) -> &str {
            "Write a file."
        }
        fn parameters_schema(&self) -> serde_json::Value {
            json!({"type": "object", "properties": {}})
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: "written".into(),
                error: None,
            })
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for FileWriteTool {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Tool(::zeroclaw_api::attribution::ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            <Self as Tool>::name(self)
        }
    }

    struct MockShellTool;
    #[async_trait]
    impl Tool for MockShellTool {
        fn name(&self) -> &str {
            "shell"
        }
        fn description(&self) -> &str {
            "Execute shell commands."
        }
        fn parameters_schema(&self) -> serde_json::Value {
            json!({"type": "object", "properties": {}})
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: ToolOutput::default(),
                error: None,
            })
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for MockShellTool {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Tool(::zeroclaw_api::attribution::ToolKind::Shell)
        }
        fn alias(&self) -> &str {
            <Self as Tool>::name(self)
        }
    }

    struct ToolListInspector {
        forbidden_names: Vec<String>,
    }
    #[async_trait]
    impl ModelProvider for ToolListInspector {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("unused".into())
        }
        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            if let Some(tools) = request.tools {
                for tool in tools {
                    if self.forbidden_names.iter().any(|f| f == &tool.name) {
                        return Ok(ChatResponse {
                            text: Some(format!("forbidden_tool_seen:{}", tool.name)),
                            tool_calls: Vec::new(),
                            usage: None,
                            reasoning_content: None,
                        });
                    }
                }
            }
            Ok(ChatResponse {
                text: Some("done".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
        fn supports_native_tools(&self) -> bool {
            true
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for ToolListInspector {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ToolListInspector"
        }
    }

    #[tokio::test]
    async fn delegate_filters_parent_tools_through_parent_policy() {
        let config = agentic_agent_config();
        let parent_security = Arc::new(SecurityPolicy {
            allowed_tools: Some(vec!["file_read".to_string(), "delegate".to_string()]),
            ..SecurityPolicy::default()
        });
        let tool = DelegateTool::new(HashMap::new(), None, parent_security)
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(Vec::new()))
            .with_parent_tools(Arc::new(RwLock::new(vec![
                Arc::new(FileReadTool),
                Arc::new(FileWriteTool),
            ])));

        let model_provider = ToolListInspector {
            forbidden_names: vec!["file_write".to_string()],
        };
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &model_provider,
                "run",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "expected success, got error: {:?}",
            result.error
        );
        assert!(
            result.output.contains("done"),
            "expected output to contain 'done', got: {}",
            result.output
        );
        assert!(
            !result.output.contains("forbidden_tool_seen"),
            "parent policy should have filtered out file_write, but got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn delegate_honors_parent_excluded_tools() {
        let config = agentic_agent_config();
        let parent_security = Arc::new(SecurityPolicy {
            excluded_tools: Some(vec!["shell".to_string()]),
            ..SecurityPolicy::default()
        });
        let tool = DelegateTool::new(HashMap::new(), None, parent_security)
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(vec![
                "shell".to_string(),
                "file_read".to_string(),
            ]))
            .with_parent_tools(Arc::new(RwLock::new(vec![
                Arc::new(MockShellTool),
                Arc::new(FileReadTool),
            ])));

        let model_provider = ToolListInspector {
            forbidden_names: vec!["shell".to_string()],
        };
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &model_provider,
                "run",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "expected success, got error: {:?}",
            result.error
        );
        assert!(
            result.output.contains("done"),
            "expected output to contain 'done', got: {}",
            result.output
        );
        assert!(
            !result.output.contains("forbidden_tool_seen"),
            "parent excluded_tools should have filtered out shell, but got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn delegate_parent_none_unrestricted_passes_target_policy() {
        let config = agentic_agent_config();
        let parent_security = Arc::new(SecurityPolicy {
            allowed_tools: None,
            ..SecurityPolicy::default()
        });
        let tool = DelegateTool::new(HashMap::new(), None, parent_security)
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(vec!["file_read".to_string()]))
            .with_parent_tools(Arc::new(RwLock::new(vec![
                Arc::new(FileReadTool),
                Arc::new(FileWriteTool),
            ])));

        let model_provider = ToolListInspector {
            forbidden_names: vec!["file_write".to_string()],
        };
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &model_provider,
                "run",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "expected success, got error: {:?}",
            result.error
        );
        assert!(
            result.output.contains("done"),
            "expected output to contain 'done', got: {}",
            result.output
        );
        assert!(
            !result.output.contains("forbidden_tool_seen"),
            "target policy should have filtered out file_write, but got: {}",
            result.output
        );
    }

    #[test]
    fn resolve_brain_oauth_target_returns_none_credential() {
        let mut providers_models: HashMap<String, HashMap<String, ModelProviderConfig>> =
            HashMap::new();
        let mut oauth_map = HashMap::new();
        oauth_map.insert(
            "codex".to_string(),
            ModelProviderConfig {
                requires_openai_auth: true,
                api_key: None,
                model: Some("gpt-4".to_string()),
                ..ModelProviderConfig::default()
            },
        );
        providers_models.insert("openai".to_string(), oauth_map);

        let tool = DelegateTool::new(
            HashMap::new(),
            Some("sk-ant-global-coordinator-key".to_string()),
            Arc::new(SecurityPolicy::default()),
        )
        .with_providers_models(providers_models);

        let (provider_type, credential, model, _) = tool.resolve_brain("openai.codex");
        assert_eq!(provider_type, "openai");
        assert!(
            credential.is_none(),
            "OAuth target must not inherit global coordinator credential"
        );
        assert_eq!(model, "gpt-4");
    }

    #[test]
    fn resolve_brain_oauth_target_preserves_explicit_alias_key() {
        let mut providers_models: HashMap<String, HashMap<String, ModelProviderConfig>> =
            HashMap::new();
        let mut oauth_map = HashMap::new();
        oauth_map.insert(
            "codex".to_string(),
            ModelProviderConfig {
                requires_openai_auth: true,
                api_key: Some("sk-codex-custom-gateway-key".to_string()),
                model: Some("gpt-4".to_string()),
                ..ModelProviderConfig::default()
            },
        );
        providers_models.insert("openai".to_string(), oauth_map);

        let tool = DelegateTool::new(
            HashMap::new(),
            Some("sk-ant-global-coordinator-key".to_string()),
            Arc::new(SecurityPolicy::default()),
        )
        .with_providers_models(providers_models);

        let (_provider_type, credential, _model, _) = tool.resolve_brain("openai.codex");
        assert_eq!(
            credential.as_deref(),
            Some("sk-codex-custom-gateway-key"),
            "OAuth target with explicit api_key must preserve the alias key"
        );
    }

    #[test]
    fn resolve_brain_non_oauth_fallback_preserved() {
        let mut providers_models: HashMap<String, HashMap<String, ModelProviderConfig>> =
            HashMap::new();
        let mut custom_map = HashMap::new();
        custom_map.insert(
            "local".to_string(),
            ModelProviderConfig {
                requires_openai_auth: false,
                api_key: None,
                model: Some("llama3".to_string()),
                ..ModelProviderConfig::default()
            },
        );
        providers_models.insert("custom".to_string(), custom_map);

        let tool = DelegateTool::new(
            HashMap::new(),
            Some("sk-ant-global-coordinator-key".to_string()),
            Arc::new(SecurityPolicy::default()),
        )
        .with_providers_models(providers_models);

        let (_provider_type, credential, _model, _) = tool.resolve_brain("custom.local");
        assert_eq!(
            credential.as_deref(),
            Some("sk-ant-global-coordinator-key"),
            "non-OAuth target without api_key must fall back to global credential"
        );
    }
}
